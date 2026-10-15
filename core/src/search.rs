//! Coarse global search: orientation-field FFT correlation of map contours vs DEM.
//!
//! Contour lines run perpendicular to the terrain gradient, so the local
//! orientation of the map's contour pattern (structure tensor of the blurred
//! contour mask) must match the orientation of the DEM gradient. Both are
//! encoded as doubled-angle complex fields `z = |g|²·e^{2iφ}` (which makes the
//! two edges of a line, and uphill vs downhill, equivalent), normalised to
//! unit-ish magnitude, and compared with a normalised complex cross-correlation
//! computed by FFT over all translations, for every candidate scale and
//! rotation. The correlation peak gives where the map is and at what scale.
//!
//! Resolution: for each scale the template is built on the coarsest DEM
//! pyramid level (the given mosaic, 2×, 4×, … downsampled) on which its
//! smaller side still has ≥ `TARGET_SIDE` cells. This keeps every template at
//! a similar number of cells, so scores are comparable across scales and the
//! FFTs stay small; templates below `MIN_SIDE` cells are never used because
//! they produce spurious peaks. Peaks are ranked by their significance within
//! their own correlation surface (see `select_candidates`).
//!
//! Rotation: instead of rotating each template, the DEM field is resampled
//! once per rotation into the map's frame (positions rotated by θ, doubled
//! angles by −2θ), so templates stay axis-aligned and window energies are box
//! sums. A rotation sweep runs in two stages: all rotations with smaller
//! templates, then a local fine search around the best hypotheses.

use crate::dem::{DemMosaic, MercBox};
use crate::fft::{next_fast_len, Fft2, C32};
use crate::geo::{haversine_m, merc_to_latlon, Affine, LatLon};
use crate::image::{GrayF32, Mask};
use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Doubled-angle orientation field derived from the contour mask at a
/// reduced working resolution.
#[derive(Clone)]
pub struct MapField {
    /// Field size in cells.
    pub w: usize,
    pub h: usize,
    /// Base-image pixels per field cell.
    pub px_per_cell: f64,
    /// Raw structure-tensor components (Jxx−Jyy, 2Jxy) before smoothing to the template scale.
    pub re: Vec<f32>,
    pub im: Vec<f32>,
}

/// Field cell size in base-image pixels: 2 for a 300-dpi A3/A4 sheet, so the
/// blur below spans a typical (2–4 px) contour line.
pub fn field_px_per_cell(w: usize, h: usize) -> usize {
    ((w.max(h) as f64 / 2480.0).round() as usize).clamp(1, 4)
}

/// Gaussian sigma (field cells) applied to the contour density before Sobel.
const MAP_BLUR_SIGMA: f64 = 1.0;

/// Build the map field from the full-resolution contour mask.
pub fn map_field(mask: &Mask) -> MapField {
    map_field_at(mask, field_px_per_cell(mask.w, mask.h), 1)
}

/// Build the map field from a contour mask with `ppc` mask pixels per field
/// cell, the mask having `level` base-image pixels per pixel (a mask made at
/// a downsampled analysis level).
pub fn map_field_at(mask: &Mask, ppc: usize, level: usize) -> MapField {
    let ppc = ppc.max(1);
    let px_per_cell = (ppc * level.max(1)) as f64;
    if mask.w == 0 || mask.h == 0 {
        return MapField { w: 0, h: 0, px_per_cell, re: Vec::new(), im: Vec::new() };
    }
    let density = box_downsample_mask(mask, ppc);
    let g = density.gaussian_blur(MAP_BLUR_SIGMA);
    let (gx, gy) = g.sobel();
    let n = g.w * g.h;
    let mut re = vec![0.0f32; n];
    let mut im = vec![0.0f32; n];
    re.par_iter_mut().zip(im.par_iter_mut()).enumerate().for_each(|(i, (r, m))| {
        let (x, y) = (gx.data[i], gy.data[i]);
        *r = x * x - y * y;
        *m = 2.0 * x * y;
    });
    MapField { w: g.w, h: g.h, px_per_cell, re, im }
}

/// Mean of each `f`×`f` pixel block (blocks past the edge count missing pixels as 0).
fn box_downsample_mask(mask: &Mask, f: usize) -> GrayF32 {
    let (w, h) = (mask.w.div_ceil(f), mask.h.div_ceil(f));
    let mut out = vec![0.0f32; w * h];
    let inv = 1.0 / (f * f) as f32;
    out.par_chunks_mut(w).enumerate().for_each(|(cy, row)| {
        for yy in cy * f..((cy + 1) * f).min(mask.h) {
            let src = &mask.data[yy * mask.w..(yy + 1) * mask.w];
            for (x, &v) in src.iter().enumerate() {
                row[x / f] += v as f32;
            }
        }
        for v in row.iter_mut() {
            *v *= inv;
        }
    });
    GrayF32 { w, h, data: out }
}

#[derive(Debug, Clone)]
pub struct SearchParams {
    /// GPS position in Mercator metres.
    pub center_merc: (f64, f64),
    /// Ground-metre radius around the GPS position the map centre may lie in.
    pub search_radius_m: f64,
    /// Ground metres per base-image pixel range to scan.
    pub scale_min: f64,
    pub scale_max: f64,
    /// Map-up bearings to try, degrees (default [0.0]). See `rotation_sweep`.
    pub rotations_deg: Vec<f64>,
    /// Max number of distinct candidates to return.
    pub top_k: usize,
    /// Restrict the map centre to a box and annulus instead of the disc of
    /// `search_radius_m` around `center_merc` (a chunk of the wide search).
    /// `center_merc` should then be the box centre.
    pub region: Option<Region>,
}

impl Default for SearchParams {
    fn default() -> Self {
        SearchParams {
            center_merc: (0.0, 0.0),
            search_radius_m: 20_000.0,
            scale_min: 0.5,
            scale_max: 10.0,
            rotations_deg: vec![0.0],
            top_k: 3,
            region: None,
        }
    }
}

/// Where the map centre may lie in one chunk of a wide search: inside
/// `bounds` (half-open in Mercator x and y, so neighbouring chunks never
/// share a placement), outside `exclude` (half-open too; placements another
/// chunk searched first) and at a ground distance in `[r_min_m, r_max_m]`
/// from `origin` (the GPS fix; the near disc is searched separately).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub bounds: MercBox,
    /// Mercator position of the annulus centre.
    pub origin: (f64, f64),
    pub r_min_m: f64,
    pub r_max_m: f64,
    pub exclude: Option<MercBox>,
}

/// Is (x, y) in the half-open box?
fn in_box(b: &MercBox, x: f64, y: f64) -> bool {
    x >= b.min_x && x < b.max_x && y >= b.min_y && y < b.max_y
}

/// The point of `b` nearest to `p` in Mercator, across the antimeridian
/// when that is nearer (x taken modulo the world's width).
pub fn nearest_in_box(b: &MercBox, p: (f64, f64)) -> (f64, f64) {
    let world = 2.0 * std::f64::consts::PI * crate::geo::EARTH_RADIUS;
    let y = p.1.clamp(b.min_y, b.max_y);
    [p.0 - world, p.0, p.0 + world]
        .into_iter()
        .map(|x| (x, x.clamp(b.min_x, b.max_x)))
        .min_by(|a, c| (a.0 - a.1).abs().total_cmp(&(c.0 - c.1).abs()))
        .map_or((p.0.clamp(b.min_x, b.max_x), y), |(_, x)| (x, y))
}

impl Region {
    /// Ground distance (metres, great circle) from `origin` to a Mercator point.
    pub fn distance_m(&self, mx: f64, my: f64) -> f64 {
        let (la, lo) = merc_to_latlon(self.origin.0, self.origin.1);
        let (lb, lob) = merc_to_latlon(mx, my);
        haversine_m(LatLon::new(la, lo), LatLon::new(lb, lob))
    }

    /// Is a map centre at this Mercator position inside the region?
    pub fn contains(&self, mx: f64, my: f64) -> bool {
        if !in_box(&self.bounds, mx, my) || self.exclude.is_some_and(|e| in_box(&e, mx, my)) {
            return false;
        }
        let d = self.distance_m(mx, my);
        d >= self.r_min_m && d <= self.r_max_m
    }

    /// True when every point of `bounds` lies within the annulus and none in
    /// `exclude`, so only the box needs testing. The disc is convex (corners
    /// within `r_max_m`); the box's point nearest the origin must be at least
    /// `r_min_m` away.
    pub fn box_inside_annulus(&self) -> bool {
        let b = &self.bounds;
        if let Some(e) = &self.exclude {
            if e.min_x < b.max_x && e.max_x > b.min_x && e.min_y < b.max_y && e.max_y > b.min_y {
                return false;
            }
        }
        let corners = [(b.min_x, b.min_y), (b.max_x, b.min_y), (b.max_x, b.max_y), (b.min_x, b.max_y)];
        if corners.iter().any(|&(x, y)| self.distance_m(x, y) > self.r_max_m) {
            return false;
        }
        let near = nearest_in_box(b, self.origin);
        self.r_min_m <= 0.0 || self.distance_m(near.0, near.1) >= self.r_min_m
    }
}

/// Rotations for a full sweep: 0, step, 2·step, … < 360. 10° is a good step
/// (the fine stage resolves the rest). A sweep costs ~ (360/step) times a
/// north-up search at a quarter of the resolution, so narrow the scale range
/// (e.g. from an OCR'd "1:25 000" and the dpi) when sweeping.
pub fn rotation_sweep(step_deg: f64) -> Vec<f64> {
    let step = step_deg.clamp(1.0, 180.0);
    let n = (360.0 / step).round().max(1.0) as usize;
    (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
}

#[derive(Debug, Clone)]
pub struct Candidate {
    /// Pixel → Mercator similarity.
    pub transform: Affine,
    /// Normalised correlation score of the peak.
    pub score: f32,
    /// Peak significance / best significance at a location ≥ 2 km away (any
    /// scale), where significance z = (NCC − mean) / std of the template's
    /// whole correlation surface (comparable across scales and DEM levels).
    pub peak_ratio: f32,
    /// Ground metres per pixel.
    pub scale_m_per_px: f64,
    pub rotation_deg: f64,
}

impl Candidate {
    /// Mercator position of the image centre.
    pub fn center_merc(&self, img_w: usize, img_h: usize) -> (f64, f64) {
        self.transform.apply(img_w as f64 / 2.0, img_h as f64 / 2.0)
    }
}

/// Geometric scale step between searched scales.
pub const SCALE_STEP: f64 = 1.04;
/// Preferred minimum template side (cells) when picking the DEM level.
const TARGET_SIDE: f64 = 64.0;
/// Same for the first stage of a rotation sweep (smaller, cheaper templates).
const SWEEP_TARGET_SIDE: f64 = 32.0;
/// Templates smaller than this are not searched (spurious peaks).
pub const MIN_SIDE: f64 = 24.0;
/// A coarser level is used instead of FFT tiling while templates keep this many cells.
const TILED_MIN_SIDE: f64 = 40.0;
/// Largest FFT side; bigger windows are tiled.
const FFT_CAP: usize = 1024;
/// Gaussian sigma (template cells) applied after area-resampling the map field.
const TEMPLATE_SIGMA: f64 = 0.3;
/// Gaussian sigma (DEM cells) applied to the DEM orientation field.
const DEM_SIGMA: f64 = 0.3;
/// Candidates closer than this (ground metres) are the same location…
pub const DISTINCT_M: f64 = 2000.0;
/// …or than this fraction of the map's short side, for maps showing a large
/// area: their correlation peaks are wider than 2 km (a 3 500 × 2 500 px
/// export at 50 m/px is searched on ~1 km cells), and a peak's own shoulder
/// must not count as another place. A 1:25 000 sheet (6.7 km) keeps 2 km.
pub const DISTINCT_FRACTION: f64 = 0.08;

/// Ground distance (metres) under which two placements of a map of
/// `img_w` × `img_h` px at `scale_m_per_px` are the same location.
pub fn distinct_m(scale_m_per_px: f64, img_w: usize, img_h: usize) -> f64 {
    let side = img_w.min(img_h) as f64 * scale_m_per_px;
    if side.is_finite() {
        DISTINCT_M.max(DISTINCT_FRACTION * side)
    } else {
        DISTINCT_M
    }
}
/// Bytes of whole-frame NCC surfaces in flight at once (`coarse_search`):
/// two per template pair, f32 per placement.
const SURFACE_BUDGET_BYTES: usize = 128 << 20;
/// Peaks kept per (template, rotation) after non-maximum suppression.
const PEAKS_PER_TEMPLATE: usize = 6;
/// Placements whose template window has less valid DEM than this are skipped.
const MIN_COVERAGE: f64 = 0.5;
/// Energy floor per template cell in the NCC denominator (|D|² units), so
/// flat or missing terrain cannot produce high scores from noise.
const ENERGY_FLOOR: f64 = 0.02;
/// Largest half-size (ground metres) of the coarse DEM box.
pub const MAX_BOX_HALF_M: f64 = 60_000.0;
/// Floor of the runner-up significance in `peak_ratio` (a search area with
/// no structure elsewhere must not give an unbounded ratio).
const MIN_RATIO_DENOM: f32 = 2.0;
/// Fine stage of a sweep: search radius (ground metres) around each hypothesis.
const SWEEP_LOCAL_RADIUS_M: f64 = 1500.0;
/// Peaks with less correlation than this are not candidates (a map without
/// contours correlates ~0 everywhere; wrong locations still reach ~0.15).
const MIN_CANDIDATE_NCC: f32 = 0.05;
/// Memory allowed for caching template spectra across rotations.
const HAT_CACHE_BYTES: usize = 192 << 20;

fn scale_list(p: &SearchParams, step: f64) -> Vec<f64> {
    let (lo, hi) = (p.scale_min.max(1e-3), p.scale_max.max(p.scale_min.max(1e-3)));
    let n = ((hi / lo).ln() / step.ln() - 1e-9).ceil().max(0.0) as usize + 1;
    if n == 1 {
        return vec![lo];
    }
    (0..n).map(|i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64)).collect()
}

fn rotation_list(p: &SearchParams) -> Vec<f64> {
    if p.rotations_deg.is_empty() {
        vec![0.0]
    } else {
        p.rotations_deg.clone()
    }
}

fn is_zero_rotation(deg: f64) -> bool {
    let r = deg.rem_euclid(360.0);
    r < 1e-9 || (360.0 - r) < 1e-9
}

/// Mercator metres per ground metre at the GPS latitude.
fn merc_per_ground(p: &SearchParams) -> f64 {
    let (lat, _) = merc_to_latlon(p.center_merc.0, p.center_merc.1);
    1.0 / lat.to_radians().cos().max(1e-6)
}

/// Mercator box the coarse DEM must cover for these params and image size.
///
/// GPS ± (search radius + half the map extent at `scale_max`), plus a small
/// margin for gradients, capped at `MAX_BOX_HALF_M` ground metres per side.
/// With a `region` (a wide-search chunk): its box plus the map's extent and
/// the margin, uncapped (the chunk planner keeps chunks small).
pub fn required_dem_box(params: &SearchParams, img_w: usize, img_h: usize) -> MercBox {
    required_dem_box_capped(params, img_w, img_h, MAX_BOX_HALF_M)
}

/// `required_dem_box` with another cap on the half-size (ground metres):
/// a search on coarser tiles (a scale band of small-scale maps) affords a
/// proportionally larger box for the same number of cells.
pub fn required_dem_box_capped(params: &SearchParams, img_w: usize, img_h: usize, max_half_m: f64) -> MercBox {
    let k = merc_per_ground(params);
    let (hx, hy) = map_half_extent(params, img_w, img_h);
    let margin = 500.0 * k;
    if let Some(region) = &params.region {
        let b = region.bounds;
        return MercBox {
            min_x: b.min_x - hx - margin,
            min_y: b.min_y - hy - margin,
            max_x: b.max_x + hx + margin,
            max_y: b.max_y + hy + margin,
        };
    }
    let r = params.search_radius_m.max(0.0) * k;
    let cap = max_half_m * k;
    let (cx, cy) = params.center_merc;
    let half_x = (r + hx + margin).min(cap);
    let half_y = (r + hy + margin).min(cap);
    MercBox { min_x: cx - half_x, min_y: cy - half_y, max_x: cx + half_x, max_y: cy + half_y }
}

/// Half extents (Mercator metres, x and y) of the map at `scale_max` (the
/// rotated map's half diagonal when rotations are searched), the margin a
/// coarse DEM needs around the placements.
pub fn map_half_extent(params: &SearchParams, img_w: usize, img_h: usize) -> (f64, f64) {
    let k = merc_per_ground(params);
    let s = params.scale_max.max(params.scale_min) * k;
    if rotation_list(params).iter().all(|&r| is_zero_rotation(r)) {
        (img_w as f64 * s / 2.0, img_h as f64 * s / 2.0)
    } else {
        let d = ((img_w * img_w + img_h * img_h) as f64).sqrt() * s / 2.0;
        (d, d)
    }
}

/// Smallest ground scale (m/px) searchable on a DEM of `dem_res` Mercator
/// metres per cell for an image of this size (template side ≥ `MIN_SIDE`).
pub fn min_searchable_scale(img_w: usize, img_h: usize, dem_res: f64, lat_deg: f64) -> f64 {
    MIN_SIDE * dem_res * lat_deg.to_radians().cos() / img_w.min(img_h).max(1) as f64
}

/// Map-field pyramid (level q = 2^q field cells per cell), zero outside.
struct FieldPyramid {
    levels: Vec<(usize, usize, Vec<f32>, Vec<f32>)>,
}

impl FieldPyramid {
    fn new(f: &MapField) -> FieldPyramid {
        let mut levels = vec![(f.w, f.h, f.re.clone(), f.im.clone())];
        loop {
            let (w, h, re, im) = levels.last().unwrap();
            if *w < 64 || *h < 64 {
                break;
            }
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let half = |src: &[f32]| -> Vec<f32> {
                let mut out = vec![0.0f32; nw * nh];
                out.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
                    for (x, o) in row.iter_mut().enumerate() {
                        let mut s = 0.0;
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let (xx, yy) = (2 * x + dx, 2 * y + dy);
                            if xx < *w && yy < *h {
                                s += src[yy * w + xx];
                            }
                        }
                        *o = 0.25 * s;
                    }
                });
                out
            };
            let next = (nw, nh, half(re), half(im));
            levels.push(next);
        }
        FieldPyramid { levels }
    }
}

/// 1-D area-resampling weights: output cell i covers source [i/k, (i+1)/k).
fn area_weights(src_len: usize, k: f64) -> (usize, Vec<(usize, Vec<f32>)>) {
    let n = ((src_len as f64 * k).ceil() as usize).max(1);
    let w = (0..n)
        .map(|i| {
            let a = i as f64 / k;
            let b = (i + 1) as f64 / k;
            let s0 = a.floor() as usize;
            let s1 = (b.ceil() as usize).min(src_len);
            let ws = (s0..s1).map(|s| (((b.min(s as f64 + 1.0) - a.max(s as f64)).max(0.0)) * k) as f32).collect();
            (s0, ws)
        })
        .collect();
    (n, w)
}

/// Separable area resample of (re, im) by factor k (< 1 shrinks); outside is 0.
fn area_resample(re: &[f32], im: &[f32], w: usize, h: usize, k: f64) -> (usize, usize, Vec<f32>, Vec<f32>) {
    let (nw, wx) = area_weights(w, k);
    let (nh, wy) = area_weights(h, k);
    // Horizontal.
    let mut tr = vec![0.0f32; nw * h];
    let mut ti = vec![0.0f32; nw * h];
    tr.par_chunks_mut(nw).zip(ti.par_chunks_mut(nw)).enumerate().for_each(|(y, (rr, ri))| {
        let (sr, si) = (&re[y * w..(y + 1) * w], &im[y * w..(y + 1) * w]);
        for (o, (s0, ws)) in wx.iter().enumerate() {
            let (mut a, mut b) = (0.0f32, 0.0f32);
            for (j, wt) in ws.iter().enumerate() {
                a += wt * sr[s0 + j];
                b += wt * si[s0 + j];
            }
            rr[o] = a;
            ri[o] = b;
        }
    });
    // Vertical.
    let mut or = vec![0.0f32; nw * nh];
    let mut oi = vec![0.0f32; nw * nh];
    or.par_chunks_mut(nw).zip(oi.par_chunks_mut(nw)).enumerate().for_each(|(o, (rr, ri))| {
        let (s0, ws) = &wy[o];
        for (j, wt) in ws.iter().enumerate() {
            let y = s0 + j;
            let (sr, si) = (&tr[y * nw..(y + 1) * nw], &ti[y * nw..(y + 1) * nw]);
            for (((a, b), c), d) in rr.iter_mut().zip(ri.iter_mut()).zip(sr).zip(si) {
                *a += wt * c;
                *b += wt * d;
            }
        }
    });
    (nw, nh, or, oi)
}

/// Normalise a complex field to unit-ish magnitude: z / max(|z|, p90(|z|)).
fn normalise(re: &mut [f32], im: &mut [f32], valid: Option<&[bool]>) {
    let mags: Vec<f32> = re
        .iter()
        .zip(im.iter())
        .enumerate()
        .filter(|(i, _)| valid.is_none_or(|v| v[*i]))
        .map(|(_, (a, b))| (a * a + b * b).sqrt())
        .filter(|&m| m > 0.0 && m.is_finite())
        .collect();
    if mags.is_empty() {
        return;
    }
    let g = GrayF32 { w: mags.len(), h: 1, data: mags };
    let p90 = g.percentile(90.0, true).max(1e-12);
    re.par_iter_mut().zip(im.par_iter_mut()).for_each(|(a, b)| {
        let m = (*a * *a + *b * *b).sqrt();
        let d = m.max(p90);
        if d.is_finite() && d > 0.0 {
            *a /= d;
            *b /= d;
        } else {
            *a = 0.0;
            *b = 0.0;
        }
    });
}

/// A built, normalised, north-up template (map frame).
struct Template {
    w: usize,
    h: usize,
    data: Vec<C32>,
    /// Template coordinates of the map centre.
    cx: f64,
    cy: f64,
    /// Exact Mercator metres per pixel.
    s_merc: f64,
    norm: f64,
}

fn build_template(pyr: &FieldPyramid, ppc: f64, img_w: usize, img_h: usize, s_merc: f64, res: f64) -> Option<Template> {
    // Template cells per field cell.
    let k = s_merc * ppc / res;
    if !(k.is_finite() && k > 0.0) {
        return None;
    }
    // Resample from the pyramid level whose cells are still ≤ half a template cell.
    let mut q = 0usize;
    while q + 1 < pyr.levels.len() && k * (1u64 << (q + 1)) as f64 <= 0.5 {
        q += 1;
    }
    let kq = k * (1u64 << q) as f64;
    let (fw, fh, re, im) = &pyr.levels[q];
    let (tw, th, tr, ti) = area_resample(re, im, *fw, *fh, kq);
    let mut tr = GrayF32 { w: tw, h: th, data: tr }.gaussian_blur(TEMPLATE_SIGMA).data;
    let mut ti = GrayF32 { w: tw, h: th, data: ti }.gaussian_blur(TEMPLATE_SIGMA).data;
    normalise(&mut tr, &mut ti, None);
    let data: Vec<C32> = tr.iter().zip(&ti).map(|(&a, &b)| C32::new(a, b)).collect();
    let norm = data.iter().map(|z| z.norm_sqr() as f64).sum::<f64>().sqrt();
    Some(Template {
        w: tw,
        h: th,
        data,
        cx: k * img_w as f64 / (2.0 * ppc),
        cy: k * img_h as f64 / (2.0 * ppc),
        s_merc: res * k / ppc,
        norm,
    })
}

/// Normalised DEM orientation field over a window of a pyramid level.
struct GridField {
    /// Window origin in level cells (may be outside the mosaic).
    x0: i64,
    y0: i64,
    w: usize,
    h: usize,
    d: Vec<C32>,
    valid: Vec<bool>,
}

impl GridField {
    /// Bilinear sample at window index coordinates (cell centres at integers);
    /// None unless all four cells are valid.
    #[inline]
    fn sample(&self, x: f64, y: f64) -> Option<C32> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        if x0 + 1 >= self.w || y0 + 1 >= self.h {
            return None;
        }
        let i = y0 * self.w + x0;
        let j = i + self.w;
        if !(self.valid[i] && self.valid[i + 1] && self.valid[j] && self.valid[j + 1]) {
            return None;
        }
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        Some(
            self.d[i] * ((1.0 - fx) * (1.0 - fy))
                + self.d[i + 1] * (fx * (1.0 - fy))
                + self.d[j] * ((1.0 - fx) * fy)
                + self.d[j + 1] * (fx * fy),
        )
    }
}

/// Elevation grid of a pyramid level cropped to [x0, x0+w) × [y0, y0+h) (level cells); NaN outside.
fn crop(dem: &DemMosaic, x0: i64, y0: i64, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![f32::NAN; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(r, row)| {
        let yy = y0 + r as i64;
        if yy < 0 || yy >= dem.h() as i64 {
            return;
        }
        for (c, o) in row.iter_mut().enumerate() {
            let xx = x0 + c as i64;
            if xx >= 0 && xx < dem.w() as i64 {
                *o = dem.grid.get(xx as usize, yy as usize);
            }
        }
    });
    out
}

/// DEM orientation field (gradient doubled angle, smoothed, normalised) over
/// the window [x0, x0+w) × [y0, y0+h) of a level.
fn grid_field(dem: &DemMosaic, x0: i64, y0: i64, w: usize, h: usize) -> GridField {
    let m = 2 + (3.0 * DEM_SIGMA).ceil() as usize;
    let (cw, ch) = (w + 2 * m, h + 2 * m);
    let e = crop(dem, x0 - m as i64, y0 - m as i64, cw, ch);
    let mut re = vec![0.0f32; cw * ch];
    let mut im = vec![0.0f32; cw * ch];
    let mut valid = vec![false; cw * ch];
    re.par_chunks_mut(cw).zip(im.par_chunks_mut(cw)).zip(valid.par_chunks_mut(cw)).enumerate().for_each(
        |(y, ((rr, ri), rv))| {
            if y == 0 || y + 1 >= ch {
                return;
            }
            for x in 1..cw - 1 {
                let c = e[y * cw + x];
                let (l, r) = (e[y * cw + x - 1], e[y * cw + x + 1]);
                let (u, d) = (e[(y - 1) * cw + x], e[(y + 1) * cw + x]);
                if !(c.is_finite() && l.is_finite() && r.is_finite() && u.is_finite() && d.is_finite()) {
                    continue;
                }
                let gx = 0.5 * (r - l);
                let gy = 0.5 * (d - u);
                rr[x] = gx * gx - gy * gy;
                ri[x] = 2.0 * gx * gy;
                rv[x] = true;
            }
        },
    );
    let br = GrayF32 { w: cw, h: ch, data: re }.gaussian_blur(DEM_SIGMA);
    let bi = GrayF32 { w: cw, h: ch, data: im }.gaussian_blur(DEM_SIGMA);
    let mut dre = vec![0.0f32; w * h];
    let mut dim = vec![0.0f32; w * h];
    let mut dv = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let s = (y + m) * cw + x + m;
            if valid[s] {
                dre[y * w + x] = br.data[s];
                dim[y * w + x] = bi.data[s];
                dv[y * w + x] = true;
            }
        }
    }
    normalise(&mut dre, &mut dim, Some(&dv));
    let d = dre.iter().zip(&dim).map(|(&a, &b)| C32::new(a, b)).collect();
    GridField { x0, y0, w, h, d, valid: dv }
}

/// One FFT tile of a frame: a sub-rectangle and its FFT plan.
struct Tile {
    /// Offset in the frame (cells).
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    fft: Fft2,
    /// Index of the tile's FFT size among the layout's distinct sizes
    /// (tiles of one size share a template's spectrum).
    size: usize,
}

/// FFT tiling of a level's frames (identical for every rotation): one tile
/// if it fits, else overlapping tiles so that every placement of the largest
/// template lies wholly inside some tile.
fn tile_layout(fw: usize, fh: usize, max_t: (usize, usize)) -> Vec<Tile> {
    tile_layout_capped(fw, fh, max_t, FFT_CAP)
}

fn tile_layout_capped(fw: usize, fh: usize, max_t: (usize, usize), cap: usize) -> Vec<Tile> {
    let axis = |n: usize, t: usize| -> Vec<(usize, usize)> {
        if next_fast_len(n) <= cap || t >= cap {
            return vec![(0, n)];
        }
        let stride = cap + 1 - t;
        let mut v = Vec::new();
        let mut s = 0;
        loop {
            let len = cap.min(n - s);
            v.push((s, len));
            if s + len >= n {
                break;
            }
            s += stride;
        }
        v
    };
    let mut tiles = Vec::new();
    let mut sizes: Vec<(usize, usize)> = Vec::new();
    for &(y0, h) in &axis(fh, max_t.1) {
        for &(x0, w) in &axis(fw, max_t.0) {
            let (nw, nh) = (next_fast_len(w), next_fast_len(h));
            let size = sizes.iter().position(|&z| z == (nw, nh)).unwrap_or_else(|| {
                sizes.push((nw, nh));
                sizes.len() - 1
            });
            tiles.push(Tile { x0, y0, w, h, fft: Fft2::new(nw, nh), size });
        }
    }
    tiles
}

/// Number of distinct FFT sizes of a layout.
fn n_sizes(layout: &[Tile]) -> usize {
    layout.iter().map(|t| t.size + 1).max().unwrap_or(0)
}

/// Column-major spectrum of a template zero-padded to `tile`'s FFT size
/// (empty when the template does not fit the tile).
fn template_hat(t: &Template, tile: &Tile) -> Vec<C32> {
    let mut out = Vec::new();
    if t.w <= tile.fft.w && t.h <= tile.fft.h {
        tile.fft.forward_padded_to_cols(&t.data, t.w, t.h, &mut out);
    }
    out
}

/// A template's spectra on each distinct FFT size of a layout.
fn template_hats(t: &Template, layout: &[Tile]) -> Vec<Vec<C32>> {
    (0..n_sizes(layout))
        .map(|s| layout.iter().find(|tile| tile.size == s).map_or_else(Vec::new, |tile| template_hat(t, tile)))
        .collect()
}

/// The DEM field resampled into the map's frame for one rotation: frame cell
/// (u, v) ↔ level edge coordinates G = gc + R(θ)·((u, v) + ½ − fc).
struct Frame {
    w: usize,
    h: usize,
    theta: f64,
    /// GPS position in level edge coordinates, and its frame coordinates.
    gc: (f64, f64),
    fc: (f64, f64),
    /// Integral images ((w+1)×(h+1)) of |D|² and validity.
    e_int: Vec<f64>,
    v_int: Vec<f64>,
    /// Column-major FFT of the field in each tile of the level's layout.
    d_hats: Vec<Vec<C32>>,
    /// With a search region: may a map centre lie in frame cell (u, v)?
    allowed: Option<Vec<bool>>,
}

/// A search region in a pyramid level's coordinates: the region, and the
/// level's Mercator origin and resolution.
#[derive(Clone, Copy)]
struct LevelRegion<'a> {
    region: &'a Region,
    ox: f64,
    oy: f64,
    res: f64,
}

impl Frame {
    #[cfg(test)]
    fn box_sum(&self, int: &[f64], x: usize, y: usize, w: usize, h: usize) -> f64 {
        let s = self.w + 1;
        int[(y + h) * s + x + w] - int[y * s + x + w] - int[(y + h) * s + x] + int[y * s + x]
    }

    /// Frame coordinates → level edge coordinates.
    fn to_level(&self, u: f64, v: f64) -> (f64, f64) {
        let (s, c) = self.theta.sin_cos();
        let (du, dv) = (u - self.fc.0, v - self.fc.1);
        (self.gc.0 + c * du - s * dv, self.gc.1 + s * du + c * dv)
    }
}

/// Integral image ((w+1)×(h+1)) of `val(i)` over a row-major w×h grid:
/// row prefix sums in parallel, then each row added to the one above it in
/// parallel pieces (the same additions as a running sum).
fn integral(w: usize, h: usize, val: impl Fn(usize) -> f64 + Sync) -> Vec<f64> {
    let s = w + 1;
    let mut out = vec![0.0f64; s * (h + 1)];
    out[s..].par_chunks_mut(s).enumerate().for_each(|(y, row)| {
        let mut run = 0.0;
        for x in 0..w {
            run += val(y * w + x);
            row[x + 1] = run;
        }
    });
    for y in 2..=h {
        let (above, rest) = out[(y - 1) * s..].split_at_mut(s);
        rest[..s].par_chunks_mut(1024).zip(above.par_chunks(1024)).for_each(|(r, a)| {
            for (v, p) in r.iter_mut().zip(a) {
                *v += *p;
            }
        });
    }
    out
}

/// Resample the grid field into the frame of rotation θ (size fw × fh) and
/// transform it on the level's tile layout; with a region, mark the frame
/// cells a map centre may lie in.
fn build_frame(
    g: &GridField,
    gc: (f64, f64),
    theta: f64,
    fw: usize,
    fh: usize,
    layout: &[Tile],
    region: Option<LevelRegion>,
) -> Frame {
    let zero = is_zero_rotation(theta.to_degrees());
    // North-up frames are an exact integer-offset crop of the grid field.
    let fc = if zero {
        (gc.0 - (gc.0 - fw as f64 / 2.0).floor(), gc.1 - (gc.1 - fh as f64 / 2.0).floor())
    } else {
        (fw as f64 / 2.0, fh as f64 / 2.0)
    };
    let (s, c) = theta.sin_cos();
    // Doubled angles turn by −2θ from the level grid into the map frame.
    let rot = C32::new((2.0 * theta).cos() as f32, -(2.0 * theta).sin() as f32);
    let mut d = vec![C32::new(0.0, 0.0); fw * fh];
    let mut valid = vec![false; fw * fh];
    d.par_chunks_mut(fw).zip(valid.par_chunks_mut(fw)).enumerate().for_each(|(v, (rd, rv))| {
        for u in 0..fw {
            let (du, dv) = (u as f64 + 0.5 - fc.0, v as f64 + 0.5 - fc.1);
            let (gx, gy) = (gc.0 + c * du - s * dv, gc.1 + s * du + c * dv);
            // Window index coordinates (cell centres at integers).
            let (wx, wy) = (gx - 0.5 - g.x0 as f64, gy - 0.5 - g.y0 as f64);
            let val = if zero {
                let (ix, iy) = (wx.round() as i64, wy.round() as i64);
                if ix >= 0 && iy >= 0 && (ix as usize) < g.w && (iy as usize) < g.h {
                    let i = iy as usize * g.w + ix as usize;
                    g.valid[i].then_some(g.d[i])
                } else {
                    None
                }
            } else {
                g.sample(wx, wy).map(|z| z * rot)
            };
            if let Some(z) = val {
                rd[u] = z;
                rv[u] = true;
            }
        }
    });
    let e_int = integral(fw, fh, |i| d[i].norm_sqr() as f64);
    let v_int = integral(fw, fh, |i| valid[i] as u8 as f64);
    let d_hats = layout
        .iter()
        .map(|tile| {
            let (nw, nh) = (tile.fft.w, tile.fft.h);
            let mut buf = vec![C32::new(0.0, 0.0); nw * nh];
            for y in 0..tile.h {
                let src = (tile.y0 + y) * fw + tile.x0;
                buf[y * nw..y * nw + tile.w].copy_from_slice(&d[src..src + tile.w]);
            }
            tile.fft.forward_to_cols(&buf)
        })
        .collect();
    let allowed = region.map(|lr| {
        let box_only = lr.region.box_inside_annulus();
        let mut m = vec![false; fw * fh];
        m.par_chunks_mut(fw).enumerate().for_each(|(v, row)| {
            for (u, a) in row.iter_mut().enumerate() {
                let (du, dv) = (u as f64 + 0.5 - fc.0, v as f64 + 0.5 - fc.1);
                let (lx, ly) = (gc.0 + c * du - s * dv, gc.1 + s * du + c * dv);
                let (mx, my) = (lr.ox + lx * lr.res, lr.oy - ly * lr.res);
                *a = if box_only { in_box(&lr.region.bounds, mx, my) } else { lr.region.contains(mx, my) };
            }
        });
        m
    });
    Frame { w: fw, h: fh, theta, gc, fc, e_int, v_int, d_hats, allowed }
}

/// A correlation peak (map centre position in Mercator), for diagnostics.
#[derive(Debug, Clone)]
pub struct Peak {
    /// Normalised correlation.
    pub score: f32,
    /// Mean and standard deviation of the template's whole NCC surface.
    pub surface_mean: f32,
    pub surface_std: f32,
    /// Map-centre Mercator position.
    pub mx: f64,
    pub my: f64,
    /// Mercator metres per pixel.
    pub s_merc: f64,
    pub rot_deg: f64,
    /// DEM pyramid level and template size (cells).
    pub level: usize,
    pub tw: usize,
    pub th: usize,
    scale_idx: usize,
    rot_idx: usize,
    scale_step: f64,
}

impl Peak {
    /// Peak height in standard deviations of its template's NCC surface.
    pub fn z(&self) -> f32 {
        (self.score - self.surface_mean) / self.surface_std.max(1e-6)
    }
}

/// Correlate one or two templates over a frame; returns each one's NCC
/// surface over all placements (fw−tw+1)×(fh−th+1), −∞ where invalid or
/// outside the disc of `radius` frame cells around the GPS position (or
/// outside the frame's region). `hats` are the templates' cached spectra
/// per FFT size (`template_hats`; computed here when None).
///
/// Only the real part of a correlation is wanted, so two templates share
/// one inverse transform: with X = D̂·conj(T̂), the Hermitian part
/// H(X)[k] = (X[k] + conj(X[−k]))/2 transforms back to Re(x), and
/// IFFT(H(X₁) + i·H(X₂)) = Re(x₁) + i·Re(x₂).
fn correlate_set(
    ts: &[&Template],
    hats: Option<&[&[Vec<C32>]]>,
    f: &Frame,
    layout: &[Tile],
    radius: f64,
) -> Vec<(usize, usize, Vec<f32>)> {
    debug_assert!(!ts.is_empty() && ts.len() <= 2);
    let mut surfaces: Vec<(usize, usize, Vec<f32>)> = ts
        .iter()
        .map(|t| {
            if t.w > f.w || t.h > f.h {
                (0, 0, Vec::new())
            } else {
                let (pw, ph) = (f.w - t.w + 1, f.h - t.h + 1);
                (pw, ph, vec![f32::NEG_INFINITY; pw * ph])
            }
        })
        .collect();
    // Tiles of one FFT size in a row, so that spectra computed here serve them all.
    let mut order: Vec<usize> = (0..layout.len()).collect();
    order.sort_by_key(|&k| layout[k].size);
    let mut own: Vec<(usize, Vec<C32>)> = Vec::new();
    let (mut y, mut out) = (Vec::new(), Vec::new());
    for k in order {
        let tile = &layout[k];
        let fits: Vec<usize> =
            (0..ts.len()).filter(|&i| surfaces[i].0 > 0 && ts[i].w <= tile.w && ts[i].h <= tile.h).collect();
        if fits.is_empty() {
            continue;
        }
        if hats.is_none() && own.first().is_none_or(|o| o.0 != tile.size) {
            own = ts.iter().map(|t| (tile.size, template_hat(t, tile))).collect();
        }
        let hat = |i: usize| -> &[C32] {
            match hats {
                Some(h) => &h[i][tile.size],
                None => &own[i].1,
            }
        };
        let d = &f.d_hats[k];
        let (nw, nh) = (tile.fft.w, tile.fft.h);
        if y.len() != nw * nh {
            // Every element is written below.
            y.clear();
            y.resize(nw * nh, C32::new(0.0, 0.0));
        }
        if let [a, b] = fits[..] {
            let (ha, hb) = (hat(a), hat(b));
            y.par_chunks_mut(nh).enumerate().for_each(|(x, col)| {
                let xn = (nw - x) % nw;
                let (dc, dn) = (&d[x * nh..(x + 1) * nh], &d[xn * nh..(xn + 1) * nh]);
                let (ac, an) = (&ha[x * nh..(x + 1) * nh], &ha[xn * nh..(xn + 1) * nh]);
                let (bc, bn) = (&hb[x * nh..(x + 1) * nh], &hb[xn * nh..(xn + 1) * nh]);
                // (H(X_a) + i·H(X_b))[k] from k and −k (y index 0 is its own mirror).
                let comb = |dk: C32, dm: C32, ak: C32, am: C32, bk: C32, bm: C32| -> C32 {
                    let dm = dm.conj();
                    let xa = dk * ak.conj() + dm * am;
                    let xb = dk * bk.conj() + dm * bm;
                    C32::new(0.5 * (xa.re - xb.im), 0.5 * (xa.im + xb.re))
                };
                col[0] = comb(dc[0], dn[0], ac[0], an[0], bc[0], bn[0]);
                let mirror = dn[1..].iter().rev().zip(an[1..].iter().rev()).zip(bn[1..].iter().rev());
                for ((((o, &dk), &ak), &bk), ((&dm, &am), &bm)) in
                    col[1..].iter_mut().zip(&dc[1..]).zip(&ac[1..]).zip(&bc[1..]).zip(mirror)
                {
                    *o = comb(dk, dm, ak, am, bk, bm);
                }
            });
        } else {
            let ha = hat(fits[0]);
            y.par_iter_mut().zip(d.par_iter().zip(ha.par_iter())).for_each(|(o, (dk, hk))| *o = dk * hk.conj());
        }
        let rows = fits.iter().map(|&i| tile.h - ts[i].h + 1).max().unwrap_or(0);
        tile.fft.inverse_cols_to_rows(&mut y, rows, &mut out);
        let scale = 1.0 / (nw * nh) as f64;
        for (j, &i) in fits.iter().enumerate() {
            let t = ts[i];
            let imag = j == 1;
            let (pw, _, ncc) = &mut surfaces[i];
            scan_tile(t, f, tile, &out, nw, imag, scale, radius, *pw, ncc);
        }
    }
    surfaces
}

/// Write the NCC of template `t` at its placements in `tile` into `ncc`
/// (keeping the larger value where tiles overlap) from the tile's inverse
/// transform `out` (row-major, `nw` columns; the real or imaginary part,
/// times `scale`).
#[allow(clippy::too_many_arguments)]
fn scan_tile(
    t: &Template,
    f: &Frame,
    tile: &Tile,
    out: &[C32],
    nw: usize,
    imag: bool,
    scale: f64,
    radius: f64,
    pw: usize,
    ncc: &mut [f32],
) {
    let (tpw, tph) = (tile.w - t.w + 1, tile.h - t.h + 1);
    let r2 = radius * radius;
    let n_cells = (t.w * t.h) as f64;
    let floor = ENERGY_FLOOR * n_cells;
    let norm = t.norm.max(1e-12);
    let rows = &mut ncc[tile.y0 * pw..(tile.y0 + tph) * pw];
    let s = f.w + 1;
    let min_cover = MIN_COVERAGE * n_cells;
    rows.par_chunks_mut(pw).enumerate().for_each(|(ly, row)| {
        let iy = tile.y0 + ly;
        let dy = iy as f64 + t.cy - f.fc.1;
        let my = iy as f64 + t.cy;
        let (vt, vb) = (&f.v_int[iy * s..(iy + 1) * s], &f.v_int[(iy + t.h) * s..(iy + t.h + 1) * s]);
        let (et, eb) = (&f.e_int[iy * s..(iy + 1) * s], &f.e_int[(iy + t.h) * s..(iy + t.h + 1) * s]);
        let src = &out[ly * nw..ly * nw + tpw];
        let mut put = |lx: usize| {
            let ix = tile.x0 + lx;
            // `Frame::box_sum`, from the two rows of the integral images.
            let cover = vb[ix + t.w] - vt[ix + t.w] - vb[ix] + vt[ix];
            if cover < min_cover {
                return;
            }
            let energy = eb[ix + t.w] - et[ix + t.w] - eb[ix] + et[ix];
            let z = src[lx];
            let c = if imag { z.im } else { z.re } as f64 * scale;
            let v = (c / (norm * (energy.max(0.0) + floor).sqrt())) as f32;
            let o = &mut row[ix];
            if v > *o {
                *o = v;
            }
        };
        match &f.allowed {
            Some(a) => {
                for lx in 0..tpw {
                    // The frame cell holding the map centre.
                    let (u, v) = (((tile.x0 + lx) as f64 + t.cx).floor(), my.floor());
                    if u < 0.0 || v < 0.0 || u >= f.w as f64 || v >= f.h as f64 {
                        continue;
                    }
                    if a[v as usize * f.w + u as usize] {
                        put(lx);
                    }
                }
            }
            None => {
                // Placements whose map centre lies in the disc: an interval of
                // the row (estimated, then trimmed by the exact test).
                let rem = r2 - dy * dy;
                if rem < 0.0 {
                    return;
                }
                let sq = rem.sqrt();
                let c = f.fc.0 - t.cx - tile.x0 as f64;
                let inside = |lx: usize| {
                    let dx = (tile.x0 + lx) as f64 + t.cx - f.fc.0;
                    dx * dx + dy * dy <= r2
                };
                let mut lo = ((c - sq).floor() - 1.0).clamp(0.0, tpw as f64) as usize;
                let mut hi = ((c + sq).ceil() + 2.0).clamp(0.0, tpw as f64) as usize;
                while lo < hi && !inside(lo) {
                    lo += 1;
                }
                while hi > lo && !inside(hi - 1) {
                    hi -= 1;
                }
                for lx in lo..hi {
                    put(lx);
                }
            }
        }
    });
}

/// A surface's mean and standard deviation, and its peaks (value, x, y).
type SurfacePeaks = ((f32, f32), Vec<(f32, f64, f64)>);
/// A block's maximum (value, index) and its finite values' count, sum and
/// sum of squares (`find_peaks`).
type BlockMax = (f32, usize);
type BlockSums = (usize, f64, f64);

/// Non-maximum suppression: up to `n` peaks at least `rad` cells apart, with
/// sub-cell (parabolic) positions: the greatest value (the first in row-major
/// order among equal ones), then the greatest beyond `rad` of it, and so on.
/// Also returns the mean and standard deviation of the finite values (the
/// spread of the template's surface, against which a peak's height counts:
/// `Peak::z`). The surface is scanned once, in blocks: each block
/// keeps its maximum, and only blocks near a chosen peak are scanned again.
fn find_peaks(ncc: &[f32], pw: usize, ph: usize, n: usize, rad: f64) -> SurfacePeaks {
    const B: usize = 32;
    let (bw, bh) = (pw.div_ceil(B), ph.div_ceil(B));
    let r = rad.ceil() as isize;
    let near = |chosen: &[(isize, isize)], x: isize, y: isize| {
        chosen.iter().any(|&(cx, cy)| {
            let (dx, dy) = ((x - cx) as f64, (y - cy) as f64);
            dx * dx + dy * dy <= rad * rad
        })
    };
    // Block maximum (value, index; the first in row-major order) of cells
    // not within `rad` of a chosen peak, and the block's finite sums.
    let scan = |bi: usize, chosen: &[(isize, isize)]| -> (BlockMax, BlockSums) {
        let (bx, by) = (bi % bw, bi / bw);
        let (x0, y0) = (bx * B, by * B);
        let (mut bv, mut bidx) = (f32::NEG_INFINITY, usize::MAX);
        let (mut cnt, mut s, mut s2) = (0usize, 0.0f64, 0.0f64);
        for y in y0..(y0 + B).min(ph) {
            for (x, &v) in ncc[y * pw + x0..y * pw + (x0 + B).min(pw)].iter().enumerate().map(|(i, v)| (x0 + i, v)) {
                if v.is_finite() {
                    cnt += 1;
                    s += v as f64;
                    s2 += (v as f64) * (v as f64);
                }
                if v > bv && (chosen.is_empty() || !near(chosen, x as isize, y as isize)) {
                    bv = v;
                    bidx = y * pw + x;
                }
            }
        }
        ((bv, bidx), (cnt, s, s2))
    };
    let first: Vec<(BlockMax, BlockSums)> = (0..bw * bh).into_par_iter().map(|bi| scan(bi, &[])).collect();
    let (cnt, sum, sum2) = first.iter().fold((0usize, 0.0f64, 0.0f64), |a, (_, b)| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
    let stats = if cnt < 2 {
        (0.0, 1.0)
    } else {
        let m = sum / cnt as f64;
        (m as f32, ((sum2 / cnt as f64 - m * m).max(0.0)).sqrt() as f32)
    };
    let mut best: Vec<(f32, usize)> = first.into_iter().map(|b| b.0).collect();
    let mut chosen: Vec<(isize, isize)> = Vec::new();
    let mut out = Vec::new();
    for _ in 0..n {
        let (mut bv, mut bi) = (f32::NEG_INFINITY, usize::MAX);
        for &(v, i) in &best {
            if v > bv || (v == bv && v.is_finite() && i < bi) {
                bv = v;
                bi = i;
            }
        }
        if bi == usize::MAX || !bv.is_finite() {
            break;
        }
        let (x, y) = ((bi % pw) as isize, (bi / pw) as isize);
        let at = |xx: isize, yy: isize| -> Option<f32> {
            if xx < 0 || yy < 0 || xx >= pw as isize || yy >= ph as isize {
                return None;
            }
            let v = ncc[yy as usize * pw + xx as usize];
            v.is_finite().then_some(v)
        };
        let sub = |a: Option<f32>, c: f32, b: Option<f32>| -> f64 {
            match (a, b) {
                (Some(a), Some(b)) => {
                    let den = (a - 2.0 * c + b) as f64;
                    if den < 0.0 {
                        (0.5 * (a - b) as f64 / den).clamp(-0.5, 0.5)
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            }
        };
        let fx = x as f64 + sub(at(x - 1, y), bv, at(x + 1, y));
        let fy = y as f64 + sub(at(x, y - 1), bv, at(x, y + 1));
        out.push((bv, fx, fy));
        chosen.push((x, y));
        // Blocks within reach of the new peak: their maxima again.
        let bx0 = ((x - r).max(0) as usize) / B;
        let bx1 = ((x + r).min(pw as isize - 1) as usize) / B;
        let by0 = ((y - r).max(0) as usize) / B;
        let by1 = ((y + r).min(ph as isize - 1) as usize) / B;
        let redo: Vec<usize> = (by0..=by1).flat_map(|by| (bx0..=bx1).map(move |bx| by * bw + bx)).collect();
        let again: Vec<(f32, usize)> = redo.par_iter().map(|&bi| scan(bi, &chosen).0).collect();
        for (&bi, v) in redo.iter().zip(again) {
            best[bi] = v;
        }
    }
    (stats, out)
}

/// Run the coarse search. `dem` is a coarse mosaic (zoom 11, ~76 m) covering
/// `required_dem_box`. Returns up to `top_k` candidates, best first.
/// `cancel()` is polled between scales; `progress(f)` receives 0..1.
pub fn coarse_search(
    field: &MapField,
    dem: &DemMosaic,
    img_w: usize,
    img_h: usize,
    params: &SearchParams,
    cancel: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Vec<Candidate> {
    let rots = rotation_list(params);
    if rots.len() <= 1 {
        let peaks = search_peaks_with(field, dem, img_w, img_h, params, FULL_PASS, cancel, progress);
        return select_candidates(&peaks, img_w, img_h, params);
    }
    // Rotation sweep, stage 1: every rotation with smaller templates.
    let stage1 = |f: f32| progress(0.8 * f);
    let peaks = search_peaks_with(field, dem, img_w, img_h, params, SWEEP_PASS, cancel, &stage1);
    let mut cands = select_candidates(&peaks, img_w, img_h, params);
    // Stage 2: around each hypothesis, finer rotations and scales at full resolution.
    let mut sorted = rots.iter().map(|r| r.rem_euclid(360.0)).collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let step = sorted.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 1e-6).fold(10.0f64, f64::min);
    let n = cands.len().max(1);
    for (k, c) in cands.iter_mut().enumerate() {
        if cancel() {
            return Vec::new();
        }
        let local = SearchParams {
            center_merc: c.center_merc(img_w, img_h),
            search_radius_m: SWEEP_LOCAL_RADIUS_M,
            scale_min: c.scale_m_per_px / SCALE_STEP.powi(2),
            scale_max: c.scale_m_per_px * SCALE_STEP.powi(2),
            rotations_deg: (-2..=2).map(|i| c.rotation_deg + i as f64 * step / 4.0).collect(),
            top_k: 1,
            region: None,
        };
        let sub = |f: f32| progress(0.8 + 0.2 * (k as f32 + f) / n as f32);
        let lp = search_peaks_with(field, dem, img_w, img_h, &local, FULL_PASS, cancel, &sub);
        if let Some(best) = lp.iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
        {
            let (lat, _) = merc_to_latlon(best.mx, best.my);
            c.transform = Affine::pixel_to_merc_similarity(
                best.s_merc,
                best.rot_deg.to_radians(),
                img_w as f64 / 2.0,
                img_h as f64 / 2.0,
                best.mx,
                best.my,
            );
            c.scale_m_per_px = best.s_merc * lat.to_radians().cos();
            c.rotation_deg = best.rot_deg.rem_euclid(360.0);
            c.score = best.score;
        }
    }
    progress(1.0);
    cands
}

/// All correlation peaks (every scale and rotation), unsorted. Diagnostics;
/// `coarse_search` turns these into candidates.
pub fn search_peaks(
    field: &MapField,
    dem: &DemMosaic,
    img_w: usize,
    img_h: usize,
    params: &SearchParams,
    cancel: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Vec<Peak> {
    search_peaks_with(field, dem, img_w, img_h, params, FULL_PASS, cancel, progress)
}

/// Resolution settings of one search pass.
#[derive(Debug, Clone, Copy)]
struct Pass {
    /// Preferred minimum template side (cells) when picking the DEM level.
    target_side: f64,
    /// Geometric step between scales.
    scale_step: f64,
}

const FULL_PASS: Pass = Pass { target_side: TARGET_SIDE, scale_step: SCALE_STEP };
/// First stage of a rotation sweep: smaller templates, 8 % scale steps.
const SWEEP_PASS: Pass = Pass { target_side: SWEEP_TARGET_SIDE, scale_step: SCALE_STEP * SCALE_STEP };

/// Centre and half extents (Mercator metres) of the area holding the map
/// centres: the search disc, or a chunk's box.
fn placement_area(params: &SearchParams) -> ((f64, f64), f64, f64) {
    match &params.region {
        Some(r) => {
            let b = r.bounds;
            (((b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0), b.width() / 2.0, b.height() / 2.0)
        }
        None => {
            let r = params.search_radius_m.max(0.0) * merc_per_ground(params);
            (params.center_merc, r, r)
        }
    }
}

/// The scales searched, each on its DEM pyramid level (scale index,
/// Mercator m/px, level): the coarsest level keeping ≥ `target_side` cells,
/// or coarser while the search window would need FFT tiling and the
/// template keeps ≥ `TILED_MIN_SIDE` cells. `r0`: the DEM's Mercator metres
/// per cell.
fn scale_plan(params: &SearchParams, img_w: usize, img_h: usize, r0: f64, pass: Pass) -> Vec<(usize, f64, usize)> {
    let kmg = merc_per_ground(params);
    let (_, phx, phy) = placement_area(params);
    let radius_cells0 = phx.max(phy) / r0;
    let mut plan = Vec::new();
    for (si, &s) in scale_list(params, pass.scale_step).iter().enumerate() {
        let s_merc = s * kmg;
        let m0 = img_w.min(img_h) as f64 * s_merc / r0;
        if m0 < MIN_SIDE {
            continue;
        }
        let big0 = img_w.max(img_h) as f64 * s_merc / r0;
        let at = |l: usize| (1u64 << l) as f64;
        let mut level = 0usize;
        while level < 8
            && (m0 / at(level + 1) >= pass.target_side
                || (m0 / at(level + 1) >= TILED_MIN_SIDE
                    && next_fast_len(((2.0 * radius_cells0 + big0) / at(level)).ceil() as usize + 4) > FFT_CAP))
        {
            level += 1;
        }
        plan.push((si, s_merc, level));
    }
    plan
}

/// The work of a north-up coarse search with `params` for an image of
/// `img_w` × `img_h` px on a DEM of `dem_res` Mercator metres per cell, in
/// millions of FFT cells: per pyramid level, the level's FFT tiles once per
/// template and twice for the frame (its spectrum and integral images).
/// The search estimate (`wide::estimate`) turns it into seconds.
pub fn search_work(params: &SearchParams, img_w: usize, img_h: usize, dem_res: f64) -> f64 {
    if img_w == 0 || img_h == 0 || dem_res.is_nan() || dem_res <= 0.0 {
        return 0.0;
    }
    let (_, phx, phy) = placement_area(params);
    let plan = scale_plan(params, img_w, img_h, dem_res, FULL_PASS);
    let mut work = 0.0;
    for lvl in 0..=plan.iter().map(|p| p.2).max().unwrap_or(0) {
        let res = dem_res * (1u64 << lvl) as f64;
        let sizes: Vec<(usize, usize)> = plan
            .iter()
            .filter(|p| p.2 == lvl)
            .map(|p| ((img_w as f64 * p.1 / res).ceil() as usize, (img_h as f64 * p.1 / res).ceil() as usize))
            .collect();
        if sizes.is_empty() {
            continue;
        }
        let max_w = sizes.iter().map(|t| t.0).max().unwrap_or(0);
        let max_h = sizes.iter().map(|t| t.1).max().unwrap_or(0);
        let fw = (2.0 * phx / res).ceil() as usize + max_w + 4;
        let fh = (2.0 * phy / res).ceil() as usize + max_h + 4;
        let cells: usize = tile_layout(fw, fh, (max_w, max_h)).iter().map(|t| t.fft.w * t.fft.h).sum();
        work += (sizes.len() + 2) as f64 * cells as f64 / 1e6;
    }
    work
}

#[allow(clippy::too_many_arguments)]
fn search_peaks_with(
    field: &MapField,
    dem: &DemMosaic,
    img_w: usize,
    img_h: usize,
    params: &SearchParams,
    pass: Pass,
    cancel: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Vec<Peak> {
    if img_w == 0 || img_h == 0 || dem.w() < 4 || dem.h() < 4 || field.w == 0 || field.h == 0 {
        return Vec::new();
    }
    let kmg = merc_per_ground(params);
    let r0 = dem.res;
    let rots = rotation_list(params);
    let any_rot = rots.iter().any(|&r| !is_zero_rotation(r));
    let (centre, phx, phy) = placement_area(params);
    let mut plan = scale_plan(params, img_w, img_h, r0, pass);
    if plan.is_empty() {
        return Vec::new();
    }
    let n_levels = plan.iter().map(|p| p.2).max().unwrap() + 1;
    let mut pyramid = vec![dem.clone()];
    for _ in 1..n_levels {
        let next = pyramid.last().unwrap().downsample2();
        if next.w() < 4 || next.h() < 4 {
            break;
        }
        pyramid.push(next);
    }
    for p in plan.iter_mut() {
        p.2 = p.2.min(pyramid.len() - 1);
    }
    let fpyr = FieldPyramid::new(field);
    let ppc = field.px_per_cell;
    let total = (plan.len() + pyramid.len()) * rots.len();
    let done = AtomicUsize::new(0);
    let tick = || {
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress((d as f32 / total as f32).min(1.0));
    };
    let radius_m = params.search_radius_m.max(0.0) * kmg;
    let mut peaks = Vec::new();
    for (lvl, ldem) in pyramid.iter().enumerate() {
        let lplan: Vec<&(usize, f64, usize)> = plan.iter().filter(|p| p.2 == lvl).collect();
        if lplan.is_empty() {
            (0..rots.len()).for_each(|_| tick());
            continue;
        }
        if cancel() {
            return Vec::new();
        }
        let res = ldem.res;
        let templates: Vec<(usize, Template)> = lplan
            .par_iter()
            .filter_map(|&&(si, s_merc, _)| build_template(&fpyr, ppc, img_w, img_h, s_merc, res).map(|t| (si, t)))
            .collect();
        if templates.is_empty() {
            continue;
        }
        let max_w = templates.iter().map(|t| t.1.w).max().unwrap();
        let max_h = templates.iter().map(|t| t.1.h).max().unwrap();
        let rc = radius_m / res;
        // Frame: every placement whose map centre lies in the search disc
        // (or the chunk's box).
        let fw = (2.0 * phx / res).ceil() as usize + max_w + 4;
        let fh = (2.0 * phy / res).ceil() as usize + max_h + 4;
        let gc = ((centre.0 - ldem.ox) / res, (ldem.oy - centre.1) / res);
        let lregion = params.region.as_ref().map(|region| LevelRegion { region, ox: ldem.ox, oy: ldem.oy, res });
        // Grid window covering every rotated frame (or just the frame).
        let (hx, hy) = if any_rot {
            let d = ((fw * fw + fh * fh) as f64).sqrt() / 2.0 + 3.0;
            (d, d)
        } else {
            (fw as f64 / 2.0 + 3.0, fh as f64 / 2.0 + 3.0)
        };
        let gx0 = (gc.0 - hx).floor() as i64;
        let gy0 = (gc.1 - hy).floor() as i64;
        let gw = (2.0 * hx).ceil() as usize + 2;
        let gh = (2.0 * hy).ceil() as usize + 2;
        // Skip if the window misses the mosaic entirely.
        if gx0 + gw as i64 <= 0 || gy0 + gh as i64 <= 0 || gx0 >= ldem.w() as i64 || gy0 >= ldem.h() as i64 {
            continue;
        }
        let grid = grid_field(ldem, gx0, gy0, gw, gh);
        // Peaks of one template closer than the distinct distance are one place.
        let rad_cells = |s_merc: f64| distinct_m(s_merc / kmg, img_w, img_h) * kmg / res;
        let layout = tile_layout(fw, fh, (max_w, max_h));
        // With several rotations, transform each template once per level
        // (within a memory budget) instead of once per rotation.
        let size_bytes: usize = (0..n_sizes(&layout))
            .filter_map(|s| layout.iter().find(|t| t.size == s))
            .map(|t| t.fft.w * t.fft.h * 8)
            .sum();
        let hat_bytes = size_bytes * templates.len();
        let hats: Option<Vec<Vec<Vec<C32>>>> = (rots.len() > 1 && hat_bytes <= HAT_CACHE_BYTES)
            .then(|| templates.par_iter().map(|(_, t)| template_hats(t, &layout)).collect());
        for (ri, &rot) in rots.iter().enumerate() {
            if cancel() {
                return Vec::new();
            }
            let frame = build_frame(&grid, gc, rot.to_radians(), fw, fh, &layout, lregion);
            tick();
            // Templates in pairs (neighbouring scales: similar sizes) share
            // their inverse transforms (`correlate_set`). Each pair in
            // flight holds two whole-frame surfaces, while the work within a
            // pair runs in parallel too: as many pairs at a time as threads,
            // and their surfaces within `SURFACE_BUDGET_BYTES` (the 50 km
            // disc on zoom 11: ~12 MB a surface at 30°, ~25 MB at 57° N,
            // where a pair per thread had raised the peak by 0.1–0.25 GB).
            let pair_bytes = 2 * 4 * fw * fh;
            let batch = (SURFACE_BUDGET_BYTES / pair_bytes.max(1)).clamp(1, rayon::current_num_threads().max(1));
            let pairs: Vec<(usize, &[(usize, Template)])> = templates.chunks(2).enumerate().collect();
            let fpeaks: Vec<Peak> = pairs
                .chunks(batch)
                .flat_map(|b| {
                    b.par_iter()
                        .flat_map_iter(|&(pi, pair)| {
                            if cancel() {
                                return Vec::new();
                            }
                            let ts: Vec<&Template> = pair.iter().map(|(_, t)| t).collect();
                            let cached: Option<Vec<&[Vec<C32>]>> =
                                hats.as_ref().map(|h| (0..pair.len()).map(|j| h[2 * pi + j].as_slice()).collect());
                            let surfaces = correlate_set(&ts, cached.as_deref(), &frame, &layout, rc);
                            let mut out: Vec<Peak> = Vec::new();
                            for ((si, t), (pw, ph, ncc)) in pair.iter().zip(surfaces) {
                                let ((mu, sd), found) =
                                    find_peaks(&ncc, pw, ph, PEAKS_PER_TEMPLATE, rad_cells(t.s_merc));
                                out.extend(found.into_iter().map(|(score, fx, fy)| {
                                    let (lx, ly) = frame.to_level(fx + t.cx, fy + t.cy);
                                    Peak {
                                        score,
                                        surface_mean: mu,
                                        surface_std: sd,
                                        mx: ldem.ox + lx * res,
                                        my: ldem.oy - ly * res,
                                        s_merc: t.s_merc,
                                        rot_deg: rot,
                                        level: lvl,
                                        tw: t.w,
                                        th: t.h,
                                        scale_idx: *si,
                                        rot_idx: ri,
                                        scale_step: pass.scale_step,
                                    }
                                }));
                                tick();
                            }
                            out
                        })
                        .collect::<Vec<Peak>>()
                })
                .collect();
            peaks.extend(fpeaks);
        }
    }
    if cancel() {
        return Vec::new();
    }
    peaks
}

fn ground_dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat, _) = merc_to_latlon(0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1));
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() * lat.to_radians().cos()
}

/// Distinct candidates (≥ `distinct_m` apart), most significant first, with
/// peak ratios and a parabolic scale refinement from the neighbouring scales.
///
/// Peaks are ranked by significance z = (NCC − mean)/std of their own
/// template's correlation surface rather than by raw NCC: small templates
/// have noisy surfaces whose maxima reach high NCC by chance, and z makes
/// templates of different sizes and DEM levels comparable.
fn select_candidates(peaks: &[Peak], img_w: usize, img_h: usize, params: &SearchParams) -> Vec<Candidate> {
    let mut order: Vec<usize> = (0..peaks.len()).collect();
    order.sort_by(|&a, &b| peaks[b].z().partial_cmp(&peaks[a].z()).unwrap_or(std::cmp::Ordering::Equal));
    // Two peaks are distinct places when apart by the distinct distance of
    // the larger of their maps' extents (`distinct_m`).
    let distinct = |p: &Peak| {
        let (lat, _) = merc_to_latlon(p.mx, p.my);
        distinct_m(p.s_merc * lat.to_radians().cos(), img_w, img_h)
    };
    let apart = |a: &Peak, b: &Peak| ground_dist((a.mx, a.my), (b.mx, b.my)) >= distinct(a).max(distinct(b));
    let mut chosen: Vec<usize> = Vec::new();
    for &i in &order {
        if chosen.len() >= params.top_k.max(1) {
            break;
        }
        if peaks[i].score < MIN_CANDIDATE_NCC {
            continue;
        }
        if chosen.iter().all(|&j| apart(&peaks[i], &peaks[j])) {
            chosen.push(i);
        }
    }
    chosen
        .into_iter()
        .map(|i| {
            let pk = &peaks[i];
            let here = (pk.mx, pk.my);
            let other = order.iter().map(|&j| &peaks[j]).find(|q| apart(pk, q)).map(|q| q.z());
            let peak_ratio = match other {
                Some(o) => pk.z().max(0.0) / o.max(MIN_RATIO_DENOM),
                None => 1.0,
            };
            // Parabolic refinement of log-scale from the same location at the
            // neighbouring scales (same rotation).
            let near = |scale_idx: usize| -> Option<f32> {
                peaks
                    .iter()
                    .filter(|q| {
                        q.scale_idx == scale_idx
                            && q.rot_idx == pk.rot_idx
                            && ground_dist(here, (q.mx, q.my)) < 0.5 * distinct(pk)
                    })
                    .map(|q| q.score)
                    .fold(None, |a: Option<f32>, s| Some(a.map_or(s, |a| a.max(s))))
            };
            let mut s_merc = pk.s_merc;
            if pk.scale_idx > 0 {
                if let (Some(a), Some(b)) = (near(pk.scale_idx - 1), near(pk.scale_idx + 1)) {
                    let c = pk.score;
                    let den = (a - 2.0 * c + b) as f64;
                    if den < 0.0 {
                        let off = (0.5 * (a - b) as f64 / den).clamp(-0.5, 0.5);
                        s_merc *= pk.scale_step.powf(off);
                    }
                }
            }
            let transform = Affine::pixel_to_merc_similarity(
                s_merc,
                pk.rot_deg.to_radians(),
                img_w as f64 / 2.0,
                img_h as f64 / 2.0,
                pk.mx,
                pk.my,
            );
            let (lat, _) = merc_to_latlon(pk.mx, pk.my);
            Candidate {
                transform,
                score: pk.score,
                peak_ratio,
                scale_m_per_px: s_merc * lat.to_radians().cos(),
                rotation_deg: pk.rot_deg.rem_euclid(360.0),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_are_geometric() {
        let p = SearchParams { scale_min: 1.0, scale_max: 2.0, ..Default::default() };
        let s = scale_list(&p, SCALE_STEP);
        assert!((s[0] - 1.0).abs() < 1e-12 && (s[s.len() - 1] - 2.0).abs() < 1e-12);
        for w in s.windows(2) {
            assert!(w[1] / w[0] <= SCALE_STEP + 1e-9 && w[1] / w[0] > 1.03);
        }
        assert_eq!(rotation_sweep(10.0).len(), 36);
        assert_eq!(rotation_sweep(90.0), vec![0.0, 90.0, 180.0, 270.0]);
    }

    #[test]
    fn area_resample_exact_geometry() {
        // A single bright column at x = 50..51 in a 100-wide field lands at
        // template x = 50·k for k = 0.1.
        let (w, h) = (100usize, 40usize);
        let mut re = vec![0.0f32; w * h];
        for y in 0..h {
            re[y * w + 50] = 1.0;
        }
        let im = vec![0.0f32; w * h];
        let (nw, nh, r, _) = area_resample(&re, &im, w, h, 0.1);
        assert_eq!((nw, nh), (10, 4));
        let col: Vec<f32> = (0..nw).map(|x| r[x]).collect();
        assert!((col[5] - 0.1).abs() < 1e-6, "{col:?}");
        assert!(col.iter().enumerate().all(|(i, &v)| i == 5 || v == 0.0));
        // Mass is preserved (scaled by k²).
        let total: f32 = r.iter().sum();
        assert!((total - 40.0 * 0.01).abs() < 1e-4);
    }

    #[test]
    fn dem_box_covers_radius_and_map() {
        let p = SearchParams {
            center_merc: crate::geo::latlon_to_merc(-25.85, 27.4),
            search_radius_m: 20_000.0,
            scale_min: 0.5,
            scale_max: 10.0,
            ..Default::default()
        };
        let b = required_dem_box(&p, 3507, 4960);
        let k = 1.0 / (-25.85f64).to_radians().cos();
        assert!(b.width() / 2.0 >= (20_000.0 + 3507.0 * 5.0) * k);
        assert!(b.height() / 2.0 >= (20_000.0 + 4960.0 * 5.0) * k);
        assert!(b.height() / 2.0 <= MAX_BOX_HALF_M * k + 1.0);
    }

    #[test]
    fn region_box_and_annulus() {
        // A box 150–250 km (Mercator) east of the origin: 136–227 km of ground at 25°S.
        let o = crate::geo::latlon_to_merc(-25.0, 28.0);
        let b = MercBox::around(o.0 + 200_000.0, o.1, 50_000.0);
        let r = Region { bounds: b, origin: o, r_min_m: 50_000.0, r_max_m: 400_000.0, exclude: None };
        assert!(r.box_inside_annulus());
        assert!(r.contains(o.0 + 200_000.0, o.1));
        // Half-open: the far edges belong to the neighbours.
        assert!(r.contains(b.min_x, b.min_y) && !r.contains(b.max_x, o.1) && !r.contains(o.0 + 200_000.0, b.max_y));
        // An outer radius through the box: only the near part is in.
        let r2 = Region { r_max_m: 150_000.0, ..r };
        assert!(!r2.box_inside_annulus());
        assert!(r2.contains(o.0 + 155_000.0, o.1) && !r2.contains(o.0 + 200_000.0, o.1));
        // An inner radius through the box: only the far part is in.
        let r3 = Region { r_min_m: 200_000.0, ..r };
        assert!(!r3.box_inside_annulus());
        assert!(!r3.contains(o.0 + 155_000.0, o.1) && r3.contains(o.0 + 245_000.0, o.1));
        // An excluded band (searched by another chunk first) is left out.
        let band = MercBox { min_x: -1e9, min_y: o.1 - 5_000.0, max_x: 1e9, max_y: o.1 + 5_000.0 };
        let r4 = Region { exclude: Some(band), ..r };
        assert!(!r4.box_inside_annulus());
        assert!(!r4.contains(o.0 + 200_000.0, o.1) && r4.contains(o.0 + 200_000.0, o.1 + 6_000.0));
    }

    #[test]
    fn nearest_point_across_the_antimeridian() {
        let half = std::f64::consts::PI * crate::geo::EARTH_RADIUS;
        // A box just east of -180° and a point just west of +180°.
        let b = MercBox { min_x: -half, min_y: 0.0, max_x: -half + 100_000.0, max_y: 100_000.0 };
        let p = (half - 20_000.0, 50_000.0);
        let n = nearest_in_box(&b, p);
        assert!((n.0 - b.min_x).abs() < 1e-6 && (n.1 - 50_000.0).abs() < 1e-6, "{n:?}");
        // Away from it: a plain clamp.
        let q = (-half + 300_000.0, 150_000.0);
        assert_eq!(nearest_in_box(&b, q), (b.max_x, b.max_y));
    }

    /// The FFT correlation (tiled, two templates per inverse transform, with
    /// and without cached spectra) equals the NCC computed directly.
    #[test]
    fn correlation_matches_direct_ncc() {
        let mut seed = 99u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        let (gw, gh) = (90usize, 70usize);
        let d: Vec<C32> = (0..gw * gh).map(|_| C32::new(rnd(), rnd())).collect();
        let valid: Vec<bool> = (0..gw * gh).map(|i| i % 17 != 3).collect();
        let g = GridField { x0: 0, y0: 0, w: gw, h: gh, d, valid };
        let mk = |w: usize, h: usize, rnd: &mut dyn FnMut() -> f32| {
            let data: Vec<C32> = (0..w * h).map(|_| C32::new(rnd(), rnd())).collect();
            let norm = data.iter().map(|z| z.norm_sqr() as f64).sum::<f64>().sqrt();
            Template { w, h, data, cx: w as f64 / 2.0, cy: h as f64 / 2.0, s_merc: 1.0, norm }
        };
        let ts = [mk(9, 7, &mut rnd), mk(11, 8, &mut rnd), mk(6, 10, &mut rnd)];
        let (fw, fh) = (80usize, 60usize);
        for cap in [128usize, 30] {
            // cap 30: 3 × 3 tiles of up to four FFT sizes.
            let layout = tile_layout_capped(fw, fh, (11, 10), cap);
            assert!(cap > 100 || (layout.len() > 4 && n_sizes(&layout) > 1));
            for theta in [0.0f64, 0.3] {
                let frame = build_frame(&g, (45.0, 35.0), theta, fw, fh, &layout, None);
                let radius = 25.0;
                let hats: Vec<Vec<Vec<C32>>> = ts.iter().map(|t| template_hats(t, &layout)).collect();
                for (set, cached) in [(vec![0usize, 1], false), (vec![2], false), (vec![1, 2], true)] {
                    let tr: Vec<&Template> = set.iter().map(|&i| &ts[i]).collect();
                    let ch: Vec<&[Vec<C32>]> = set.iter().map(|&i| hats[i].as_slice()).collect();
                    let got = correlate_set(&tr, cached.then_some(ch.as_slice()), &frame, &layout, radius);
                    for (t, (pw, ph, ncc)) in tr.iter().zip(&got) {
                        assert_eq!((*pw, *ph), (fw - t.w + 1, fh - t.h + 1));
                        let mut n_valid = 0;
                        for iy in 0..*ph {
                            for ix in 0..*pw {
                                // Direct NCC at this placement (frame field from the
                                // tiles' inverse: rebuild it from the integral images'
                                // source, the frame's own resampling).
                                let dx = ix as f64 + t.cx - frame.fc.0;
                                let dy = iy as f64 + t.cy - frame.fc.1;
                                let n_cells = (t.w * t.h) as f64;
                                let cover = frame.box_sum(&frame.v_int, ix, iy, t.w, t.h);
                                let want = if dx * dx + dy * dy > radius * radius || cover < MIN_COVERAGE * n_cells {
                                    None
                                } else {
                                    let mut c = 0.0f64;
                                    for v in 0..t.h {
                                        for u in 0..t.w {
                                            let z = frame_value(&g, &frame, ix + u, iy + v);
                                            c += (z * t.data[v * t.w + u].conj()).re as f64;
                                        }
                                    }
                                    let e = frame.box_sum(&frame.e_int, ix, iy, t.w, t.h);
                                    Some(c / (t.norm * (e.max(0.0) + ENERGY_FLOOR * n_cells).sqrt()))
                                };
                                let v = ncc[iy * pw + ix];
                                match want {
                                    None => assert!(v == f32::NEG_INFINITY, "{ix},{iy}: {v}"),
                                    Some(w) => {
                                        n_valid += 1;
                                        assert!((v as f64 - w).abs() < 1e-4, "{ix},{iy}: {v} vs {w}");
                                    }
                                }
                            }
                        }
                        assert!(n_valid > 100, "{n_valid}");
                    }
                }
            }
        }
    }

    /// `find_peaks` as it was: a full scan per peak over a suppressed copy.
    fn find_peaks_reference(ncc: &[f32], pw: usize, ph: usize, n: usize, rad: f64) -> Vec<(f32, usize)> {
        let mut work = ncc.to_vec();
        let mut out = Vec::new();
        let r = rad.ceil() as isize;
        for _ in 0..n {
            let (mut bi, mut bv) = (usize::MAX, f32::NEG_INFINITY);
            for (i, &v) in work.iter().enumerate() {
                if v > bv {
                    bv = v;
                    bi = i;
                }
            }
            if bi == usize::MAX || !bv.is_finite() {
                break;
            }
            out.push((bv, bi));
            let (x, y) = ((bi % pw) as isize, (bi / pw) as isize);
            for yy in (y - r).max(0)..=(y + r).min(ph as isize - 1) {
                for xx in (x - r).max(0)..=(x + r).min(pw as isize - 1) {
                    let (ddx, ddy) = ((xx - x) as f64, (yy - y) as f64);
                    if ddx * ddx + ddy * ddy <= rad * rad {
                        work[yy as usize * pw + xx as usize] = f32::NEG_INFINITY;
                    }
                }
            }
        }
        out
    }

    /// Block-wise peak picking finds the peaks a full scan per peak finds,
    /// ties and invalid cells included, and the surface's mean and spread.
    #[test]
    fn peaks_match_full_scans() {
        let mut seed = 7u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f32 / (1u64 << 53) as f32
        };
        for (pw, ph, rad) in [(150usize, 97usize, 9.5f64), (33, 200, 40.0), (64, 64, 1.0), (5, 3, 2.0)] {
            // Coarse values (many ties), some cells invalid.
            let ncc: Vec<f32> = (0..pw * ph)
                .map(|_| {
                    let v = rnd();
                    if v < 0.1 {
                        f32::NEG_INFINITY
                    } else {
                        (v * 20.0).floor() / 20.0
                    }
                })
                .collect();
            let ((mu, sd), got) = find_peaks(&ncc, pw, ph, 6, rad);
            let want = find_peaks_reference(&ncc, pw, ph, 6, rad);
            assert_eq!(got.len(), want.len());
            for (g, w) in got.iter().zip(&want) {
                assert_eq!(g.0, w.0);
                // The same cell (the position is refined by at most half a cell).
                let (x, y) = ((w.1 % pw) as f64, (w.1 / pw) as f64);
                assert!((g.1 - x).abs() <= 0.5 && (g.2 - y).abs() <= 0.5, "{g:?} {w:?}");
            }
            let fin: Vec<f64> = ncc.iter().filter(|v| v.is_finite()).map(|&v| v as f64).collect();
            let m = fin.iter().sum::<f64>() / fin.len() as f64;
            let v = fin.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / fin.len() as f64;
            assert!((mu as f64 - m).abs() < 1e-5 && (sd as f64 - v.sqrt()).abs() < 1e-4, "{mu} {sd} {m} {v}");
        }
    }

    /// The frame's field at cell (u, v), as `build_frame` resamples it.
    fn frame_value(g: &GridField, f: &Frame, u: usize, v: usize) -> C32 {
        let zero = is_zero_rotation(f.theta.to_degrees());
        let (s, c) = f.theta.sin_cos();
        let (du, dv) = (u as f64 + 0.5 - f.fc.0, v as f64 + 0.5 - f.fc.1);
        let (gx, gy) = (f.gc.0 + c * du - s * dv, f.gc.1 + s * du + c * dv);
        let (wx, wy) = (gx - 0.5 - g.x0 as f64, gy - 0.5 - g.y0 as f64);
        let rot = C32::new((2.0 * f.theta).cos() as f32, -(2.0 * f.theta).sin() as f32);
        let val = if zero {
            let (ix, iy) = (wx.round() as i64, wy.round() as i64);
            if ix >= 0 && iy >= 0 && (ix as usize) < g.w && (iy as usize) < g.h {
                let i = iy as usize * g.w + ix as usize;
                g.valid[i].then_some(g.d[i])
            } else {
                None
            }
        } else {
            g.sample(wx, wy).map(|z| z * rot)
        };
        val.unwrap_or(C32::new(0.0, 0.0))
    }

    #[test]
    fn frame_rotation_maps_back() {
        // A frame rotated by θ maps its reference point to the GPS and moves
        // "map right" (+u) along bearing 90° + θ.
        let g = GridField { x0: 0, y0: 0, w: 4, h: 4, d: vec![C32::new(0.0, 0.0); 16], valid: vec![false; 16] };
        let th = 30f64.to_radians();
        let layout = tile_layout(40, 30, (8, 8));
        let f = build_frame(&g, (100.0, 200.0), th, 40, 30, &layout, None);
        let (x, y) = f.to_level(f.fc.0, f.fc.1);
        assert!((x - 100.0).abs() < 1e-9 && (y - 200.0).abs() < 1e-9);
        let (x, y) = f.to_level(f.fc.0 + 10.0, f.fc.1);
        // Level y points south: +u maps to east·cosθ + south·sinθ.
        assert!((x - 100.0 - 10.0 * th.cos()).abs() < 1e-9 && (y - 200.0 - 10.0 * th.sin()).abs() < 1e-9);
    }

    /// A rotation sweep finds a map rotated 30° on synthetic terrain: the
    /// position, the scale and the rotation.
    #[test]
    fn rotation_sweep_finds_rotated_map() {
        use crate::dem::{DemStore, TILE_SIZE};
        // Non-periodic terrain: two octaves of blurred noise on a 75 m grid
        // (deterministic LCG), bilinearly interpolated.
        let centre = (3.0e6, -2.9e6);
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        let (gn, cell) = (420usize, 75.0f64);
        let mut grid = vec![0.0f32; gn * gn];
        for (sigma, amp) in [(12.0, 300.0f32), (4.0, 40.0)] {
            let noise = GrayF32 { w: gn, h: gn, data: (0..gn * gn).map(|_| rnd()).collect() }.gaussian_blur(sigma);
            let sd = (noise.data.iter().map(|v| v * v).sum::<f32>() / noise.data.len() as f32).sqrt();
            for (g, v) in grid.iter_mut().zip(&noise.data) {
                *g += amp * v / sd;
            }
        }
        let terrain = GrayF32 { w: gn, h: gn, data: grid };
        let half = gn as f64 * cell / 2.0;
        let elev = |mx: f64, my: f64| -> f64 {
            let (x, y) = ((mx - centre.0 + half) / cell, (centre.1 + half - my) / cell);
            terrain.bilinear(x.clamp(0.0, gn as f64 - 1.0), y.clamp(0.0, gn as f64 - 1.0)).unwrap_or(0.0) as f64
        };
        let z = 12u8;
        let b = MercBox::around(centre.0, centre.1, 13_000.0);
        let mut store = DemStore::new();
        for t in crate::dem::tiles_covering(z, &b) {
            let tb = crate::dem::tile_bounds(t);
            let res = crate::dem::resolution(z);
            let e: Vec<f32> = (0..TILE_SIZE * TILE_SIZE)
                .into_par_iter()
                .map(|i| {
                    let (r, c) = (i / TILE_SIZE, i % TILE_SIZE);
                    elev(tb.min_x + (c as f64 + 0.5) * res, tb.max_y - (r as f64 + 0.5) * res) as f32
                })
                .collect();
            store.add_elevations(t, e);
        }
        let dem = store.mosaic(z, &b);
        // The "map": a mask of 10 m contours, rotated 30°, 3.6 m/px ground.
        let (lat, _) = merc_to_latlon(centre.0, centre.1);
        let lc = lat.to_radians().cos();
        let (w, h) = (900usize, 1200usize);
        let truth_center = (centre.0 + 2500.0, centre.1 - 1500.0);
        let tr = Affine::pixel_to_merc_similarity(
            3.6 / lc,
            30f64.to_radians(),
            450.0,
            600.0,
            truth_center.0,
            truth_center.1,
        );
        let lvl: Vec<i64> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (mx, my) = tr.apply((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
                (elev(mx, my) / 10.0).floor() as i64
            })
            .collect();
        let mut mask = Mask::new(w, h);
        for y in 0..h - 1 {
            for x in 0..w - 1 {
                let l = lvl[y * w + x];
                if l != lvl[y * w + x + 1] || l != lvl[(y + 1) * w + x] {
                    mask.set(x, y, true);
                }
            }
        }
        let field = map_field(&mask);
        let params = SearchParams {
            center_merc: centre,
            search_radius_m: 6000.0,
            scale_min: 3.0,
            scale_max: 4.4,
            rotations_deg: rotation_sweep(10.0),
            top_k: 2,
            region: None,
        };
        let c = coarse_search(&field, &dem, w, h, &params, &|| false, &|_| {});
        let got = c[0].center_merc(w, h);
        let err = ((got.0 - truth_center.0).powi(2) + (got.1 - truth_center.1).powi(2)).sqrt() * lc;
        assert!(err < 100.0, "{err} {:?}", c[0]);
        let dr = (c[0].rotation_deg - 30.0 + 540.0).rem_euclid(360.0) - 180.0;
        assert!(dr.abs() <= 3.0, "{:?}", c[0]);
        assert!((c[0].scale_m_per_px / 3.6 - 1.0).abs() < 0.04, "{:?}", c[0]);
        assert!(c[0].peak_ratio > 1.3, "{:?}", c);
    }
}
