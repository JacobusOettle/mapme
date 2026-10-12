//! Wide search planning: the coarse zoom for a radius, the chunks a wide
//! area is searched in (nearest first, each a small DEM mosaic), and the
//! honest estimate of what a search will download and how long it takes
//! (`mm_search_estimate`). The search itself runs in `georef.rs`.
//!
//! Numbers come from `tests/wide_measure.rs` (real Terrarium tiles, the four
//! Magaliesberg sheets and photos); see docs/ARCHITECTURE.md, "Wide search".

use crate::dem::{merc_to_tile, resolution, tile_bounds, tiles_covering, MercBox, TileId, TILE_SIZE};
use crate::geo::{destination, haversine_m, latlon_to_merc, merc_to_latlon, LatLon, EARTH_RADIUS};
use crate::hints::SearchHint;
use crate::search::{map_half_extent, min_searchable_scale, required_dem_box, SearchParams};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

/// Zoom of the near area and hint searches, and of candidate re-checks.
pub const NEAR_ZOOM: u8 = 11;
/// Wide searches up to this radius use zoom 10, beyond it zoom 9 (a quarter
/// of the tiles; measured as reliable for sheets showing ≥ 6.6 km, and the
/// cost then grows with the radius: 150 km at zoom 10 ≈ 300 km at zoom 9).
pub const WIDE_Z10_MAX_RADIUS_M: f64 = 150_000.0;
/// Largest chunk mosaic side (tiles): 8 × 8 tiles = 2048² cells, ~120 MB
/// of transient search memory.
pub const MAX_CHUNK_SIDE: usize = 8;
/// Smallest chunk mosaic side (tiles).
pub const MIN_CHUNK_SIDE: usize = 3;
/// Margin (ground metres) around the placements for DEM gradients.
const MARGIN_M: f64 = 500.0;

/// Coarse zoom of the wide search: 10 up to `WIDE_Z10_MAX_RADIUS_M`, else 9,
/// but finer while no scale of the range (ground m/px, up to `scale_hi`)
/// gives the map a template of `MIN_SIDE` cells there (a map showing less
/// than ~6.6 km on its short side is not searchable at zoom 9).
pub fn wide_zoom(radius_m: f64, scale_hi: f64, img_w: usize, img_h: usize, lat: f64) -> u8 {
    let mut z = if radius_m <= WIDE_Z10_MAX_RADIUS_M { 10 } else { 9 };
    while z < NEAR_ZOOM && min_searchable_scale(img_w, img_h, resolution(z), lat) > scale_hi {
        z += 1;
    }
    z
}

/// Mercator size of one tile at zoom z.
fn tile_size(z: u8) -> f64 {
    resolution(z) * TILE_SIZE as f64
}

/// One chunk of a wide search: map centres in `placement` are searched on
/// the DEM of `tiles` (covering `mosaic`).
#[derive(Debug, Clone, PartialEq)]
pub struct WideChunk {
    /// Mercator box of the map centres searched here (tile aligned;
    /// half-open, chunks tile the plane without overlap).
    pub placement: MercBox,
    /// Mercator box the coarse DEM must cover: the placements plus the map's
    /// extent and a margin.
    pub mosaic: MercBox,
    pub tiles: Vec<TileId>,
    /// Ground distance (metres) from the origin to the nearest and farthest
    /// point of `placement`.
    pub min_dist_m: f64,
    pub max_dist_m: f64,
}

/// A wide search's chunks, nearest first, and for every tile the last chunk
/// (index) that needs it, so tiles can be released once it is searched.
#[derive(Debug, Clone)]
pub struct WidePlan {
    pub zoom: u8,
    pub chunks: Vec<WideChunk>,
    pub last_use: HashMap<TileId, usize>,
    /// Distinct coarse tiles over all chunks.
    pub unique_tiles: usize,
}

impl WidePlan {
    /// Radius around the origin searched completely once the chunks before
    /// `next` are done: the nearest point of the first chunk not searched
    /// (all of `radius_m` when none is left), at least `near_m`.
    pub fn searched_radius_m(&self, next: usize, near_m: f64, radius_m: f64) -> f64 {
        match self.chunks.get(next) {
            Some(c) => c.min_dist_m.clamp(near_m, radius_m),
            None => radius_m,
        }
    }
}

/// Mercator bounding box of the disc of `radius_m` ground metres around `o`.
fn disc_bounds(o: LatLon, radius_m: f64) -> MercBox {
    let lim = latlon_to_merc(85.0, 0.0).1;
    let mut pts = Vec::with_capacity(73);
    for i in 0..72 {
        let p = destination(o, i as f64 * 5.0, radius_m);
        let (mx, my) = latlon_to_merc(p.lat.clamp(-85.0, 85.0), p.lon);
        pts.push((mx, my));
    }
    let mut b = MercBox::from_points(&pts);
    // The disc reaches past the bearings sampled by up to 1 - cos(2.5°).
    let pad = 0.002 * b.width().max(b.height());
    b = b.expand(pad);
    // Past a pole the latitude extremes are the pole itself.
    let north = destination(o, 0.0, radius_m);
    let south = destination(o, 180.0, radius_m);
    if north.lat >= 84.9 || (north.lat - o.lat) < 0.0 {
        b.max_y = lim;
    }
    if south.lat <= -84.9 || (south.lat - o.lat) > 0.0 {
        b.min_y = -lim;
    }
    let half = PI * EARTH_RADIUS;
    MercBox { min_x: b.min_x.max(-half), min_y: b.min_y.max(-lim), max_x: b.max_x.min(half), max_y: b.max_y.min(lim) }
}

/// Plan the chunks of a wide search around `origin` (Mercator): the disc of
/// `radius_m` minus the near disc of `near_m` (ground metres), at `zoom`,
/// for a map whose half extent at the largest scale searched is `half_extent`
/// (Mercator metres, x and y). A chunk's mosaic is at most
/// `floor(sqrt(max_tiles))` tiles a side (3…8), its placements that minus
/// the tiles the map's extent needs on each side. Chunks are ordered by the
/// distance of their nearest point (then bearing), so the search grows in
/// rings from the fix. With a `band` (Mercator; from coordinate labels of
/// one axis) the placements are clipped to it and chunks and tiles beyond
/// its reach are left out.
pub fn plan_wide(
    origin: (f64, f64),
    radius_m: f64,
    near_m: f64,
    zoom: u8,
    half_extent: (f64, f64),
    max_tiles: usize,
    band: Option<MercBox>,
) -> WidePlan {
    let (olat, olon) = merc_to_latlon(origin.0, origin.1);
    let o = LatLon::new(olat, olon);
    let k = 1.0 / olat.to_radians().cos().max(1e-6);
    let ts = tile_size(zoom);
    let side = ((max_tiles.max(1) as f64).sqrt().floor() as usize).clamp(MIN_CHUNK_SIDE, MAX_CHUNK_SIDE);
    let (mx, my) = (half_extent.0 + MARGIN_M * k, half_extent.1 + MARGIN_M * k);
    // How far (ground metres) the DEM under a map reaches from its centre.
    let reach = mx.hypot(my) / k + 2.0 * MARGIN_M;
    let ring = |m: f64| (m / ts).ceil().max(0.0) as usize;
    let sx = side.saturating_sub(2 * ring(mx)).max(1) as u32;
    let sy = side.saturating_sub(2 * ring(my)).max(1) as u32;
    let b = disc_bounds(o, radius_m);
    let n = 1u32 << zoom;
    let clamp = |v: f64| (v.floor().max(0.0) as u64).min(n as u64 - 1) as u32;
    let (fx0, fy0) = merc_to_tile(zoom, b.min_x, b.max_y);
    let (fx1, fy1) = merc_to_tile(zoom, b.max_x, b.min_y);
    let (x0, x1, y0, y1) = (clamp(fx0), clamp(fx1), clamp(fy0), clamp(fy1));
    let mut chunks: Vec<(f64, f64, WideChunk)> = Vec::new();
    let mut cy = y0;
    while cy <= y1 {
        let mut cx = x0;
        while cx <= x1 {
            let (ex, ey) = ((cx + sx - 1).min(n - 1), (cy + sy - 1).min(n - 1));
            let tl = tile_bounds(TileId { z: zoom, x: cx, y: cy });
            let br = tile_bounds(TileId { z: zoom, x: ex, y: ey });
            let mut placement = MercBox { min_x: tl.min_x, min_y: br.min_y, max_x: br.max_x, max_y: tl.max_y };
            if let Some(bd) = &band {
                placement = MercBox {
                    min_x: placement.min_x.max(bd.min_x),
                    min_y: placement.min_y.max(bd.min_y),
                    max_x: placement.max_x.min(bd.max_x),
                    max_y: placement.max_y.min(bd.max_y),
                };
            }
            let empty = placement.min_x >= placement.max_x || placement.min_y >= placement.max_y;
            // (An empty placement box clamps to its min corner; it is skipped.)
            let near = (
                origin.0.max(placement.min_x).min(placement.max_x.max(placement.min_x)),
                origin.1.max(placement.min_y).min(placement.max_y.max(placement.min_y)),
            );
            let dist = |p: (f64, f64)| {
                let (la, lo) = merc_to_latlon(p.0, p.1);
                haversine_m(o, LatLon::new(la, lo))
            };
            let min_d = dist(near);
            let max_d = [
                (placement.min_x, placement.min_y),
                (placement.max_x, placement.min_y),
                (placement.max_x, placement.max_y),
                (placement.min_x, placement.max_y),
            ]
            .into_iter()
            .map(dist)
            .fold(0.0, f64::max);
            if !empty && min_d <= radius_m && max_d >= near_m {
                let mosaic = MercBox {
                    min_x: placement.min_x - mx,
                    min_y: placement.min_y - my,
                    max_x: placement.max_x + mx,
                    max_y: placement.max_y + my,
                };
                let c = ((placement.min_x + placement.max_x) / 2.0, (placement.min_y + placement.max_y) / 2.0);
                let bearing = (c.0 - origin.0).atan2(c.1 - origin.1).rem_euclid(2.0 * PI);
                // Only tiles a map centred in the annulus can reach: the
                // chunks at the rim would otherwise fetch far past it.
                let tiles: Vec<TileId> = tiles_covering(zoom, &mosaic)
                    .into_iter()
                    .filter(|t| {
                        let tb = tile_bounds(*t);
                        let nearest = (origin.0.clamp(tb.min_x, tb.max_x), origin.1.clamp(tb.min_y, tb.max_y));
                        let far =
                            [(tb.min_x, tb.min_y), (tb.max_x, tb.min_y), (tb.max_x, tb.max_y), (tb.min_x, tb.max_y)]
                                .into_iter()
                                .map(dist)
                                .fold(0.0, f64::max);
                        dist(nearest) <= radius_m + reach && far >= near_m - reach
                    })
                    .collect();
                chunks.push((
                    min_d,
                    bearing,
                    WideChunk { placement, mosaic, tiles, min_dist_m: min_d, max_dist_m: max_d },
                ));
            }
            if cx > x1.saturating_sub(sx) {
                break;
            }
            cx += sx;
        }
        if cy > y1.saturating_sub(sy) {
            break;
        }
        cy += sy;
    }
    chunks.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let chunks: Vec<WideChunk> = chunks.into_iter().map(|c| c.2).collect();
    let mut last_use = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        for t in &c.tiles {
            last_use.insert(*t, i);
        }
    }
    let unique_tiles = last_use.len();
    WidePlan { zoom, chunks, last_use, unique_tiles }
}

// ---------------------------------------------------------------------------
// Estimate (mm_search_estimate)
// ---------------------------------------------------------------------------

/// Mean Terrarium tile size (bytes) per zoom, measured over 60 tiles each
/// spread over 2 300 km around the Magaliesberg (land and the sea off
/// Durban; `wide_measure::tile_weights`).
pub fn tile_bytes(z: u8) -> f64 {
    let kb = match z {
        0..=8 => 76.8,
        9 => 68.9,
        10 => 45.9,
        11 => 26.4,
        12 => 20.0,
        _ => 15.9,
    };
    kb * 1024.0
}

/// The map the estimate assumes: a 300-dpi A3 sheet, any scale from 0.4 to
/// 12 m/px (no scale label read), north up.
pub const TYPICAL_IMAGE: (usize, usize) = (3507, 4960);
pub const TYPICAL_SCALE: (f64, f64) = (0.4, 12.0);

/// Zoom-11 re-check tiles per wide chunk (3 candidates, ~4 tiles each) and
/// zoom-13 tiles per refinement (~16, measured 4–42).
const RECHECK_TILES_PER_CHUNK: usize = 12;
const REFINE_TILES: usize = 16;
/// Refinements per area search (the best candidate and up to two others).
const REFINES_PER_AREA: usize = 2;

/// Compute time on the reference machine (4-core 2.1 GHz Xeon shared with
/// other jobs, release), measured by `wide_measure::wide_session`: seconds
/// per million coarse mosaic cells (one tile is 65 536) searched over the
/// default scale range (0.4–12 m/px; Durban → Grootkloof, 650 km: 21.7 s
/// for 42 Mcells) and over a range from an OCR'd scale (±25 %: Windhoek,
/// 1 000 km: 20.8 s for 107 Mcells), and per zoom-13 refinement.
const SEARCH_S_PER_MCELL: f64 = 0.47;
const SEARCH_S_PER_MCELL_SCALE_KNOWN: f64 = 0.17;
const REFINE_S: f64 = 0.6;
/// An iPhone (2 performance + 4 efficiency cores) against the reference box.
pub const IPHONE_FACTOR: f64 = 1.5;
/// Download model: bytes per second and latency per need_dem round.
pub const DOWNLOAD_BYTES_PER_S: f64 = 2.0e6;
pub const ROUND_LATENCY_S: f64 = 0.5;

/// What a search plans to fetch and compute.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub coarse_zoom: u8,
    /// Tiles by zoom (coarse searches plus candidate checks).
    pub tiles: Vec<(u8, usize)>,
    pub bytes: f64,
    pub compute_s: f64,
    pub download_s: f64,
    pub rounds: usize,
    pub chunks: usize,
}

impl Estimate {
    pub fn total_tiles(&self) -> usize {
        self.tiles.iter().map(|t| t.1).sum()
    }
}

/// Coarse tiles of a zoom-11 area search around `c` (Mercator) of radius `r`.
fn area_tiles(c: (f64, f64), r: f64, img: (usize, usize), scale: (f64, f64)) -> Vec<TileId> {
    let p = SearchParams {
        center_merc: c,
        search_radius_m: r,
        scale_min: scale.0,
        scale_max: scale.1,
        rotations_deg: vec![0.0],
        top_k: 1,
        region: None,
    };
    tiles_covering(NEAR_ZOOM, &required_dem_box(&p, img.0, img.1))
}

/// Estimate a search of `radius_m` around `gps` (or of the hints alone)
/// for the map in `img` at `scale` (ground m/px range; `scale_known` when
/// it comes from an OCR'd scale, which makes the search ~3× faster), the
/// way the search plans it: hint areas and the near disc at zoom 11, the
/// rest in chunks at `wide_zoom`; candidate checks are an allowance from
/// the measurements.
#[allow(clippy::too_many_arguments)]
pub fn estimate(
    radius_m: f64,
    near_m: f64,
    gps: Option<LatLon>,
    hints: &[SearchHint],
    img: (usize, usize),
    scale: (f64, f64),
    scale_known: bool,
    max_tiles: usize,
) -> Estimate {
    let mut coarse11: HashSet<TileId> = HashSet::new();
    let mut areas = 0usize;
    let mut mcells = 0.0;
    let mut rounds = 0usize;
    for h in hints {
        if gps.is_some_and(|g| haversine_m(g, h.latlon()) > radius_m) || !h.is_valid() {
            continue;
        }
        let t = area_tiles(latlon_to_merc(h.lat, h.lon), h.radius_m.clamp(1_000.0, near_m), img, scale);
        mcells += t.len() as f64 * 65_536.0 / 1e6;
        rounds += t.len().div_ceil(max_tiles.max(1)) + 1;
        coarse11.extend(t);
        areas += 1;
    }
    let mut coarse_zoom = NEAR_ZOOM;
    let mut wide_tiles = 0usize;
    let mut chunks = 0usize;
    if let Some(g) = gps {
        let c = latlon_to_merc(g.lat, g.lon);
        let t = area_tiles(c, radius_m.min(near_m), img, scale);
        mcells += t.len() as f64 * 65_536.0 / 1e6;
        rounds += t.len().div_ceil(max_tiles.max(1)) + 1;
        coarse11.extend(t);
        areas += 1;
        // The inner disc is searched first (its tiles are among those).
        let first = crate::georef::NEAR_FIRST_RADIUS_M;
        if radius_m.min(near_m) > first {
            mcells += area_tiles(c, first, img, scale).len() as f64 * 65_536.0 / 1e6;
            rounds += 1;
            areas += 1;
        }
        if radius_m > near_m {
            coarse_zoom = wide_zoom(radius_m, scale.1, img.0, img.1, g.lat);
            let p = SearchParams { center_merc: c, scale_min: scale.0, scale_max: scale.1, ..SearchParams::default() };
            let plan = plan_wide(c, radius_m, near_m, coarse_zoom, map_half_extent(&p, img.0, img.1), max_tiles, None);
            wide_tiles = plan.unique_tiles;
            chunks = plan.chunks.len();
            mcells += plan.chunks.iter().map(|c| c.tiles.len()).sum::<usize>() as f64 * 65_536.0 / 1e6;
            // One round per chunk (re-checks ride along with the next chunk).
            rounds += chunks + 1;
        }
    }
    let refines = areas * REFINES_PER_AREA + usize::from(chunks > 0);
    let mut tiles = vec![(NEAR_ZOOM, coarse11.len() + chunks * RECHECK_TILES_PER_CHUNK), (13, refines * REFINE_TILES)];
    if chunks > 0 {
        tiles.insert(0, (coarse_zoom, wide_tiles));
    }
    tiles.retain(|t| t.1 > 0);
    let bytes: f64 = tiles.iter().map(|&(z, n)| tile_bytes(z) * n as f64).sum();
    let per_mcell = if scale_known { SEARCH_S_PER_MCELL_SCALE_KNOWN } else { SEARCH_S_PER_MCELL };
    let compute_s = (mcells * per_mcell + refines as f64 * REFINE_S) * IPHONE_FACTOR;
    let download_s = bytes / DOWNLOAD_BYTES_PER_S + rounds as f64 * ROUND_LATENCY_S;
    Estimate { coarse_zoom, tiles, bytes, compute_s, download_s, rounds, chunks }
}

/// `mm_search_estimate` request.
#[derive(Deserialize, Default)]
#[serde(default)]
struct EstimateRequest {
    radius_m: Option<f64>,
    gps: Option<EstimateGps>,
    hints: Option<Vec<EstimateHint>>,
    max_tiles_per_round: Option<f64>,
    scale_known: Option<bool>,
}

#[derive(Deserialize)]
struct EstimateGps {
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct EstimateHint {
    lat: f64,
    lon: f64,
    radius_m: Option<f64>,
}

/// `mm_search_estimate` on JSON; never fails (errors become a failed response).
pub fn search_estimate_json(json: &str) -> String {
    let req: EstimateRequest = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(e) => return crate::georef::failed_json(&format!("Invalid request: {e}")),
    };
    let radius = req
        .radius_m
        .filter(|r| r.is_finite())
        .unwrap_or(crate::georef::DEFAULT_RADIUS_M)
        .clamp(crate::georef::RADIUS_RANGE_M.0, crate::georef::RADIUS_RANGE_M.1);
    let gps = req
        .gps
        .filter(|g| g.lat.is_finite() && g.lon.is_finite() && g.lat.abs() <= 85.0 && g.lon.abs() <= 180.0)
        .map(|g| LatLon::new(g.lat, g.lon));
    let hints: Vec<SearchHint> = req
        .hints
        .unwrap_or_default()
        .into_iter()
        .map(|h| SearchHint {
            lat: h.lat,
            lon: h.lon,
            radius_m: h.radius_m.filter(|r| r.is_finite()).unwrap_or(crate::hints::DEFAULT_HINT_RADIUS_M),
            source: crate::hints::HintSource::Place,
            name: None,
        })
        .collect();
    let max_tiles =
        req.max_tiles_per_round.filter(|v| v.is_finite()).map_or(crate::georef::DEFAULT_MAX_TILES_PER_ROUND, |v| {
            v.round().clamp(crate::georef::MAX_TILES_RANGE.0 as f64, crate::georef::MAX_TILES_RANGE.1 as f64) as usize
        });
    let known = req.scale_known.unwrap_or(false);
    let e = estimate(radius, crate::georef::NEAR_RADIUS_M, gps, &hints, TYPICAL_IMAGE, TYPICAL_SCALE, known, max_tiles);
    let mb = |b: f64| (b / 1e5).round() / 10.0;
    json!({
        "status": "ok",
        "radius_m": radius,
        "coarse_zoom": e.coarse_zoom,
        "tiles": e.total_tiles(),
        "approx_bytes": e.bytes.round(),
        "approx_seconds": (e.compute_s + e.download_s).round(),
        "approx_compute_seconds": (e.compute_s * 10.0).round() / 10.0,
        "approx_download_seconds": (e.download_s * 10.0).round() / 10.0,
        "approx_megabytes": mb(e.bytes),
        "rounds": e.rounds,
        "chunks": e.chunks,
        "tiles_by_zoom": e.tiles.iter().map(|&(z, n)| json!({"z": z, "tiles": n})).collect::<Vec<Value>>(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> (f64, f64) {
        latlon_to_merc(-29.86, 31.03)
    }

    #[test]
    fn zoom_rule() {
        let lat = -25.85;
        assert_eq!(wide_zoom(100_000.0, 12.0, 3507, 4960, lat), 10);
        assert_eq!(wide_zoom(150_000.0, 12.0, 3507, 4960, lat), 10);
        assert_eq!(wide_zoom(250_000.0, 12.0, 3507, 4960, lat), 9);
        assert_eq!(wide_zoom(1_000_000.0, 12.0, 3507, 4960, lat), 9);
        // A map showing 3.8 km × 4.9 km (1:25 000 A5 at 1.9 m/px) is too
        // small for zoom 9 (14 cells): zoom 10 (28 cells).
        assert_eq!(wide_zoom(1_000_000.0, 1.9, 2000, 2600, lat), 10);
        // 3 km: below 24 cells at zoom 10 too.
        assert_eq!(wide_zoom(1_000_000.0, 1.9, 1600, 2100, lat), 11);
    }

    #[test]
    fn chunks_cover_the_annulus_nearest_first_within_the_tile_budget() {
        let o = origin();
        let (olat, olon) = merc_to_latlon(o.0, o.1);
        let k = 1.0 / olat.to_radians().cos();
        for (radius, zoom, max_tiles) in
            [(300_000.0, 9u8, 64usize), (240_000.0, 10, 64), (400_000.0, 9, 25), (180_000.0, 10, 4)]
        {
            let plan = plan_wide(o, radius, 50_000.0, zoom, (21_000.0 * k, 30_000.0 * k), max_tiles, None);
            assert!(!plan.chunks.is_empty());
            let side = ((max_tiles as f64).sqrt().floor() as usize).clamp(MIN_CHUNK_SIDE, MAX_CHUNK_SIDE);
            for c in &plan.chunks {
                // The mosaic stays within the chunk side (3×3 at least).
                assert!(c.tiles.len() <= side * side, "{} tiles > {side}²", c.tiles.len());
                assert!(c.min_dist_m <= radius && c.max_dist_m >= 50_000.0);
                // The mosaic holds the placements plus the map's extent.
                assert!(c.mosaic.min_x < c.placement.min_x && c.mosaic.max_y > c.placement.max_y);
            }
            // Nearest first.
            for w in plan.chunks.windows(2) {
                assert!(w[0].min_dist_m <= w[1].min_dist_m + 1e-6);
            }
            // Every point of the annulus lies in exactly one chunk's placements.
            let o_ll = LatLon::new(olat, olon);
            let mut checked = 0;
            for i in 0..400 {
                let bearing = i as f64 * 137.507_764;
                let d = 50_000.0 + (radius - 50_000.0) * ((i as f64 * 0.618_034) % 1.0);
                let p = destination(o_ll, bearing, d);
                let (mx, my) = latlon_to_merc(p.lat, p.lon);
                let n = plan
                    .chunks
                    .iter()
                    .filter(|c| {
                        let b = &c.placement;
                        mx >= b.min_x && mx < b.max_x && my >= b.min_y && my < b.max_y
                    })
                    .count();
                assert_eq!(n, 1, "{p:?} at {d} m in {n} chunks");
                checked += 1;
            }
            assert_eq!(checked, 400);
            // last_use: every tile maps to the last chunk holding it.
            for (t, &i) in &plan.last_use {
                assert!(plan.chunks[i].tiles.contains(t));
                assert!(plan.chunks[i + 1..].iter().all(|c| !c.tiles.contains(t)));
            }
            assert_eq!(plan.searched_radius_m(0, 50_000.0, radius), 50_000.0);
            assert_eq!(plan.searched_radius_m(plan.chunks.len(), 50_000.0, radius), radius);
        }
    }

    #[test]
    fn estimate_grows_with_radius_and_stays_honest() {
        let g = Some(LatLon::new(-25.85, 27.4));
        let near = estimate(50_000.0, 50_000.0, g, &[], TYPICAL_IMAGE, TYPICAL_SCALE, false, 64);
        assert_eq!(near.coarse_zoom, 11);
        assert_eq!(near.chunks, 0);
        let mid = estimate(120_000.0, 50_000.0, g, &[], TYPICAL_IMAGE, TYPICAL_SCALE, false, 64);
        assert_eq!(mid.coarse_zoom, 10);
        let far = estimate(1_000_000.0, 50_000.0, g, &[], TYPICAL_IMAGE, TYPICAL_SCALE, false, 64);
        assert_eq!(far.coarse_zoom, 9);
        assert!(near.bytes < mid.bytes && mid.bytes < far.bytes);
        assert!(near.compute_s < far.compute_s);
        // The lead's arithmetic: ~630 zoom-9 tiles in the 1 000 km disc
        // (plus the chunks' margins), ~40-60 MB.
        let z9 = far.tiles.iter().find(|t| t.0 == 9).unwrap().1;
        assert!((630..1100).contains(&z9), "{z9}");
        assert!((35e6..90e6).contains(&far.bytes), "{}", far.bytes);
        // No GPS: the hints alone.
        let h = SearchHint {
            lat: -25.85,
            lon: 27.4,
            radius_m: 15_000.0,
            source: crate::hints::HintSource::Place,
            name: None,
        };
        let only =
            estimate(1_000_000.0, 50_000.0, None, std::slice::from_ref(&h), TYPICAL_IMAGE, TYPICAL_SCALE, false, 64);
        assert_eq!(only.coarse_zoom, 11);
        assert!(only.total_tiles() > 0 && only.bytes < near.bytes * 1.5);
        assert_eq!(
            estimate(1_000_000.0, 50_000.0, None, &[], TYPICAL_IMAGE, TYPICAL_SCALE, false, 64).total_tiles(),
            0
        );
        // A hint beyond the radius of the fix is not searched.
        let far_hint = SearchHint { lat: -33.9, lon: 18.4, ..h };
        let e = estimate(50_000.0, 50_000.0, g, &[far_hint], TYPICAL_IMAGE, TYPICAL_SCALE, false, 64);
        assert_eq!(e.total_tiles(), near.total_tiles());
    }

    #[test]
    fn estimate_json() {
        let v: Value = serde_json::from_str(&search_estimate_json(
            r#"{"radius_m": 1000000, "gps": {"lat": -25.85, "lon": 27.4}}"#,
        ))
        .unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["coarse_zoom"], 9);
        assert!(v["tiles"].as_u64().unwrap() > 600);
        assert!(v["approx_bytes"].as_f64().unwrap() > 30e6);
        assert!(v["approx_seconds"].as_f64().unwrap() > 10.0);
        let v: Value = serde_json::from_str(&search_estimate_json(r#"{"gps": null}"#)).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["tiles"], 0);
        assert_eq!(v["radius_m"], 50_000.0);
        let v: Value = serde_json::from_str(&search_estimate_json("{nope")).unwrap();
        assert_eq!(v["status"], "failed");
        // Clamped radius.
        let v: Value =
            serde_json::from_str(&search_estimate_json(r#"{"radius_m": 9e9, "gps": {"lat": -25.85, "lon": 27.4}}"#))
                .unwrap();
        assert_eq!(v["radius_m"], 2_000_000.0);
    }
}
