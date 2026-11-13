//! PROGRAM ENTRY POINT: starts the web server on LISTEN_ADDR (default 0.0.0.0:8080).
//! If DATABASE_URL is set (e.g. postgres://user:pass@localhost/chalotrace) data is saved to
//! PostgreSQL and survives restarts; otherwise it runs in memory only.
//! All the actual endpoints live in lib.rs; storage lives in store.rs.

use ledger_server::{app_with_admin_token, Store};

#[tokio::main]
async fn main() {
    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let admin_token = std::env::var("ADMIN_TOKEN").ok();

    let store = match std::env::var("DATABASE_URL") {
        Ok(url) => match Store::connect(&url).await {
            Ok(s) => {
                println!(
                    "Connected to PostgreSQL; loaded and verified {} saved products",
                    s.product_count()
                );
                s
            }
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        },
        Err(_) => {
            println!("WARNING: DATABASE_URL not set. Running IN MEMORY: data is lost when the server stops.");
            Store::in_memory()
        }
    };

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("ERROR: cannot listen on {addr}: {e}");
            std::process::exit(1);
        });
    println!("ChaloTrace ledger-server listening on {addr}");
    axum::serve(listener, app_with_admin_token(store, admin_token))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
}
