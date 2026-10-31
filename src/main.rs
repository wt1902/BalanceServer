mod attachments;
mod auth;
mod config;
mod database;
mod go_json;
mod ledgers;
mod runtime;
mod server;
mod sync;

use std::sync::{Arc, Mutex};
fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("create async runtime");
    let result = runtime.block_on(run());
    // Connection draining already has a ten-second budget. Blocking database
    // or password jobs must not extend process shutdown beyond that budget.
    runtime.shutdown_timeout(std::time::Duration::ZERO);
    if let Err(e) = result {
        eprintln!("Balance server startup failed: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cfg = config::Config::load()?;
    let db = database::open(&cfg.database_path)?;
    let address = cfg.address.clone();
    let listener = runtime::bind(&address).await?;
    let state = Arc::new(server::State::new(cfg, Mutex::new(db)));
    let app = axum::Router::new()
        .fallback(server::handle)
        .with_state(state);
    eprintln!("Balance server listening on {address}");
    runtime::serve(listener, app, runtime::Timeouts::default(), async {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    })
    .await?;
    Ok(())
}
