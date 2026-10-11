//! Manual calibration from user-picked control points.
//!
//! OWNER: grid agent. Contract (may be extended, not broken):

use crate::geo::{latlon_to_merc, Affine, Homography};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ControlPoint {
    pub x: f64,
    pub y: f64,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFit {
    pub transform: Homography,
    /// "similarity" | "affine" | "homography"
    pub model: String,
    pub rms_m: f64,
    pub residuals_m: Vec<f64>,
}

/// Points closer than this (pixels) on the map are the same point.
const MIN_PIXEL_SEP: f64 = 1.0;
/// Points closer than this (ground metres) are the same place.
const MIN_GROUND_SEP_M: f64 = 0.5;

/// Thickness / length ratio below which a point set counts as a straight line.
fn is_collinear(pts: &[(f64, f64)]) -> bool {
    let n = pts.len() as f64;
    let (mx, my) = pts.iter().fold((0.0, 0.0), |s, p| (s.0 + p.0 / n, s.1 + p.1 / n));
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in pts {
        let (dx, dy) = (p.0 - mx, p.1 - my);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    let tr = sxx + syy;
    let det = sxx * syy - sxy * sxy;
    let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
    let (l1, l2) = (tr / 2.0 + disc, (tr / 2.0 - disc).max(0.0));
    l1 <= 0.0 || l2 <= 1e-4 * l1
}

/// Pixel → Mercator similarity including the image y-flip (a reflection in
/// the plane, so a north-up map never comes out mirrored).
fn similarity(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Affine> {
    let flipped: Vec<(f64, f64)> = src.iter().map(|&(x, y)| (x, -y)).collect();
    let s = Affine::fit_similarity(&flipped, dst, false)?;
    Some(Affine { a: s.a, b: -s.b, c: s.c, d: s.d, e: -s.e, f: s.f })
}

/// Jacobian determinant of a homography at a point.
fn jacobian_det(h: &Homography, x: f64, y: f64) -> f64 {
    let m = &h.m;
    let w = m[6] * x + m[7] * y + m[8];
    let u = m[0] * x + m[1] * y + m[2];
    let v = m[3] * x + m[4] * y + m[5];
    let dux = (m[0] * w - u * m[6]) / (w * w);
    let duy = (m[1] * w - u * m[7]) / (w * w);
    let dvx = (m[3] * w - v * m[6]) / (w * w);
    let dvy = (m[4] * w - v * m[7]) / (w * w);
    dux * dvy - duy * dvx
}

/// `model`: "auto" (2 → similarity, 3 → exact affine, ≥4 → least-squares
/// affine; collinear points fall back to a similarity), "similarity",
/// "affine" (≥3), "homography" (≥4). Fits are done in Web Mercator metres;
/// residuals and RMS are ground metres.
pub fn fit(points: &[ControlPoint], model: &str) -> Result<ControlFit, String> {
    let n = points.len();
    if n < 2 {
        return Err("At least 2 control points are needed.".into());
    }
    for (i, p) in points.iter().enumerate() {
        if !(p.x.is_finite() && p.y.is_finite() && p.lat.is_finite() && p.lon.is_finite())
            || p.lat.abs() > 85.0
            || p.lon.abs() > 180.0
        {
            return Err(format!("Control point {} has invalid coordinates.", i + 1));
        }
    }
    let src: Vec<(f64, f64)> = points.iter().map(|p| (p.x, p.y)).collect();
    let dst: Vec<(f64, f64)> = points.iter().map(|p| latlon_to_merc(p.lat, p.lon)).collect();
    for i in 0..n {
        for j in i + 1..n {
            let dp = (src[i].0 - src[j].0).hypot(src[i].1 - src[j].1);
            let g = points[i].lat.to_radians().cos();
            let dg = (dst[i].0 - dst[j].0).hypot(dst[i].1 - dst[j].1) * g;
            if dp < MIN_PIXEL_SEP || dg < MIN_GROUND_SEP_M {
                return Err(format!("Control points {} and {} are at the same place.", i + 1, j + 1));
            }
        }
    }
    let collinear = n >= 3 && (is_collinear(&src) || is_collinear(&dst));
    let kind = match model {
        "auto" => {
            if n == 2 || collinear {
                "similarity"
            } else {
                "affine"
            }
        }
        "similarity" => "similarity",
        "affine" => {
            if n < 3 {
                return Err("An affine fit needs at least 3 control points.".into());
            }
            if collinear {
                return Err("Control points lie on a line; spread them out.".into());
            }
            "affine"
        }
        "homography" => {
            if n < 4 {
                return Err("A homography needs at least 4 control points.".into());
            }
            if collinear {
                return Err("Control points lie on a line; spread them out.".into());
            }
            "homography"
        }
        other => return Err(format!("Unknown model \"{other}\".")),
    };
    let transform = match kind {
        "similarity" => similarity(&src, &dst).ok_or("Similarity fit failed.")?.to_homography(),
        "affine" => {
            let a = Affine::fit(&src, &dst).ok_or("Affine fit failed (degenerate points).")?;
            if a.det() >= 0.0 {
                return Err("Control points imply a mirrored map; check each map point matches its place.".into());
            }
            a.to_homography()
        }
        _ => {
            let h = Homography::fit(&src, &dst).ok_or("Homography fit failed (degenerate points).")?;
            if src.iter().any(|&(x, y)| {
                let d = jacobian_det(&h, x, y);
                d.is_nan() || d >= 0.0
            }) {
                return Err("Control points imply a folded or mirrored map; check the point pairs.".into());
            }
            h
        }
    };
    let residuals_m: Vec<f64> = src
        .iter()
        .zip(&dst)
        .zip(points)
        .map(|((s, d), p)| {
            let q = transform.apply(s.0, s.1);
            (q.0 - d.0).hypot(q.1 - d.1) * p.lat.to_radians().cos()
        })
        .collect();
    if !residuals_m.iter().all(|r| r.is_finite()) {
        return Err("Fit is numerically unstable.".into());
    }
    let rms_m = (residuals_m.iter().map(|r| r * r).sum::<f64>() / n as f64).sqrt();
    Ok(ControlFit { transform, model: kind.to_string(), rms_m, residuals_m })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::merc_to_latlon;

    fn truth() -> Affine {
        // ≈ Grootkloof: 1.9 m/px, rotated 2°.
        Affine::pixel_to_merc_similarity(2.12, 2f64.to_radians(), 1750.0, 2480.0, 3_050_000.0, -2_980_000.0)
    }

    fn pt(x: f64, y: f64) -> ControlPoint {
        let (mx, my) = truth().apply(x, y);
        let (lat, lon) = merc_to_latlon(mx, my);
        ControlPoint { x, y, lat, lon }
    }

    #[test]
    fn auto_models() {
        let two = [pt(100.0, 200.0), pt(3000.0, 4000.0)];
        let f = fit(&two, "auto").unwrap();
        assert_eq!(f.model, "similarity");
        assert!(f.rms_m < 1e-6);
        let (x, y) = f.transform.apply(1500.0, 900.0);
        let (tx, ty) = truth().apply(1500.0, 900.0);
        assert!((x - tx).abs() < 1e-4 && (y - ty).abs() < 1e-4);
        assert!(f.transform.to_affine().det() < 0.0);

        let three = [pt(100.0, 200.0), pt(3000.0, 300.0), pt(1500.0, 4000.0)];
        let f = fit(&three, "auto").unwrap();
        assert_eq!(f.model, "affine");
        assert!(f.rms_m < 1e-6);

        let mut five: Vec<ControlPoint> =
            [(100.0, 200.0), (3000.0, 300.0), (1500.0, 4000.0), (200.0, 4500.0), (2900.0, 4600.0)]
                .iter()
                .map(|&(x, y)| pt(x, y))
                .collect();
        five[4].lat += 2e-5; // ≈ 2.2 m error on one point
        let f = fit(&five, "auto").unwrap();
        assert_eq!(f.model, "affine");
        assert!(f.rms_m > 0.1 && f.rms_m < 2.5, "{}", f.rms_m);
        assert_eq!(f.residuals_m.len(), 5);
        let h = fit(&five, "homography").unwrap();
        assert_eq!(h.model, "homography");
        let s = fit(&five, "similarity").unwrap();
        assert_eq!(s.model, "similarity");
    }

    #[test]
    fn errors() {
        assert!(fit(&[pt(1.0, 2.0)], "auto").is_err());
        assert!(fit(&[pt(1.0, 2.0), pt(1.2, 2.1)], "auto").is_err(), "duplicate");
        let line = [pt(0.0, 0.0), pt(100.0, 100.0), pt(200.0, 200.0)];
        assert!(fit(&line, "affine").is_err());
        assert_eq!(fit(&line, "auto").unwrap().model, "similarity");
        assert!(fit(&[pt(0.0, 0.0), pt(100.0, 0.0), pt(0.0, 100.0)], "homography").is_err());
        assert!(fit(&[pt(0.0, 0.0), pt(100.0, 0.0)], "bogus").is_err());
        // Mirrored: swap the ground positions of two of three points.
        let mut m = [pt(0.0, 0.0), pt(1000.0, 0.0), pt(0.0, 1000.0)];
        let (a, b) = ((m[1].lat, m[1].lon), (m[2].lat, m[2].lon));
        (m[1].lat, m[1].lon, m[2].lat, m[2].lon) = (b.0, b.1, a.0, a.1);
        assert!(fit(&m, "affine").is_err());
    }
}
