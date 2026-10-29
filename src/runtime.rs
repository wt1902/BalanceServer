//! HTTP/1 transport deadlines matching net/http.Server's socket deadlines.
use axum::{Router, body::Body, extract::ConnectInfo};
use http_body::{Body as HttpBody, Frame, SizeHint};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
    time::{Instant, Sleep},
};
use tower::ServiceExt;

#[derive(Clone, Copy)]
pub struct Timeouts {
    pub header: Duration,
    pub read: Duration,
    pub write: Duration,
    pub idle: Duration,
    pub shutdown: Duration,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self {
            header: Duration::from_secs(5),
            read: Duration::from_secs(15),
            write: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            shutdown: Duration::from_secs(10),
        }
    }
}

struct Deadlines {
    started: Instant,
    read: Option<Instant>,
    write: Option<Instant>,
    idle: bool,
    response_done: bool,
}
struct DeadlineIo {
    stream: TcpStream,
    shared: Arc<Mutex<Deadlines>>,
    timeouts: Timeouts,
    read_timer: Pin<Box<Sleep>>,
    write_timer: Pin<Box<Sleep>>,
}
fn expired(timer: &mut Pin<Box<Sleep>>, deadline: Option<Instant>, cx: &mut Context<'_>) -> bool {
    if let Some(deadline) = deadline {
        timer.as_mut().reset(deadline);
        timer.as_mut().poll(cx).is_ready()
    } else {
        false
    }
}
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "HTTP connection deadline exceeded")
}
impl AsyncRead for DeadlineIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let deadline = self.shared.lock().unwrap().read;
        if expired(&mut self.read_timer, deadline, cx) {
            return Poll::Ready(Err(timed_out()));
        }
        let before = buf.filled().len();
        let result = Pin::new(&mut self.stream).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            let mut state = self.shared.lock().unwrap();
            if state.idle {
                state.idle = false;
                state.started = Instant::now();
                state.read = Some(state.started + self.timeouts.header);
                cx.waker().wake_by_ref();
            }
        }
        result
    }
}
impl AsyncWrite for DeadlineIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let deadline = self.shared.lock().unwrap().write;
        if expired(&mut self.write_timer, deadline, cx) {
            return Poll::Ready(Err(timed_out()));
        }
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let deadline = self.shared.lock().unwrap().write;
        if expired(&mut self.write_timer, deadline, cx) {
            return Poll::Ready(Err(timed_out()));
        }
        let result = Pin::new(&mut self.stream).poll_flush(cx);
        if matches!(result, Poll::Ready(Ok(()))) {
            let mut state = self.shared.lock().unwrap();
            if state.response_done {
                state.write = None;
                state.read = Some(Instant::now() + self.timeouts.idle);
                state.idle = true;
                state.response_done = false;
                cx.waker().wake_by_ref();
            }
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
struct ResponseBody {
    inner: Body,
    shared: Arc<Mutex<Deadlines>>,
}
impl HttpBody for ResponseBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let frame = Pin::new(&mut self.inner).poll_frame(cx);
        if frame.is_ready() && self.inner.is_end_stream() {
            self.shared.lock().unwrap().response_done = true;
        }
        frame
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

async fn connection(
    stream: TcpStream,
    peer: SocketAddr,
    app: Router,
    timeouts: Timeouts,
    mut shutdown: watch::Receiver<bool>,
) {
    let now = Instant::now();
    let state = Arc::new(Mutex::new(Deadlines {
        started: now,
        read: Some(now + timeouts.header),
        write: None,
        idle: false,
        response_done: false,
    }));
    let io = DeadlineIo {
        stream,
        shared: Arc::clone(&state),
        timeouts,
        read_timer: Box::pin(tokio::time::sleep(timeouts.header)),
        write_timer: Box::pin(tokio::time::sleep(timeouts.write)),
    };
    let service = service_fn(move |mut request: hyper::Request<hyper::body::Incoming>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        let app = app.clone();
        let state = Arc::clone(&state);
        {
            let mut deadlines = state.lock().unwrap();
            if deadlines.idle {
                deadlines.started = Instant::now();
            }
            deadlines.idle = false;
            deadlines.response_done = false;
            deadlines.read = Some(deadlines.started + timeouts.read);
            deadlines.write = Some(Instant::now() + timeouts.write);
        }
        async move {
            let response = app.oneshot(request.map(Body::new)).await?;
            let (parts, body) = response.into_parts();
            if body.is_end_stream() {
                state.lock().unwrap().response_done = true;
            }
            Ok::<_, std::convert::Infallible>(hyper::Response::from_parts(
                parts,
                ResponseBody {
                    inner: body,
                    shared: state,
                },
            ))
        }
    });
    let conn = http1::Builder::new().serve_connection(TokioIo::new(io), service);
    tokio::pin!(conn);
    tokio::select! {
        _ = &mut conn => {},
        _ = shutdown.changed() => { conn.as_mut().graceful_shutdown(); let _ = conn.await; }
    }
}

/// Go's empty host listens on both IP families where IPv6 is available.
pub async fn bind(address: &str) -> io::Result<TcpListener> {
    if address.starts_with(':') {
        match TcpListener::bind(format!("[::]{address}")).await {
            Ok(listener) => Ok(listener),
            Err(_) => TcpListener::bind(format!("0.0.0.0{address}")).await,
        }
    } else {
        TcpListener::bind(address).await
    }
}

pub async fn serve(
    listener: TcpListener,
    app: Router,
    timeouts: Timeouts,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    let (stop, _) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            finished = connections.join_next(), if !connections.is_empty() => { let _ = finished; },
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                stream.set_nodelay(true)?;
                connections.spawn(connection(stream, peer, app.clone(), timeouts, stop.subscribe()));
            }
        }
    }
    drop(listener);
    let _ = stop.send(true);
    if tokio::time::timeout(timeouts.shutdown, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::oneshot,
    };

    fn limits() -> Timeouts {
        Timeouts {
            header: Duration::from_millis(90),
            read: Duration::from_millis(220),
            write: Duration::from_millis(300),
            idle: Duration::from_millis(500),
            shutdown: Duration::from_millis(120),
        }
    }
    async fn start(
        app: Router,
    ) -> (
        SocketAddr,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve(listener, app, limits(), async {
            let _ = stopped.await;
        }));
        (addr, stop, task)
    }
    async fn closed(stream: &mut TcpStream) {
        let mut data = [0; 4096];
        tokio::time::timeout(Duration::from_secs(2), async {
            match stream.read(&mut data).await {
                Ok(0) | Err(_) => {}
                Ok(count) => panic!(
                    "unexpected response before connection closed: {:?}",
                    &data[..count]
                ),
            }
        })
        .await
        .expect("connection deadline was not enforced");
    }
    async fn response(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            bytes.push(
                tokio::time::timeout(Duration::from_secs(2), stream.read_u8())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let header = String::from_utf8(bytes).unwrap();
        let len: usize = header
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|s| s.trim().parse().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; len];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(body, b"ok");
        header
    }

    #[tokio::test]
    async fn incomplete_headers_expire_from_connection_start() {
        let (addr, stop, task) = start(Router::new().route("/", get(|| async { "ok" }))).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\nHost: ").await.unwrap();
        let started = Instant::now();
        closed(&mut stream).await;
        assert!(started.elapsed() < Duration::from_millis(500));
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn body_deadline_includes_header_time_and_is_not_reset_by_progress() {
        let app = Router::new().route(
            "/",
            post(|body: axum::body::Bytes| async move {
                assert!(body.len() < 10);
                "ok"
            }),
        );
        let (addr, stop, task) = start(app).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let started = Instant::now();
        stream
            .write_all(b"POST / HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        stream
            .write_all(b"Content-Length: 10\r\n\r\na")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        stream.write_all(b"b").await.unwrap();
        // Axum reports a body-read error, which may generate an HTTP error
        // response; drain it and wait for the read deadline to close the socket.
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_millis(400));
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn idle_deadline_is_longer_than_header_and_next_headers_get_own_deadline() {
        let (addr, stop, task) = start(Router::new().route("/", get(|| async { "ok" }))).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        assert!(response(&mut stream).await.starts_with("HTTP/1.1 200"));
        tokio::time::sleep(Duration::from_millis(180)).await;
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        assert!(response(&mut stream).await.starts_with("HTTP/1.1 200"));
        tokio::time::sleep(Duration::from_millis(150)).await;
        stream.write_all(b"GET / HTTP/1.1\r\nHost: ").await.unwrap();
        let started = Instant::now();
        closed(&mut stream).await;
        assert!(started.elapsed() < Duration::from_millis(250));
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn idle_keepalive_eventually_closes_without_another_request() {
        let (addr, stop, task) = start(Router::new().route("/", get(|| async { "ok" }))).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        response(&mut stream).await;
        let started = Instant::now();
        closed(&mut stream).await;
        assert!(started.elapsed() >= Duration::from_millis(450));
        assert!(started.elapsed() < Duration::from_millis(800));
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn responses_cannot_be_written_after_write_deadline() {
        let app = Router::new().route(
            "/",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(350)).await;
                "ok"
            }),
        );
        let (addr, stop, task) = start(app).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        closed(&mut stream).await;
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_drains_active_requests_and_bounds_stuck_handlers() {
        let (entered, notified) = oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let app = Router::new().route(
            "/",
            get(move || {
                let entered = Arc::clone(&entered);
                async move {
                    if let Some(entered) = entered.lock().unwrap().take() {
                        let _ = entered.send(());
                    }
                    std::future::pending::<()>().await;
                    "ok"
                }
            }),
        );
        let (addr, stop, task) = start(app).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        notified.await.unwrap();
        let started = Instant::now();
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_millis(400));
        assert!(TcpStream::connect(addr).await.is_err());
        closed(&mut stream).await;
    }

    #[tokio::test]
    async fn shutdown_allows_in_flight_response_to_complete() {
        let (entered, notified) = oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let app = Router::new().route(
            "/",
            get(move || {
                let entered = Arc::clone(&entered);
                async move {
                    if let Some(entered) = entered.lock().unwrap().take() {
                        let _ = entered.send(());
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    "ok"
                }
            }),
        );
        let (addr, stop, task) = start(app).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        notified.await.unwrap();
        stop.send(()).unwrap();
        assert!(response(&mut stream).await.starts_with("HTTP/1.1 200"));
        task.await.unwrap().unwrap();
        closed(&mut stream).await;
    }
    #[tokio::test]
    async fn address_binding_supports_hostnames_ip_literals_and_empty_host() {
        for address in ["127.0.0.1:0", "localhost:0", ":0"] {
            let listener = bind(address).await.unwrap();
            let local = listener.local_addr().unwrap();
            assert_ne!(local.port(), 0);
            if address == ":0" {
                let client = TcpStream::connect(("127.0.0.1", local.port()))
                    .await
                    .unwrap();
                let _ = listener.accept().await.unwrap();
                drop(client);
            }
        }
        // The IPv6 syntax remains accepted on hosts with an IPv6 loopback.
        match bind("[::1]:0").await {
            Ok(listener) => assert!(listener.local_addr().unwrap().is_ipv6()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
                ) => {}
            Err(error) => panic!("IPv6 listener: {error}"),
        }
    }
    #[tokio::test]
    async fn blocked_socket_writes_expire_without_waiting_for_client_reads() {
        const LENGTH: usize = 16 * 1024 * 1024;
        let app = Router::new().route("/", get(|| async { "x".repeat(LENGTH) }));
        let (addr, stop, task) = start(app).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        // A response larger than the socket buffer must eventually block.
        // The write deadline must wake that pending write and close the
        // connection even though the client has not consumed any response.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert!(received.starts_with(b"HTTP/1.1 200"));
        assert!(received.len() < LENGTH);
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }
}
