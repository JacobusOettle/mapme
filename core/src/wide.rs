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
use crate::search::{
    map_half_extent, min_searchable_scale, nearest_in_box, required_dem_box_capped, search_work, SearchParams,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

/// Zoom of the near area and hint searches, and of candidate re-checks, at
/// low and middle latitudes (`near_zoom` gives it at any latitude).
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

/// Zoom `z` adjusted for latitude: Mercator tiles shrink on the ground by
/// cos(lat), so far from the equator a coarser zoom has the ground detail
/// `z` has at the equator (and as many tiles per area). The coarsest zoom
/// whose ground cell at `lat` is no larger than zoom `z`'s at the equator:
/// `z` up to 60°, one coarser to 75.5°, two to 82.8° (Tromsø 69.6°N: zoom
/// 10 for 11; without this a 1 000 km search there plans ~7× the tiles).
pub fn zoom_at_latitude(z: u8, lat: f64) -> u8 {
    let cos = lat.to_radians().cos().abs().max(1e-6);
    let target = resolution(z) * (1.0 + 1e-9);
    let mut out = z;
    while out > 1 && resolution(out - 1) * cos <= target {
        out -= 1;
    }
    out
}

/// Zoom of the near area and hint searches (and re-checks) at `lat`.
pub fn near_zoom(lat: f64) -> u8 {
    zoom_at_latitude(NEAR_ZOOM, lat)
}

/// Scales (ground m/px) up to this are searched on the near zoom's tiles
/// (zoom 11): every 300-dpi sheet from 1:5 000 to 1:140 000 and screen maps
/// down to ~zoom 13. Coarser scales (small-scale maps, large-area exports:
/// a Gaia GPS raster at 23 m/px, a 1:250 000 sheet at 21 m/px) are a band of
/// their own on coarser tiles: on zoom 11 their templates would be searched
/// on a 2–4× downsampled mosaic anyway, and the mosaic that holds such a
/// map would be hundreds of zoom-11 tiles.
pub const FINE_MAX_SCALE: f64 = 12.0;
/// A coarse band's zoom keeps its smallest scale's template at least this
/// many cells on its short side (`search::TARGET_SIDE`: the coarse search
/// would downsample to about this anyway)…
const BAND_TARGET_SIDE: f64 = 64.0;
/// …and is at most this many zooms coarser than the near zoom.
const BAND_MAX_ZOOM_STEPS: u8 = 3;
/// A range of scales too fine for the near zoom is searched at most this
/// many zooms finer (zoom 13: a box of ±15 km).
const FINE_BAND_MAX_ZOOM_STEPS: u8 = 2;

/// Largest ground extent (metres, the image's long side) a contour map is
/// assumed to show without a scale prior: the default range stops at the
/// scale that gives it (a 4 960 px A3 page: 30 m/px; a 1 118 px export:
/// the default 50 m/px).
pub const MAX_MAP_EXTENT_M: f64 = 150_000.0;

/// The scale range (ground m/px) searched without a scale prior for an
/// image of `img_w` × `img_h` px: `georef::DEFAULT_SCALE_RANGE`, up to the
/// scale at which it would show `MAX_MAP_EXTENT_M` (never below
/// `FINE_MAX_SCALE`).
pub fn default_scale_range(img_w: usize, img_h: usize) -> (f64, f64) {
    let (lo, hi) = crate::georef::DEFAULT_SCALE_RANGE;
    let long = img_w.max(img_h).max(1) as f64;
    (lo, hi.min((MAX_MAP_EXTENT_M / long).max(FINE_MAX_SCALE)))
}

/// A part of the scale range searched on DEM tiles of one zoom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleBand {
    pub zoom: u8,
    /// Ground m/px.
    pub lo: f64,
    pub hi: f64,
}

impl ScaleBand {
    /// Largest half-size (ground metres) of the coarse DEM box of an area
    /// searched in this band: `search::MAX_BOX_HALF_M` on the near zoom, as
    /// many cells on other zooms (240 km on zoom 9, 15 km on zoom 13).
    pub fn max_box_half_m(&self, lat: f64) -> f64 {
        let near = near_zoom(lat);
        let coarser = near.saturating_sub(self.zoom).min(BAND_MAX_ZOOM_STEPS);
        let finer = self.zoom.saturating_sub(near).min(FINE_BAND_MAX_ZOOM_STEPS);
        crate::search::MAX_BOX_HALF_M * f64::from(1u32 << coarser) / f64::from(1u32 << finer)
    }

    /// The near zoom's band (scales up to `FINE_MAX_SCALE`)?
    pub fn is_fine(&self) -> bool {
        self.lo < FINE_MAX_SCALE
    }
}

/// The scale range `lo`…`hi` (ground m/px) as bands searched in turn, finest
/// first: up to `FINE_MAX_SCALE` on the near zoom (as always), beyond it on
/// `band_zoom` of its smallest scale. A range from a scale label ±25 % lies
/// in one band (two when it straddles `FINE_MAX_SCALE`).
pub fn scale_bands(lo: f64, hi: f64, img_w: usize, img_h: usize, lat: f64) -> Vec<ScaleBand> {
    let near = near_zoom(lat);
    let mut out = Vec::new();
    if !(lo.is_finite() && hi.is_finite() && lo > 0.0 && hi >= lo) {
        return out;
    }
    if lo < FINE_MAX_SCALE {
        // A range wholly too fine for the near zoom (no template of
        // `MIN_SIDE` cells: a 1:5 000 plan at 300 dpi, 0.42 m/px) is searched
        // on the finer zoom that reaches its largest scale, in a smaller box.
        let fhi = hi.min(FINE_MAX_SCALE);
        let mut zoom = near;
        while zoom < near + FINE_BAND_MAX_ZOOM_STEPS && min_searchable_scale(img_w, img_h, resolution(zoom), lat) > fhi
        {
            zoom += 1;
        }
        out.push(ScaleBand { zoom, lo, hi: fhi });
    }
    if hi > FINE_MAX_SCALE {
        let blo = lo.max(FINE_MAX_SCALE);
        out.push(ScaleBand { zoom: band_zoom(blo, img_w, img_h, lat), lo: blo, hi });
    }
    out
}

/// The coarsest zoom, at most `BAND_MAX_ZOOM_STEPS` coarser than the near
/// zoom, on which a map of `img_w` × `img_h` px at `scale` (ground m/px) has
/// a template of `BAND_TARGET_SIDE` cells on its short side: zoom 9 for a
/// 3 500 × 2 500 px page at 12 m/px, zoom 10 for a 1 118 × 791 px export.
pub fn band_zoom(scale: f64, img_w: usize, img_h: usize, lat: f64) -> u8 {
    let near = near_zoom(lat);
    let cos = lat.to_radians().cos().abs().max(1e-6);
    let cells = |z: u8| img_w.min(img_h) as f64 * scale / (resolution(z) * cos);
    let floor = near.saturating_sub(BAND_MAX_ZOOM_STEPS).max(1);
    let mut z = near;
    while z > floor && cells(z - 1) >= BAND_TARGET_SIDE {
        z -= 1;
    }
    z
}

/// Zoom of the local re-check of a candidate at `scale` (ground m/px): the
/// near zoom up to `FINE_MAX_SCALE`, beyond it one zoom finer than the
/// candidate's band (a template of 128–256 cells, like a 1:25 000 sheet's
/// on zoom 11).
pub fn recheck_zoom(scale: f64, img_w: usize, img_h: usize, lat: f64) -> u8 {
    let near = near_zoom(lat);
    if scale <= FINE_MAX_SCALE {
        // The near zoom, or the finer one a plan too detailed for it needs.
        scale_bands(scale, scale, img_w, img_h, lat).first().map_or(near, |b| b.zoom)
    } else {
        (band_zoom(scale, img_w, img_h, lat) + 1).min(near)
    }
}

/// Coarse zoom of the wide search and the radius it covers, for a map of
/// `img_w` × `img_h` px at up to `scale_hi` ground m/px around latitude
/// `lat`: zoom 10 up to `WIDE_Z10_MAX_RADIUS_M`, zoom 9 beyond (both for
/// latitude, `zoom_at_latitude`). A map too small for zoom 9 (no scale of
/// the range gives it a template of `MIN_SIDE` cells: less than ~6.6 km on
/// its short side) is searched at zoom 10 up to 150 km only; one too small
/// for zoom 10 (under ~3.3 km) not at all (None): zoom 11 over a wide area
/// would be ~10 000 tiles per 1 000 km of radius.
pub fn wide_reach(radius_m: f64, scale_hi: f64, img_w: usize, img_h: usize, lat: f64) -> Option<(u8, f64)> {
    let ok = |z: u8| min_searchable_scale(img_w, img_h, resolution(z), lat) <= scale_hi;
    let (z9, z10) = (zoom_at_latitude(9, lat), zoom_at_latitude(10, lat));
    if radius_m > WIDE_Z10_MAX_RADIUS_M && ok(z9) {
        return Some((z9, radius_m));
    }
    ok(z10).then_some((z10, radius_m.min(WIDE_Z10_MAX_RADIUS_M)))
}

/// Mercator size of one tile at zoom z.
fn tile_size(z: u8) -> f64 {
    resolution(z) * TILE_SIZE as f64
}

/// One chunk of a wide search: map centres in `placement` (but not in
/// `exclude`) are searched on the DEM of `tiles` (covering `mosaic`).
#[derive(Debug, Clone, PartialEq)]
pub struct WideChunk {
    /// Mercator box of the map centres searched here (tile aligned, or cut
    /// to a band; half-open, chunks tile the plane without overlap).
    pub placement: MercBox,
    /// Placements left out: the band of coordinate labels, searched first.
    pub exclude: Option<MercBox>,
    /// Mercator box the coarse DEM must cover: the placements plus the map's
    /// extent and a margin.
    pub mosaic: MercBox,
    pub tiles: Vec<TileId>,
    /// Ground distance (metres) from the origin to the nearest and farthest
    /// point of `placement`.
    pub min_dist_m: f64,
    pub max_dist_m: f64,
    /// Zoom of `tiles`, and the scale band (index into the bands a merged
    /// plan was made of, `merge_plans`; 0 in a plan of one band).
    pub zoom: u8,
    pub scale_band: usize,
}

/// A wide search's chunks, nearest first (those of a labels' band before
/// all others), and for every tile the last chunk (index) that needs it, so
/// tiles can be released once it is searched.
#[derive(Debug, Clone)]
pub struct WidePlan {
    pub zoom: u8,
    pub chunks: Vec<WideChunk>,
    pub last_use: HashMap<TileId, usize>,
    /// Distinct coarse tiles over all chunks.
    pub unique_tiles: usize,
    /// The first `band_chunks` chunks hold the band's placements.
    pub band_chunks: usize,
}

impl WidePlan {
    /// Radius around the origin searched completely once the chunks before
    /// `next` are done: the nearest point of the chunks not searched yet
    /// (all of `radius_m` when none is left), at least `near_m`.
    pub fn searched_radius_m(&self, next: usize, near_m: f64, radius_m: f64) -> f64 {
        self.chunks[next.min(self.chunks.len())..]
            .iter()
            .map(|c| c.min_dist_m)
            .fold(f64::INFINITY, f64::min)
            .clamp(near_m.min(radius_m), radius_m)
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

/// Does `inner` lie entirely in `outer`?
fn box_within(inner: &MercBox, outer: &MercBox) -> bool {
    inner.min_x >= outer.min_x && inner.max_x <= outer.max_x && inner.min_y >= outer.min_y && inner.max_y <= outer.max_y
}

/// Plan the chunks of a wide search around `origin` (Mercator): the disc of
/// `radius_m` minus the near disc of `near_m` (ground metres), at `zoom`,
/// for a map whose half extent at the largest scale searched is `half_extent`
/// (Mercator metres, x and y). A chunk's mosaic is at most
/// `floor(sqrt(max_tiles))` tiles a side (3…8), its placements that minus
/// the tiles the map's extent needs on each side. Chunks are ordered by the
/// distance of their nearest point (then bearing), so the search grows in
/// rings from the fix. With a `band` (Mercator; from coordinate labels of
/// one axis) the band's placements come first (chunks clipped to it, and
/// only the tiles it reaches), then the rest of the disc, nearest first, so
/// a misread label costs time but never leaves part of the radius unsearched.
pub fn plan_wide(
    origin: (f64, f64),
    radius_m: f64,
    near_m: f64,
    zoom: u8,
    half_extent: (f64, f64),
    max_tiles: usize,
    band: Option<MercBox>,
) -> WidePlan {
    let mut chunks = plan_chunks(origin, radius_m, near_m, zoom, half_extent, max_tiles, band, None);
    let band_chunks = if band.is_some() { chunks.len() } else { 0 };
    if let Some(b) = band {
        chunks.extend(plan_chunks(origin, radius_m, near_m, zoom, half_extent, max_tiles, None, Some(b)));
    }
    let mut last_use = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        for t in &c.tiles {
            last_use.insert(*t, i);
        }
    }
    let unique_tiles = last_use.len();
    WidePlan { zoom, chunks, last_use, unique_tiles, band_chunks }
}

/// One plan of the chunks of several scale bands (each planned on its own
/// zoom and with its own map extent by `plan_wide`), searched in one sweep
/// nearest first: the labels' band's chunks of every scale band first, then
/// the others by the distance of their nearest point, the finer scale band
/// first at equal distance. Tiles are released after their last use in this
/// order, so bands sharing a zoom never ask for a released tile again. The
/// plan's `zoom` is the first band's.
pub fn merge_plans(plans: Vec<WidePlan>) -> WidePlan {
    let zoom = plans.first().map_or(NEAR_ZOOM, |p| p.zoom);
    let mut first: Vec<WideChunk> = Vec::new();
    let mut rest: Vec<WideChunk> = Vec::new();
    for (b, plan) in plans.into_iter().enumerate() {
        for (i, mut c) in plan.chunks.into_iter().enumerate() {
            c.scale_band = b;
            if i < plan.band_chunks {
                first.push(c);
            } else {
                rest.push(c);
            }
        }
    }
    // Stable: equal distances keep the bands' order.
    first.sort_by(|a, b| a.min_dist_m.total_cmp(&b.min_dist_m));
    rest.sort_by(|a, b| a.min_dist_m.total_cmp(&b.min_dist_m));
    let band_chunks = first.len();
    let chunks: Vec<WideChunk> = first.into_iter().chain(rest).collect();
    let mut last_use = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        for t in &c.tiles {
            last_use.insert(*t, i);
        }
    }
    let unique_tiles = last_use.len();
    WidePlan { zoom, chunks, last_use, unique_tiles, band_chunks }
}

/// `wide_band_reach` of each band of a wide search, None for a band inside
/// another one searched on the same zoom at least as far (a scale bar's
/// inside the default range's): in one sweep nearest first the other finds
/// what it would, at the same chunks.
pub fn wide_band_reaches(
    radius_m: f64,
    bands: &[ScaleBand],
    img_w: usize,
    img_h: usize,
    lat: f64,
) -> Vec<Option<(u8, f64)>> {
    let mut reaches: Vec<Option<(u8, f64)>> =
        bands.iter().map(|b| wide_band_reach(radius_m, b, img_w, img_h, lat)).collect();
    for i in 0..bands.len() {
        let Some((zi, ri)) = reaches[i] else { continue };
        let covered = (0..bands.len()).any(|j| {
            j != i
                && reaches[j].is_some_and(|(zj, rj)| zj == zi && rj >= ri)
                && bands[j].lo <= bands[i].lo
                && bands[j].hi >= bands[i].hi
                && (j < i || bands[j] != bands[i])
        });
        if covered {
            reaches[i] = None;
        }
    }
    reaches
}

/// Zoom of the wide search for a scale band: `wide_reach`'s for the near
/// zoom's band; a coarser band (scales over `FINE_MAX_SCALE`) goes up to two
/// zooms coarser while every scale of it keeps a template of `MIN_SIDE`
/// cells (`min_searchable_scale`): zoom 7 for a 3 500 × 2 500 px page at
/// 12–50 m/px, zoom 9 for a 1 118 × 791 px export.
pub fn wide_band_reach(radius_m: f64, band: &ScaleBand, img_w: usize, img_h: usize, lat: f64) -> Option<(u8, f64)> {
    let (zoom, reach) = wide_reach(radius_m, band.hi, img_w, img_h, lat)?;
    if band.is_fine() {
        return Some((zoom, reach));
    }
    let mut z = zoom;
    while z > zoom.saturating_sub(2).max(1) && min_searchable_scale(img_w, img_h, resolution(z - 1), lat) <= band.lo {
        z -= 1;
    }
    Some((z, reach))
}

/// The chunks of `plan_wide`, nearest first: placements clipped to `clip`,
/// or leaving out `exclude`.
#[allow(clippy::too_many_arguments)]
fn plan_chunks(
    origin: (f64, f64),
    radius_m: f64,
    near_m: f64,
    zoom: u8,
    half_extent: (f64, f64),
    max_tiles: usize,
    clip: Option<MercBox>,
    exclude: Option<MercBox>,
) -> Vec<WideChunk> {
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
    let dist = |p: (f64, f64)| {
        let (la, lo) = merc_to_latlon(p.0, p.1);
        haversine_m(o, LatLon::new(la, lo))
    };
    let mut chunks: Vec<(f64, f64, WideChunk)> = Vec::new();
    let mut cy = y0;
    while cy <= y1 {
        let mut cx = x0;
        while cx <= x1 {
            let (ex, ey) = ((cx + sx - 1).min(n - 1), (cy + sy - 1).min(n - 1));
            let tl = tile_bounds(TileId { z: zoom, x: cx, y: cy });
            let br = tile_bounds(TileId { z: zoom, x: ex, y: ey });
            let mut placement = MercBox { min_x: tl.min_x, min_y: br.min_y, max_x: br.max_x, max_y: tl.max_y };
            if let Some(bd) = &clip {
                placement = MercBox {
                    min_x: placement.min_x.max(bd.min_x),
                    min_y: placement.min_y.max(bd.min_y),
                    max_x: placement.max_x.min(bd.max_x),
                    max_y: placement.max_y.min(bd.max_y),
                };
            }
            let empty = placement.min_x >= placement.max_x
                || placement.min_y >= placement.max_y
                || exclude.is_some_and(|e| box_within(&placement, &e));
            // (An empty placement box clamps to its min corner; it is skipped.)
            // The nearest point counts across the antimeridian too.
            let min_d = if empty { f64::INFINITY } else { dist(nearest_in_box(&placement, origin)) };
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
                        let far =
                            [(tb.min_x, tb.min_y), (tb.max_x, tb.min_y), (tb.max_x, tb.max_y), (tb.min_x, tb.max_y)]
                                .into_iter()
                                .map(dist)
                                .fold(0.0, f64::max);
                        dist(nearest_in_box(&tb, origin)) <= radius_m + reach && far >= near_m - reach
                    })
                    .collect();
                chunks.push((
                    min_d,
                    bearing,
                    WideChunk {
                        placement,
                        exclude,
                        mosaic,
                        tiles,
                        min_dist_m: min_d,
                        max_dist_m: max_d,
                        zoom,
                        scale_band: 0,
                    },
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
    chunks.into_iter().map(|c| c.2).collect()
}

// ---------------------------------------------------------------------------
// Estimate (mm_search_estimate)
// ---------------------------------------------------------------------------

/// Mean Terrarium tile size (bytes) per zoom: zooms 9, 12 and 13 over every
/// tile the measured searches fetched (1 860 zoom-9 tiles of southern Africa
/// from Windhoek and Cape Town to Durban, coast included; 586 zoom-13 tiles
/// of the refinements, rugged terrain where contour maps are), zoom 8, 10
/// and 11 over 60 tiles each spread over 2 300 km around the Magaliesberg
/// (`wide_measure::tile_weights`). A 1 000 km search from Cape Town fetched
/// 90.0 MB; this predicts 89.6 MB (with the 60-tile zoom-9 mean of 68.9 KB
/// it predicted 75 MB).
pub fn tile_bytes(z: u8) -> f64 {
    let kb = match z {
        0..=8 => 76.8,
        9 => 86.6,
        10 => 45.9,
        11 => 26.4,
        12 => 22.6,
        _ => 26.3,
    };
    kb * 1024.0
}

/// The map the estimate assumes when the request names none: a 300-dpi A3
/// sheet, no scale read (the default range, 0.4–30 m/px), north up.
pub const TYPICAL_IMAGE: (usize, usize) = (3507, 4960);

/// What the estimate knows of the map's scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScalePrior {
    /// Nothing read: the default range (`default_scale_range`).
    None,
    /// A printed scale with the dpi (ground m/px range): a constraint.
    Label(f64, f64),
    /// A scale bar's range: searched first in the hint and near areas, the
    /// default range after it.
    Bar(f64, f64),
}

/// The map whose search is estimated: its size in pixels and scale prior.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstimateMap {
    pub size: (usize, usize),
    pub prior: ScalePrior,
}

impl EstimateMap {
    /// The typical sheet (`TYPICAL_IMAGE`), no scale read.
    pub fn typical() -> EstimateMap {
        EstimateMap { size: TYPICAL_IMAGE, prior: ScalePrior::None }
    }
}

/// Near-zoom re-check tiles per wide chunk (3 candidates; measured 8–11)
/// and zoom-13 tiles per refinement (measured 8–16 a refinement).
const RECHECK_TILES_PER_CHUNK: usize = 10;
const REFINE_TILES: usize = 14;

/// Compute on the reference machine (4-core 2.1 GHz Xeon shared with other
/// jobs, release build), fitted to `wide_measure::estimate_check` (searches
/// run to the end, the map outside the radius: five maps of 0.9–17 MP,
/// 50–2 000 km; the fits within 20 % and 12 %): per hint or near area,
/// seconds per million FFT cells of coarse search work
/// (`search::search_work`), plus a fixed part and one per megapixel of the
/// map (its field pyramid and templates); the same per wide chunk (its
/// three re-checks included); per megapixel of contour extraction the
/// search waits for (most of it overlaps the first download); per
/// refinement. A search that does not find the map refines little (a
/// candidate must stand out, `georef::REFINE_MIN_RATIO`: 0–3, 1.5 on
/// average); a search that finds it, once.
const AREA_S_PER_MWORK: f64 = 0.0049;
const AREA_S: f64 = 0.033;
const AREA_S_PER_MP: f64 = 0.0035;
const CHUNK_S_PER_MWORK: f64 = 0.0062;
const CHUNK_S: f64 = 0.197;
const CHUNK_S_PER_MP: f64 = 0.0049;
const PREP_S_PER_MP: f64 = 0.03;
const REFINE_S: f64 = 0.35;
const REFINES: f64 = 1.5;
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
    /// Coarse search work (millions of FFT cells, `search::search_work`)
    /// and refinements counted.
    pub work: f64,
    pub refines: f64,
    /// Of `work`, the wide chunks'.
    pub wide_work: f64,
}

impl Estimate {
    pub fn total_tiles(&self) -> usize {
        self.tiles.iter().map(|t| t.1).sum()
    }
}

/// Coarse search parameters of an area of radius `r` around `c` (Mercator)
/// in one scale band, as `georef` plans it.
fn area_params(c: (f64, f64), r: f64, band: &ScaleBand) -> SearchParams {
    SearchParams {
        center_merc: c,
        search_radius_m: r,
        scale_min: band.lo,
        scale_max: band.hi,
        rotations_deg: vec![0.0],
        top_k: 1,
        region: None,
    }
}

/// Coarse tiles of an area search (the band's zoom and box cap).
fn area_tiles(p: &SearchParams, img: (usize, usize), band: &ScaleBand) -> Vec<TileId> {
    let lat = merc_to_latlon(p.center_merc.0, p.center_merc.1).0;
    tiles_covering(band.zoom, &required_dem_box_capped(p, img.0, img.1, band.max_box_half_m(lat)))
}

/// The scale bands of the hint and near areas, the prior's first, with how
/// many are the prior's (`georef::Run::area_band_plan`).
fn estimate_bands(map: &EstimateMap, lat: f64) -> (Vec<ScaleBand>, usize) {
    let (w, h) = map.size;
    let (dlo, dhi) = default_scale_range(w, h);
    match map.prior {
        ScalePrior::None => (scale_bands(dlo, dhi, w, h, lat), 0),
        ScalePrior::Label(lo, hi) => {
            let b = scale_bands(lo, hi, w, h, lat);
            let n = b.len();
            (b, n)
        }
        ScalePrior::Bar(lo, hi) => {
            let mut b = scale_bands(lo, hi, w, h, lat);
            let n = b.len();
            b.extend(scale_bands(dlo, dhi, w, h, lat));
            (b, n)
        }
    }
}

/// Estimate a search of `radius_m` around `gps` (or of the hints alone)
/// for `map`, the way the search plans it: hint areas and the near disc at
/// `near_zoom` (the inner 20 km first, coarser bands before the near zoom's
/// whole disc), the rest in chunks of each band's wide zoom; the compute of
/// every coarse search from its planned work (`search::search_work`). An
/// upper bound: a search that finds the map stops there.
pub fn estimate(
    radius_m: f64,
    near_m: f64,
    gps: Option<LatLon>,
    hints: &[SearchHint],
    map: &EstimateMap,
    max_tiles: usize,
) -> Estimate {
    let img = map.size;
    let max_tiles = max_tiles.max(1);
    // The hints searched: valid, within the radius of the fix, ordered and
    // deduplicated, at most `MAX_HINTS` (as `georef` plans them).
    let hints: Vec<SearchHint> = crate::hints::order_hints(
        hints
            .iter()
            .filter(|h| h.is_valid() && gps.is_none_or(|g| haversine_m(g, h.latlon()) <= radius_m))
            .cloned()
            .collect(),
    )
    .into_iter()
    .take(crate::georef::MAX_HINTS)
    .collect();
    let lat = gps.map(|g| g.lat).or(hints.first().map(|h| h.lat)).unwrap_or(0.0);
    let (bands, n_prior) = estimate_bands(map, lat);
    let mut work = 0.0;
    let mut areas = 0usize;
    let mut rounds = 0usize;
    let mut coarse: HashSet<TileId> = HashSet::new();
    // One area search: its work, and a round for the tiles not fetched yet.
    let mut area = |p: SearchParams, band: &ScaleBand, coarse: &mut HashSet<TileId>| {
        work += search_work(&p, img.0, img.1, resolution(band.zoom));
        areas += 1;
        let fresh = area_tiles(&p, img, band).into_iter().filter(|t| coarse.insert(*t)).count();
        rounds += fresh.div_ceil(max_tiles);
    };
    for h in &hints {
        let c = latlon_to_merc(h.lat, h.lon);
        for band in &bands {
            area(area_params(c, h.radius_m.clamp(1_000.0, near_m), band), band, &mut coarse);
        }
    }
    let mut coarse_zoom = near_zoom(lat);
    let mut wide_tiles: Vec<TileId> = Vec::new();
    let mut chunks = 0usize;
    let mut wide_work = 0.0;
    if let Some(g) = gps {
        let c = latlon_to_merc(g.lat, g.lon);
        let near = radius_m.min(near_m);
        // The inner disc first, then the prior's bands, then coarse before fine.
        let first = crate::georef::NEAR_FIRST_RADIUS_M;
        if let Some(b) = bands.first().filter(|b| b.is_fine() && near > first) {
            area(area_params(c, first, b), b, &mut coarse);
        }
        let mut rest = bands[n_prior..].to_vec();
        rest.sort_by_key(|b| b.is_fine());
        for b in bands[..n_prior].iter().chain(&rest) {
            area(area_params(c, near, b), b, &mut coarse);
        }
        // The wide search: each band out to its reach, a band inside another
        // on the same zoom left out (`wide_setup`), their chunks in one sweep.
        if radius_m > near_m {
            let reaches = wide_band_reaches(radius_m, &bands, img.0, img.1, g.lat);
            let mut plans = Vec::new();
            let mut bases = Vec::new();
            for (band, reach) in bands.iter().zip(reaches) {
                let Some((zoom, reach_m)) = reach else { continue };
                let p = SearchParams { search_radius_m: reach_m, ..area_params(c, reach_m, band) };
                plans.push(plan_wide(c, reach_m, near_m, zoom, map_half_extent(&p, img.0, img.1), max_tiles, None));
                bases.push((p, zoom));
            }
            if let Some(first) = plans.first() {
                coarse_zoom = first.zoom;
            }
            let plan = merge_plans(plans);
            for ch in &plan.chunks {
                let Some((base, zoom)) = bases.get(ch.scale_band) else { continue };
                let region = crate::search::Region {
                    bounds: ch.placement,
                    origin: c,
                    r_min_m: near_m,
                    r_max_m: base.search_radius_m,
                    exclude: None,
                };
                let p = SearchParams { region: Some(region), ..base.clone() };
                wide_work += search_work(&p, img.0, img.1, resolution(*zoom));
            }
            wide_tiles = plan.last_use.keys().copied().collect();
            chunks = plan.chunks.len();
            // One round per chunk (re-checks ride along with the next chunk).
            rounds += chunks + usize::from(chunks > 0);
        }
    }
    let near_work = work;
    work += wide_work;
    let refines = if areas + chunks > 0 { REFINES } else { 0.0 };
    rounds += refines.ceil() as usize;
    let near_z = near_zoom(lat);
    let mut tiles: Vec<(u8, usize)> = Vec::new();
    let mut add = |z: u8, n: usize| match tiles.iter_mut().find(|t| t.0 == z) {
        Some(t) => t.1 += n,
        None => tiles.push((z, n)),
    };
    // A tile of both (a coarse band's near disc and its wide chunks) is
    // fetched once.
    coarse.extend(wide_tiles);
    for t in &coarse {
        add(t.z, 1);
    }
    add(near_z, chunks * RECHECK_TILES_PER_CHUNK);
    add(13, (refines * REFINE_TILES as f64).round() as usize);
    tiles.retain(|t| t.1 > 0);
    tiles.sort_by_key(|t| t.0);
    let bytes: f64 = tiles.iter().map(|&(z, n)| tile_bytes(z) * n as f64).sum();
    let mp = (img.0 * img.1) as f64 / 1e6;
    let prep = if areas + chunks > 0 { PREP_S_PER_MP * mp } else { 0.0 };
    let compute_s = (prep
        + near_work * AREA_S_PER_MWORK
        + areas as f64 * (AREA_S + AREA_S_PER_MP * mp)
        + wide_work * CHUNK_S_PER_MWORK
        + chunks as f64 * (CHUNK_S + CHUNK_S_PER_MP * mp)
        + refines * REFINE_S)
        * IPHONE_FACTOR;
    let download_s = bytes / DOWNLOAD_BYTES_PER_S + rounds as f64 * ROUND_LATENCY_S;
    Estimate { coarse_zoom, tiles, bytes, compute_s, download_s, rounds, chunks, work, refines, wide_work }
}

/// `mm_search_estimate` request.
#[derive(Deserialize, Default)]
#[serde(default)]
struct EstimateRequest {
    radius_m: Option<f64>,
    gps: Option<EstimateGps>,
    hints: Option<Vec<EstimateHint>>,
    max_tiles_per_round: Option<f64>,
    /// A printed scale was read (the typical sheet's 1:25 000 at 300 dpi).
    scale_known: Option<bool>,
    /// The map's size in pixels (default: a 300-dpi A3 sheet).
    image_size: Option<[f64; 2]>,
    /// Ground m/px range of a printed scale (with the dpi) …
    scale_m_per_px: Option<[f64; 2]>,
    /// … or of a scale bar (`diagnostics.scale_range` of an earlier search).
    scale_bar_m_per_px: Option<[f64; 2]>,
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

/// A ground m/px range from a request: finite, positive, ordered.
fn range_of(v: Option<[f64; 2]>) -> Option<(f64, f64)> {
    let [a, b] = v?;
    (a.is_finite() && b.is_finite() && a > 0.0 && b >= a).then_some((a, b))
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
    let size = req
        .image_size
        .filter(|s| s.iter().all(|v| v.is_finite() && (16.0..=100_000.0).contains(v)))
        .map_or(TYPICAL_IMAGE, |s| (s[0].round() as usize, s[1].round() as usize));
    let lat = gps.map_or(0.0, |g| g.lat);
    let prior = if let Some((lo, hi)) = range_of(req.scale_m_per_px) {
        ScalePrior::Label(lo, hi)
    } else if let Some((lo, hi)) = range_of(req.scale_bar_m_per_px) {
        ScalePrior::Bar(lo, hi)
    } else if req.scale_known.unwrap_or(false) {
        // The typical sheet's printed 1:25 000 at 300 dpi, ±25 % (`georef`).
        let prior = 25_000.0 * 0.0254 / 300.0;
        ScalePrior::Label(0.75 * prior * lat.to_radians().cos(), 1.25 * prior)
    } else {
        ScalePrior::None
    };
    let map = EstimateMap { size, prior };
    let e = estimate(radius, crate::georef::NEAR_RADIUS_M, gps, &hints, &map, max_tiles);
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
        "search_work": (e.work * 10.0).round() / 10.0,
        "search_work_wide": (e.wide_work * 10.0).round() / 10.0,
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
    fn scale_bands_by_map_size() {
        let lat = -29.3;
        // A 300-dpi A3 page: the default range stops at 150 km across (30 m/px);
        // up to 12 m/px on zoom 11 as always, beyond on zoom 8.
        let (lo, hi) = default_scale_range(3507, 4960);
        assert!((hi - 150_000.0 / 4960.0).abs() < 1e-9 && lo == 0.4, "{hi}");
        let b = scale_bands(lo, hi, 3507, 4960, lat);
        assert_eq!(b, vec![ScaleBand { zoom: 11, lo: 0.4, hi: 12.0 }, ScaleBand { zoom: 8, lo: 12.0, hi }]);
        assert!(b[0].is_fine() && !b[1].is_fine());
        // A Gaia export of 1 118 × 791 px: the whole default range, zoom 10 beyond 12 m/px.
        let (lo, hi) = default_scale_range(1118, 791);
        assert_eq!((lo, hi), crate::georef::DEFAULT_SCALE_RANGE);
        assert_eq!(scale_bands(lo, hi, 1118, 791, lat)[1].zoom, 10);
        // A scale label's range lies in one band; one straddling 12 m/px in two.
        assert_eq!(scale_bands(1.43, 2.65, 3507, 4960, lat).len(), 1);
        assert_eq!(scale_bands(15.0, 28.7, 1118, 791, lat), vec![ScaleBand { zoom: 10, lo: 15.0, hi: 28.7 }]);
        assert_eq!(scale_bands(10.4, 26.5, 2400, 1800, lat).len(), 2);
        // Coarse bands' boxes hold as many cells as zoom 11's; re-checks one zoom finer.
        assert_eq!(ScaleBand { zoom: 9, lo: 12.0, hi: 50.0 }.max_box_half_m(lat), 240_000.0);
        assert_eq!(recheck_zoom(1.9, 3507, 4960, lat), 11);
        assert_eq!(recheck_zoom(23.0, 1118, 791, lat), 10);
        assert!(scale_bands(f64::NAN, 2.0, 100, 100, lat).is_empty());
        // A 1:5 000 plan at 300 dpi (0.42 m/px ±25 %) is too fine for zoom 11:
        // zoom 12, in a box of ±30 km.
        let plan = scale_bands(0.32, 0.53, 2480, 3508, lat);
        assert_eq!(plan, vec![ScaleBand { zoom: 12, lo: 0.32, hi: 0.53 }]);
        assert_eq!(plan[0].max_box_half_m(lat), 30_000.0);
        assert_eq!(recheck_zoom(0.42, 2480, 3508, lat), 12);
    }

    #[test]
    fn merged_plans_release_tiles_after_their_last_use() {
        let o = origin();
        let (olat, _) = merc_to_latlon(o.0, o.1);
        let k = 1.0 / olat.to_radians().cos();
        let fine = plan_wide(o, 300_000.0, 50_000.0, 9, (21_000.0 * k, 30_000.0 * k), 64, None);
        let coarse = plan_wide(o, 300_000.0, 50_000.0, 9, (60_000.0 * k, 85_000.0 * k), 64, None);
        let (nf, nc) = (fine.chunks.len(), coarse.chunks.len());
        let m = merge_plans(vec![fine, coarse]);
        assert_eq!(m.chunks.len(), nf + nc);
        // Nearest first, both bands interleaved.
        for w in m.chunks.windows(2) {
            assert!(w[0].min_dist_m <= w[1].min_dist_m + 1e-6);
        }
        assert!(m.chunks.iter().any(|c| c.scale_band == 1) && m.chunks.iter().any(|c| c.scale_band == 0));
        // Every tile's last use is the last chunk (of either band) that needs it.
        for (t, &last) in &m.last_use {
            assert!(m.chunks[last].tiles.contains(t));
            assert!(m.chunks[last + 1..].iter().all(|c| !c.tiles.contains(t)));
        }
        assert_eq!(m.unique_tiles, m.last_use.len());
    }

    #[test]
    fn zoom_rule() {
        let lat = -25.85;
        let z = |r: f64, hi: f64, w: usize, h: usize| wide_reach(r, hi, w, h, lat);
        assert_eq!(z(100_000.0, 12.0, 3507, 4960), Some((10, 100_000.0)));
        assert_eq!(z(150_000.0, 12.0, 3507, 4960), Some((10, 150_000.0)));
        assert_eq!(z(250_000.0, 12.0, 3507, 4960), Some((9, 250_000.0)));
        assert_eq!(z(1_000_000.0, 12.0, 3507, 4960), Some((9, 1_000_000.0)));
        // A map showing 3.8 km × 4.9 km (1:25 000 A5 at 1.9 m/px) is too
        // small for zoom 9 (14 cells): zoom 10 (28 cells), to 150 km only.
        assert_eq!(z(1_000_000.0, 1.9, 2000, 2600), Some((10, 150_000.0)));
        assert_eq!(z(120_000.0, 1.9, 2000, 2600), Some((10, 120_000.0)));
        // 3 km: below 24 cells at zoom 10 too: no wide search (zoom 11 over
        // 1 000 km would be ~10 000 tiles).
        assert_eq!(z(1_000_000.0, 1.9, 1600, 2100), None);
        assert_eq!(z(100_000.0, 1.9, 1600, 2100), None);
    }

    #[test]
    fn zooms_follow_latitude() {
        // Up to 60° the zooms stand; beyond, coarser zooms have the same
        // ground detail (and far fewer tiles).
        for lat in [0.0, -25.85, 46.5, -59.9] {
            assert_eq!(near_zoom(lat), 11, "{lat}");
            assert_eq!(zoom_at_latitude(9, lat), 9, "{lat}");
        }
        assert_eq!(near_zoom(69.65), 10);
        assert_eq!(zoom_at_latitude(9, 69.65), 8);
        assert_eq!(near_zoom(78.2), 9);
        assert_eq!(zoom_at_latitude(9, -84.0), 6);
        for lat in [0.0, 30.0, 60.5, 70.0, 78.0, 84.9] {
            for z in [9u8, 10, 11] {
                let zl = zoom_at_latitude(z, lat);
                let ground = resolution(zl) * f64::to_radians(lat).cos();
                assert!(ground <= resolution(z) * 1.000_001 && ground > resolution(z) / 2.0, "{lat} {z}: {ground}");
            }
        }
        // Tromsø: a 1 000 km search plans about as many tiles as at 26°S.
        let plan_at = |lat: f64| {
            let c = latlon_to_merc(lat, 18.96);
            let (z, r) = wide_reach(1_000_000.0, 12.0, 3507, 4960, lat).unwrap();
            let p = SearchParams { center_merc: c, scale_min: 0.4, scale_max: 12.0, ..SearchParams::default() };
            plan_wide(c, r, 50_000.0, z, map_half_extent(&p, 3507, 4960), 64, None).unique_tiles
        };
        let (south, north) = (plan_at(-25.85), plan_at(69.65));
        assert!((north as f64) < 2.0 * south as f64, "{north} vs {south}");
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
    fn band_chunks_come_first_and_the_rest_still_covers_the_annulus() {
        let o = origin();
        let (olat, olon) = merc_to_latlon(o.0, o.1);
        let k = 1.0 / olat.to_radians().cos();
        let radius = 400_000.0;
        // A band of latitudes 150 km north of the fix, 6 km wide.
        let by = latlon_to_merc(olat + 150.0 / 111.32, 0.0).1;
        let band = MercBox { min_x: -1e8, min_y: by - 3_000.0 * k, max_x: 1e8, max_y: by + 3_000.0 * k };
        let plan = plan_wide(o, radius, 50_000.0, 9, (21_000.0 * k, 30_000.0 * k), 64, Some(band));
        let full = plan_wide(o, radius, 50_000.0, 9, (21_000.0 * k, 30_000.0 * k), 64, None);
        assert!(plan.band_chunks > 0 && plan.band_chunks < plan.chunks.len());
        for c in &plan.chunks[..plan.band_chunks] {
            assert!(c.placement.min_y >= band.min_y && c.placement.max_y <= band.max_y && c.exclude.is_none());
        }
        for c in &plan.chunks[plan.band_chunks..] {
            assert_eq!(c.exclude, Some(band));
        }
        // The band's chunks fetch a fraction of the disc's tiles.
        let band_tiles: HashSet<TileId> =
            plan.chunks[..plan.band_chunks].iter().flat_map(|c| c.tiles.iter().copied()).collect();
        assert!(band_tiles.len() * 3 < full.unique_tiles, "{} of {}", band_tiles.len(), full.unique_tiles);
        assert_eq!(plan.unique_tiles, full.unique_tiles);
        // While band chunks are left, the radius searched stays the near disc.
        assert_eq!(plan.searched_radius_m(0, 50_000.0, radius), 50_000.0);
        assert_eq!(plan.searched_radius_m(plan.band_chunks, 50_000.0, radius), 50_000.0);
        // Every point of the annulus is searched exactly once.
        let o_ll = LatLon::new(olat, olon);
        let inside = |b: &MercBox, x: f64, y: f64| x >= b.min_x && x < b.max_x && y >= b.min_y && y < b.max_y;
        for i in 0..600 {
            let bearing = i as f64 * 137.507_764;
            let d = 50_000.0 + (radius - 50_000.0) * ((i as f64 * 0.618_034) % 1.0);
            let mut pts = vec![destination(o_ll, bearing, d)];
            // Points in the band too.
            let (_, lx) = merc_to_latlon(o.0 + (i as f64 - 300.0) * 1_000.0, by);
            pts.push(LatLon::new(merc_to_latlon(0.0, by + ((i % 7) as f64 - 3.0) * 900.0 * k).0, lx));
            for p in pts {
                if haversine_m(o_ll, p) < 50_000.0 || haversine_m(o_ll, p) > radius {
                    continue;
                }
                let (mx, my) = latlon_to_merc(p.lat, p.lon);
                let n = plan
                    .chunks
                    .iter()
                    .filter(|c| inside(&c.placement, mx, my) && !c.exclude.is_some_and(|e| inside(&e, mx, my)))
                    .count();
                assert_eq!(n, 1, "{p:?} in {n} chunks");
            }
        }
    }

    #[test]
    fn chunks_across_the_antimeridian_count_their_near_side() {
        // A fix 70 km west of the antimeridian: placements just across it
        // are ~70 km away, not the chunk's far edge.
        let o = latlon_to_merc(-17.8, 179.35);
        let k = 1.0 / (-17.8f64).to_radians().cos();
        let plan = plan_wide(o, 300_000.0, 50_000.0, 9, (6_000.0 * k, 8_000.0 * k), 64, None);
        let east = plan.chunks.iter().find(|c| c.placement.min_x < -1.9e7).expect("a chunk east of 180°");
        assert!(east.min_dist_m < 80_000.0, "{}", east.min_dist_m);
        let (la, lo) = merc_to_latlon(o.0, o.1);
        let p = latlon_to_merc(-17.7, -179.7);
        assert!(haversine_m(LatLon::new(la, lo), LatLon::new(-17.7, -179.7)) < 120_000.0);
        let holding = plan.chunks.iter().filter(|c| {
            let b = &c.placement;
            p.0 >= b.min_x && p.0 < b.max_x && p.1 >= b.min_y && p.1 < b.max_y
        });
        assert_eq!(holding.count(), 1);
    }

    #[test]
    fn estimate_grows_with_radius_and_stays_honest() {
        let g = Some(LatLon::new(-25.85, 27.4));
        let near = estimate(50_000.0, 50_000.0, g, &[], &EstimateMap::typical(), 64);
        assert_eq!(near.coarse_zoom, 11);
        assert_eq!(near.chunks, 0);
        let mid = estimate(120_000.0, 50_000.0, g, &[], &EstimateMap::typical(), 64);
        assert_eq!(mid.coarse_zoom, 10);
        let far = estimate(1_000_000.0, 50_000.0, g, &[], &EstimateMap::typical(), 64);
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
        let only = estimate(1_000_000.0, 50_000.0, None, std::slice::from_ref(&h), &EstimateMap::typical(), 64);
        assert_eq!(only.coarse_zoom, 11);
        assert!(only.total_tiles() > 0 && only.bytes < near.bytes * 1.5);
        assert_eq!(estimate(1_000_000.0, 50_000.0, None, &[], &EstimateMap::typical(), 64).total_tiles(), 0);
        // A hint beyond the radius of the fix is not searched.
        let far_hint = SearchHint { lat: -33.9, lon: 18.4, ..h };
        let e = estimate(50_000.0, 50_000.0, g, &[far_hint], &EstimateMap::typical(), 64);
        assert_eq!(e.total_tiles(), near.total_tiles());
    }

    /// The estimate follows the map: a small export searches less than a
    /// 300-dpi sheet, a printed scale far less; a scale bar's band is searched
    /// first near the fix (its inner 20 km in place of the default range's),
    /// the default range after it, and the wide search is the default
    /// range's: about the work of no prior.
    #[test]
    fn estimate_follows_the_map() {
        let g = Some(LatLon::new(-22.56, 17.08));
        let e = |size: (usize, usize), prior: ScalePrior| {
            estimate(1_000_000.0, 50_000.0, g, &[], &EstimateMap { size, prior }, 64)
        };
        let typical = e(TYPICAL_IMAGE, ScalePrior::None);
        let small = e((1118, 791), ScalePrior::None);
        let label = e(TYPICAL_IMAGE, ScalePrior::Label(1.59, 2.65));
        let bar = e(TYPICAL_IMAGE, ScalePrior::Bar(1.8, 2.4));
        assert!(small.work < typical.work && small.compute_s < typical.compute_s, "{small:?} {typical:?}");
        assert!(label.work < typical.work / 2.0, "{label:?} {typical:?}");
        assert!((bar.work - typical.work).abs() < 0.1 * typical.work, "{bar:?} {typical:?}");
        assert!(bar.work > label.work * 1.5, "{bar:?} {label:?}");
        assert_eq!(bar.chunks, typical.chunks);
        assert_eq!(bar.wide_work, typical.wide_work);
        // A small export's coarse band reaches beyond 150 km on coarse tiles; its
        // fine band stops there (`wide_reach`).
        assert!(small.chunks > 0 && small.tiles.iter().all(|t| t.1 > 0));
        let v: Value = serde_json::from_str(&search_estimate_json(
            r#"{"radius_m": 1000000, "gps": {"lat": -22.56, "lon": 17.08}, "image_size": [1118, 791]}"#,
        ))
        .unwrap();
        assert_eq!(v["tiles"].as_u64().unwrap() as usize, small.total_tiles());
        let v: Value = serde_json::from_str(&search_estimate_json(
            r#"{"radius_m": 1000000, "gps": {"lat": -22.56, "lon": 17.08}, "scale_bar_m_per_px": [1.8, 2.4]}"#,
        ))
        .unwrap();
        assert_eq!(v["chunks"].as_u64().unwrap() as usize, bar.chunks);
        // Nonsense sizes and ranges fall back to the typical sheet, no prior.
        let v: Value = serde_json::from_str(&search_estimate_json(
            r#"{"radius_m": 1000000, "gps": {"lat": -22.56, "lon": 17.08}, "image_size": [0, -5],
                "scale_m_per_px": [3, 1]}"#,
        ))
        .unwrap();
        assert_eq!(v["tiles"].as_u64().unwrap() as usize, typical.total_tiles());
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
