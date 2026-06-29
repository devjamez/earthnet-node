//! earthnet-node entrypoint: starts the HTTP ingest server.

use std::path::PathBuf;
use std::sync::Arc;

use earthnet_node::{fusion::Fusion, server::app, NodeIdentity};
use tokio::net::TcpListener;

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "earthnet_node=info".into()),
        )
        .init();

    let key_file = PathBuf::from(
        std::env::var("EARTHNET_NODE_KEY_FILE").unwrap_or_else(|_| "node_key.hex".into()),
    );
    let identity = NodeIdentity::load_or_create(&key_file).expect("load/create node identity");
    tracing::info!(pubkey = %identity.pubkey_hex(), key_file = %key_file.display(), "node identity");

    let consensus_n: usize = env_parse("EARTHNET_CONSENSUS_N", 3);
    let radius_km: f64 = env_parse("EARTHNET_CONSENSUS_RADIUS_KM", 100.0);
    let window_secs: u64 = env_parse("EARTHNET_CONSENSUS_WINDOW_S", 30);
    let fusion = Arc::new(Fusion::new(identity, consensus_n, radius_km, window_secs));

    let addr = std::env::var("EARTHNET_NODE_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = TcpListener::bind(&addr).await.expect("bind address");
    tracing::info!(%addr, consensus_n, "earthnet-node listening");

    axum::serve(listener, app(fusion))
        .await
        .expect("server error");
}
