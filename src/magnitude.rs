//! First-order magnitude estimation.
//!
//! Official sources carry an authoritative magnitude (used directly). For
//! phone-only consensus there is none, so we estimate from peak ground
//! acceleration and distance via an inverted GMPE-lite relation:
//!
//!   M ≈ A·log10(PGA[cm/s²]) + B·log10(R[km]) + C
//!
//! Coefficients CALIBRATED by least squares from 48 real CX strong-motion records
//! (instrument response removed) of M5.0–5.8 northern-Chile earthquakes, 2023–24
//! (backtest/calibrate_gmpe.py); in-sample residual RMS ≈ 0.15 mag. Caveat: the
//! calibration magnitude range is narrow, so estimates outside ~M5 are less
//! certain — recalibrate with a wider catalog. Still reported with uncertainty.

const A: f64 = 0.1825;
const B: f64 = 0.6666;
const C: f64 = 3.8335;
const G_TO_CM_S2: f64 = 980.665;

/// Provisional magnitude from PGA (in g) and hypocentral distance (km).
/// Returns 0.0 when PGA is non-positive.
pub fn estimate_from_pga(pga_g: f32, distance_km: f64) -> f32 {
    if pga_g <= 0.0 {
        return 0.0;
    }
    let pga = pga_g as f64 * G_TO_CM_S2;
    let r = distance_km.max(1.0);
    let m = A * pga.log10() + B * r.log10() + C;
    m.clamp(0.0, 10.0) as f32
}

/// Uncertainty (magnitude units) attached to a provisional PGA estimate.
/// Above the in-sample fit RMS to reflect out-of-calibration-range uncertainty.
pub const PROVISIONAL_UNCERT: f32 = 0.5;
/// Uncertainty attached to an authoritative official magnitude.
pub const OFFICIAL_UNCERT: f32 = 0.2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_pga_yields_zero() {
        assert_eq!(estimate_from_pga(0.0, 30.0), 0.0);
    }

    #[test]
    fn monotonic_in_pga() {
        let weak = estimate_from_pga(0.01, 30.0);
        let strong = estimate_from_pga(0.3, 30.0);
        assert!(strong > weak, "weak={weak} strong={strong}");
    }

    #[test]
    fn produces_sane_range() {
        // a moderate near-ish shake should land in a believable magnitude band
        let m = estimate_from_pga(0.05, 35.0);
        assert!((3.0..=8.0).contains(&m), "m={m}");
    }

    #[test]
    fn clamped_to_ten() {
        assert!(estimate_from_pga(50.0, 1.0) <= 10.0);
    }
}
