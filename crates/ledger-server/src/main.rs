//! PROGRAM ENTRY POINT: starts the web server on LISTEN_ADDR (default 0.0.0.0:8080).
//! All the actual endpoints live in lib.rs; this file only binds the port and runs them.

#[tokio::main]
async fn main() {
    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind failed");
    println!("ChaloTrace ledger-server listening on {addr}");
    axum::serve(listener, ledger_server::app())
        .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
        .await
        .unwrap();
}
