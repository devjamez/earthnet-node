//! Identity reputation: time-decayed weights with file persistence.
//!
//! Each identity's weight decays toward [`DEFAULT_WEIGHT`] with a half-life, so a
//! one-off burst of good picks doesn't grant permanent trust and inactive
//! identities fade. Persisted to a TSV file so reputation survives node restarts.

use std::collections::HashMap;
use std::path::Path;

/// Weight of an unknown/new identity.
pub const DEFAULT_WEIGHT: f64 = 1.0;
/// Reputation cap.
pub const MAX_WEIGHT: f64 = 5.0;
/// Weight gained when an identity's pick is an inlier of a confirmed event.
pub const REWARD: f64 = 0.5;
/// Half-life (ns) for decay of `(weight - default)` toward default — 7 days.
pub const HALF_LIFE_NS: i64 = 7 * 24 * 3600 * 1_000_000_000;

/// Reputation store: pubkey -> (weight, last_update_ns).
#[derive(Default)]
pub struct Reputation {
    weights: HashMap<Vec<u8>, (f64, i64)>,
}

impl Reputation {
    pub fn new() -> Self {
        Self::default()
    }

    fn decay(weight: f64, last_ns: i64, now_ns: i64) -> f64 {
        if now_ns <= last_ns {
            return weight;
        }
        let dt = (now_ns - last_ns) as f64;
        let factor = (-dt / HALF_LIFE_NS as f64 * std::f64::consts::LN_2).exp();
        DEFAULT_WEIGHT + (weight - DEFAULT_WEIGHT) * factor
    }

    /// Decayed weight of an identity at `now_ns` (default for unknown).
    pub fn weight(&self, pubkey: &[u8], now_ns: i64) -> f64 {
        match self.weights.get(pubkey) {
            Some(&(w, t)) => Self::decay(w, t, now_ns),
            None => DEFAULT_WEIGHT,
        }
    }

    /// Reward an identity (decays to now, then adds REWARD, capped).
    pub fn reward(&mut self, pubkey: &[u8], now_ns: i64) {
        let w = (self.weight(pubkey, now_ns) + REWARD).min(MAX_WEIGHT);
        self.weights.insert(pubkey.to_vec(), (w, now_ns));
    }

    /// Load from a TSV file (`hex<TAB>weight<TAB>last_ns`); empty if missing.
    pub fn load(path: &Path) -> Self {
        let mut r = Self::new();
        if let Ok(s) = std::fs::read_to_string(path) {
            for line in s.lines() {
                let mut it = line.split('\t');
                if let (Some(hx), Some(w), Some(t)) = (it.next(), it.next(), it.next()) {
                    if let (Some(pk), Ok(w), Ok(t)) =
                        (crate::hex_decode(hx), w.parse::<f64>(), t.parse::<i64>())
                    {
                        r.weights.insert(pk, (w, t));
                    }
                }
            }
        }
        r
    }

    /// Persist to a TSV file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut out = String::new();
        for (pk, (w, t)) in &self.weights {
            let hx: String = pk.iter().map(|b| format!("{b:02x}")).collect();
            out.push_str(&format!("{hx}\t{w}\t{t}\n"));
        }
        std::fs::write(path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reward_then_decay_toward_default() {
        let mut r = Reputation::new();
        let pk = vec![1u8; 32];
        let t0 = 1_000_000_000_000_000_000;
        r.reward(&pk, t0); // 1.0 -> 1.5
        assert!((r.weight(&pk, t0) - 1.5).abs() < 1e-9);
        // after one half-life, (1.5 - 1.0) halves -> 1.25
        let after = r.weight(&pk, t0 + HALF_LIFE_NS);
        assert!((after - 1.25).abs() < 1e-3, "after={after}");
    }

    #[test]
    fn reward_is_capped() {
        let mut r = Reputation::new();
        let pk = vec![2u8; 32];
        let t = 1_000_000_000_000_000_000;
        for _ in 0..50 {
            r.reward(&pk, t);
        }
        assert!(r.weight(&pk, t) <= MAX_WEIGHT);
    }

    #[test]
    fn save_load_roundtrip() {
        let mut r = Reputation::new();
        let pk = vec![3u8; 32];
        let t = 1_700_000_000_000_000_000;
        r.reward(&pk, t);
        let path = std::env::temp_dir().join(format!("earthnet_rep_{}.tsv", std::process::id()));
        r.save(&path).unwrap();
        let r2 = Reputation::load(&path);
        assert!((r2.weight(&pk, t) - r.weight(&pk, t)).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }
}
