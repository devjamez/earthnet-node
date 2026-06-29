//! earthnet-node — ingests signed [`Observation`](earthnet_protocol::Observation)s,
//! fuses them + reaches consensus, and emits signed
//! [`ConfirmedEvent`](earthnet_protocol::ConfirmedEvent)s that trigger client alarms.
//!
//! Trust model (DESIGN §5): an OFFICIAL source fires on its own; PHONE sources
//! require consensus of ≥ N correlated picks.

pub mod fusion;
pub mod geo;
pub mod locate;
pub mod magnitude;
pub mod persistence;
pub mod relay_client;
pub mod reputation;
pub mod server;
pub mod ttable;

use std::path::Path;

use ed25519_dalek::SigningKey;
use rand::{rngs::OsRng, RngCore};

/// The node's Ed25519 identity. Signs every [`ConfirmedEvent`](earthnet_protocol::ConfirmedEvent)
/// it emits. v0.1 uses an ephemeral key generated at startup; persistence is a later slice.
pub struct NodeIdentity {
    key: SigningKey,
}

impl NodeIdentity {
    /// Generates a fresh in-memory identity. Not persisted.
    pub fn ephemeral() -> Self {
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        Self {
            key: SigningKey::from_bytes(&secret),
        }
    }

    /// Builds an identity from a 32-byte hex seed (64 hex chars).
    pub fn from_seed_hex(seed_hex: &str) -> std::io::Result<Self> {
        let bytes = hex_decode(seed_hex.trim())
            .filter(|b| b.len() == 32)
            .ok_or_else(|| io_err("seed must be 32 bytes (64 hex chars)"))?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(Self {
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// Hex of the 32-byte secret seed. Sensitive — never log this.
    pub fn seed_hex(&self) -> String {
        self.key
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Loads a persisted identity, creating + saving one if absent.
    ///
    /// Precedence: `EARTHNET_NODE_KEY` env (hex seed) → `path` file → generate
    /// a new key and write its seed to `path`.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        if let Ok(env_seed) = std::env::var("EARTHNET_NODE_KEY") {
            return Self::from_seed_hex(&env_seed);
        }
        if path.exists() {
            let seed = std::fs::read_to_string(path)?;
            return Self::from_seed_hex(&seed);
        }
        let identity = Self::ephemeral();
        std::fs::write(path, identity.seed_hex())?;
        Ok(identity)
    }

    /// Raw 32-byte public key.
    pub fn pubkey(&self) -> Vec<u8> {
        self.key.verifying_key().to_bytes().to_vec()
    }

    /// Hex of the public key (safe to log — never log the secret).
    pub fn pubkey_hex(&self) -> String {
        self.pubkey().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Signing key, for producing ConfirmedEvent signatures.
    pub fn signing_key(&self) -> &SigningKey {
        &self.key
    }
}

/// Random 16-byte identifier (event_id).
pub(crate) fn random_id() -> Vec<u8> {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    id.to_vec()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}
