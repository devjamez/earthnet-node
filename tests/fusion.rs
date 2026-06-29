use earthnet_node::fusion::{Fusion, IngestError};
use earthnet_node::NodeIdentity;
use earthnet_protocol::{
    sign, verify, EvidenceKind, Location, Observation, SourceType, PROTOCOL_VERSION,
};
use ed25519_dalek::SigningKey;
use prost::Message;
use rand::{rngs::OsRng, RngCore};

const T0: i64 = 1_700_000_000_000_000_000;

fn signed(source: SourceType, p_wave: bool, geohash: &str, t_ns: i64) -> Observation {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let key = SigningKey::from_bytes(&secret);
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    let mut obs = Observation {
        protocol_version: PROTOCOL_VERSION,
        observation_id: id.to_vec(),
        pubkey: key.verifying_key().to_bytes().to_vec(),
        source_type: source as i32,
        source_id: String::new(),
        captured_at_ns: t_ns,
        clock_uncert_ms: 10,
        location: Some(Location {
            geohash: geohash.into(),
            precision_m: 2400,
        }),
        sta_lta_ratio: 8.0,
        p_wave_detected: p_wave,
        estimated_pga: 0.01,
        reported_magnitude: 5.5,
        signature: Vec::new(),
    };
    sign(&key, &mut obs);
    obs
}

fn phone(geohash: &str, t_ns: i64) -> Observation {
    signed(SourceType::Phone, true, geohash, t_ns)
}

// consensus_n = 3, radius = 100 km, window = 30 s
fn fusion() -> Fusion {
    Fusion::new(NodeIdentity::ephemeral(), 3, 100.0, 30)
}

#[test]
fn official_with_pwave_emits_signed_event() {
    let evt = fusion()
        .ingest(signed(SourceType::Official, true, "66jd2", T0))
        .unwrap()
        .expect("official + p-wave must emit");
    assert_eq!(evt.evidence, EvidenceKind::Official as i32);
    assert_eq!(evt.num_observations, 1);
    assert!(verify(&evt).is_ok());
}

#[test]
fn official_without_pwave_does_not_emit() {
    assert!(fusion()
        .ingest(signed(SourceType::Official, false, "66jd2", T0))
        .unwrap()
        .is_none());
}

#[test]
fn correlated_phones_reach_consensus() {
    let f = fusion();
    assert!(f.ingest(phone("66jd2", T0)).unwrap().is_none());
    assert!(f
        .ingest(phone("66jd2", T0 + 1_000_000_000))
        .unwrap()
        .is_none());
    let evt = f
        .ingest(phone("66jd2", T0 + 2_000_000_000))
        .unwrap()
        .expect("third correlated phone must reach consensus");
    assert_eq!(evt.evidence, EvidenceKind::Consensus as i32);
    assert_eq!(evt.num_observations, 3);
    assert!(verify(&evt).is_ok());
    // cluster consumed
    assert!(f
        .ingest(phone("66jd2", T0 + 3_000_000_000))
        .unwrap()
        .is_none());
}

#[test]
fn spatially_distant_phones_do_not_correlate() {
    let f = fusion();
    f.ingest(phone("66jd2", T0)).unwrap(); // Chile
    f.ingest(phone("u33db", T0)).unwrap(); // Europe (>> 100 km)
                                           // a third near Chile makes 2 correlated there — still below N=3
    assert!(f
        .ingest(phone("66jd2", T0 + 1_000_000_000))
        .unwrap()
        .is_none());
}

#[test]
fn temporally_distant_phones_do_not_correlate() {
    let f = fusion();
    f.ingest(phone("66jd2", T0)).unwrap();
    f.ingest(phone("66jd2", T0 + 1_000_000_000)).unwrap();
    // 60 s later: outside the 30 s window → prunes the earlier two, no consensus
    assert!(f
        .ingest(phone("66jd2", T0 + 60_000_000_000))
        .unwrap()
        .is_none());
}

#[test]
fn invalid_signature_is_rejected() {
    let mut obs = signed(SourceType::Official, true, "66jd2", T0);
    obs.sta_lta_ratio = 999.0;
    assert_eq!(
        fusion().ingest_bytes(&obs.encode_to_vec()),
        Err(IngestError::Signature)
    );
}

#[test]
fn undecodable_bytes_rejected() {
    assert_eq!(
        fusion().ingest_bytes(&[0xff, 0xff, 0xff]),
        Err(IngestError::Decode)
    );
}

#[test]
fn phone_without_location_rejected() {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let key = SigningKey::from_bytes(&secret);
    let mut obs = phone("66jd2", T0);
    obs.location = None;
    obs.pubkey = key.verifying_key().to_bytes().to_vec();
    sign(&key, &mut obs);
    assert_eq!(fusion().ingest(obs), Err(IngestError::BadFields));
}

#[test]
fn identity_persists_across_loads() {
    let path = std::env::temp_dir().join(format!("earthnet_node_key_{}.hex", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let a = NodeIdentity::load_or_create(&path).unwrap();
    let b = NodeIdentity::load_or_create(&path).unwrap();
    assert_eq!(a.pubkey(), b.pubkey());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn identity_seed_hex_roundtrip() {
    let a = NodeIdentity::ephemeral();
    let b = NodeIdentity::from_seed_hex(&a.seed_hex()).unwrap();
    assert_eq!(a.pubkey(), b.pubkey());
}
