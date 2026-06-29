//! Travel-time phase association / hypocenter location.
//!
//! Grid-searches the hypocenter (lat, lon, **depth**) + origin time that best
//! fit P picks, returning the fit and its RMS residual. A low RMS means the
//! picks are consistent with one earthquake; a high RMS means they are not
//! (random coincidence) — that is how association rejects false positives, and
//! the rejection sharpens as picks become over-determined.
//!
//! P first-arrival times come from a precomputed **taup (iasp91) table**
//! ([`travel_time`], [`crate::ttable`]) bilinearly interpolated over distance and
//! depth — real ray-bending (Pn/Pg head waves), not straight rays. Deterministic;
//! no ML (DESIGN guardrail).

use crate::geo::haversine_km;
use crate::ttable;

/// Candidate source depths (km) for the grid search.
const DEPTHS_KM: [f64; 7] = [0.0, 5.0, 15.0, 30.0, 50.0, 80.0, 120.0];

/// A located source.
#[derive(Debug, Clone, Copy)]
pub struct Hypocenter {
    pub lat: f64,
    pub lon: f64,
    pub depth_km: f64,
    pub origin_ns: i64,
    pub rms_s: f64,
    pub n: usize,
}

/// P first-arrival travel time (s) from a source at (`epi_km` epicentral
/// distance, `depth_km` depth) to a surface station — bilinear interpolation of
/// the taup (iasp91) table over (depth, distance).
pub fn travel_time(epi_km: f64, depth_km: f64) -> f64 {
    let dist_deg = epi_km / ttable::DEG_TO_KM;
    // distance axis (uniform step). Beyond the table we EXTRAPOLATE on the last
    // segment's slope (≈ Pn) rather than clamp — clamping would make far-apart
    // stations share a travel time and look spuriously coherent.
    let fd = (dist_deg / ttable::DIST_STEP_DEG).max(0.0);
    let i0 = (fd.floor() as usize).min(ttable::NDIST - 2);
    let i1 = i0 + 1;
    let tx = fd - i0 as f64;
    // depth axis (non-uniform): find bracketing rows
    let depths = &ttable::DEPTHS_KM;
    let dc = depth_km.clamp(depths[0], depths[depths.len() - 1]);
    let mut j0 = 0;
    while j0 + 1 < depths.len() && depths[j0 + 1] <= dc {
        j0 += 1;
    }
    let j1 = (j0 + 1).min(depths.len() - 1);
    let ty = if depths[j1] > depths[j0] {
        (dc - depths[j0]) / (depths[j1] - depths[j0])
    } else {
        0.0
    };
    let a = ttable::TT[j0][i0] + (ttable::TT[j0][i1] - ttable::TT[j0][i0]) * tx;
    let b = ttable::TT[j1][i0] + (ttable::TT[j1][i1] - ttable::TT[j1][i0]) * tx;
    a + (b - a) * ty
}

/// Locates the best-fitting hypocenter for picks `(lat, lon, t_ns)`.
/// Returns None for fewer than 3 picks.
pub fn locate(picks: &[(f64, f64, i64)]) -> Option<Hypocenter> {
    if picks.len() < 3 {
        return None;
    }
    let t0 = picks.iter().map(|p| p.2).min().unwrap();
    let obs: Vec<(f64, f64, f64)> = picks
        .iter()
        .map(|&(la, lo, t)| (la, lo, (t - t0) as f64 / 1e9))
        .collect();
    let clat = obs.iter().map(|o| o.0).sum::<f64>() / obs.len() as f64;
    let clon = obs.iter().map(|o| o.1).sum::<f64>() / obs.len() as f64;

    // Seed with the centroid fit (best depth) so spatially-unconstrained picks
    // keep the centroid rather than drifting to a grid corner.
    let (mut blat, mut blon, mut bdep, mut brms, mut borigin) = {
        let (depth, rms, origin) = best_depth(&obs, clat, clon);
        (clat, clon, depth, rms, origin)
    };
    // Stage 1: coarse +-3 deg @ 0.25; stage 2: fine +-0.25 @ 0.02.
    for &(span, step) in &[(3.0_f64, 0.25_f64), (0.25, 0.02)] {
        let (cla, clo) = (blat, blon);
        let steps = (2.0 * span / step) as i64;
        for i in 0..=steps {
            let la = cla - span + i as f64 * step;
            for j in 0..=steps {
                let lo = clo - span + j as f64 * step;
                let (depth, rms, origin) = best_depth(&obs, la, lo);
                if rms < brms {
                    brms = rms;
                    blat = la;
                    blon = lo;
                    bdep = depth;
                    borigin = origin;
                }
            }
        }
    }
    Some(Hypocenter {
        lat: blat,
        lon: blon,
        depth_km: bdep,
        origin_ns: t0 + (borigin * 1e9) as i64,
        rms_s: brms,
        n: picks.len(),
    })
}

/// Windowed phase association (binder-style): over a window of picks (which may
/// include noise), grid-search the hypocenter that explains the MOST picks as
/// inliers — picks whose implied origin (`t - travel_time`) clusters around a
/// common value within `residual_tol_s`. Returns the located hypocenter and the
/// indices of the inlier picks, iff at least `min_n` inliers are found.
///
/// This is the dense-network fix: it extracts the largest coherent subset
/// instead of greedily firing on the first N, so a real event is recovered even
/// when mixed with unrelated noise picks.
pub fn associate_window(
    picks: &[(f64, f64, i64)],
    min_n: usize,
    residual_tol_s: f64,
    span_deg: f64,
) -> Option<(Hypocenter, Vec<usize>)> {
    if picks.len() < min_n {
        return None;
    }
    let t0 = picks.iter().map(|p| p.2).min().unwrap();
    let obs: Vec<(f64, f64, f64)> = picks
        .iter()
        .map(|&(la, lo, t)| (la, lo, (t - t0) as f64 / 1e9))
        .collect();
    let clat = obs.iter().map(|o| o.0).sum::<f64>() / obs.len() as f64;
    let clon = obs.iter().map(|o| o.1).sum::<f64>() / obs.len() as f64;
    let step = (span_deg / 12.0).max(0.02);
    let steps = (2.0 * span_deg / step) as i64;

    struct Cand {
        count: usize,
        rms: f64,
        lat: f64,
        lon: f64,
        depth: f64,
        origin: f64,
        inliers: Vec<usize>,
    }
    let mut best: Option<Cand> = None;
    for i in 0..=steps {
        let la = clat - span_deg + i as f64 * step;
        for j in 0..=steps {
            let lo = clon - span_deg + j as f64 * step;
            for &depth in &DEPTHS_KM {
                let implied: Vec<f64> = obs
                    .iter()
                    .map(|&(sla, slo, t)| {
                        t - travel_time(haversine_km((la, lo), (sla, slo)), depth)
                    })
                    .collect();
                let origin = median(&implied);
                let inliers: Vec<usize> = implied
                    .iter()
                    .enumerate()
                    .filter(|(_, &v)| (v - origin).abs() <= residual_tol_s)
                    .map(|(k, _)| k)
                    .collect();
                if inliers.len() < min_n {
                    continue;
                }
                let ss: f64 = inliers.iter().map(|&k| (implied[k] - origin).powi(2)).sum();
                let rms = (ss / inliers.len() as f64).sqrt();
                let better = match &best {
                    None => true,
                    Some(b) => inliers.len() > b.count || (inliers.len() == b.count && rms < b.rms),
                };
                if better {
                    best = Some(Cand {
                        count: inliers.len(),
                        rms,
                        lat: la,
                        lon: lo,
                        depth,
                        origin,
                        inliers,
                    });
                }
            }
        }
    }
    best.map(|c| {
        (
            Hypocenter {
                lat: c.lat,
                lon: c.lon,
                depth_km: c.depth,
                origin_ns: t0 + (c.origin * 1e9) as i64,
                rms_s: c.rms,
                n: c.count,
            },
            c.inliers,
        )
    })
}

fn median(xs: &[f64]) -> f64 {
    let mut v: Vec<f64> = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = v.len() / 2;
    if v.len() % 2 == 0 {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    }
}

/// Best (depth, rms, origin) over the depth grid for a candidate epicenter.
fn best_depth(obs: &[(f64, f64, f64)], la: f64, lo: f64) -> (f64, f64, f64) {
    let mut best = (0.0, f64::INFINITY, 0.0);
    for &depth in &DEPTHS_KM {
        let (rms, origin) = fit(obs, la, lo, depth);
        if rms < best.1 {
            best = (depth, rms, origin);
        }
    }
    best
}

/// For a candidate (epicenter, depth): best origin time and RMS residual.
fn fit(obs: &[(f64, f64, f64)], la: f64, lo: f64, depth: f64) -> (f64, f64) {
    let preds: Vec<f64> = obs
        .iter()
        .map(|&(sla, slo, t)| t - travel_time(haversine_km((la, lo), (sla, slo)), depth))
        .collect();
    let origin = preds.iter().sum::<f64>() / preds.len() as f64;
    let ss = preds.iter().map(|p| (p - origin).powi(2)).sum::<f64>();
    ((ss / preds.len() as f64).sqrt(), origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(
        lat: f64,
        lon: f64,
        depth: f64,
        origin_s: f64,
        stations: &[(f64, f64)],
    ) -> Vec<(f64, f64, i64)> {
        stations
            .iter()
            .map(|&(la, lo)| {
                let t = origin_s + travel_time(haversine_km((lat, lon), (la, lo)), depth);
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
    fn locates_consistent_source_with_low_rms_and_depth() {
        let picks = synth(-21.0, -69.8, 30.0, 1000.0, &STATIONS);
        let h = locate(&picks).expect("should locate");
        assert!(h.rms_s < 0.3, "rms={}", h.rms_s);
        assert!(
            haversine_km((h.lat, h.lon), (-21.0, -69.8)) < 40.0,
            "epicenter off"
        );
        assert!((h.depth_km - 30.0).abs() <= 20.0, "depth={}", h.depth_km);
    }

    #[test]
    fn inconsistent_times_give_high_rms() {
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
        let h = locate(&picks).expect("best fit");
        assert!(h.rms_s > 1.5, "incoherent picks rms={}", h.rms_s);
    }

    #[test]
    fn deeper_source_resolved() {
        let picks = synth(-21.0, -69.8, 80.0, 500.0, &STATIONS);
        let h = locate(&picks).expect("locate");
        assert!(h.rms_s < 0.3);
        assert!(
            h.depth_km >= 50.0,
            "should resolve a deep source, got {}",
            h.depth_km
        );
    }

    #[test]
    fn too_few_picks_is_none() {
        assert!(locate(&[(-21.0, -69.5, 0), (-21.3, -69.9, 1_000_000_000)]).is_none());
    }

    #[test]
    fn windowed_association_extracts_coherent_subset_amid_noise() {
        let mut picks = synth(-21.0, -69.8, 30.0, 1000.0, &STATIONS); // 5 coherent
                                                                      // unrelated noise picks (times don't fit the source moveout)
        picks.push((-20.5, -69.4, (1000.0 + 12.0) as i64 * 1_000_000_000));
        picks.push((-21.4, -70.1, (1000.0 + 25.0) as i64 * 1_000_000_000));
        let (h, inliers) = associate_window(&picks, 4, 1.5, 2.0).expect("should associate");
        assert!(
            inliers.len() >= 5,
            "recover the 5 coherent picks, got {}",
            inliers.len()
        );
        assert!(
            haversine_km((h.lat, h.lon), (-21.0, -69.8)) < 50.0,
            "epicenter off"
        );
    }

    #[test]
    fn windowed_association_rejects_overdetermined_noise() {
        // incoherent times; requiring ALL 5 to fit one source (over-determined)
        // has no solution -> rejected. (At low min_n a chance subset can fit;
        // rejection power comes from over-determination / density.)
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
        assert!(associate_window(&picks, 5, 1.0, 2.0).is_none());
    }
}
