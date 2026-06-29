use earthnet_node::fusion::{Fusion, IngestError};
use earthnet_node::geo::{decode_geohash, encode_geohash, haversine_km};
use earthnet_node::locate::travel_time;
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
        estimated_pga: 0.05,
        reported_magnitude: 0.0, // phones don't report magnitude
        signature: Vec::new(),
    };
    sign(&key, &mut obs);
    obs
}

fn resign(obs: &mut Observation) {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let key = SigningKey::from_bytes(&secret);
    obs.pubkey = key.verifying_key().to_bytes().to_vec();
    sign(&key, obs);
}

fn phone(geohash: &str, t_ns: i64) -> Observation {
    signed(SourceType::Phone, true, geohash, t_ns)
}

fn phone_signed_by(key: &SigningKey, geohash: &str, t_ns: i64) -> Observation {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    let mut obs = Observation {
        protocol_version: PROTOCOL_VERSION,
        observation_id: id.to_vec(),
        pubkey: key.verifying_key().to_bytes().to_vec(),
        source_type: SourceType::Phone as i32,
        source_id: String::new(),
        captured_at_ns: t_ns,
        clock_uncert_ms: 10,
        location: Some(Location {
            geohash: geohash.into(),
            precision_m: 2400,
        }),
        sta_lta_ratio: 8.0,
        p_wave_detected: true,
        estimated_pga: 0.05,
        reported_magnitude: 0.0,
        signature: Vec::new(),
    };
    sign(key, &mut obs);
    obs
}

// consensus_n = 3, radius = 100 km, window = 30 s
fn fusion() -> Fusion {
    Fusion::new(NodeIdentity::ephemeral(), 3, 100.0, 30)
}

fn key() -> SigningKey {
    let mut s = [0u8; 32];
    OsRng.fill_bytes(&mut s);
    SigningKey::from_bytes(&s)
}

fn phone_at(lat: f64, lon: f64, t_ns: i64) -> Observation {
    phone_signed_by(&key(), &encode_geohash(lat, lon, 7), t_ns)
}

const SRC_DEPTH_KM: f64 = 30.0;
// four stations within ~100 km of each other
const CLUSTER: [(f64, f64); 4] = [
    (-21.0, -69.5),
    (-21.2, -69.7),
    (-20.8, -69.6),
    (-21.1, -69.35),
];

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
fn official_magnitude_passes_through() {
    let mut obs = signed(SourceType::Official, true, "66jd2", T0);
    obs.reported_magnitude = 6.0;
    resign(&mut obs);
    let evt = fusion().ingest(obs).unwrap().unwrap();
    assert_eq!(evt.magnitude, 6.0);
    assert!(evt.magnitude_uncert > 0.0 && evt.magnitude_uncert < 0.5);
}

#[test]
fn consensus_uses_provisional_magnitude() {
    let f = fusion();
    f.ingest(phone("66jd2", T0)).unwrap();
    f.ingest(phone("66jd2", T0 + 1_000_000_000)).unwrap();
    let evt = f
        .ingest(phone("66jd2", T0 + 2_000_000_000))
        .unwrap()
        .expect("consensus");
    assert!(
        evt.magnitude > 0.0,
        "provisional magnitude should be estimated"
    );
    assert!(
        (evt.magnitude_uncert - 0.5).abs() < 1e-6,
        "provisional uncertainty"
    );
}

#[test]
fn event_has_centroid_epicenter() {
    let evt = fusion()
        .ingest(signed(SourceType::Official, true, "66jd2", T0))
        .unwrap()
        .unwrap();
    let epi = evt.epicenter.expect("epicenter");
    assert!(!epi.geohash.is_empty());
}

#[test]
fn same_identity_cannot_manufacture_consensus() {
    let f = fusion();
    let mut s = [0u8; 32];
    OsRng.fill_bytes(&mut s);
    let key = SigningKey::from_bytes(&s);
    // Same pubkey resending three times = one vote, never reaches N=3.
    assert!(f
        .ingest(phone_signed_by(&key, "66jd2", T0))
        .unwrap()
        .is_none());
    assert!(f
        .ingest(phone_signed_by(&key, "66jd2", T0 + 1_000_000_000))
        .unwrap()
        .is_none());
    assert!(f
        .ingest(phone_signed_by(&key, "66jd2", T0 + 2_000_000_000))
        .unwrap()
        .is_none());
}

#[test]
fn revision_supersedes_recent_correlated_event() {
    let f = fusion();
    let e1 = f
        .ingest(signed(SourceType::Official, true, "66jd2", T0))
        .unwrap()
        .unwrap();
    assert!(e1.supersedes.is_empty(), "first event supersedes nothing");
    let e2 = f
        .ingest(signed(
            SourceType::Official,
            true,
            "66jd2",
            T0 + 5_000_000_000,
        ))
        .unwrap()
        .unwrap();
    assert_eq!(e2.supersedes, e1.event_id, "second revises the first");
}

#[test]
fn distant_event_does_not_supersede() {
    let f = fusion();
    f.ingest(signed(SourceType::Official, true, "66jd2", T0))
        .unwrap()
        .unwrap();
    let e2 = f
        .ingest(signed(SourceType::Official, true, "u33db", T0))
        .unwrap()
        .unwrap();
    assert!(e2.supersedes.is_empty(), "far-away event is independent");
}

#[test]
fn association_locates_and_fires_consistent_cluster() {
    let f = Fusion::new(NodeIdentity::ephemeral(), 4, 100.0, 30);
    let src = (-21.05, -69.55);
    let origin_s = 1_700_000_000.0;
    let mut evt = None;
    for &(la, lo) in &CLUSTER {
        let t = origin_s + travel_time(haversine_km(src, (la, lo)), SRC_DEPTH_KM);
        evt = f.ingest(phone_at(la, lo, (t * 1e9) as i64)).unwrap();
    }
    let e = evt.expect("4 coherent picks must associate and fire");
    assert_eq!(e.evidence, EvidenceKind::Consensus as i32);
    assert_eq!(e.num_observations, 4);
    let (elat, elon) = decode_geohash(&e.epicenter.as_ref().unwrap().geohash).unwrap();
    assert!(
        haversine_km((elat, elon), src) < 40.0,
        "located epicenter too far from source"
    );
    assert!(verify(&e).is_ok());
}

#[test]
fn association_rejects_incoherent_cluster() {
    let f = Fusion::new(NodeIdentity::ephemeral(), 4, 100.0, 30);
    let src = (-21.05, -69.55);
    let origin_s = 1_700_000_000.0;
    let mut last = None;
    for (i, &(la, lo)) in CLUSTER.iter().enumerate() {
        let mut t = origin_s + travel_time(haversine_km(src, (la, lo)), SRC_DEPTH_KM);
        if i == 0 {
            t += 6.0; // one badly mistimed pick — no single source fits
        }
        last = f.ingest(phone_at(la, lo, (t * 1e9) as i64)).unwrap();
    }
    assert!(
        last.is_none(),
        "an incoherent cluster must be rejected by association"
    );
}

#[test]
fn windowed_association_fires_coherent_amid_noise() {
    let f = Fusion::new(NodeIdentity::ephemeral(), 4, 200.0, 30);
    let origin_s = 1_700_000_000.0;
    // two noise picks: plausible locations but times that fit no common origin
    f.ingest(phone_at(-21.0, -69.4, ((origin_s + 20.0) * 1e9) as i64))
        .unwrap();
    f.ingest(phone_at(-20.7, -69.9, ((origin_s + 25.0) * 1e9) as i64))
        .unwrap();
    // four coherent picks (moveout) from one source, alongside the buffered noise
    let src = (-21.05, -69.55);
    let mut evt = None;
    for &(la, lo) in &CLUSTER {
        let t = origin_s + travel_time(haversine_km(src, (la, lo)), SRC_DEPTH_KM);
        evt = f.ingest(phone_at(la, lo, (t * 1e9) as i64)).unwrap();
    }
    let e = evt.expect("coherent subset must fire despite noise picks");
    assert_eq!(
        e.num_observations, 4,
        "should associate exactly the 4 coherent picks (noise excluded)"
    );
    let (elat, elon) = decode_geohash(&e.epicenter.as_ref().unwrap().geohash).unwrap();
    assert!(
        haversine_km((elat, elon), src) < 40.0,
        "located epicenter off"
    );
}

#[test]
fn reputation_rewards_inliers() {
    let f = fusion(); // N=3, default min_weight=3
    let k1 = key();
    f.ingest(phone_signed_by(&k1, "66jd2", T0)).unwrap();
    f.ingest(phone_signed_by(&key(), "66jd2", T0 + 1_000_000_000))
        .unwrap();
    let evt = f
        .ingest(phone_signed_by(&key(), "66jd2", T0 + 2_000_000_000))
        .unwrap();
    assert!(
        evt.is_some(),
        "fresh trio (weight 3 >= 3) should reach consensus"
    );
    let pk = k1.verifying_key().to_bytes().to_vec();
    assert!(
        f.reputation_of(&pk) > 1.0,
        "an inlier's reputation should increase"
    );
}

#[test]
fn insufficient_reputation_is_rejected() {
    // require summed weight >= 4, but three fresh identities only sum to 3.0
    let f = Fusion::new(NodeIdentity::ephemeral(), 3, 100.0, 30).with_min_weight(4.0);
    f.ingest(phone("66jd2", T0)).unwrap();
    f.ingest(phone("66jd2", T0 + 1_000_000_000)).unwrap();
    assert!(
        f.ingest(phone("66jd2", T0 + 2_000_000_000))
            .unwrap()
            .is_none(),
        "coherent but low-reputation cluster must not confirm"
    );
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
