use crate::{auth, config::Config, sync};
use axum::{
    body::{Body, HttpBody},
    extract::{ConnectInfo, State as ExtractState},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::Response,
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{
    Deserializer,
    de::{self, MapAccess, Visitor},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

pub struct State {
    cfg: Config,
    db: Mutex<Connection>,
    tokens: auth::TokenManager,
    clients: Mutex<HashMap<String, (Instant, u64)>>,
}
impl State {
    pub fn new(cfg: Config, db: Mutex<Connection>) -> Self {
        Self {
            tokens: auth::TokenManager::new(&cfg.jwt_secret, cfg.access_ttl),
            cfg,
            db,
            clients: Mutex::new(HashMap::new()),
        }
    }
}

pub async fn handle(
    ExtractState(state): ExtractState<Arc<State>>,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let path = parts.uri.path().to_owned();
    let method = parts.method;
    let query = parts.uri.query().map(str::to_owned);
    let canonical = clean_path(&path, true);
    let host = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|v| v.0.ip().to_string())
        .unwrap_or_default();
    let is_head = method == Method::HEAD;
    let mut response = if method == Method::OPTIONS {
        empty(204)
    } else if method != Method::CONNECT && canonical != path {
        let location = match &query {
            Some(q) => format!("{canonical}?{q}"),
            None => canonical,
        };
        redirect(&location, &method)
    } else {
        // Bound memory independently of Content-Length, including chunked requests.
        let body = read_body(body).await;
        let state = state.clone();
        let headers = headers.clone();
        let escaped_slash = path.to_ascii_lowercase().contains("%2f");
        let path = percent_decode(&path);
        match tokio::task::spawn_blocking(move || {
            // ServeMux unescapes individual segments; an escaped slash cannot
            // turn one segment into the separators of a protected API route.
            if escaped_slash {
                if method == Method::GET || method == Method::HEAD {
                    return web(&path, &headers);
                }
                let mut r = plain(405, "Method Not Allowed\n");
                r.headers_mut()
                    .insert("Allow", HeaderValue::from_static("GET, HEAD"));
                return r;
            }
            dispatch(&state, &method, &path, &headers, body.as_deref(), &host)
        })
        .await
        {
            Ok(response) => response,
            Err(_) => error(500, "internal_error", "Internal server error"),
        }
    };
    let local_location = match (
        &query,
        response
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok()),
    ) {
        (Some(q), Some(location))
            if (response.status() == StatusCode::MOVED_PERMANENTLY
                || response.status() == StatusCode::TEMPORARY_REDIRECT)
                && !location.contains('?') =>
        {
            Some(format!("{location}?{q}"))
        }
        _ => None,
    };
    if let Some(value) = local_location.and_then(|v| HeaderValue::from_str(&v).ok()) {
        response.headers_mut().insert("Location", value);
    }
    let h = response.headers_mut();
    h.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    h.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    h.insert(
        "Permissions-Policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
    h.insert("Content-Security-Policy", HeaderValue::from_static("default-src 'self'; base-uri 'none'; connect-src 'self'; font-src 'self'; form-action 'self'; frame-ancestors 'none'; img-src 'self' data:; manifest-src 'self'; script-src 'self'; style-src 'self'; worker-src 'self'"));
    if percent_decode(&path).starts_with("/v1/") {
        h.insert("Cache-Control", HeaderValue::from_static("no-store"));
    }
    let origin = header(&headers, "origin");
    if !origin.is_empty()
        && state
            .cfg
            .cors_origins
            .iter()
            .any(|v| v == origin || v == "*")
    {
        if let Ok(value) = HeaderValue::from_str(origin) {
            h.insert("Access-Control-Allow-Origin", value);
        }
        h.insert("Vary", HeaderValue::from_static("Origin"));
        h.insert(
            "Access-Control-Allow-Headers",
            HeaderValue::from_static("Authorization, Content-Type"),
        );
        h.insert(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
    }
    if is_head {
        *response.body_mut() = Body::empty();
    }
    response
}

// Go's decoder consumes one value, so a valid first value may be followed by
// ignored data beyond MaxBytesReader's cap. Keep the bounded prefix available.
async fn read_body(mut body: Body) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    while bytes.len() < 2 << 20 {
        let frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await;
        let Some(frame) = frame else {
            break;
        };
        let frame = frame.map_err(|_| ())?;
        if let Ok(data) = frame.into_data() {
            let remaining = (2 << 20) - bytes.len();
            bytes.extend_from_slice(&data[..data.len().min(remaining)]);
        }
    }
    Ok(bytes)
}

fn dispatch(
    state: &State,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: Result<&[u8], &()>,
    host: &str,
) -> Response {
    let post_route = matches!(
        path,
        "/v1/auth/register"
            | "/v1/auth/login"
            | "/v1/auth/refresh"
            | "/v1/auth/logout"
            | "/v1/sync"
    );
    if method != Method::GET && method != Method::HEAD && !(method == Method::POST && post_route) {
        let mut r = plain(405, "Method Not Allowed\n");
        r.headers_mut().insert(
            "Allow",
            HeaderValue::from_static(if post_route {
                "GET, HEAD, POST"
            } else {
                "GET, HEAD"
            }),
        );
        return r;
    }
    if method != Method::POST {
        return match path {
            "/health" => json_response(200, json!({"status":"ok", "time":timestamp(Utc::now())})),
            "/v1/me" => match authorize(state, headers) {
                Ok(c) => json_response(200, json!({"id":c.subject,"email":c.email})),
                Err(r) => *r,
            },
            _ => web(path, headers),
        };
    }
    if matches!(
        path,
        "/v1/auth/register" | "/v1/auth/login" | "/v1/auth/refresh"
    ) {
        let now = Instant::now();
        let mut clients = state.clients.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = clients.entry(host.to_owned()).or_insert((now, 0));
        if now.duration_since(bucket.0) >= Duration::from_secs(60) {
            *bucket = (now, 0);
        }
        bucket.1 = bucket.1.saturating_add(1);
        if bucket.1 > 30 {
            let mut r = error(429, "rate_limited", "Too many authentication requests");
            r.headers_mut()
                .insert("Retry-After", HeaderValue::from_static("60"));
            return r;
        }
    }
    let claims = if path == "/v1/sync" {
        match authorize(state, headers) {
            Ok(c) => Some(c),
            Err(r) => return *r,
        }
    } else {
        None
    };
    if path == "/v1/auth/register" && !state.cfg.allow_registration {
        return error(403, "registration_disabled", "Registration is disabled");
    }
    let Ok(body) = body else {
        return invalid_json();
    };
    if let Some(claims) = claims {
        return match sync::synchronize_wire(
            &mut state.db.lock().unwrap_or_else(|e| e.into_inner()),
            &claims.subject,
            body,
        ) {
            Ok(value) => raw_json_response(200, value),
            Err((status, code, message)) => error(status, code, &message),
        };
    }
    let request = match decode(
        body,
        path == "/v1/auth/register" || path == "/v1/auth/login",
    ) {
        Ok(r) => r,
        Err(_) => return invalid_json(),
    };
    match path {
        "/v1/auth/register" => register(state, headers, request),
        "/v1/auth/login" => login(state, headers, request),
        "/v1/auth/refresh" => refresh(state, headers, request),
        "/v1/auth/logout" => {
            let token = refresh_token(headers, &request.refresh_token);
            if !token.is_empty() {
                let _ = state.db.lock().unwrap_or_else(|e| e.into_inner()).execute("UPDATE refresh_tokens SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL", params![timestamp(Utc::now()), auth::token_hash(&token)]);
            }
            let mut r = empty(204);
            for name in ["balance_access", "balance_refresh"] {
                append_cookie(
                    &mut r,
                    name,
                    "",
                    DateTime::from_timestamp(1, 0).unwrap(),
                    -1,
                    headers,
                );
            }
            r
        }
        _ => error(404, "not_found", "Endpoint not found"),
    }
}

#[derive(Default)]
struct Input {
    email: String,
    password: String,
    refresh_token: String,
}
fn decode(body: &[u8], credentials: bool) -> Result<Input, serde_json::Error> {
    struct InputVisitor(bool);
    impl<'de> Visitor<'de> for InputVisitor {
        type Value = Input;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an object")
        }
        fn visit_unit<E: de::Error>(self) -> Result<Input, E> {
            Ok(Input::default())
        }
        fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<Input, M::Error> {
            let mut r = Input::default();
            while let Some(key) = m.next_key::<String>()? {
                let destination = match key
                    .chars()
                    .map(|c| match c {
                        'ſ' => 's',
                        'K' => 'k',
                        _ => c.to_ascii_lowercase(),
                    })
                    .collect::<String>()
                    .as_str()
                {
                    "email" if self.0 => &mut r.email,
                    "password" if self.0 => &mut r.password,
                    "refreshtoken" if !self.0 => &mut r.refresh_token,
                    _ => return Err(de::Error::custom("unknown field")),
                };
                if let Some(v) = m.next_value::<Option<String>>()? {
                    *destination = v;
                }
            }
            Ok(r)
        }
    }
    serde_json::Deserializer::from_slice(&crate::go_json::normalize(body))
        .deserialize_any(InputVisitor(credentials))
}
fn normalize_email(email: &str) -> Option<String> {
    let email: String = email
        .trim()
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    (email.len() >= 3 && email.len() <= 254 && email.contains('@')).then_some(email)
}
fn register(state: &State, headers: &HeaderMap, input: Input) -> Response {
    let Some(email) = normalize_email(&input.email) else {
        return error(400, "invalid_email", "Enter a valid email address");
    };
    let password = match auth::hash_password(&input.password) {
        Ok(v) => v,
        Err(e) => return error(400, "invalid_password", &e),
    };
    let id = match auth::identifier() {
        Ok(v) => v,
        Err(_) => return error(500, "internal_error", "Could not create account"),
    };
    let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(e) = db.execute(
        "INSERT INTO users(id,email,password_hash,created_at) VALUES(?,?,?,?)",
        params![id, email, password, timestamp(Utc::now())],
    ) {
        return if e.to_string().contains("UNIQUE constraint failed") {
            error(
                409,
                "email_exists",
                "An account with this email already exists",
            )
        } else {
            error(500, "internal_error", "Could not create account")
        };
    }
    match session(state, &db, &id, &email) {
        Ok(s) => session_response(state, headers, 201, s),
        Err(_) => error(500, "internal_error", "Could not create session"),
    }
}
fn login(state: &State, headers: &HeaderMap, input: Input) -> Response {
    let invalid = || error(401, "invalid_credentials", "Email or password is incorrect");
    let Some(email) = normalize_email(&input.email) else {
        return invalid();
    };
    let user = state
        .db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .query_row(
            "SELECT id,password_hash FROM users WHERE email=?",
            [&email],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        );
    let Ok((id, hash)) = user else {
        return invalid();
    };
    if !auth::verify_password(&hash, &input.password) {
        return invalid();
    }
    let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
    match session(state, &db, &id, &email) {
        Ok(s) => session_response(state, headers, 200, s),
        Err(_) => error(500, "internal_error", "Could not create session"),
    }
}
fn refresh(state: &State, headers: &HeaderMap, input: Input) -> Response {
    let token = refresh_token(headers, &input.refresh_token);
    if token.is_empty() {
        return error(400, "refresh_token_required", "Refresh token is required");
    }
    let failed = || error(500, "internal_error", "Could not refresh session");
    let invalid = || {
        error(
            401,
            "invalid_refresh_token",
            "Refresh token is invalid or expired",
        )
    };
    let mut db = state.db.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(tx) = db.transaction() else {
        return failed();
    };
    let user = tx.query_row("SELECT u.id,u.email,t.expires_at FROM refresh_tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=? AND t.revoked_at IS NULL",[auth::token_hash(&token)],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional();
    let Ok(Some((id, email, expiry))) = user else {
        return invalid();
    };
    if !DateTime::parse_from_rfc3339(&expiry.replace(',', ".")).is_ok_and(|v| Utc::now() < v) {
        return invalid();
    }
    if tx
        .execute(
            "UPDATE refresh_tokens SET revoked_at=? WHERE token_hash=?",
            params![timestamp(Utc::now()), auth::token_hash(&token)],
        )
        .is_err()
    {
        return failed();
    }
    let Ok(s) = session(state, &tx, &id, &email) else {
        return failed();
    };
    if tx.commit().is_err() {
        return failed();
    }
    session_response(state, headers, 200, s)
}
struct Session {
    value: Value,
    expires: DateTime<Utc>,
    access: String,
    refresh: String,
}
fn session(state: &State, db: &Connection, id: &str, email: &str) -> Result<Session, String> {
    let (access, expires) = state.tokens.access_token(id, email)?;
    let refresh = auth::random_token(48)?;
    let now = Utc::now();
    db.execute(
        "INSERT INTO refresh_tokens(token_hash,user_id,expires_at,created_at) VALUES(?,?,?,?)",
        params![
            auth::token_hash(&refresh),
            id,
            timestamp(now + state.cfg.refresh_ttl),
            timestamp(now)
        ],
    )
    .map_err(|_| "session insert failed")?;
    Ok(Session {
        value: json!({"accessToken":access,"refreshToken":refresh,"expiresAt":timestamp(expires),"user":{"id":id,"email":email}}),
        expires,
        access,
        refresh,
    })
}
fn session_response(state: &State, headers: &HeaderMap, status: u16, s: Session) -> Response {
    let mut r = json_response(status, s.value);
    for (name, value, expires) in [
        ("balance_access", s.access, s.expires),
        (
            "balance_refresh",
            s.refresh,
            Utc::now() + state.cfg.refresh_ttl,
        ),
    ] {
        append_cookie(
            &mut r,
            name,
            &value,
            expires,
            (expires - Utc::now()).num_seconds().max(1),
            headers,
        );
    }
    r
}
fn append_cookie(
    r: &mut Response,
    name: &str,
    value: &str,
    expires: DateTime<Utc>,
    max_age: i64,
    headers: &HeaderMap,
) {
    let time = if expires.timestamp() >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(expires.timestamp() as u64)
    } else {
        SystemTime::UNIX_EPOCH
    };
    let secure = if header(headers, "x-forwarded-proto")
        .trim()
        .eq_ignore_ascii_case("https")
    {
        "; Secure"
    } else {
        ""
    };
    let value = format!(
        "{name}={value}; Path=/; Expires={}; Max-Age={}; HttpOnly{secure}; SameSite=Strict",
        httpdate::fmt_http_date(time),
        max_age.max(0)
    );
    if let Ok(value) = HeaderValue::from_str(&value) {
        r.headers_mut().append("Set-Cookie", value);
    }
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}
fn cookie(headers: &HeaderMap, name: &str) -> String {
    for h in headers.get_all("cookie") {
        if let Ok(h) = h.to_str() {
            for part in h.split(';') {
                match part.trim().split_once('=') {
                    Some((key, value)) if key == name => {
                        return value.trim_matches('"').trim().to_owned();
                    }
                    _ => {}
                }
            }
        }
    }
    String::new()
}
fn refresh_token(headers: &HeaderMap, value: &str) -> String {
    if value.trim().is_empty() {
        cookie(headers, "balance_refresh")
    } else {
        value.trim().to_owned()
    }
}
fn authorize(state: &State, headers: &HeaderMap) -> Result<auth::Claims, Box<Response>> {
    let h = header(headers, "authorization").trim();
    let token = if let Some(token) = h.strip_prefix("Bearer ") {
        token.trim().to_owned()
    } else {
        cookie(headers, "balance_access")
    };
    if token.is_empty() {
        return Err(Box::new(error(
            401,
            "authorization_required",
            "Authorization Bearer token is required",
        )));
    }
    state.tokens.verify(&token).map_err(|_| {
        Box::new(error(
            401,
            "invalid_token",
            "Access token is invalid or expired",
        ))
    })
}
fn timestamp(t: DateTime<Utc>) -> String {
    let s = t.to_rfc3339_opts(SecondsFormat::Nanos, true);
    format!(
        "{}Z",
        s.trim_end_matches('Z')
            .trim_end_matches('0')
            .trim_end_matches('.')
    )
}
fn empty(status: u16) -> Response {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap())
        .body(Body::empty())
        .unwrap()
}
fn plain(status: u16, text: &'static str) -> Response {
    let mut r = empty(status);
    r.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    *r.body_mut() = Body::from(text);
    r
}
fn json_response(status: u16, value: Value) -> Response {
    let mut r = empty(status);
    r.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    *r.body_mut() = Body::from(format!("{}\n", value));
    r
}
fn raw_json_response(status: u16, value: Vec<u8>) -> Response {
    let mut r = empty(status);
    r.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    *r.body_mut() = Body::from(value);
    r
}
fn error(status: u16, code: &str, message: &str) -> Response {
    json_response(status, json!({"error":{"code":code,"message":message}}))
}
fn invalid_json() -> Response {
    error(400, "invalid_json", "Request body is not valid JSON")
}

fn clean_path(path: &str, preserve_slash: bool) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let mut result = format!("/{}", parts.join("/"));
    if preserve_slash && path.ends_with('/') && result != "/" {
        result.push('/');
    }
    result
}
fn percent_decode(path: &str) -> String {
    let mut bytes = Vec::new();
    let mut i = 0;
    let p = path.as_bytes();
    while i < p.len() {
        let decoded = if p[i] == b'%' && i + 2 < p.len() {
            u8::from_str_radix(&path[i + 1..i + 3], 16).ok()
        } else {
            None
        };
        if let Some(v) = decoded {
            bytes.push(v);
            i += 3;
            continue;
        }
        bytes.push(p[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
fn redirect(location: &str, method: &Method) -> Response {
    let mut r = empty(307);
    if let Ok(v) = HeaderValue::from_str(location) {
        r.headers_mut().insert("Location", v);
    }
    if method == Method::GET || method == Method::HEAD {
        r.headers_mut().insert(
            "Content-Type",
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
    }
    if method == Method::GET {
        let escaped = location
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&#34;")
            .replace('\'', "&#39;");
        *r.body_mut() = Body::from(format!("<a href=\"{escaped}\">Temporary Redirect</a>.\n\n"));
    }
    r
}
fn web(path: &str, headers: &HeaderMap) -> Response {
    if path.starts_with("/v1/") {
        return error(404, "not_found", "Endpoint not found");
    }
    let cleaned = clean_path(path, false);
    let cache = if matches!(cleaned.as_str(), "/" | "/index.html" | "/service-worker.js") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    if path.ends_with("/index.html") {
        let mut r = empty(301);
        r.headers_mut()
            .insert("Location", HeaderValue::from_static("./"));
        r.headers_mut()
            .insert("Cache-Control", HeaderValue::from_static(cache));
        return r;
    }
    let (bytes, mime): (&'static [u8], &'static str) = match cleaned.as_str() {
        "/" => (
            include_bytes!("../web/index.html"),
            "text/html; charset=utf-8",
        ),
        "/app.js" => (
            include_bytes!("../web/app.js"),
            "text/javascript; charset=utf-8",
        ),
        "/service-worker.js" => (
            include_bytes!("../web/service-worker.js"),
            "text/javascript; charset=utf-8",
        ),
        "/styles.css" => (
            include_bytes!("../web/styles.css"),
            "text/css; charset=utf-8",
        ),
        "/icon.svg" => (include_bytes!("../web/icon.svg"), "image/svg+xml"),
        "/manifest.webmanifest" => (
            include_bytes!("../web/manifest.webmanifest"),
            "text/plain; charset=utf-8",
        ),
        _ => return plain(404, "404 page not found\n"),
    };
    if path.ends_with('/') && cleaned != "/" {
        let mut r = empty(301);
        let base = cleaned.rsplit('/').next().unwrap_or("");
        r.headers_mut().insert(
            "Location",
            HeaderValue::from_str(&format!("../{base}")).unwrap(),
        );
        r.headers_mut()
            .insert("Cache-Control", HeaderValue::from_static(cache));
        return r;
    }
    // Go's embedded files have neither ETags nor modification timestamps.
    // Wildcard existence checks still apply before serving their content.
    let if_match = header(headers, "if-match");
    let if_none_match = header(headers, "if-none-match");
    let precondition = if !if_match.is_empty()
        && !if_match
            .trim_start_matches([' ', '\t', ','])
            .starts_with('*')
    {
        Some(412)
    } else if if_none_match
        .trim_start_matches([' ', '\t', ','])
        .starts_with('*')
    {
        Some(304)
    } else {
        None
    };
    if let Some(status) = precondition {
        let mut r = empty(status);
        r.headers_mut()
            .insert("Cache-Control", HeaderValue::from_static(cache));
        return r;
    }
    // Embedded files have no modification time. Any nonempty If-Range thus
    // fails its validator and requests the entire representation.
    let range = if header(headers, "if-range").is_empty() {
        header(headers, "range")
    } else {
        ""
    };
    let ranges = match parse_ranges(range, bytes.len()) {
        Ok(r) => r,
        Err(overlap) => {
            let mut r = plain(
                416,
                if overlap {
                    "invalid range: failed to overlap\n"
                } else {
                    "invalid range\n"
                },
            );
            if overlap {
                r.headers_mut().insert(
                    "Content-Range",
                    HeaderValue::from_str(&format!("bytes */{}", bytes.len())).unwrap(),
                );
            }
            return r;
        }
    };
    let mut r = empty(if ranges.is_empty() { 200 } else { 206 });
    r.headers_mut()
        .insert("Content-Type", HeaderValue::from_static(mime));
    r.headers_mut()
        .insert("Cache-Control", HeaderValue::from_static(cache));
    r.headers_mut()
        .insert("Accept-Ranges", HeaderValue::from_static("bytes"));
    let content = if let Some(&(start, length)) = ranges.first() {
        r.headers_mut().insert(
            "Content-Range",
            HeaderValue::from_str(&format!(
                "bytes {}-{}/{}",
                start,
                start + length - 1,
                bytes.len()
            ))
            .unwrap(),
        );
        &bytes[start..start + length]
    } else {
        bytes
    };
    r.headers_mut().insert(
        "Content-Length",
        HeaderValue::from_str(&content.len().to_string()).unwrap(),
    );
    *r.body_mut() = Body::from(content);
    r
}
// The error flag distinguishes malformed syntax from a valid non-overlap.
fn parse_ranges(header: &str, size: usize) -> Result<Vec<(usize, usize)>, bool> {
    if header.is_empty() {
        return Ok(Vec::new());
    }
    let range = header.strip_prefix("bytes=").ok_or(false)?;
    // The Go FileServer reference ignores multipart ranges. Preserve its
    // observable full-representation response for multiple requested ranges.
    if range.split(',').count() > 1 {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut no_overlap = false;
    for part in range.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        let (start, end) = part.split_once('-').ok_or(false)?;
        let start = start.trim();
        let end = end.trim();
        let integer = |s: &str| {
            s.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .map(|n| n as usize)
        };
        if start.is_empty() {
            if end.starts_with('-') {
                return Err(false);
            }
            let length = integer(end).ok_or(false)?.min(size);
            result.push((size - length, length));
        } else {
            let start = integer(start).ok_or(false)?;
            if start >= size {
                no_overlap = true;
                continue;
            }
            let end = if end.is_empty() {
                size - 1
            } else {
                integer(end).ok_or(false)?.min(size - 1)
            };
            if start > end {
                return Err(false);
            }
            result.push((start, end - start + 1));
        }
    }
    if result.is_empty() && no_overlap {
        return Err(true);
    }
    if result.iter().map(|(_, n)| *n).sum::<usize>() > size {
        result.clear();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        State::new(
            Config {
                address: "127.0.0.1:0".into(),
                database_path: ":memory:".into(),
                jwt_secret: "test secret".into(),
                access_ttl: chrono::TimeDelta::minutes(15),
                refresh_ttl: chrono::TimeDelta::days(30),
                allow_registration: true,
                cors_origins: vec!["https://allowed.example".into()],
            },
            Mutex::new(crate::database::open(":memory:").unwrap()),
        )
    }
    #[test]
    fn strict_go_auth_json_defaults_duplicates_and_trailing_document() {
        assert!(decode(b"null", true).unwrap().email.is_empty());
        let r = decode(
            br#"{"EMAIL":"first","email":null,"PASSWORD":"pw"} {}"#,
            true,
        )
        .unwrap();
        assert_eq!(r.email, "first");
        assert_eq!(r.password, "pw");
        for invalid in [
            br#"{"extra":null}"#.as_slice(),
            br#"{"email":12}"#,
            br#"[]"#,
        ] {
            assert!(decode(invalid, true).is_err());
        }
        assert!(decode(br#"{"email":"a@b"}"#, false).is_err());
    }
    #[tokio::test]
    async fn security_cors_and_head_apply_to_all_responses() {
        let state = Arc::new(state());
        let r = handle(
            ExtractState(state.clone()),
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/sync")
                .header("Origin", "https://allowed.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(r.headers()["Cache-Control"], "no-store");
        assert_eq!(
            r.headers()["Access-Control-Allow-Origin"],
            "https://allowed.example"
        );
        assert_eq!(r.headers()["X-Frame-Options"], "DENY");
        let r = handle(
            ExtractState(state),
            Request::builder()
                .method("HEAD")
                .uri("/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["Cache-Control"], "public, max-age=3600");
        assert!(
            axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn limiter_is_shared_across_auth_endpoints_and_checks_before_json() {
        let state = state();
        let headers = HeaderMap::new();
        for _ in 0..30 {
            assert_eq!(
                dispatch(
                    &state,
                    &Method::POST,
                    "/v1/auth/login",
                    &headers,
                    Ok(b"{"),
                    "ip"
                )
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        let r = dispatch(
            &state,
            &Method::POST,
            "/v1/auth/register",
            &headers,
            Ok(b"{"),
            "ip",
        );
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()["Retry-After"], "60");
        assert_eq!(
            dispatch(
                &state,
                &Method::POST,
                "/v1/auth/login",
                &headers,
                Ok(b"{"),
                "other"
            )
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    #[test]
    fn bearer_precedence_cookie_fallback_and_static_range() {
        let state = state();
        let (token, _) = state.tokens.access_token("u", "u@example.com").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "Cookie",
            HeaderValue::from_str(&format!("balance_access={token}")).unwrap(),
        );
        headers.insert("Authorization", HeaderValue::from_static("Basic ignored"));
        assert_eq!(authorize(&state, &headers).unwrap().subject, "u");
        headers.insert("Authorization", HeaderValue::from_static("Bearer bad"));
        assert_eq!(
            authorize(&state, &headers).unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );
        headers.insert("Range", HeaderValue::from_static("bytes=0-2"));
        let r = web("/app.js", &headers);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.headers()["Content-Length"], "3");
    }
    #[test]
    fn multi_range_matches_go_reference_and_malformed_ranges_stay_errors() {
        let mut headers = HeaderMap::new();
        headers.insert("Range", HeaderValue::from_static("bytes=0-0,2-2"));
        let r = web("/icon.svg", &headers);
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["Content-Type"], "image/svg+xml");
        headers.insert("Range", HeaderValue::from_static("bytes=10-1"));
        assert_eq!(
            web("/icon.svg", &headers).status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
        headers.insert("Range", HeaderValue::from_static("items=0-1"));
        assert_eq!(
            web("/icon.svg", &headers).status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
    }
}
