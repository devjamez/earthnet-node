use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use earthnet_node::fusion::Fusion;
use earthnet_node::{server::app, NodeIdentity};
use earthnet_protocol::{sign, Location, Observation, SourceType, PROTOCOL_VERSION};
use ed25519_dalek::SigningKey;
use http_body_util::BodyExt;
use prost::Message;
use rand::{rngs::OsRng, RngCore};
use tower::ServiceExt; // for `oneshot`

fn router() -> axum::Router {
    app(Arc::new(Fusion::new(
        NodeIdentity::ephemeral(),
        3,
        100.0,
        30,
    )))
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
