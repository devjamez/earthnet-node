use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use earthnet_node::fusion::Fusion;
use earthnet_node::persistence::Persistence;
use earthnet_node::relay_client::RelayForwarder;
use earthnet_node::server::{app, AppState};
use earthnet_node::NodeIdentity;
use earthnet_protocol::{sign, Location, Observation, SourceType, PROTOCOL_VERSION};
use ed25519_dalek::SigningKey;
use http_body_util::BodyExt;
use prost::Message;
use rand::{rngs::OsRng, RngCore};
use tower::ServiceExt; // for `oneshot`

fn router() -> axum::Router {
    app(AppState {
        fusion: Arc::new(Fusion::new(NodeIdentity::ephemeral(), 3, 100.0, 30)),
        relay: RelayForwarder::new(None),
        persistence: Persistence::disabled(),
    })
}

fn signed_official_bytes() -> Vec<u8> {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let key = SigningKey::from_bytes(&secret);
    let mut obs = Observation {
        protocol_version: PROTOCOL_VERSION,
        observation_id: vec![1u8; 16],
        pubkey: key.verifying_key().to_bytes().to_vec(),
        source_type: SourceType::Official as i32,
        source_id: "CX:PB01".into(),
        captured_at_ns: 1_700_000_000_000_000_000,
        clock_uncert_ms: 5,
        location: Some(Location {
            geohash: "66jd2".into(),
            precision_m: 100,
        }),
        sta_lta_ratio: 12.0,
        p_wave_detected: true,
        estimated_pga: 0.05,
        reported_magnitude: 6.0,
        signature: Vec::new(),
    };
    sign(&key, &mut obs);
    obs.encode_to_vec()
}

#[tokio::test]
async fn health_ok() {
    let resp = router()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

fn signed_signal_bytes() -> Vec<u8> {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let key = SigningKey::from_bytes(&secret);
    let obs = Observation {
        protocol_version: PROTOCOL_VERSION,
        observation_id: vec![2u8; 16],
        pubkey: key.verifying_key().to_bytes().to_vec(),
        source_type: SourceType::Official as i32,
        source_id: "CX:PB01".into(),
        captured_at_ns: 1_700_000_000_000_000_000,
        clock_uncert_ms: 5,
        location: Some(Location {
            geohash: "66jd2".into(),
            precision_m: 100,
        }),
        sta_lta_ratio: 12.0,
        p_wave_detected: true,
        estimated_pga: 0.05,
        reported_magnitude: 6.0,
        signature: Vec::new(),
    };
    let mut sig = earthnet_protocol::compat::signal_from_observation(&obs);
    earthnet_protocol::sign(&key, &mut sig);
    sig.encode_to_vec()
}

#[tokio::test]
async fn post_valid_v02_signal_accepted() {
    // dual-stack: a v0.2 Signal{seismic.pick.v1} normalizes and fires like v0.1
    let resp = router()
        .oneshot(
            Request::post("/signals")
                .body(Body::from(signed_signal_bytes()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn post_tampered_signal_unauthorized() {
    let mut bytes = signed_signal_bytes();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let resp = router()
        .oneshot(Request::post("/signals").body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn metrics_endpoint_exposes_prometheus() {
    let app = router();
    // ingest one observation so a counter is non-zero
    app.clone()
        .oneshot(
            Request::post("/observations")
                .body(Body::from(signed_official_bytes()))
                .unwrap(),
        )
        .await
        .unwrap();
    let resp = app
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("earthnet_observations_ingested_total"));
    assert!(text.contains("earthnet_ingest_seconds"));
}

#[tokio::test]
async fn post_valid_observation_accepted() {
    let resp = router()
        .oneshot(
            Request::post("/observations")
                .body(Body::from(signed_official_bytes()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn post_garbage_is_bad_request() {
    let resp = router()
        .oneshot(
            Request::post("/observations")
                .body(Body::from(vec![0xde, 0xad, 0xbe, 0xef]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_tampered_observation_unauthorized() {
    let mut bytes = signed_official_bytes();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff; // corrupt the signature
    let resp = router()
        .oneshot(
            Request::post("/observations")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
