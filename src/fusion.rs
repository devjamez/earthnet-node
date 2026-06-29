//! Fusion + consensus (v0.2).
//!
//! - OFFICIAL + P-wave: emit a ConfirmedEvent immediately (high trust).
//! - PHONE: a pick joins the buffer; it fires consensus only when ≥ N picks fall
//!   within `radius_km` AND `window` seconds of each other (correlated in space
//!   and time). Stale picks (outside the window vs the newest) are pruned.
//!
//! NOT YET MODELED (later slices): magnitude estimation from the cluster,
//! epicenter estimation, supersede/revision of events, Sybil/reputation.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use earthnet_protocol::{
    sign, verify, ConfirmedEvent, EvidenceKind, Observation, SourceType, PROTOCOL_VERSION,
};
use prost::Message;

use crate::geo::{decode_geohash, haversine_km};
use crate::{random_id, NodeIdentity};

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

struct State {
    phone_buffer: Vec<BufferedPick>,
    emitted: Vec<ConfirmedEvent>,
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
    /// `consensus_n` phone picks within `radius_km` and `window_secs` trigger consensus.
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
                emitted: Vec::new(),
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

        let event = match SourceType::try_from(obs.source_type) {
            Ok(SourceType::Official) if obs.p_wave_detected => {
                Some(self.make_event(&[obs], EvidenceKind::Official))
            }
            Ok(SourceType::Official) => None,
            Ok(SourceType::Phone) => self.ingest_phone(obs)?,
            _ => return Err(IngestError::BadFields),
        };

        if let Some(ref evt) = event {
            self.state
                .lock()
                .expect("fusion state poisoned")
                .emitted
                .push(evt.clone());
        }
        Ok(event)
    }

    /// Buffers a phone pick and fires consensus if a correlated cluster forms.
    fn ingest_phone(&self, obs: Observation) -> Result<Option<ConfirmedEvent>, IngestError> {
        let (lat, lon) = obs
            .location
            .as_ref()
            .and_then(|l| decode_geohash(&l.geohash))
            .ok_or(IngestError::BadFields)?;
        let t_ns = obs.captured_at_ns;

        let mut st = self.state.lock().expect("fusion state poisoned");

        // Drop picks too far in time from this one, then add it.
        st.phone_buffer
            .retain(|p| (p.t_ns - t_ns).abs() <= self.window_ns);
        st.phone_buffer.push(BufferedPick {
            lat,
            lon,
            t_ns,
            obs,
        });

        // Partition the buffer into the cluster correlated with the new pick and the rest.
        let mut clustered = Vec::new();
        let mut rest = Vec::new();
        for p in st.phone_buffer.drain(..) {
            let near = haversine_km((lat, lon), (p.lat, p.lon)) <= self.radius_km
                && (p.t_ns - t_ns).abs() <= self.window_ns;
            if near {
                clustered.push(p);
            } else {
                rest.push(p);
            }
        }
        st.phone_buffer = rest;

        if clustered.len() >= self.consensus_n {
            let picks: Vec<Observation> = clustered.into_iter().map(|p| p.obs).collect();
            Ok(Some(self.make_event(&picks, EvidenceKind::Consensus)))
        } else {
            // not enough yet — keep the cluster buffered
            st.phone_buffer.extend(clustered);
            Ok(None)
        }
    }

    /// Number of events emitted so far.
    pub fn emitted_count(&self) -> usize {
        self.state
            .lock()
            .expect("fusion state poisoned")
            .emitted
            .len()
    }

    /// Builds + signs a ConfirmedEvent from the contributing picks.
    fn make_event(&self, picks: &[Observation], evidence: EvidenceKind) -> ConfirmedEvent {
        let lead = &picks[0];
        let mut evt = ConfirmedEvent {
            protocol_version: PROTOCOL_VERSION,
            event_id: random_id(),
            pubkey: self.identity.pubkey(),
            origin_time_ns: lead.captured_at_ns,
            issued_at_ns: now_ns(),
            epicenter: lead.location.clone(),
            depth_km: 0.0,
            magnitude: lead.reported_magnitude,
            magnitude_uncert: 0.0,
            evidence: evidence as i32,
            num_observations: picks.len() as u32,
            obs_ids: picks.iter().map(|p| p.observation_id.clone()).collect(),
            supersedes: Vec::new(),
            signature: Vec::new(),
        };
        sign(self.identity.signing_key(), &mut evt);
        evt
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
