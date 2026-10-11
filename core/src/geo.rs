//! Geodesy and planar transforms.
//!
//! Ground frame is Web Mercator (EPSG:3857) metres; image pixels are top-left
//! origin with y down. See docs/ARCHITECTURE.md for the conventions.

use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// WGS84 semi-major axis, also the Web Mercator sphere radius.
pub const EARTH_RADIUS: f64 = 6_378_137.0;
/// Mean Earth radius for great-circle distances.
pub const MEAN_EARTH_RADIUS: f64 = 6_371_008.8;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }
}

/// (lat, lon) degrees → Web Mercator metres.
pub fn latlon_to_merc(lat: f64, lon: f64) -> (f64, f64) {
    let lat = lat.clamp(-85.051_128_78, 85.051_128_78);
    let x = EARTH_RADIUS * lon.to_radians();
    let y = EARTH_RADIUS * (PI / 4.0 + lat.to_radians() / 2.0).tan().ln();
    (x, y)
}

/// Web Mercator metres → (lat, lon) degrees.
pub fn merc_to_latlon(x: f64, y: f64) -> (f64, f64) {
    let lon = (x / EARTH_RADIUS).to_degrees();
    let lat = (2.0 * (y / EARTH_RADIUS).exp().atan() - PI / 2.0).to_degrees();
    (lat, lon)
}

/// Ground metres per Mercator metre at a latitude (= cos φ).
pub fn ground_per_merc(lat: f64) -> f64 {
    lat.to_radians().cos()
}

/// Great-circle distance in metres.
pub fn haversine_m(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lon - a.lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * MEAN_EARTH_RADIUS * h.sqrt().min(1.0).asin()
}

/// Initial bearing from `a` to `b`, degrees clockwise from true north in [0, 360).
pub fn initial_bearing_deg(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dl = (b.lon - a.lon).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Point reached from `a` travelling `dist_m` along `bearing_deg`.
pub fn destination(a: LatLon, bearing_deg: f64, dist_m: f64) -> LatLon {
    let d = dist_m / MEAN_EARTH_RADIUS;
    let b = bearing_deg.to_radians();
    let p1 = a.lat.to_radians();
    let l1 = a.lon.to_radians();
    let p2 = (p1.sin() * d.cos() + p1.cos() * d.sin() * b.cos()).asin();
    let l2 = l1 + (b.sin() * d.sin() * p1.cos()).atan2(d.cos() - p1.sin() * p2.sin());
    LatLon::new(p2.to_degrees(), ((l2.to_degrees() + 540.0) % 360.0) - 180.0)
}

/// 2-D affine transform: `x' = a·x + b·y + c`, `y' = d·x + e·y + f`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 0.0, e: 1.0, f: 0.0 };

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.b * y + self.c, self.d * x + self.e * y + self.f)
    }

    /// Linear part only (for vectors).
    pub fn apply_vec(&self, dx: f64, dy: f64) -> (f64, f64) {
        (self.a * dx + self.b * dy, self.d * dx + self.e * dy)
    }

    pub fn det(&self) -> f64 {
        self.a * self.e - self.b * self.d
    }

    pub fn inverse(&self) -> Option<Affine> {
        let det = self.det();
        if !det.is_finite() || det.abs() < 1e-300 {
            return None;
        }
        let ia = self.e / det;
        let ib = -self.b / det;
        let id = -self.d / det;
        let ie = self.a / det;
        Some(Affine { a: ia, b: ib, c: -(ia * self.c + ib * self.f), d: id, e: ie, f: -(id * self.c + ie * self.f) })
    }

    /// `self ∘ other` (apply `other` first, then `self`).
    pub fn compose(&self, other: &Affine) -> Affine {
        Affine {
            a: self.a * other.a + self.b * other.d,
            b: self.a * other.b + self.b * other.e,
            c: self.a * other.c + self.b * other.f + self.c,
            d: self.d * other.a + self.e * other.d,
            e: self.d * other.b + self.e * other.e,
            f: self.d * other.c + self.e * other.f + self.f,
        }
    }

    pub fn translation(tx: f64, ty: f64) -> Affine {
        Affine { a: 1.0, b: 0.0, c: tx, d: 0.0, e: 1.0, f: ty }
    }

    /// Pixel → Mercator similarity for a map whose "up" points `theta_rad`
    /// clockwise from north, with `scale` Mercator metres per pixel, mapping
    /// pixel (cx, cy) to Mercator (tx, ty). Includes the y-flip (image y is down).
    pub fn pixel_to_merc_similarity(scale: f64, theta_rad: f64, cx: f64, cy: f64, tx: f64, ty: f64) -> Affine {
        let (s, c) = theta_rad.sin_cos();
        let a = scale * c;
        let b = -scale * s;
        let d = -scale * s;
        let e = -scale * c;
        Affine { a, b, c: tx - (a * cx + b * cy), d, e, f: ty - (d * cx + e * cy) }
    }

    /// Mean scale (sqrt|det|) in destination units per source unit.
    pub fn mean_scale(&self) -> f64 {
        self.det().abs().sqrt()
    }

    /// For a pixel→Mercator transform: clockwise angle (degrees) of map-up from north.
    pub fn map_up_bearing_deg(&self) -> f64 {
        // pixel "up" vector (0,-1) → Mercator (-b, -e)
        let (vx, vy) = (-self.b, -self.e);
        (vx.atan2(vy).to_degrees() + 360.0) % 360.0
    }

    /// Least-squares affine fit `dst ≈ A(src)`. Needs ≥3 non-collinear pairs.
    pub fn fit(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Affine> {
        fit_affine_weighted(src, dst, None)
    }

    /// Least-squares similarity (rotation+uniform scale+translation, reflection
    /// allowed when `allow_reflection`). Needs ≥2 distinct pairs.
    pub fn fit_similarity(src: &[(f64, f64)], dst: &[(f64, f64)], allow_reflection: bool) -> Option<Affine> {
        let n = src.len();
        if n < 2 || dst.len() != n {
            return None;
        }
        let nf = n as f64;
        let (mut sx, mut sy, mut dx, mut dy) = (0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            sx += src[i].0;
            sy += src[i].1;
            dx += dst[i].0;
            dy += dst[i].1;
        }
        let (sx, sy, dx, dy) = (sx / nf, sy / nf, dx / nf, dy / nf);
        // Proper similarity: dst = s R src.  Reflected: dst = s R F src with F = diag(1,-1).
        let solve = |reflect: bool| -> Option<(Affine, f64)> {
            let (mut p, mut q, mut ss) = (0.0, 0.0, 0.0);
            for i in 0..n {
                let ux = src[i].0 - sx;
                let uy = if reflect { -(src[i].1 - sy) } else { src[i].1 - sy };
                let vx = dst[i].0 - dx;
                let vy = dst[i].1 - dy;
                p += ux * vx + uy * vy;
                q += ux * vy - uy * vx;
                ss += ux * ux + uy * uy;
            }
            if ss < 1e-12 {
                return None;
            }
            let a = p / ss; // s cosθ
            let b = q / ss; // s sinθ
                            // [vx,vy] = [[a,-b],[b,a]] [ux,uy]
            let (m00, m01, m10, m11) = if reflect { (a, b, b, -a) } else { (a, -b, b, a) };
            let t =
                Affine { a: m00, b: m01, c: dx - (m00 * sx + m01 * sy), d: m10, e: m11, f: dy - (m10 * sx + m11 * sy) };
            let mut err = 0.0;
            for i in 0..n {
                let (x, y) = t.apply(src[i].0, src[i].1);
                err += (x - dst[i].0).powi(2) + (y - dst[i].1).powi(2);
            }
            Some((t, err))
        };
        let proper = solve(false);
        if !allow_reflection {
            return proper.map(|x| x.0);
        }
        let refl = solve(true);
        match (proper, refl) {
            (Some(a), Some(b)) => Some(if b.1 < a.1 { b.0 } else { a.0 }),
            (Some(a), None) => Some(a.0),
            (None, Some(b)) => Some(b.0),
            _ => None,
        }
    }

    pub fn to_homography(&self) -> Homography {
        Homography { m: [self.a, self.b, self.c, self.d, self.e, self.f, 0.0, 0.0, 1.0] }
    }
}

/// Weighted least-squares affine fit. `w` = per-pair weights (None = all 1).
pub fn fit_affine_weighted(src: &[(f64, f64)], dst: &[(f64, f64)], w: Option<&[f64]>) -> Option<Affine> {
    let n = src.len();
    if n < 3 || dst.len() != n {
        return None;
    }
    // Normalise for conditioning.
    let (mut mx, mut my, mut wsum) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let wi = w.map_or(1.0, |w| w[i]);
        mx += wi * src[i].0;
        my += wi * src[i].1;
        wsum += wi;
    }
    if wsum <= 0.0 {
        return None;
    }
    mx /= wsum;
    my /= wsum;
    // Normal equations for [a b c] with design rows [x-mx, y-my, 1].
    let mut ata = [[0.0f64; 3]; 3];
    let mut atx = [0.0f64; 3];
    let mut aty = [0.0f64; 3];
    for i in 0..n {
        let wi = w.map_or(1.0, |w| w[i]);
        let r = [src[i].0 - mx, src[i].1 - my, 1.0];
        for j in 0..3 {
            for k in 0..3 {
                ata[j][k] += wi * r[j] * r[k];
            }
            atx[j] += wi * r[j] * dst[i].0;
            aty[j] += wi * r[j] * dst[i].1;
        }
    }
    let px = solve3(ata, atx)?;
    let py = solve3(ata, aty)?;
    // x' = px0 (x-mx) + px1 (y-my) + px2
    Some(Affine {
        a: px[0],
        b: px[1],
        c: px[2] - px[0] * mx - px[1] * my,
        d: py[0],
        e: py[1],
        f: py[2] - py[0] * mx - py[1] * my,
    })
}

/// Solve a 3×3 linear system with partial pivoting.
pub fn solve3(m: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let mut a = [[0.0f64; 4]; 3];
    for i in 0..3 {
        a[i][..3].copy_from_slice(&m[i]);
        a[i][3] = b[i];
    }
    solve_dense(&mut a.iter().map(|r| r.to_vec()).collect::<Vec<_>>()).map(|v| [v[0], v[1], v[2]])
}

/// Gaussian elimination with partial pivoting on an augmented n×(n+1) matrix.
#[allow(clippy::needless_range_loop)]
pub fn solve_dense(a: &mut [Vec<f64>]) -> Option<Vec<f64>> {
    let n = a.len();
    let scale = a.iter().flat_map(|r| r[..n].iter()).fold(0.0f64, |m, v| m.max(v.abs()));
    if scale == 0.0 || !scale.is_finite() {
        return None;
    }
    for col in 0..n {
        let piv = (col..n)
            .max_by(|&i, &j| a[i][col].abs().partial_cmp(&a[j][col].abs()).unwrap_or(std::cmp::Ordering::Equal))?;
        if a[piv][col].abs() < 1e-13 * scale {
            return None;
        }
        a.swap(col, piv);
        for row in (col + 1)..n {
            let f = a[row][col] / a[col][col];
            if f != 0.0 {
                for k in col..=n {
                    a[row][k] -= f * a[col][k];
                }
            }
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut s = a[i][n];
        for k in (i + 1)..n {
            s -= a[i][k] * x[k];
        }
        x[i] = s / a[i][i];
        if !x[i].is_finite() {
            return None;
        }
    }
    Some(x)
}

/// 3×3 projective transform, row-major: `[X,Y,W] = M·[x,y,1]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Homography {
    pub m: [f64; 9],
}

impl Homography {
    pub const IDENTITY: Homography = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] };

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.m;
        let w = m[6] * x + m[7] * y + m[8];
        ((m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w)
    }

    pub fn is_affine(&self) -> bool {
        self.m[6].abs() < 1e-15 && self.m[7].abs() < 1e-15 && (self.m[8] - 1.0).abs() < 1e-12
    }

    /// Affine part (exact when `is_affine`). Normalises by m[8].
    pub fn to_affine(&self) -> Affine {
        let m = &self.m;
        let s = m[8];
        Affine { a: m[0] / s, b: m[1] / s, c: m[2] / s, d: m[3] / s, e: m[4] / s, f: m[5] / s }
    }

    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.m;
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        if !det.is_finite() || det.abs() < 1e-300 {
            return None;
        }
        let inv = [
            (m[4] * m[8] - m[5] * m[7]) / det,
            (m[2] * m[7] - m[1] * m[8]) / det,
            (m[1] * m[5] - m[2] * m[4]) / det,
            (m[5] * m[6] - m[3] * m[8]) / det,
            (m[0] * m[8] - m[2] * m[6]) / det,
            (m[2] * m[3] - m[0] * m[5]) / det,
            (m[3] * m[7] - m[4] * m[6]) / det,
            (m[1] * m[6] - m[0] * m[7]) / det,
            (m[0] * m[4] - m[1] * m[3]) / det,
        ];
        Some(Homography { m: inv })
    }

    /// Normalised DLT fit `dst ≈ H(src)` from ≥4 pairs (least squares via normal equations on h33 = 1).
    pub fn fit(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Homography> {
        let n = src.len();
        if n < 4 || dst.len() != n {
            return None;
        }
        let norm = |pts: &[(f64, f64)]| -> Affine {
            let nf = pts.len() as f64;
            let (mx, my) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
            let (mx, my) = (mx / nf, my / nf);
            let d = pts.iter().map(|p| ((p.0 - mx).powi(2) + (p.1 - my).powi(2)).sqrt()).sum::<f64>() / nf;
            let s = if d > 0.0 { std::f64::consts::SQRT_2 / d } else { 1.0 };
            Affine { a: s, b: 0.0, c: -s * mx, d: 0.0, e: s, f: -s * my }
        };
        let ts = norm(src);
        let td = norm(dst);
        // Solve 8 unknowns (h33 = 1) in least squares: A h = b.
        let mut ata = vec![vec![0.0f64; 9]; 8];
        for i in 0..n {
            let (x, y) = ts.apply(src[i].0, src[i].1);
            let (u, v) = td.apply(dst[i].0, dst[i].1);
            let rows =
                [([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u), ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v)];
            for (r, rhs) in rows.iter() {
                for j in 0..8 {
                    for k in 0..8 {
                        ata[j][k] += r[j] * r[k];
                    }
                    ata[j][8] += r[j] * rhs;
                }
            }
        }
        let h = solve_dense(&mut ata)?;
        let hn = Homography { m: [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0] };
        // Denormalise: H = Td⁻¹ · Hn · Ts
        let tdi = td.inverse()?.to_homography();
        let out = tdi.mul(&hn).mul(&ts.to_homography());
        let s = out.m[8];
        if s.abs() < 1e-300 {
            return None;
        }
        let mut m = out.m;
        for v in m.iter_mut() {
            *v /= s;
        }
        Some(Homography { m })
    }

    pub fn mul(&self, o: &Homography) -> Homography {
        let a = &self.m;
        let b = &o.m;
        let mut m = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                m[r * 3 + c] = a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c];
            }
        }
        Homography { m }
    }
}

/// Pixel → lat/lon through a pixel→Mercator homography.
pub fn pixel_to_latlon(h: &Homography, x: f64, y: f64) -> LatLon {
    let (mx, my) = h.apply(x, y);
    let (lat, lon) = merc_to_latlon(mx, my);
    LatLon::new(lat, lon)
}

/// UTM coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Utm {
    pub zone: u8,
    pub north: bool,
    pub easting: f64,
    pub northing: f64,
}

const WGS84_A: f64 = 6_378_137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;
const UTM_K0: f64 = 0.9996;

pub fn utm_zone_for(lat: f64, lon: f64) -> u8 {
    // Norway / Svalbard exceptions.
    if (56.0..64.0).contains(&lat) && (3.0..12.0).contains(&lon) {
        return 32;
    }
    if (72.0..84.0).contains(&lat) {
        if (0.0..9.0).contains(&lon) {
            return 31;
        } else if (9.0..21.0).contains(&lon) {
            return 33;
        } else if (21.0..33.0).contains(&lon) {
            return 35;
        } else if (33.0..42.0).contains(&lon) {
            return 37;
        }
    }
    let z = ((lon + 180.0) / 6.0).floor() as i32 + 1;
    z.clamp(1, 60) as u8
}

/// lat/lon → UTM in the natural zone.
pub fn latlon_to_utm(lat: f64, lon: f64) -> Utm {
    latlon_to_utm_zone(lat, lon, utm_zone_for(lat, lon))
}

/// lat/lon → UTM in a forced zone (Snyder, USGS PP 1395).
pub fn latlon_to_utm_zone(lat: f64, lon: f64, zone: u8) -> Utm {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let ep2 = e2 / (1.0 - e2);
    let phi = lat.to_radians();
    let lon0 = ((zone as f64 - 1.0) * 6.0 - 180.0 + 3.0).to_radians();
    let n = WGS84_A / (1.0 - e2 * phi.sin().powi(2)).sqrt();
    let t = phi.tan().powi(2);
    let c = ep2 * phi.cos().powi(2);
    let a = phi.cos() * (lon.to_radians() - lon0);
    let m = meridian_arc(phi, e2);
    let x = UTM_K0
        * n
        * (a + (1.0 - t + c) * a.powi(3) / 6.0 + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * a.powi(5) / 120.0);
    let y = UTM_K0
        * (m + n
            * phi.tan()
            * (a * a / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * a.powi(4) / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * a.powi(6) / 720.0));
    let north = lat >= 0.0;
    Utm { zone, north, easting: x + 500_000.0, northing: if north { y } else { y + 10_000_000.0 } }
}

fn meridian_arc(phi: f64, e2: f64) -> f64 {
    let e4 = e2 * e2;
    let e6 = e4 * e2;
    WGS84_A
        * ((1.0 - e2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0) * phi
            - (3.0 * e2 / 8.0 + 3.0 * e4 / 32.0 + 45.0 * e6 / 1024.0) * (2.0 * phi).sin()
            + (15.0 * e4 / 256.0 + 45.0 * e6 / 1024.0) * (4.0 * phi).sin()
            - (35.0 * e6 / 3072.0) * (6.0 * phi).sin())
}

/// UTM → lat/lon (Snyder inverse).
pub fn utm_to_latlon(u: &Utm) -> LatLon {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let ep2 = e2 / (1.0 - e2);
    let x = u.easting - 500_000.0;
    let y = if u.north { u.northing } else { u.northing - 10_000_000.0 };
    let m = y / UTM_K0;
    let mu = m / (WGS84_A * (1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2.powi(3) / 256.0));
    let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());
    let phi1 = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1 * e1 / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1.powi(4) / 512.0) * (8.0 * mu).sin();
    let n1 = WGS84_A / (1.0 - e2 * phi1.sin().powi(2)).sqrt();
    let t1 = phi1.tan().powi(2);
    let c1 = ep2 * phi1.cos().powi(2);
    let r1 = WGS84_A * (1.0 - e2) / (1.0 - e2 * phi1.sin().powi(2)).powf(1.5);
    let d = x / (n1 * UTM_K0);
    let lat = phi1
        - (n1 * phi1.tan() / r1)
            * (d * d / 2.0 - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ep2) * d.powi(4) / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 252.0 * ep2 - 3.0 * c1 * c1) * d.powi(6) / 720.0);
    let lon0 = (u.zone as f64 - 1.0) * 6.0 - 180.0 + 3.0;
    let lon = lon0
        + ((d - (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0
            + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ep2 + 24.0 * t1 * t1) * d.powi(5) / 120.0)
            / phi1.cos())
        .to_degrees();
    LatLon::new(lat.to_degrees(), lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merc_roundtrip() {
        for &(lat, lon) in &[(-25.85, 27.4), (0.0, 0.0), (51.5, -0.12), (-45.0, 170.0)] {
            let (x, y) = latlon_to_merc(lat, lon);
            let (la, lo) = merc_to_latlon(x, y);
            assert!((la - lat).abs() < 1e-10 && (lo - lon).abs() < 1e-10);
        }
    }

    #[test]
    fn haversine_and_bearing() {
        let a = LatLon::new(-25.85, 27.40);
        let b = destination(a, 45.0, 1000.0);
        assert!((haversine_m(a, b) - 1000.0).abs() < 0.01);
        assert!((initial_bearing_deg(a, b) - 45.0).abs() < 0.01);
    }

    #[test]
    fn affine_fit_and_inverse() {
        let t = Affine::pixel_to_merc_similarity(2.1, 0.1, 100.0, 200.0, 3.0e6, -2.9e6);
        let src: Vec<(f64, f64)> =
            (0..10).map(|i| ((i * 37 % 11) as f64 * 100.0, (i * 53 % 7) as f64 * 150.0)).collect();
        let dst: Vec<(f64, f64)> = src.iter().map(|&(x, y)| t.apply(x, y)).collect();
        let f = Affine::fit(&src, &dst).unwrap();
        let s = Affine::fit_similarity(&src, &dst, true).unwrap();
        for (p, q) in src.iter().zip(dst.iter()) {
            let (x, y) = f.apply(p.0, p.1);
            assert!((x - q.0).abs() < 1e-6 && (y - q.1).abs() < 1e-6);
            let (x, y) = s.apply(p.0, p.1);
            assert!((x - q.0).abs() < 1e-6 && (y - q.1).abs() < 1e-6);
        }
        let inv = f.inverse().unwrap();
        let (x, y) = inv.apply(dst[3].0, dst[3].1);
        assert!((x - src[3].0).abs() < 1e-6 && (y - src[3].1).abs() < 1e-6);
        assert!((t.map_up_bearing_deg() - 0.1f64.to_degrees()).abs() < 1e-9);
        assert!((t.mean_scale() - 2.1).abs() < 1e-12);
    }

    #[test]
    fn homography_fit() {
        let h = Homography { m: [1.2, 0.1, 5.0, -0.05, 0.9, 7.0, 1e-4, -2e-4, 1.0] };
        let src: Vec<(f64, f64)> =
            vec![(0.0, 0.0), (1000.0, 0.0), (1000.0, 800.0), (0.0, 800.0), (500.0, 400.0), (250.0, 600.0)];
        let dst: Vec<(f64, f64)> = src.iter().map(|&(x, y)| h.apply(x, y)).collect();
        let f = Homography::fit(&src, &dst).unwrap();
        for (p, q) in src.iter().zip(dst.iter()) {
            let (x, y) = f.apply(p.0, p.1);
            assert!((x - q.0).abs() < 1e-6 && (y - q.1).abs() < 1e-6, "{x} {y} vs {q:?}");
        }
        let inv = f.inverse().unwrap();
        let (x, y) = inv.apply(dst[2].0, dst[2].1);
        assert!((x - 1000.0).abs() < 1e-6 && (y - 800.0).abs() < 1e-6);
    }

    #[test]
    fn utm_roundtrip() {
        let u = latlon_to_utm(-25.85, 27.40);
        assert_eq!(u.zone, 35);
        assert!(!u.north);
        // Reference value (pyproj EPSG:32735): E 540081.97, N 7140866.54.
        assert!((u.easting - 540_081.97).abs() < 0.05, "{}", u.easting);
        assert!((u.northing - 7_140_866.54).abs() < 0.05, "{}", u.northing);
        let ll = utm_to_latlon(&u);
        assert!((ll.lat + 25.85).abs() < 1e-7 && (ll.lon - 27.40).abs() < 1e-7);
    }
}
