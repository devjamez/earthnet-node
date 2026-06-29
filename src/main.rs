//! earthnet-node entrypoint: starts the HTTP ingest server.

use std::path::PathBuf;
use std::sync::Arc;

use earthnet_node::{
    fusion::Fusion, persistence::Persistence, relay_client::RelayForwarder, server::app,
    server::AppState, NodeIdentity,
};
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

    // Defaults from the FP-vs-detection parameter study (backtest/param_study.py):
    // N=4, radius 200 km gave 100% detection at the lowest false-positive rate.
    let consensus_n: usize = env_parse("EARTHNET_CONSENSUS_N", 4);
    let radius_km: f64 = env_parse("EARTHNET_CONSENSUS_RADIUS_KM", 200.0);
    let window_secs: u64 = env_parse("EARTHNET_CONSENSUS_WINDOW_S", 30);
    let min_weight: f64 = env_parse("EARTHNET_CONSENSUS_MIN_WEIGHT", consensus_n as f64);
    let mut fusion =
        Fusion::new(identity, consensus_n, radius_km, window_secs).with_min_weight(min_weight);
    if let Ok(rep) = std::env::var("EARTHNET_REPUTATION_FILE") {
        fusion = fusion.with_reputation_file(rep.into());
    }
    let fusion = Arc::new(fusion);

    let relay = RelayForwarder::new(std::env::var("EARTHNET_RELAY_URL").ok());

    let persistence = match std::env::var("EARTHNET_DATABASE_URL") {
        Ok(url) => match Persistence::connect(&url).await {
            Ok(p) => {
                tracing::info!("persistence enabled");
                p
            }
            Err(e) => {
                tracing::warn!(error = %e, "persistence disabled (connect failed)");
                Persistence::disabled()
            }
        },
        Err(_) => Persistence::disabled(),
    };

    let state = AppState {
        fusion,
        relay,
        persistence,
    };

    let addr = std::env::var("EARTHNET_NODE_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = TcpListener::bind(&addr).await.expect("bind address");
    tracing::info!(%addr, consensus_n, relay = state.relay.is_enabled(), "earthnet-node listening");

    axum::serve(listener, app(state))
        .await
        .expect("server error");
}
