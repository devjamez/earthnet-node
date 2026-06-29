//! Travel-time phase association / hypocenter location.
//!
//! Given P picks (lat, lon, time) it grid-searches the epicenter + origin time
//! that best fit a single source under a homogeneous-velocity model
//! (t_pred = origin + distance / Vp), returning the best fit and its RMS
//! residual. A low RMS means the picks are consistent with one earthquake; a
//! high RMS means they are not (random coincidence) — that is how association
//! rejects false positives, and the rejection sharpens as picks become
//! over-determined (> 3). Deterministic; no ML (DESIGN guardrail).

use crate::geo::haversine_km;

/// Crustal P velocity (km/s) for the homogeneous model.
pub const VP_KM_S: f64 = 6.0;

/// A located source.
#[derive(Debug, Clone, Copy)]
pub struct Hypocenter {
    pub lat: f64,
    pub lon: f64,
    pub origin_ns: i64,
    pub rms_s: f64,
    pub n: usize,
}

/// Locates the best-fitting hypocenter for the given picks `(lat, lon, t_ns)`.
/// Returns None for fewer than 3 picks. Uses a two-stage grid search centered
/// on the picks' centroid.
pub fn locate(picks: &[(f64, f64, i64)], vp_km_s: f64) -> Option<Hypocenter> {
    if picks.len() < 3 {
        return None;
    }
    // Work in seconds relative to the earliest pick for numerical stability.
    let t0 = picks.iter().map(|p| p.2).min().unwrap();
    let obs: Vec<(f64, f64, f64)> = picks
        .iter()
        .map(|&(la, lo, t)| (la, lo, (t - t0) as f64 / 1e9))
        .collect();

    let clat = obs.iter().map(|o| o.0).sum::<f64>() / obs.len() as f64;
    let clon = obs.iter().map(|o| o.1).sum::<f64>() / obs.len() as f64;

    // Stage 1: coarse grid +-3 deg @ 0.25; stage 2: fine +-0.25 @ 0.02.
    // Seed with the centroid fit so that spatially-unconstrained picks (no
    // moveout) keep the centroid rather than drifting to a grid corner.
    let (mut blat, mut blon, mut brms, mut borigin) = {
        let (rms, origin) = fit(&obs, clat, clon, vp_km_s)?;
        (clat, clon, rms, origin)
    };
    for &(span, step) in &[(3.0_f64, 0.25_f64), (0.25, 0.02)] {
        let (cla, clo) = (blat, blon);
        let steps = (2.0 * span / step) as i64;
        for i in 0..=steps {
            let la = cla - span + i as f64 * step;
            for j in 0..=steps {
                let lo = clo - span + j as f64 * step;
                if let Some((rms, origin)) = fit(&obs, la, lo, vp_km_s) {
                    if rms < brms {
                        brms = rms;
                        blat = la;
                        blon = lo;
                        borigin = origin;
                    }
                }
            }
        }
    }
    if !brms.is_finite() {
        return None;
    }
    Some(Hypocenter {
        lat: blat,
        lon: blon,
        origin_ns: t0 + (borigin * 1e9) as i64,
        rms_s: brms,
        n: picks.len(),
    })
}

/// For a candidate epicenter, the best origin time (mean of pick - travel) and
/// the RMS residual of the fit.
fn fit(obs: &[(f64, f64, f64)], la: f64, lo: f64, vp: f64) -> Option<(f64, f64)> {
    let mut preds = Vec::with_capacity(obs.len());
    for &(sla, slo, t) in obs {
        let tau = haversine_km((la, lo), (sla, slo)) / vp;
        preds.push(t - tau); // implied origin for this station
    }
    let origin = preds.iter().sum::<f64>() / preds.len() as f64;
    let ss = preds.iter().map(|p| (p - origin).powi(2)).sum::<f64>();
    Some(((ss / preds.len() as f64).sqrt(), origin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::haversine_km;

    // Build synthetic picks consistent with a true source.
    fn synth(
        true_lat: f64,
        true_lon: f64,
        origin_s: f64,
        stations: &[(f64, f64)],
        jitter: &[f64],
    ) -> Vec<(f64, f64, i64)> {
        stations
            .iter()
            .zip(jitter)
            .map(|(&(la, lo), &j)| {
                let tau = haversine_km((true_lat, true_lon), (la, lo)) / VP_KM_S;
                let t = origin_s + tau + j;
                (la, lo, (t * 1e9) as i64)
            })
            .collect()
    }

    const STATIONS: [(f64, f64); 5] = [
        (-21.0, -69.5),
        (-21.3, -69.9),
        (-20.1, -69.2),
        (-22.0, -70.0),
        (-19.8, -69.7),
    ];

    #[test]
    fn locates_consistent_source_with_low_rms() {
        let picks = synth(-21.0, -69.8, 1000.0, &STATIONS, &[0.0; 5]);
        let h = locate(&picks, VP_KM_S).expect("should locate");
        assert!(h.rms_s < 0.5, "rms={}", h.rms_s);
        assert!(
            haversine_km((h.lat, h.lon), (-21.0, -69.8)) < 40.0,
            "epicenter off"
        );
    }

    #[test]
    fn inconsistent_times_give_high_rms() {
        // same stations, but random/incoherent times (not a single source)
        let picks: Vec<(f64, f64, i64)> = STATIONS
            .iter()
            .enumerate()
            .map(|(i, &(la, lo))| {
                (
                    la,
                    lo,
                    (1000.0 + [0.0, 9.0, 2.0, 11.0, 4.0][i]) as i64 * 1_000_000_000,
                )
            })
            .collect();
        let h = locate(&picks, VP_KM_S).expect("locate returns a best fit");
        assert!(
            h.rms_s > 1.5,
            "incoherent picks should not fit one source, rms={}",
            h.rms_s
        );
    }

    #[test]
    fn too_few_picks_is_none() {
        assert!(locate(&[(-21.0, -69.5, 0), (-21.3, -69.9, 1_000_000_000)], VP_KM_S).is_none());
    }
}
