//! HTTP ingest surface. Adapters POST signed Observation protobuf bytes to
//! `POST /observations`; the node verifies, persists (async), feeds the fusion
//! engine, and forwards any resulting ConfirmedEvent to the relay.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use earthnet_protocol::{compat::observation_from_signal, verify, Observation, Signal};
use prost::Message as _;

use std::time::Instant;

use crate::fusion::{Fusion, IngestError};
use crate::metrics::metrics;
use crate::persistence::Persistence;
use crate::relay_client::RelayForwarder;

/// Shared server state.
#[derive(Clone)]
pub struct AppState {
    pub fusion: Arc<Fusion>,
    pub relay: RelayForwarder,
    pub persistence: Persistence,
}

/// Builds the router.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/observations", post(ingest)) // v0.1 Observation
        .route("/signals", post(ingest_signal)) // v0.2 Signal (dual-stack)
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

/// Prometheus metrics in text exposition format.
async fn metrics_handler() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        crate::metrics::encode(),
    )
}

/// v0.1: accepts one signed `Observation` (raw protobuf body).
///   202 Accepted     — verified, persisted, ingested
///   400 Bad Request  — undecodable / bad fields
///   401 Unauthorized — signature failed
async fn ingest(State(state): State<AppState>, body: Bytes) -> StatusCode {
    let start = Instant::now();
    let m = metrics();
    let obs = match Observation::decode(body.as_ref()) {
        Ok(o) => o,
        Err(_) => {
            m.ingest_errors.with_label_values(&["decode"]).inc();
            return StatusCode::BAD_REQUEST;
        }
    };
    if verify(&obs).is_err() {
        m.ingest_errors.with_label_values(&["signature"]).inc();
        return StatusCode::UNAUTHORIZED;
    }
    let code = process_observation(&state, obs);
    m.ingest_seconds.observe(start.elapsed().as_secs_f64());
    code
}

/// v0.2 (dual-stack): accepts a signed `Signal`. A seismic-pick Signal is
/// normalized into the internal Observation and fed to the same fusion engine;
/// other modalities are accepted but not (yet) routed to the seismic alert path.
async fn ingest_signal(State(state): State<AppState>, body: Bytes) -> StatusCode {
    let start = Instant::now();
    let m = metrics();
    let sig = match Signal::decode(body.as_ref()) {
        Ok(s) => s,
        Err(_) => {
            m.ingest_errors.with_label_values(&["decode"]).inc();
            return StatusCode::BAD_REQUEST;
        }
    };
    if verify(&sig).is_err() {
        m.ingest_errors.with_label_values(&["signature"]).inc();
        return StatusCode::UNAUTHORIZED;
    }
    let code = match observation_from_signal(&sig) {
        Some(obs) => process_observation(&state, obs),
        // non-seismic modality: accepted for the research plane; alert path is seismic-only for now
        None => StatusCode::ACCEPTED,
    };
    m.ingest_seconds.observe(start.elapsed().as_secs_f64());
    code
}

/// Shared path for a verified pick (from either wire version): persist, fuse,
/// and forward any resulting ConfirmedEvent. Synchronous (no I/O awaits).
fn process_observation(state: &AppState, obs: Observation) -> StatusCode {
    let m = metrics();
    m.observations
        .with_label_values(&[&obs.source_type.to_string()])
        .inc();

    // Persist every verified observation (async, off the hot path).
    state.persistence.record_observation(obs.clone());

    match state.fusion.ingest(obs) {
        Ok(Some(event)) => {
            m.events
                .with_label_values(&[&event.evidence.to_string()])
                .inc();
            state.persistence.record_event(event.clone());
            // Consensus events update reputation — mirror the snapshot to the DB.
            if event.evidence == earthnet_protocol::EvidenceKind::Consensus as i32 {
                state
                    .persistence
                    .record_reputation(state.fusion.reputation_snapshot());
            }
            state.relay.forward(event.encode_to_vec());
            StatusCode::ACCEPTED
        }
        Ok(None) => StatusCode::ACCEPTED,
        Err(IngestError::BadFields) => {
            m.ingest_errors.with_label_values(&["bad_fields"]).inc();
            StatusCode::BAD_REQUEST
        }
        // signature already checked above; any decode/other error is a bad request
        Err(IngestError::Decode | IngestError::Signature) => StatusCode::BAD_REQUEST,
    }
}
