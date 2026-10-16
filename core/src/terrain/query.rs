//! Elevations at points, profiles along polylines, height grids.

use super::sample::{coarsest_zoom_with_cell, zoom_bounds, Chain};
use super::store::Tiles;
use super::{
    ElevationsRequest, ElevationsResponse, GridRequest, GridResponse, ProfileRequest, ProfileResponse, Status,
    MAX_PROFILE_SAMPLES, MAX_QUERY_TILES_PER_ZOOM,
};
use crate::dem::resolution;
use crate::geo::{haversine_m, latlon_to_merc, LatLon};

/// Stencil radius (tile pixels) a bilinear sample may touch around its point.
const BILINEAR_RADIUS: f64 = 1.0;

fn check_point(p: &[f64; 2], what: &str) -> Result<(), String> {
    if !(p[0].is_finite() && p[1].is_finite() && p[0].abs() <= 90.0) {
        return Err(format!("Invalid {what} [{}, {}]: latitude within ±90°, finite longitude", p[0], p[1]));
    }
    Ok(())
}

fn status(need: &[crate::dem::TileId]) -> Status {
    if need.is_empty() {
        Status::Ok
    } else {
        Status::NeedDem
    }
}

/// Rounded for compact JSON: centimetres.
fn cm(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub fn elevations(tiles: &Tiles, req: &ElevationsRequest) -> Result<ElevationsResponse, String> {
    let (z_max, z_min) = zoom_bounds(req.max_zoom, req.min_zoom)?;
    for p in &req.points {
        check_point(p, "point")?;
    }
    let merc: Vec<(f64, f64)> = req.points.iter().map(|p| latlon_to_merc(p[0], p[1])).collect();
    let mut chain = Chain::new(z_max, z_min);
    for &(mx, my) in &merc {
        chain.want_merc(mx, my, BILINEAR_RADIUS);
    }
    chain.fill(&mut tiles.snapshot());
    let elevations_m: Vec<Option<f64>> =
        merc.iter().map(|&(mx, my)| chain.bilinear(mx, my).map(|(h, _)| cm(h))).collect();
    let (cx, cy) = centre(&merc);
    let need_tiles = chain.need_tiles(cx, cy, req.max_tiles);
    Ok(ElevationsResponse { status: status(&need_tiles), elevations_m, zoom: chain.zoom_used(), need_tiles })
}

/// Centre of the points' bounding box (Mercator).
fn centre(pts: &[(f64, f64)]) -> (f64, f64) {
    if pts.is_empty() {
        return (0.0, 0.0);
    }
    let b = crate::dem::MercBox::from_points(pts);
    ((b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0)
}

pub fn profile(tiles: &Tiles, req: &ProfileRequest) -> Result<ProfileResponse, String> {
    let (z_max, z_min) = zoom_bounds(req.max_zoom, req.min_zoom)?;
    if req.points.is_empty() {
        return Err("A profile needs at least one point".into());
    }
    for p in &req.points {
        check_point(p, "point")?;
    }
    if let Some(s) = req.spacing_m {
        if !(s.is_finite() && s > 0.0) {
            return Err(format!("Invalid spacing_m {s}"));
        }
    }
    if let Some(s) = req.smoothing_m {
        if !(s.is_finite() && s >= 0.0) {
            return Err(format!("Invalid smoothing_m {s}"));
        }
    }
    if !(req.hysteresis_m.is_finite() && req.hysteresis_m >= 0.0) {
        return Err(format!("Invalid hysteresis_m {}", req.hysteresis_m));
    }

    // Vertices: cumulative geodesic distance; longitudes unwrapped along the line so a route
    // across the antimeridian interpolates the short way.
    let n = req.points.len();
    let mut lons = Vec::with_capacity(n);
    let mut vertex_distance = Vec::with_capacity(n);
    let mut d = 0.0;
    for (i, p) in req.points.iter().enumerate() {
        if i == 0 {
            lons.push(p[1]);
        } else {
            let prev: f64 = lons[i - 1];
            let mut lon = p[1];
            while lon - prev > 180.0 {
                lon -= 360.0;
            }
            while lon - prev < -180.0 {
                lon += 360.0;
            }
            lons.push(lon);
            let q = &req.points[i - 1];
            d += haversine_m(LatLon::new(q[0], q[1]), LatLon::new(p[0], p[1]));
        }
        vertex_distance.push(d);
    }
    let total = d;

    // Densify evenly along the length.
    let mean_lat = req.points.iter().map(|p| p[0]).sum::<f64>() / n as f64;
    let cos_lat = mean_lat.to_radians().cos().max(1e-6);
    let cell_m = |z: u8| resolution(z) * cos_lat;
    let spacing = req.spacing_m.unwrap_or(cell_m(z_max) / 2.0);
    let intervals = if total > 0.0 { ((total / spacing).ceil() as usize).clamp(1, MAX_PROFILE_SAMPLES - 1) } else { 0 };
    let step = if intervals > 0 { total / intervals as f64 } else { 0.0 };
    let mut dense: Vec<(f64, f64)> = Vec::with_capacity(intervals + 1);
    let mut seg = 0usize;
    for k in 0..=intervals {
        let dk = if k == intervals { total } else { k as f64 * step };
        while seg + 2 < n && vertex_distance[seg + 1] < dk {
            seg += 1;
        }
        let (lat, lon) = if n == 1 {
            (req.points[0][0], lons[0])
        } else {
            let (d0, d1) = (vertex_distance[seg], vertex_distance[seg + 1]);
            let f = if d1 > d0 { ((dk - d0) / (d1 - d0)).clamp(0.0, 1.0) } else { 0.0 };
            let (a, b) = (&req.points[seg], &req.points[seg + 1]);
            (a[0] + (b[0] - a[0]) * f, lons[seg] + (lons[seg + 1] - lons[seg]) * f)
        };
        dense.push(latlon_to_merc(lat, lon));
    }
    let vertex_merc: Vec<(f64, f64)> =
        req.points.iter().zip(&lons).map(|(p, &lon)| latlon_to_merc(p[0], lon)).collect();

    let mut chain = Chain::new(z_max, z_min);
    for &(mx, my) in dense.iter().chain(&vertex_merc) {
        chain.want_merc(mx, my, BILINEAR_RADIUS);
    }
    chain.fill(&mut tiles.snapshot());
    let raw: Vec<f64> = dense.iter().map(|&(mx, my)| chain.bilinear(mx, my).map_or(f64::NAN, |(h, _)| h)).collect();
    let vertex_elevation_m: Vec<Option<f64>> =
        vertex_merc.iter().map(|&(mx, my)| chain.bilinear(mx, my).map(|(h, _)| cm(h))).collect();
    let zoom = chain.zoom_used();

    let sigma_m = req.smoothing_m.unwrap_or_else(|| cell_m(zoom.unwrap_or(z_max)));
    let smooth = if step > 0.0 { gaussian_nan(&raw, sigma_m / step) } else { raw.clone() };
    let finite: Vec<f64> = smooth.iter().copied().filter(|h| h.is_finite()).collect();
    let (ascent_m, descent_m, min_m, max_m) = if finite.is_empty() {
        (None, None, None, None)
    } else {
        let (a, dsc) = climbs(&finite, req.hysteresis_m);
        let lo = finite.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (Some(cm(a)), Some(cm(dsc)), Some(cm(lo)), Some(cm(hi)))
    };
    let missing_fraction = raw.iter().filter(|h| !h.is_finite()).count() as f64 / raw.len().max(1) as f64;

    // Evenly spaced output samples.
    let max_samples = req.max_samples.max(2);
    let samples: Vec<(f64, Option<f64>)> = if smooth.len() <= max_samples {
        smooth.iter().enumerate().map(|(k, &h)| (cm(k as f64 * step).min(cm(total)), finite_cm(h))).collect()
    } else {
        let m = max_samples;
        (0..m)
            .map(|k| {
                let dk = if k == m - 1 { total } else { total * k as f64 / (m - 1) as f64 };
                let pos = if step > 0.0 { dk / step } else { 0.0 };
                let i0 = (pos.floor() as usize).min(smooth.len() - 1);
                let i1 = (i0 + 1).min(smooth.len() - 1);
                let f = pos - i0 as f64;
                let (a, b) = (smooth[i0], smooth[i1]);
                let h = match (a.is_finite(), b.is_finite()) {
                    (true, true) => a + (b - a) * f,
                    (true, false) => a,
                    (false, true) => b,
                    (false, false) => f64::NAN,
                };
                (cm(dk), finite_cm(h))
            })
            .collect()
    };
    let (cx, cy) = centre(&vertex_merc);
    let need_tiles = chain.need_tiles(cx, cy, req.max_tiles);
    Ok(ProfileResponse {
        status: status(&need_tiles),
        distance_m: cm(total),
        ascent_m,
        descent_m,
        min_m,
        max_m,
        samples,
        vertex_distance_m: vertex_distance.into_iter().map(cm).collect(),
        vertex_elevation_m,
        zoom,
        missing_fraction,
        need_tiles,
    })
}

fn finite_cm(h: f64) -> Option<f64> {
    h.is_finite().then(|| cm(h))
}

/// Gaussian smoothing (sigma in samples) that ignores unknown samples and leaves them unknown.
pub fn gaussian_nan(v: &[f64], sigma: f64) -> Vec<f64> {
    if !(sigma.is_finite() && sigma > 0.05) || v.len() < 2 {
        return v.to_vec();
    }
    let radius = (3.0 * sigma).ceil() as usize;
    if radius > 64 {
        // Three box passes (≈ Gaussian) in linear time for wide kernels.
        let w = ((12.0 * sigma * sigma / 3.0 + 1.0).sqrt().round() as usize).max(1) | 1;
        let mut out = v.to_vec();
        for _ in 0..3 {
            out = box_nan(&out, w / 2);
        }
        for (o, x) in out.iter_mut().zip(v) {
            if !x.is_finite() {
                *o = f64::NAN;
            }
        }
        return out;
    }
    let kernel: Vec<f64> = (0..=radius).map(|k| (-0.5 * (k as f64 / sigma).powi(2)).exp()).collect();
    (0..v.len())
        .map(|i| {
            if !v[i].is_finite() {
                return f64::NAN;
            }
            let (mut s, mut w) = (0.0, 0.0);
            let lo = i.saturating_sub(radius);
            let hi = (i + radius).min(v.len() - 1);
            for (j, &x) in v.iter().enumerate().take(hi + 1).skip(lo) {
                if x.is_finite() {
                    let k = kernel[i.abs_diff(j)];
                    s += k * x;
                    w += k;
                }
            }
            s / w
        })
        .collect()
}

/// Moving mean over ±r samples, ignoring unknown ones (unknown where none is known).
fn box_nan(v: &[f64], r: usize) -> Vec<f64> {
    let n = v.len();
    let mut sum = vec![0.0; n + 1];
    let mut cnt = vec![0usize; n + 1];
    for i in 0..n {
        let ok = v[i].is_finite();
        sum[i + 1] = sum[i] + if ok { v[i] } else { 0.0 };
        cnt[i + 1] = cnt[i] + ok as usize;
    }
    (0..n)
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(r), (i + r + 1).min(n));
            let c = cnt[hi] - cnt[lo];
            if c == 0 {
                f64::NAN
            } else {
                (sum[hi] - sum[lo]) / c as f64
            }
        })
        .collect()
}

/// Ascent and descent of an elevation series with a hysteresis: a climb or descent counts once
/// it exceeds `hysteresis`, from turning point to turning point (so a long climb counts whole,
/// and wiggles smaller than the hysteresis count nothing).
pub fn climbs(h: &[f64], hysteresis: f64) -> (f64, f64) {
    let Some(&first) = h.first() else { return (0.0, 0.0) };
    let (mut ascent, mut descent) = (0.0, 0.0);
    let (mut anchor, mut ext) = (first, first);
    let mut dir = 0i8;
    for &x in &h[1..] {
        match dir {
            0 => {
                if x - anchor >= hysteresis && x > anchor {
                    dir = 1;
                    ext = x;
                } else if anchor - x >= hysteresis && x < anchor {
                    dir = -1;
                    ext = x;
                }
            }
            1 => {
                if x > ext {
                    ext = x;
                } else if ext - x >= hysteresis && x < ext {
                    ascent += ext - anchor;
                    anchor = ext;
                    ext = x;
                    dir = -1;
                }
            }
            _ => {
                if x < ext {
                    ext = x;
                } else if x - ext >= hysteresis && x > ext {
                    descent += anchor - ext;
                    anchor = ext;
                    ext = x;
                    dir = 1;
                }
            }
        }
    }
    match dir {
        1 => ascent += ext - anchor,
        -1 => descent += anchor - ext,
        _ => {}
    }
    (ascent, descent)
}

pub fn grid(
    tiles: &Tiles,
    req: &GridRequest,
    heights: &mut [f32],
    cols: usize,
    rows: usize,
) -> Result<GridResponse, String> {
    let (z_max, z_min) = zoom_bounds(req.max_zoom, req.min_zoom)?;
    let [min_x, min_y, max_x, max_y] = req.rect;
    if !(req.rect.iter().all(|v| v.is_finite()) && max_x > min_x && max_y > min_y) {
        return Err(format!("Invalid rect {:?}: [min_x, min_y, max_x, max_y], max above min", req.rect));
    }
    if cols < 2 || rows < 2 || heights.len() < cols * rows {
        return Err("Invalid grid".into());
    }
    let (dx, dy) = ((max_x - min_x) / (cols - 1) as f64, (max_y - min_y) / (rows - 1) as f64);
    // The coarsest zoom whose cell is at most half the node spacing; coarser still when the
    // rectangle would take more tiles than a query may.
    let mut z_want = coarsest_zoom_with_cell(dx.min(dy) / 2.0, z_min, z_max);
    let mut z_lo = z_min;
    let chain = loop {
        let mut chain = Chain::new(z_want, z_lo);
        let fits =
            chain.levels.iter_mut().all(|l| l.want_rect(min_x, min_y, max_x, max_y, 2.0, MAX_QUERY_TILES_PER_ZOOM));
        if fits || z_want == 0 {
            break chain;
        }
        z_want -= 1;
        z_lo = z_lo.min(z_want);
    };
    let mut chain = chain;
    chain.fill(&mut tiles.snapshot());
    let (mut lo, mut hi, mut unknown) = (f64::INFINITY, f64::NEG_INFINITY, 0usize);
    for j in 0..rows {
        let my = max_y - j as f64 * dy;
        for i in 0..cols {
            let mx = min_x + i as f64 * dx;
            let h = chain.bilinear(mx, my).map_or(f64::NAN, |(h, _)| h);
            heights[j * cols + i] = h as f32;
            if h.is_finite() {
                lo = lo.min(h);
                hi = hi.max(h);
            } else {
                unknown += 1;
            }
        }
    }
    let need_tiles = chain.need_tiles((min_x + max_x) / 2.0, (min_y + max_y) / 2.0, req.max_tiles);
    let any = lo.is_finite();
    Ok(GridResponse {
        status: status(&need_tiles),
        zoom: chain.zoom_used(),
        min_m: any.then(|| cm(lo)),
        max_m: any.then(|| cm(hi)),
        missing_fraction: unknown as f64 / (cols * rows) as f64,
        need_tiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn climbs_count_turning_points() {
        // Up 100, down 100: the whole climb counts, not 3 m steps.
        let up: Vec<f64> = (0..=1000).map(|i| i as f64 * 0.1).collect();
        let mut v = up.clone();
        v.extend(up.iter().rev());
        assert_eq!(climbs(&v, 3.0), (100.0, 100.0));
        // Wiggles under the hysteresis count nothing.
        let w: Vec<f64> = (0..100).map(|i| if i % 2 == 0 { 10.0 } else { 12.5 }).collect();
        assert_eq!(climbs(&w, 3.0), (0.0, 0.0));
        assert_eq!(climbs(&w, 0.0).0, 2.5 * 50.0);
        assert_eq!(climbs(&[], 3.0), (0.0, 0.0));
        assert_eq!(climbs(&[5.0], 3.0), (0.0, 0.0));
    }

    #[test]
    fn smoothing_keeps_unknown_samples_unknown() {
        let v = [1.0, 2.0, f64::NAN, 4.0, 5.0];
        let s = gaussian_nan(&v, 1.0);
        assert!(s[2].is_nan());
        assert!(s.iter().enumerate().all(|(i, x)| i == 2 || x.is_finite()));
        let flat = vec![7.0; 1000];
        assert!(gaussian_nan(&flat, 100.0).iter().all(|&x| (x - 7.0).abs() < 1e-9));
    }
}
