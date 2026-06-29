//! Fusion + consensus (v0.4).
//!
//! - OFFICIAL + P-wave: emit a ConfirmedEvent immediately (high trust).
//! - PHONE: a pick joins the buffer; a coarse cluster forms from ≥ N picks of
//!   DISTINCT identities within `radius_km` AND `window` seconds (one pubkey =
//!   one vote — basic Sybil resistance). The cluster then must pass travel-time
//!   **phase association** ([`crate::locate`]): the picks must fit a single
//!   hypocenter within an RMS tolerance, which rejects incoherent coincidences
//!   (sharpening as picks become over-determined) and yields the real epicenter
//!   + origin time. Official events use the pick's own location + time.
//!
//! Magnitude = official value if reported, else a provisional PGA-based estimate
//! (see `magnitude`). A new event correlated with a recent one carries
//! `supersedes` (revision).
//!
//! NOT YET MODELED (later slices): depth estimation, calibrated GMPE
//! coefficients, reputation-weighted consensus, layered velocity model.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use earthnet_protocol::{
    sign, verify, ConfirmedEvent, EvidenceKind, Location, Observation, SourceType, PROTOCOL_VERSION,
};
use prost::Message;

use std::collections::HashSet;

use crate::geo::{decode_geohash, encode_geohash, haversine_km};
use crate::locate::{associate_window, Hypocenter};
use crate::{magnitude, random_id, NodeIdentity};

/// Max RMS travel-time residual (s) for an associated event to be accepted.
const MAX_RMS_S: f64 = 1.0;
/// Inlier threshold (s): a pick joins the associated event if its implied origin
/// is within this of the cluster's median origin.
const RESIDUAL_TOL_S: f64 = 1.5;

/// Why an ingested observation was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    /// Bytes were not a valid Observation.
    Decode,
    /// Signature did not verify.
    Signature,
    /// Unsupported protocol version, unusable fields, or missing/invalid location.
    BadFields,
}

struct BufferedPick {
    lat: f64,
    lon: f64,
    t_ns: i64,
    obs: Observation,
}

/// A reference to a recently emitted event, for supersede detection.
struct EmittedRef {
    event_id: Vec<u8>,
    lat: f64,
    lon: f64,
    origin_time_ns: i64,
}

struct State {
    phone_buffer: Vec<BufferedPick>,
    recent: Vec<EmittedRef>,
    emitted_count: usize,
}

/// The fusion engine. Thread-safe; share via `Arc`.
pub struct Fusion {
    identity: NodeIdentity,
    consensus_n: usize,
    radius_km: f64,
    window_ns: i64,
    state: Mutex<State>,
}

impl Fusion {
    /// `consensus_n` distinct phone identities within `radius_km` and
    /// `window_secs` trigger consensus.
    pub fn new(
        identity: NodeIdentity,
        consensus_n: usize,
        radius_km: f64,
        window_secs: u64,
    ) -> Self {
        Self {
            identity,
            consensus_n: consensus_n.max(1),
            radius_km: radius_km.max(0.0),
            window_ns: (window_secs as i64).saturating_mul(1_000_000_000),
            state: Mutex::new(State {
                phone_buffer: Vec::new(),
                recent: Vec::new(),
                emitted_count: 0,
            }),
        }
    }

    /// Decode + verify + ingest raw Observation bytes.
    pub fn ingest_bytes(&self, bytes: &[u8]) -> Result<Option<ConfirmedEvent>, IngestError> {
        let obs = Observation::decode(bytes).map_err(|_| IngestError::Decode)?;
        verify(&obs).map_err(|_| IngestError::Signature)?;
        self.ingest(obs)
    }

    /// Ingest an already-verified Observation and maybe produce a ConfirmedEvent.
    pub fn ingest(&self, obs: Observation) -> Result<Option<ConfirmedEvent>, IngestError> {
        if obs.protocol_version != PROTOCOL_VERSION {
            return Err(IngestError::BadFields);
        }

        let to_emit: Option<(Vec<Observation>, EvidenceKind, Option<Hypocenter>)> =
            match SourceType::try_from(obs.source_type) {
                Ok(SourceType::Official) if obs.p_wave_detected => {
                    Some((vec![obs], EvidenceKind::Official, None))
                }
                Ok(SourceType::Official) => None,
                Ok(SourceType::Phone) => self
                    .window_associate(obs)?
                    .map(|(picks, h)| (picks, EvidenceKind::Consensus, Some(h))),
                _ => return Err(IngestError::BadFields),
            };

        let Some((picks, evidence, hypo)) = to_emit else {
            return Ok(None);
        };
        let mut st = self.state.lock().expect("fusion state poisoned");
        Ok(Some(self.build_and_record(&mut st, &picks, evidence, hypo)))
    }

    /// Buffers a phone pick (one vote per identity, sliding time window) and runs
    /// windowed phase association over the whole buffer; fires the largest
    /// coherent subset (≥ N picks fitting one hypocenter), consuming those picks
    /// and leaving unrelated noise picks buffered. This is what makes consensus
    /// work under dense noise (see backtest/dense_sim.py).
    fn window_associate(
        &self,
        obs: Observation,
    ) -> Result<Option<(Vec<Observation>, Hypocenter)>, IngestError> {
        let (lat, lon) = obs
            .location
            .as_ref()
            .and_then(|l| decode_geohash(&l.geohash))
            .ok_or(IngestError::BadFields)?;
        let t_ns = obs.captured_at_ns;
        let pubkey = obs.pubkey.clone();

        let mut st = self.state.lock().expect("fusion state poisoned");
        // Sliding time window; one pubkey = one vote (basic Sybil resistance).
        st.phone_buffer
            .retain(|p| (p.t_ns - t_ns).abs() <= self.window_ns && p.obs.pubkey != pubkey);
        st.phone_buffer.push(BufferedPick {
            lat,
            lon,
            t_ns,
            obs,
        });

        let coords: Vec<(f64, f64, i64)> = st
            .phone_buffer
            .iter()
            .map(|p| (p.lat, p.lon, p.t_ns))
            .collect();
        let span_deg = (self.radius_km / 111.0).max(0.3);
        match associate_window(&coords, self.consensus_n, RESIDUAL_TOL_S, span_deg) {
            Some((h, inliers)) if h.rms_s <= MAX_RMS_S => {
                let inset: HashSet<usize> = inliers.into_iter().collect();
                let mut picks = Vec::new();
                let mut keep = Vec::new();
                for (i, p) in st.phone_buffer.drain(..).enumerate() {
                    if inset.contains(&i) {
                        picks.push(p.obs);
                    } else {
                        keep.push(p);
                    }
                }
                st.phone_buffer = keep;
                Ok(Some((picks, h)))
            }
            _ => Ok(None),
        }
    }

    /// Number of events emitted so far.
    pub fn emitted_count(&self) -> usize {
        self.state
            .lock()
            .expect("fusion state poisoned")
            .emitted_count
    }

    /// Builds, signs, and records a ConfirmedEvent (sets `supersedes` if it
    /// revises a recently emitted, correlated event).
    fn build_and_record(
        &self,
        st: &mut State,
        picks: &[Observation],
        evidence: EvidenceKind,
        located: Option<Hypocenter>,
    ) -> ConfirmedEvent {
        // Located hypocenter (consensus) gives real epicenter + origin time;
        // otherwise (official single pick) fall back to centroid + pick time.
        let (epicenter, centroid, origin_time_ns, depth_km) = match located {
            Some(h) => (
                Some(Location {
                    geohash: encode_geohash(h.lat, h.lon, 6),
                    precision_m: 600,
                }),
                Some((h.lat, h.lon)),
                h.origin_ns,
                h.depth_km as f32,
            ),
            None => {
                let (epi, c) = estimate_epicenter(picks);
                (epi, c, picks[0].captured_at_ns, 0.0)
            }
        };
        let (magnitude, magnitude_uncert) = estimate_magnitude(picks, centroid);

        // Supersede a recent correlated event, if any.
        let supersedes = centroid
            .and_then(|c| self.find_superseded(st, c, origin_time_ns))
            .unwrap_or_default();

        let mut evt = ConfirmedEvent {
            protocol_version: PROTOCOL_VERSION,
            event_id: random_id(),
            pubkey: self.identity.pubkey(),
            origin_time_ns,
            issued_at_ns: now_ns(),
            epicenter,
            depth_km,
            magnitude,
            magnitude_uncert,
            evidence: evidence as i32,
            num_observations: picks.len() as u32,
            obs_ids: picks.iter().map(|p| p.observation_id.clone()).collect(),
            supersedes,
            signature: Vec::new(),
        };
        sign(self.identity.signing_key(), &mut evt);

        if let Some((lat, lon)) = centroid {
            st.recent.push(EmittedRef {
                event_id: evt.event_id.clone(),
                lat,
                lon,
                origin_time_ns,
            });
            // keep recent bounded: drop entries far older than the window
            st.recent
                .retain(|e| (origin_time_ns - e.origin_time_ns).abs() <= self.window_ns * 4);
        }
        st.emitted_count += 1;
        evt
    }

    /// event_id of the most recent emitted event correlated (space + time) with
    /// the given centroid, or None.
    fn find_superseded(&self, st: &State, c: (f64, f64), origin_time_ns: i64) -> Option<Vec<u8>> {
        st.recent
            .iter()
            .rev()
            .find(|e| {
                haversine_km(c, (e.lat, e.lon)) <= self.radius_km
                    && (origin_time_ns - e.origin_time_ns).abs() <= self.window_ns
            })
            .map(|e| e.event_id.clone())
    }
}

/// Epicenter as the centroid of contributing pick locations. Returns the
/// protobuf Location plus the raw centroid `(lat, lon)` for reuse.
fn estimate_epicenter(picks: &[Observation]) -> (Option<Location>, Option<(f64, f64)>) {
    let coords: Vec<(f64, f64)> = picks
        .iter()
        .filter_map(|p| p.location.as_ref().and_then(|l| decode_geohash(&l.geohash)))
        .collect();
    if coords.is_empty() {
        return (picks[0].location.clone(), None);
    }
    let n = coords.len() as f64;
    let lat = coords.iter().map(|c| c.0).sum::<f64>() / n;
    let lon = coords.iter().map(|c| c.1).sum::<f64>() / n;
    let location = Location {
        geohash: encode_geohash(lat, lon, 6),
        precision_m: 600, // geohash-6 ~ +-0.6 km
    };
    (Some(location), Some((lat, lon)))
}

/// Magnitude estimate: authoritative official value if any pick reports one,
/// else a provisional PGA-based estimate (large uncertainty).
fn estimate_magnitude(picks: &[Observation], centroid: Option<(f64, f64)>) -> (f32, f32) {
    let official_max = picks
        .iter()
        .map(|p| p.reported_magnitude)
        .fold(0.0f32, f32::max);
    if official_max > 0.0 {
        return (official_max, magnitude::OFFICIAL_UNCERT);
    }
    let peak = picks.iter().max_by(|a, b| {
        a.estimated_pga
            .partial_cmp(&b.estimated_pga)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    match peak {
        Some(p) if p.estimated_pga > 0.0 => {
            let dist = match (
                centroid,
                p.location.as_ref().and_then(|l| decode_geohash(&l.geohash)),
            ) {
                (Some(c), Some(pl)) => haversine_km(c, pl),
                _ => 0.0,
            };
            (
                magnitude::estimate_from_pga(p.estimated_pga, dist),
                magnitude::PROVISIONAL_UNCERT,
            )
        }
        _ => (0.0, 0.0),
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
