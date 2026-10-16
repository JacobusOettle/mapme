//! Tile plans (`mm_tile_plan`): the tiles covering areas at zooms, as disjoint rectangles, with
//! their estimated download.
//!
//! Each area adds, per zoom, one interval of tile columns to every tile row it touches (two
//! where it crosses the antimeridian); the union of the areas is kept per row as merged
//! intervals, and consecutive rows with the same intervals become one rectangle per interval.
//! Exact for boxes; for circles (on the sphere) and quads grown by a margin, a tile counts when
//! the shape reaches into its row within the row's column interval: circles exactly, grown quads
//! as the quad's extent within the row band grown by the margin on all sides (a few tiles more at
//! the corners than a rounded outline would take).

use super::{
    continuation_margin_m, tile_bytes_high, tile_bytes_typical, PlanArea, PlanDropped, PlanRequest, PlanResponse,
    PlanZoom, Status, MAX_ZOOM, TRAILHEAD_COARSE_RADIUS_M, TRAILHEAD_MAX_TILES_PER_ZOOM, TRAILHEAD_Z11_RADIUS_M,
    TRAILHEAD_Z13_MARGIN_M,
};
use crate::geo::{haversine_m, latlon_to_merc, merc_to_latlon, LatLon, MEAN_EARTH_RADIUS};
use std::collections::BTreeMap;
use std::f64::consts::PI;

const MAX_LAT: f64 = 85.051_128_78;
const HALF: f64 = PI * crate::geo::EARTH_RADIUS;

/// One area's shape.
enum Shape {
    /// Latitudes south..north, longitude ranges (two across the antimeridian).
    Box { s: f64, n: f64, lons: Vec<(f64, f64)> },
    /// Centre and angular radius (radians).
    Circle { lat: f64, lon: f64, radius: f64 },
    /// Corners in Mercator (x unwrapped around the first), margin in Mercator metres.
    Quad { pts: [(f64, f64); 4], margin: f64 },
}

/// Rows `y0..=y1` sharing the same column intervals.
type Run<'a> = (u32, u32, &'a Vec<(u32, u32)>);

/// Tile column intervals per row of one zoom.
#[derive(Default, Clone)]
struct Rows {
    rows: BTreeMap<u32, Vec<(u32, u32)>>,
}

impl Rows {
    /// Add columns `a..=b` (unwrapped; split at the antimeridian) to row `y` of a zoom with `n`
    /// tiles per side.
    fn add(&mut self, y: u32, a: i64, b: i64, n: i64) {
        if b < a {
            return;
        }
        if b - a + 1 >= n {
            self.insert(y, 0, (n - 1) as u32);
            return;
        }
        let (a2, b2) = (a.rem_euclid(n), b.rem_euclid(n));
        if a2 <= b2 {
            self.insert(y, a2 as u32, b2 as u32);
        } else {
            self.insert(y, a2 as u32, (n - 1) as u32);
            self.insert(y, 0, b2 as u32);
        }
    }

    fn insert(&mut self, y: u32, a: u32, b: u32) {
        let row = self.rows.entry(y).or_default();
        row.push((a, b));
        row.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(row.len());
        for &(x0, x1) in row.iter() {
            match merged.last_mut() {
                Some(last) if x0 <= last.1.saturating_add(1) => last.1 = last.1.max(x1),
                _ => merged.push((x0, x1)),
            }
        }
        *row = merged;
    }

    fn merge(&mut self, other: &Rows) {
        for (&y, ivs) in &other.rows {
            for &(a, b) in ivs {
                self.insert(y, a, b);
            }
        }
    }

    fn tiles(&self) -> usize {
        self.rows.values().flatten().map(|&(a, b)| (b - a) as usize + 1).sum()
    }

    /// Disjoint inclusive rectangles `[x0, x1, y0, y1]`: consecutive rows with the same
    /// intervals merged.
    fn rects(&self) -> Vec<[u32; 4]> {
        let mut out = Vec::new();
        let mut run: Option<Run<'_>> = None;
        let flush = |run: &Option<Run<'_>>, out: &mut Vec<[u32; 4]>| {
            if let Some((y0, y1, ivs)) = run {
                for &(a, b) in ivs.iter() {
                    out.push([a, b, *y0, *y1]);
                }
            }
        };
        for (&y, ivs) in &self.rows {
            match &mut run {
                Some((_, y1, prev)) if *y1 + 1 == y && *prev == ivs => *y1 = y,
                _ => {
                    flush(&run, &mut out);
                    run = Some((y, y, ivs));
                }
            }
        }
        flush(&run, &mut out);
        out
    }
}

/// Fractional tile column of a longitude (unwrapped: any longitude) at zoom z.
fn tile_x(lon: f64, n: f64) -> f64 {
    (lon + 180.0) / 360.0 * n
}

/// Fractional tile row of a latitude.
fn tile_y(lat: f64, n: f64) -> f64 {
    let (_, my) = latlon_to_merc(lat.clamp(-MAX_LAT, MAX_LAT), 0.0);
    (HALF - my) / (2.0 * HALF) * n
}

/// Latitude of a tile row's top edge.
fn row_lat(y: f64, n: f64) -> f64 {
    merc_to_latlon(0.0, HALF - y / n * 2.0 * HALF).0
}

fn check_lat_lon(lat: f64, lon: f64, what: &str) -> Result<(), String> {
    if !(lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 540.0) {
        return Err(format!("Invalid {what} [{lat}, {lon}]"));
    }
    Ok(())
}

impl Shape {
    fn from_area(area: &PlanArea) -> Result<Shape, String> {
        let given = area.bbox.is_some() as u8 + area.circle.is_some() as u8 + area.quad.is_some() as u8;
        if given != 1 {
            return Err("Each area needs exactly one of bbox, circle, quad".into());
        }
        if let Some([s, w, n, e]) = area.bbox {
            check_lat_lon(s, w, "bbox corner")?;
            check_lat_lon(n, e, "bbox corner")?;
            if s > n {
                return Err(format!("Invalid bbox: south {s} above north {n}"));
            }
            let lons = if w <= e { vec![(w, e)] } else { vec![(w, 180.0), (-180.0, e)] };
            return Ok(Shape::Box { s, n, lons });
        }
        if let Some([lat, lon, r]) = area.circle {
            check_lat_lon(lat, lon, "circle centre")?;
            if !(r.is_finite() && r >= 0.0) {
                return Err(format!("Invalid circle radius {r}"));
            }
            return Ok(Shape::Circle { lat, lon, radius: (r / MEAN_EARTH_RADIUS).min(PI) });
        }
        let corners = area.quad.expect("one of three");
        quad(&corners, area.margin_m)
    }

    /// Add this shape's tiles at zoom z to `rows`.
    fn add(&self, z: u8, rows: &mut Rows) {
        let ni = 1i64 << z;
        let n = ni as f64;
        match self {
            Shape::Box { s, n: north, lons } => {
                let y0 = tile_y(*north, n).floor().clamp(0.0, n - 1.0) as u32;
                let y1 = tile_y(*s, n).floor().clamp(0.0, n - 1.0) as u32;
                for &(w, e) in lons {
                    let x0 = tile_x(w, n).floor().clamp(0.0, n - 1.0) as i64;
                    let x1 = tile_x(e, n).floor().clamp(0.0, n - 1.0) as i64;
                    for y in y0..=y1 {
                        rows.add(y, x0, x1, ni);
                    }
                }
            }
            Shape::Circle { lat, lon, radius } => {
                let (phi, d) = (lat.to_radians(), *radius);
                let lat_s = (phi - d).to_degrees().max(-90.0);
                let lat_n = (phi + d).to_degrees().min(90.0);
                // A pole inside the circle: every column of the rows it reaches.
                let pole = phi + d >= PI / 2.0 || phi - d <= -PI / 2.0;
                let y0 = tile_y(lat_n, n).floor().clamp(0.0, n - 1.0) as u32;
                let y1 = tile_y(lat_s, n).floor().clamp(0.0, n - 1.0) as u32;
                // Latitude of the circle's widest point (its tangent meridians).
                let widest = if d < PI / 2.0 { (phi.sin() / d.cos()).clamp(-1.0, 1.0).asin() } else { phi };
                for y in y0..=y1 {
                    if pole {
                        rows.add(y, 0, ni - 1, ni);
                        continue;
                    }
                    // The row's latitude band within the circle's.
                    let top = row_lat(y as f64, n).min(lat_n).to_radians();
                    let bot = row_lat(y as f64 + 1.0, n).max(lat_s).to_radians();
                    let mut half: f64 = 0.0;
                    for psi in [top, bot, widest.clamp(bot, top)] {
                        let c = (d.cos() - phi.sin() * psi.sin()) / (phi.cos() * psi.cos());
                        if c <= -1.0 {
                            half = PI;
                        } else if c < 1.0 {
                            half = half.max(c.acos());
                        }
                    }
                    let (w, e) = (lon - half.to_degrees(), lon + half.to_degrees());
                    rows.add(y, tile_x(w, n).floor() as i64, tile_x(e, n).floor() as i64, ni);
                }
            }
            Shape::Quad { pts, margin } => {
                let ys: Vec<f64> = pts.iter().map(|p| p.1).collect();
                let (lo, hi) = (
                    ys.iter().copied().fold(f64::INFINITY, f64::min),
                    ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                );
                let (ylo, yhi) = ((lo - margin).max(-HALF), (hi + margin).min(HALF));
                let size = 2.0 * HALF / n;
                let r0 = ((HALF - yhi) / size).floor().clamp(0.0, n - 1.0) as u32;
                let r1 = ((HALF - ylo) / size).floor().clamp(0.0, n - 1.0) as u32;
                for y in r0..=r1 {
                    // The row's Mercator band, grown by the margin; the quad's x extent within it.
                    let (top, bot) = (HALF - y as f64 * size + margin, HALF - (y as f64 + 1.0) * size - margin);
                    let Some((x0, x1)) = quad_x_extent(pts, bot, top) else { continue };
                    let c0 = ((x0 - margin + HALF) / size).floor() as i64;
                    let c1 = ((x1 + margin + HALF) / size).floor() as i64;
                    rows.add(y, c0, c1, ni);
                }
            }
        }
    }
}

/// A quad's corners (lat/lon, any order around) grown by `margin_m` ground metres.
fn quad(corners: &[[f64; 2]; 4], margin_m: f64) -> Result<Shape, String> {
    for c in corners {
        check_lat_lon(c[0], c[1], "quad corner")?;
    }
    if !(margin_m.is_finite() && margin_m >= 0.0) {
        return Err(format!("Invalid margin_m {margin_m}"));
    }
    let lon0 = corners[0][1];
    let mut pts = [(0.0, 0.0); 4];
    let mut lat_sum = 0.0;
    for (p, c) in pts.iter_mut().zip(corners) {
        let mut lon = c[1];
        while lon - lon0 > 180.0 {
            lon -= 360.0;
        }
        while lon - lon0 < -180.0 {
            lon += 360.0;
        }
        *p = latlon_to_merc(c[0], lon);
        lat_sum += c[0];
    }
    let cos = (lat_sum / 4.0).to_radians().cos().max(0.01);
    Ok(Shape::Quad { pts, margin: margin_m / cos })
}

/// The x extent of a polygon's part between y = lo and y = hi (None when it has none).
fn quad_x_extent(pts: &[(f64, f64); 4], lo: f64, hi: f64) -> Option<(f64, f64)> {
    let (mut x0, mut x1) = (f64::INFINITY, f64::NEG_INFINITY);
    for k in 0..4 {
        let (a, b) = (pts[k], pts[(k + 1) % 4]);
        if (lo..=hi).contains(&a.1) {
            x0 = x0.min(a.0);
            x1 = x1.max(a.0);
        }
        for yc in [lo, hi] {
            if (a.1 - yc) * (b.1 - yc) < 0.0 {
                let t = (yc - a.1) / (b.1 - a.1);
                let x = a.0 + t * (b.0 - a.0);
                x0 = x0.min(x);
                x1 = x1.max(x);
            }
        }
    }
    // A band wholly inside the polygon crosses no edge and holds no corner: take the polygon's
    // extent at the band's middle (the quad is convex for a sheet; a concave one is grown anyway).
    if x0 > x1 {
        let mid = (lo + hi) / 2.0;
        for k in 0..4 {
            let (a, b) = (pts[k], pts[(k + 1) % 4]);
            if (a.1 - mid) * (b.1 - mid) <= 0.0 && a.1 != b.1 {
                let x = a.0 + (mid - a.1) / (b.1 - a.1) * (b.0 - a.0);
                x0 = x0.min(x);
                x1 = x1.max(x);
            }
        }
    }
    (x0 <= x1).then_some((x0, x1))
}

pub fn tile_plan(req: &PlanRequest) -> Result<PlanResponse, String> {
    let mut per_zoom: BTreeMap<u8, Rows> = BTreeMap::new();
    for area in &req.areas {
        let shape = Shape::from_area(area)?;
        for &z in &area.zooms {
            if z > MAX_ZOOM {
                return Err(format!("zoom {z} above {MAX_ZOOM}"));
            }
            shape.add(z, per_zoom.entry(z).or_default());
        }
    }
    let mut dropped = Vec::new();
    if let Some(th) = &req.trailhead {
        for (z, rows) in trailhead_rows(&th.corners)? {
            let tiles = rows.tiles();
            if tiles > TRAILHEAD_MAX_TILES_PER_ZOOM {
                dropped.push(PlanDropped {
                    z,
                    tiles,
                    reason: format!("more than {TRAILHEAD_MAX_TILES_PER_ZOOM} tiles around the map"),
                });
            } else {
                per_zoom.entry(z).or_default().merge(&rows);
            }
        }
    }
    let zooms: Vec<PlanZoom> = per_zoom
        .into_iter()
        .filter_map(|(z, rows)| {
            let tiles = rows.tiles();
            (tiles > 0).then(|| PlanZoom {
                z,
                tiles,
                bytes: (tiles as f64 * tile_bytes_typical(z)).round(),
                bytes_high: (tiles as f64 * tile_bytes_high(z)).round(),
                ranges: rows.rects(),
            })
        })
        .collect();
    Ok(PlanResponse {
        status: Status::Ok,
        tiles: zooms.iter().map(|z| z.tiles).sum(),
        bytes: zooms.iter().map(|z| z.bytes).sum(),
        bytes_high: zooms.iter().map(|z| z.bytes_high).sum(),
        zooms,
        dropped,
    })
}

/// The trailhead rule's tiles per zoom (docs/design/explorer.md, Trailhead caching).
fn trailhead_rows(corners: &[[f64; 2]; 4]) -> Result<Vec<(u8, Rows)>, String> {
    let Shape::Quad { pts, .. } = quad(corners, 0.0)? else { unreachable!() };
    let (cx, cy) = (pts.iter().map(|p| p.0).sum::<f64>() / 4.0, pts.iter().map(|p| p.1).sum::<f64>() / 4.0);
    let (lat, lon) = merc_to_latlon(cx, cy);
    let ll = |c: &[f64; 2]| LatLon::new(c[0], c[1]);
    let long_side = (0..4).map(|k| haversine_m(ll(&corners[k]), ll(&corners[(k + 1) % 4]))).fold(0.0, f64::max);
    let circle =
        |r: f64| Shape::Circle { lat, lon: (lon + 540.0).rem_euclid(360.0) - 180.0, radius: r / MEAN_EARTH_RADIUS };
    let parts: [(u8, Shape); 5] = [
        (9, circle(TRAILHEAD_COARSE_RADIUS_M)),
        (10, circle(TRAILHEAD_COARSE_RADIUS_M)),
        (11, circle(TRAILHEAD_Z11_RADIUS_M)),
        (12, quad(corners, continuation_margin_m(long_side))?),
        (13, quad(corners, TRAILHEAD_Z13_MARGIN_M)?),
    ];
    Ok(parts
        .iter()
        .map(|(z, shape)| {
            let mut rows = Rows::default();
            shape.add(*z, &mut rows);
            (*z, rows)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_merge_and_split_at_the_antimeridian() {
        let mut r = Rows::default();
        r.add(3, 6, 9, 8); // columns 6, 7, 0, 1
        assert_eq!(r.rows[&3], vec![(0, 1), (6, 7)]);
        r.add(3, 2, 5, 8);
        assert_eq!(r.rows[&3], vec![(0, 7)]);
        r.add(4, -20, 40, 8);
        assert_eq!(r.rows[&4], vec![(0, 7)]);
        assert_eq!(r.tiles(), 16);
        assert_eq!(r.rects(), vec![[0, 7, 3, 4]]);
    }
}
