//! Fine alignment: make every contour segment have constant DEM elevation.
//!
//! Each contour segment traced on the map is a line of constant elevation.
//! Under the right pixel → Mercator transform T, the DEM elevations h(T(p))
//! sampled along a segment are therefore constant (up to DEM noise). We
//! minimise Σ ρ(h(T(pᵢ)) − c_seg(i)) over T and the per-segment constants c
//! (eliminated in closed form: robust weighted segment means), with a soft-L1
//! loss ρ, by Levenberg–Marquardt. The Jacobian is analytic: the DEM gradient
//! times ∂T/∂params, minus its segment mean (variable projection).
//!
//! The solve runs coarse-to-fine on a smoothed DEM pyramid (after a small
//! translation grid on the coarsest level) so initial errors of several
//! hundred metres still converge: first a 4-parameter similarity, then one
//! with its own aspect ratio (no shear) and, for maps spanning tens of km, a
//! projective stage, each kept only if the whole map supports it: it must
//! move the image's corners well beyond its delete-an-area jackknife spread
//! (`jackknife`, `MODEL_MIN_Z`). That spread, at the image's corners, is what
//! the reported accuracy is made of (`RefineResult::spread_m`).
//!
//! Photos of paper maps (`RefineOptions::photo`) keep some perspective and a
//! wrong aspect ratio after flattening: the affine then runs coarse-to-fine
//! and may be anisotropic, and an 8-parameter projective stage follows.

use crate::dem::{DemMosaic, MercBox};
use crate::geo::{Affine, Homography};
use crate::image::GrayF32;
use crate::search::Candidate;
use crate::skeleton::ContourSamples;
use rayon::prelude::*;

#[derive(Debug, Clone)]
pub struct RefineResult {
    /// Pixel → Mercator transform after refinement (for a projective fit,
    /// its affine approximation at the samples' centroid).
    pub transform: Affine,
    /// The full projective transform when the photo-mode projective stage
    /// was kept (`RefineOptions::photo`), else None.
    pub homography: Option<Homography>,
    /// Robust mean absolute within-segment elevation residual (metres):
    /// mean of min(|r|, `RESIDUAL_CLIP_M`).
    pub cost_m: f64,
    /// Median over segments of the elevation standard deviation (metres).
    pub median_seg_std_m: f64,
    /// Fraction of valid samples with |residual| < `INLIER_M`.
    pub inlier_fraction: f64,
    /// Fraction of samples that landed on valid DEM.
    pub dem_coverage: f64,
    pub iterations: usize,
    pub converged: bool,
    /// 1 − Σ clipped r² / Σ (h − mean h)²: how much of the elevation variation
    /// along the contours the per-segment constants explain (≈ 1 when right).
    pub explained_variance: f64,
    /// Standard deviation of the sampled elevations (terrain relief, metres).
    pub relief_m: f64,
    /// Valid samples / segments used.
    pub n_samples: usize,
    pub n_segments: usize,
    /// True if the final transform is a full affine or projective (else a similarity).
    pub affine: bool,
    /// Estimated contour interval (metres): median elevation difference
    /// between neighbouring contour lines (across, not along, the lines).
    pub contour_interval_m: f64,
    /// `contour_interval_m` / robust residual sigma (1.4826·median|r|).
    /// Adjacent contours must differ by one interval, clearly above the DEM
    /// noise; a wrong fit squeezes many lines into little relief (≪ 1).
    pub interval_snr: f64,
    /// Ground metres per map pixel under `transform` (at the samples'
    /// centroid) and per cell of the finest DEM level: the scale of the
    /// noise a right fit leaves (`seg_std_floor_m`).
    pub px_ground_m: f64,
    pub dem_cell_m: f64,
    /// Σ|g·t̂| / Σ|g|: how much the contours run along the DEM's slope
    /// instead of across it (`gradient_alignment`; NaN without samples).
    pub gradient_alignment: f64,
    /// How far the kept transform may be off at the corners of the extent
    /// it must hold (`RefineOptions::extent`, else the samples' bounding
    /// box), ground metres: the largest RMS displacement of a corner over
    /// the delete-a-group jackknife (`jackknife`). NaN when not estimated
    /// (an evaluation, too few groups of segments), ∞ when a group alone
    /// holds the fit's parameters.
    pub spread_m: f64,
    /// Fraction of that extent the samples cover (`extent_cover`): cells
    /// of an 8 × 8 grid holding samples.
    pub extent_cover: f64,
    /// The models tried, in order (diagnostics).
    pub stages: Vec<StageFit>,
}

/// One model the refinement fitted (`RefineResult::stages`).
#[derive(Debug, Clone, PartialEq)]
pub struct StageFit {
    /// "similarity", "aniso", "affine" (photos) or "projective".
    pub model: &'static str,
    pub homography: Homography,
    /// Robust cost on the finest level (the soft-L1 sum the stages compare).
    pub cost: f64,
    /// `RefineResult::spread_m` of this model.
    pub spread_m: f64,
    /// Largest displacement (ground metres) of an extent corner from the
    /// previous stage's fit (0 for the first).
    pub shift_m: f64,
    /// Was it kept?
    pub kept: bool,
}

/// Soft-L1 loss scale (metres) on the finest level (doubles per level).
const LOSS_SCALE_M: f64 = 3.0;
/// Samples within this (metres) of their segment's elevation are inliers.
pub const INLIER_M: f64 = 5.0;
/// Residual clip for `cost_m` and `explained_variance`.
const RESIDUAL_CLIP_M: f64 = 12.0;
/// Missing-DEM samples cost as if their residual were this large.
const MISSING_PENALTY_M: f64 = 10.0;
/// DEM smoothing (pixels of each pyramid level).
const DEM_SMOOTH_SIGMA: f64 = 0.7;
/// The similarity scale may move at most this far (log units) from the start.
const MAX_LOG_SCALE_CHANGE: f64 = 0.15;
/// Affine (photos) is kept only if it lowers the cost by this fraction…
const AFFINE_MIN_GAIN: f64 = 0.03;
/// The anisotropic similarity (maps) if it lowers the cost by this fraction
/// and is supported (`StageFit::supported`).
const ANISO_MIN_GAIN: f64 = 0.005;
/// A richer model is kept only if it moves the corners of the extent at
/// least this many times its jackknife spread there from the simpler one
/// (`StageFit::supported`). The jackknife sees the scatter between areas
/// of the map, not what they share (DEM and drawing disagreeing over a
/// whole region), and underestimates by 2–4× on maps with few contours:
/// the free affine of a Gaia raster whose contours survive only in its
/// west moved the far corners 3–5 spreads, by a spurious shear.
const MODEL_MIN_Z: f64 = 4.0;
/// …and its two axis scales differ by less than this ratio.
const AFFINE_MAX_ANISOTROPY: f64 = 1.08;
/// `Model::Aniso` keeps its u within this (the same ratio: e^(2u) ≤ 1.08).
const MAX_LOG_ANISO: f64 = 0.0385;
/// A map spanning more than this (ground metres, the samples' diagonal) gets
/// a projective stage after the planar ones: a regional projection (UTM, a
/// national grid) is no affine image of Web Mercator over tens of km (its
/// scale changes by tan(lat) / R per metre north: ~1 % over 75 km at 40°,
/// 100 m at the edges of a 1:250 000 sheet that an affine fit cannot take
/// out; a homography takes out about half).
const REGIONAL_SPAN_M: f64 = 40_000.0;
/// …kept if it lowers the cost by this fraction and its perspective terms
/// stay this small (the denominator over the samples' extent).
const REGIONAL_MIN_GAIN: f64 = 0.01;
const REGIONAL_MAX_PERSPECTIVE: f64 = 0.02;
/// Photo mode: a flattened photo's aspect ratio is a guess (a document
/// scanner sizes the output from the quad's edge lengths, ~10 % off at a 15°
/// camera tilt), so the affine may be this anisotropic…
const PHOTO_MAX_ANISOTROPY: f64 = 1.35;
/// …and change the similarity's scale by this much (log units).
const PHOTO_MAX_LOG_SCALE_CHANGE: f64 = 0.12;
/// Photo mode: DEM pyramid levels the affine / projective stages start from.
const PHOTO_AFFINE_LEVEL: usize = 2;
const PHOTO_PROJECTIVE_LEVEL: usize = 1;
/// Photo mode: the affine stage also starts from the similarity stretched
/// by ±this along image x against y. From the similarity alone it can stall
/// in a near-isotropic local minimum (the real Grootkloof photo flattened
/// from slightly wrong corners: ~150 m median error instead of ~40 m), and
/// the aspect error of a flattened photo is a few percent either way.
const PHOTO_AFFINE_STRETCH: f64 = 0.035;
/// The projective stage is kept only if it lowers the cost by this fraction…
const PROJECTIVE_MIN_GAIN: f64 = 0.03;
/// …and its perspective terms stay small: the denominator changes by at most
/// this much from the centroid to the edge of the samples' extent.
const PHOTO_MAX_PERSPECTIVE: f64 = 0.15;
/// Segments need this many valid samples to count.
const MIN_SEG_SAMPLES: usize = 4;
/// The pyramid goes down to cells of about this size (Mercator metres).
const COARSEST_RES_M: f64 = 120.0;
/// Translation grid (Mercator metres) scanned on the coarsest level.
const GRID_HALF_M: f64 = 450.0;
const GRID_STEP_M: f64 = 150.0;
/// …or this many map pixels, when more (a map at 50 m/px: ±750 m).
const GRID_HALF_PX: f64 = 15.0;
/// Best grid starts polished by LM on the coarsest level.
const GRID_STARTS: usize = 3;
/// Samples used on the coarse levels / the finest level: at most this many
/// (strided subsets). The finest level's cost grows with its samples: the
/// line detector's masks (`contour_lines`) give 300-dpi pages 100-550 k
/// samples, and refining three wrong candidates at 150 k cost ~1-3 s more in
/// every search that does not find the map; 60 k place the benchmark's maps
/// and the MCSA sheets as well.
const MAX_SAMPLES_COARSE: usize = 20_000;
const MAX_SAMPLES_FINE: usize = 60_000;
/// …thinned per segment, keeping at least this many of each (all of a
/// shorter one): no segment drops out of the metrics (`MIN_SEG_SAMPLES`).
const FINE_MIN_SEG_SAMPLES: usize = 2 * MIN_SEG_SAMPLES;
/// Photo mode keeps 150 k (strided): its projective stage needs them (a
/// photo flattened from corners 2.5 % off fitted 690 m off at the edges
/// with 60 k).
const MAX_SAMPLES_FINE_PHOTO: usize = 150_000;

/// DEM prepared for refinement: NaN-aware smoothed pyramid (finest first).
pub struct PreparedDem {
    pub levels: Vec<DemMosaic>,
}

impl PreparedDem {
    /// Smooth the mosaic (σ = 0.7 px, NaN-aware) and build coarser levels
    /// (2× each) down to ~120 m cells. Do this once per mosaic.
    pub fn new(dem: &DemMosaic) -> PreparedDem {
        let mut levels = vec![smooth_nan(dem, DEM_SMOOTH_SIGMA)];
        while levels.last().unwrap().res * 2.0 <= COARSEST_RES_M * 1.5 {
            let d = levels.last().unwrap().downsample2();
            if d.w() < 8 || d.h() < 8 {
                break;
            }
            levels.push(smooth_nan(&d, DEM_SMOOTH_SIGMA));
        }
        PreparedDem { levels }
    }

    /// Crop the mosaic to `b` first (cheaper smoothing for big mosaics).
    pub fn new_cropped(dem: &DemMosaic, b: &MercBox) -> PreparedDem {
        PreparedDem::new(&crop_mosaic(dem, b))
    }
}

/// Mercator box a fine (zoom 13) DEM should cover to refine from `init`:
/// the map footprint plus a margin for initial errors and scale changes.
pub fn refine_dem_box(init: &Affine, img_w: usize, img_h: usize) -> MercBox {
    let (w, h) = (img_w as f64, img_h as f64);
    let pts = [init.apply(0.0, 0.0), init.apply(w, 0.0), init.apply(w, h), init.apply(0.0, h)];
    let b = MercBox::from_points(&pts);
    let margin = (0.2 * b.width().max(b.height())).max(1500.0);
    b.expand(margin)
}

fn crop_mosaic(dem: &DemMosaic, b: &MercBox) -> DemMosaic {
    let (c0, r0) = dem.merc_to_px(b.min_x, b.max_y);
    let (c1, r1) = dem.merc_to_px(b.max_x, b.min_y);
    let c0 = (c0.floor() as i64 - 2).clamp(0, dem.w() as i64 - 1) as usize;
    let r0 = (r0.floor() as i64 - 2).clamp(0, dem.h() as i64 - 1) as usize;
    let c1 = (c1.ceil() as i64 + 3).clamp(c0 as i64 + 1, dem.w() as i64) as usize;
    let r1 = (r1.ceil() as i64 + 3).clamp(r0 as i64 + 1, dem.h() as i64) as usize;
    let (w, h) = (c1 - c0, r1 - r0);
    let mut data = vec![0.0f32; w * h];
    for y in 0..h {
        data[y * w..(y + 1) * w].copy_from_slice(&dem.grid.data[(r0 + y) * dem.w() + c0..(r0 + y) * dem.w() + c1]);
    }
    DemMosaic {
        res: dem.res,
        ox: dem.ox + c0 as f64 * dem.res,
        oy: dem.oy - r0 as f64 * dem.res,
        grid: GrayF32 { w, h, data },
    }
}

/// Gaussian smoothing that ignores NaN cells (NaN where < half the weight is valid).
fn smooth_nan(dem: &DemMosaic, sigma: f64) -> DemMosaic {
    let g = &dem.grid;
    let vals = GrayF32 { w: g.w, h: g.h, data: g.data.iter().map(|&v| if v.is_finite() { v } else { 0.0 }).collect() };
    let wts = GrayF32 { w: g.w, h: g.h, data: g.data.iter().map(|&v| v.is_finite() as u8 as f32).collect() };
    let sv = vals.gaussian_blur(sigma);
    let sw = wts.gaussian_blur(sigma);
    let data = sv.data.iter().zip(&sw.data).map(|(&v, &w)| if w >= 0.5 { v / w } else { f32::NAN }).collect();
    DemMosaic { res: dem.res, ox: dem.ox, oy: dem.oy, grid: GrayF32 { w: g.w, h: g.h, data } }
}

/// Transform parameterisation about the sample centroid (cx, cy).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Model {
    /// [ln s, θ, tx, ty] as in `Affine::pixel_to_merc_similarity`.
    Similarity,
    /// A similarity whose image axes are scaled apart: [ln s, θ, tx, ty,
    /// u], image x scaled by s·eᵘ and image y by s·e⁻ᵘ before the rotation
    /// (a page rendered or scanned at another aspect ratio; no shear).
    Aniso,
    /// [a, b, d, e, tx, ty].
    Affine,
    /// Full projective transform (photos), about the Mercator reference
    /// point `rm` in normalised pixel coordinates u = (x − cx)/sn,
    /// v = (y − cy)/sn, which keeps the 8 parameters well conditioned:
    /// merc = rm + (a·u + b·v + c, d·u + e·v + f) / (1 + g·u + h·v),
    /// parameters [a, b, c, d, e, f, g, h].
    Projective,
}

/// Most parameters of any model.
const NP: usize = 8;

impl Model {
    fn n(self) -> usize {
        match self {
            Model::Similarity => 4,
            Model::Aniso => 5,
            Model::Affine => 6,
            Model::Projective => 8,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Params {
    model: Model,
    p: [f64; NP],
    cx: f64,
    cy: f64,
    /// Pixel normalisation length and Mercator reference of `Projective`.
    sn: f64,
    rm: (f64, f64),
}

impl Params {
    fn from_affine(t: &Affine, model: Model, d: &Data) -> Params {
        let (cx, cy, sn) = (d.cx, d.cy, d.sn);
        let (tx, ty) = t.apply(cx, cy);
        let p = match model {
            Model::Similarity => {
                // Closest similarity: a = s cos θ, b = −s sin θ, d = −s sin θ, e = −s cos θ.
                let sc = 0.5 * (t.a - t.e);
                let ss = -0.5 * (t.b + t.d);
                [(sc * sc + ss * ss).sqrt().max(1e-12).ln(), ss.atan2(sc), tx, ty, 0.0, 0.0, 0.0, 0.0]
            }
            Model::Aniso => {
                // Columns (a, d) = s·eᵘ·(cos θ, −sin θ), (b, e) = s·e⁻ᵘ·(−sin θ, −cos θ).
                let (k1, k2) = (t.a.hypot(t.d).max(1e-12), t.b.hypot(t.e).max(1e-12));
                let (t1, t2) = ((-t.d).atan2(t.a), (-t.b).atan2(-t.e));
                let th = (t1.sin() + t2.sin()).atan2(t1.cos() + t2.cos());
                [0.5 * (k1 * k2).ln(), th, tx, ty, 0.5 * (k1 / k2).ln(), 0.0, 0.0, 0.0]
            }
            Model::Affine => [t.a, t.b, t.d, t.e, tx, ty, 0.0, 0.0],
            Model::Projective => [t.a * sn, t.b * sn, 0.0, t.d * sn, t.e * sn, 0.0, 0.0, 0.0],
        };
        Params { model, p, cx, cy, sn, rm: (tx, ty) }
    }

    /// Affine parameters stretched by `1 + k` along image x and `1 − k`
    /// along image y about the samples' centroid.
    fn stretched(mut self, k: f64) -> Params {
        debug_assert_eq!(self.model, Model::Affine);
        // p = [a, b, d, e, tx, ty]: (a, d) multiply x, (b, e) multiply y.
        for (i, s) in [(0, 1.0 + k), (1, 1.0 - k), (2, 1.0 + k), (3, 1.0 - k)] {
            self.p[i] *= s;
        }
        self
    }

    /// Projective parameters of a homography (None if it is singular at the centroid).
    fn from_homography(h: &Homography, d: &Data) -> Option<Params> {
        let (cx, cy, sn) = (d.cx, d.cy, d.sn);
        let k = h.m[6] * cx + h.m[7] * cy + h.m[8];
        if !k.is_finite() || k.abs() < 1e-300 {
            return None;
        }
        let m = h.m.map(|v| v / k);
        let rm = h.apply(cx, cy);
        let p = [
            (m[0] - rm.0 * m[6]) * sn,
            (m[1] - rm.0 * m[7]) * sn,
            0.0,
            (m[3] - rm.1 * m[6]) * sn,
            (m[4] - rm.1 * m[7]) * sn,
            0.0,
            m[6] * sn,
            m[7] * sn,
        ];
        Some(Params { model: Model::Projective, p, cx, cy, sn, rm })
    }

    /// Projective mapping: (Mercator x, y, denominator q).
    #[inline]
    fn project(&self, x: f64, y: f64) -> (f64, f64, f64) {
        let [a, b, c, d, e, f, g, h] = self.p;
        let (u, v) = ((x - self.cx) / self.sn, (y - self.cy) / self.sn);
        let q = 1.0 + g * u + h * v;
        (self.rm.0 + (a * u + b * v + c) / q, self.rm.1 + (d * u + e * v + f) / q, q)
    }

    fn to_affine(self) -> Affine {
        match self.model {
            Model::Similarity => {
                Affine::pixel_to_merc_similarity(self.p[0].exp(), self.p[1], self.cx, self.cy, self.p[2], self.p[3])
            }
            Model::Aniso => {
                let [ls, th, tx, ty, u, _, _, _] = self.p;
                let (k1, k2) = ((ls + u).exp(), (ls - u).exp());
                let (sn, cs) = th.sin_cos();
                let (a, b, d, e) = (k1 * cs, -k2 * sn, -k1 * sn, -k2 * cs);
                Affine { a, b, c: tx - a * self.cx - b * self.cy, d, e, f: ty - d * self.cx - e * self.cy }
            }
            Model::Affine => {
                let [a, b, d, e, tx, ty, _, _] = self.p;
                Affine { a, b, c: tx - a * self.cx - b * self.cy, d, e, f: ty - d * self.cx - e * self.cy }
            }
            Model::Projective => self.to_homography().local_affine(self.cx, self.cy),
        }
    }

    /// Mercator position of pixel (x, y).
    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        match self.model {
            Model::Projective => {
                let (mx, my, _) = self.project(x, y);
                (mx, my)
            }
            _ => self.to_affine().apply(x, y),
        }
    }

    fn to_homography(self) -> Homography {
        if self.model != Model::Projective {
            return self.to_affine().to_homography();
        }
        let [a, b, c, d, e, f, g, h] = self.p;
        let (cx, cy, sn) = (self.cx, self.cy, self.sn);
        let w = [g / sn, h / sn, 1.0 - (g * cx + h * cy) / sn];
        let r1 = [a / sn, b / sn, c - (a * cx + b * cy) / sn];
        let r2 = [d / sn, e / sn, f - (d * cx + e * cy) / sn];
        let mut m = [0.0; 9];
        for k in 0..3 {
            m[k] = self.rm.0 * w[k] + r1[k];
            m[3 + k] = self.rm.1 * w[k] + r2[k];
            m[6 + k] = w[k];
        }
        Homography { m }
    }
}

/// Transform being evaluated by `metrics`.
#[derive(Debug, Clone, Copy)]
enum Fit {
    Affine(Affine),
    Projective(Homography),
}

impl Fit {
    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            Fit::Affine(t) => t.apply(x, y),
            Fit::Projective(h) => h.apply(x, y),
        }
    }
}

#[inline]
fn soft_l1(r: f64, c: f64) -> f64 {
    let z = r / c;
    2.0 * c * c * ((1.0 + z * z).sqrt() - 1.0)
}

#[inline]
fn soft_l1_weight(r: f64, c: f64) -> f64 {
    1.0 / (1.0 + (r / c).powi(2)).sqrt()
}

/// One evaluation: robust cost and (optionally) normal equations.
struct Eval {
    cost: f64,
    ata: [[f64; NP]; NP],
    atb: [f64; NP],
    /// Per-sample residual and elevation (NaN where unused).
    resid: Vec<f64>,
    elev: Vec<f64>,
    /// Each segment group's share of `ata` and `atb` (`evaluate_grouped`).
    group_ata: Vec<[[f64; NP]; NP]>,
    group_atb: Vec<[f64; NP]>,
}

/// Elevation, gradient and Mercator position of every sample (NaN = off DEM).
fn sample_all(d: &Data, dem: &DemMosaic, par: &Params) -> Vec<[f64; 5]> {
    let t = par.to_affine();
    let projective = par.model == Model::Projective;
    (0..d.xs.len())
        .into_par_iter()
        .map(|i| {
            let (mx, my) = if projective {
                let (mx, my, _) = par.project(d.xs[i], d.ys[i]);
                (mx, my)
            } else {
                t.apply(d.xs[i], d.ys[i])
            };
            match dem.sample_with_grad(mx, my) {
                Some((h, gx, gy)) => [h as f64, gx as f64, gy as f64, mx, my],
                None => [f64::NAN; 5],
            }
        })
        .collect()
}

/// Evaluate `par` on `dem` for all samples of `d`.
fn evaluate_params(d: &Data, dem: &DemMosaic, par: &Params, c: f64, jac: bool) -> Eval {
    evaluate_inner(d, dem, par, c, jac, None)
}

/// `evaluate_params` with normal equations, also split by segment group.
fn evaluate_grouped(d: &Data, dem: &DemMosaic, par: &Params, c: f64, groups: &Groups) -> Eval {
    evaluate_inner(d, dem, par, c, true, Some(groups))
}

#[allow(clippy::needless_range_loop)]
fn evaluate_inner(d: &Data, dem: &DemMosaic, par: &Params, c: f64, jac: bool, groups: Option<&Groups>) -> Eval {
    let n = d.xs.len();
    let np = par.model.n();
    let samp = sample_all(d, dem, par);
    // Segments need enough valid samples.
    let mut cnt = vec![0usize; d.n_segs];
    for i in 0..n {
        if samp[i][0].is_finite() {
            cnt[d.seg[i] as usize] += 1;
        }
    }
    let used: Vec<bool> = (0..n).map(|i| samp[i][0].is_finite() && cnt[d.seg[i] as usize] >= MIN_SEG_SAMPLES).collect();
    // Robust segment means: plain mean, then one IRLS re-weighting.
    let seg_mean = |w: &dyn Fn(usize) -> f64| -> Vec<f64> {
        let mut sw = vec![0.0f64; d.n_segs];
        let mut sh = vec![0.0f64; d.n_segs];
        for i in 0..n {
            if used[i] {
                let (s, wi) = (d.seg[i] as usize, w(i));
                sw[s] += wi;
                sh[s] += wi * samp[i][0];
            }
        }
        sh.iter().zip(&sw).map(|(&a, &b)| if b > 0.0 { a / b } else { 0.0 }).collect()
    };
    let m0 = seg_mean(&|_| 1.0);
    let mean = seg_mean(&|i| soft_l1_weight(samp[i][0] - m0[d.seg[i] as usize], c));
    let mut resid = vec![f64::NAN; n];
    let mut elev = vec![f64::NAN; n];
    let mut w = vec![0.0f64; n];
    let mut cost = 0.0;
    let penalty = soft_l1(MISSING_PENALTY_M, c);
    for i in 0..n {
        if used[i] {
            let h = samp[i][0];
            let r = h - mean[d.seg[i] as usize];
            resid[i] = r;
            elev[i] = h;
            w[i] = soft_l1_weight(r, c);
            cost += soft_l1(r, c);
        } else {
            cost += penalty;
        }
    }
    let mut ata = [[0.0f64; NP]; NP];
    let mut atb = [0.0f64; NP];
    let n_groups = groups.map_or(0, |g| g.n);
    let mut group_ata = vec![[[0.0f64; NP]; NP]; n_groups];
    let mut group_atb = vec![[0.0f64; NP]; n_groups];
    if jac {
        let row = |i: usize| -> [f64; NP] {
            let [_, gx, gy, mx, my] = samp[i];
            let p = &par.p;
            match par.model {
                Model::Similarity => {
                    let (dx, dy) = (mx - p[2], my - p[3]);
                    [gx * dx + gy * dy, gx * dy - gy * dx, gx, gy, 0.0, 0.0, 0.0, 0.0]
                }
                Model::Aniso => {
                    let (dx, dy) = (mx - p[2], my - p[3]);
                    // ∂/∂u: the image x part grows, the y part shrinks.
                    let s = p[0].exp();
                    let (bx, by) = ((p[4]).exp() * (d.xs[i] - par.cx), (-p[4]).exp() * (d.ys[i] - par.cy));
                    let (sn, cs) = p[1].sin_cos();
                    let (ux, uy) = (s * (cs * bx + sn * by), s * (-sn * bx + cs * by));
                    [gx * dx + gy * dy, gx * dy - gy * dx, gx, gy, gx * ux + gy * uy, 0.0, 0.0, 0.0]
                }
                Model::Affine => {
                    let (x, y) = (d.xs[i] - par.cx, d.ys[i] - par.cy);
                    [gx * x, gx * y, gy * x, gy * y, gx, gy, 0.0, 0.0]
                }
                Model::Projective => {
                    let (u, v) = ((d.xs[i] - par.cx) / par.sn, (d.ys[i] - par.cy) / par.sn);
                    let q = 1.0 + p[6] * u + p[7] * v;
                    let (gxq, gyq) = (gx / q, gy / q);
                    // ∂(merc − rm)/∂(g, h) = −(merc − rm)·(u, v)/q.
                    let k = -(gx * (mx - par.rm.0) + gy * (my - par.rm.1)) / q;
                    [gxq * u, gxq * v, gxq, gyq * u, gyq * v, gyq, k * u, k * v]
                }
            }
        };
        // Weighted segment means of the Jacobian rows (variable projection).
        let mut sj = vec![[0.0f64; NP]; d.n_segs];
        let mut sw = vec![0.0f64; d.n_segs];
        for i in 0..n {
            if used[i] {
                let (s, j) = (d.seg[i] as usize, row(i));
                sw[s] += w[i];
                for k in 0..np {
                    sj[s][k] += w[i] * j[k];
                }
            }
        }
        for v in sj.iter_mut().zip(&sw) {
            if *v.1 > 0.0 {
                for x in v.0.iter_mut() {
                    *x /= v.1;
                }
            }
        }
        for i in 0..n {
            if !used[i] {
                continue;
            }
            let s = d.seg[i] as usize;
            let mut j = row(i);
            for k in 0..np {
                j[k] -= sj[s][k];
            }
            let (wi, r) = (w[i], resid[i]);
            for a in 0..np {
                atb[a] += wi * j[a] * r;
                for b in a..np {
                    ata[a][b] += wi * j[a] * j[b];
                }
            }
            if let Some(g) = groups.map(|g| g.of_seg[s] as usize).filter(|&g| g < n_groups) {
                for a in 0..np {
                    group_atb[g][a] += wi * j[a] * r;
                    for b in a..np {
                        group_ata[g][a][b] += wi * j[a] * j[b];
                    }
                }
            }
        }
        for m in std::iter::once(&mut ata).chain(group_ata.iter_mut()) {
            for a in 0..np {
                for b in 0..a {
                    m[a][b] = m[b][a];
                }
            }
        }
    }
    Eval { cost, ata, atb, resid, elev, group_ata, group_atb }
}

/// Samples in a refinement-friendly layout.
struct Data {
    xs: Vec<f64>,
    ys: Vec<f64>,
    seg: Vec<u32>,
    n_segs: usize,
    cx: f64,
    cy: f64,
    /// Half the diagonal of the samples' bounding box (pixels, ≥ 1): the
    /// coordinate normalisation of the projective model.
    sn: f64,
}

impl Data {
    fn new(s: &ContourSamples) -> Data {
        let n = s.pts.len().max(1) as f64;
        let cx = s.pts.iter().map(|p| p[0] as f64).sum::<f64>() / n;
        let cy = s.pts.iter().map(|p| p[1] as f64).sum::<f64>() / n;
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in &s.pts {
            x0 = x0.min(p[0] as f64);
            x1 = x1.max(p[0] as f64);
            y0 = y0.min(p[1] as f64);
            y1 = y1.max(p[1] as f64);
        }
        let sn = if s.pts.is_empty() { 1.0 } else { (0.5 * (x1 - x0).hypot(y1 - y0)).max(1.0) };
        Data {
            xs: s.pts.iter().map(|p| p[0] as f64).collect(),
            ys: s.pts.iter().map(|p| p[1] as f64).collect(),
            seg: s.seg.clone(),
            n_segs: s.n_segs,
            cx,
            cy,
            sn,
        }
    }

    /// Every `stride`-th sample (same centroid, so parameters carry over).
    /// Every `stride`-th sample of each segment, at least
    /// `FINE_MIN_SEG_SAMPLES` of it (segments are contiguous runs).
    fn thin_segments(&self, stride: usize) -> Data {
        let (mut xs, mut ys, mut seg) = (Vec::new(), Vec::new(), Vec::new());
        let mut i = 0;
        while i < self.xs.len() {
            let mut j = i + 1;
            while j < self.xs.len() && self.seg[j] == self.seg[i] {
                j += 1;
            }
            let st = stride.min((j - i) / FINE_MIN_SEG_SAMPLES).max(1);
            for k in (i..j).step_by(st) {
                xs.push(self.xs[k]);
                ys.push(self.ys[k]);
                seg.push(self.seg[k]);
            }
            i = j;
        }
        Data { xs, ys, seg, n_segs: self.n_segs, cx: self.cx, cy: self.cy, sn: self.sn }
    }

    fn subset(&self, stride: usize) -> Data {
        let idx = (0..self.xs.len()).step_by(stride.max(1));
        let (mut xs, mut ys, mut seg) = (Vec::new(), Vec::new(), Vec::new());
        for i in idx {
            xs.push(self.xs[i]);
            ys.push(self.ys[i]);
            seg.push(self.seg[i]);
        }
        Data { xs, ys, seg, n_segs: self.n_segs, cx: self.cx, cy: self.cy, sn: self.sn }
    }
}

/// Segments are grouped for the jackknife by where they lie: cells of a
/// `JK_GRID` × `JK_GRID` grid over the samples' bounding box (by segment
/// centroid)…
const JK_GRID: usize = 5;
/// …and the jackknife needs at least this many groups (else each segment
/// is a group of its own; with fewer segments there is no estimate).
const JK_MIN_GROUPS: usize = 6;

/// Segment groups: the units `jackknife` leaves out. Contour samples are
/// not independent (a stretch of map that the DEM or the drawing gets
/// wrong shifts all its samples together), so the fit's uncertainty is
/// judged by leaving out whole areas of the map, not samples.
struct Groups {
    /// Group of each segment (`u32::MAX`: a segment without samples).
    of_seg: Vec<u32>,
    n: usize,
}

impl Groups {
    fn new(d: &Data) -> Groups {
        let mut sum = vec![(0.0f64, 0.0f64, 0usize); d.n_segs];
        for i in 0..d.xs.len() {
            let s = &mut sum[d.seg[i] as usize];
            s.0 += d.xs[i];
            s.1 += d.ys[i];
            s.2 += 1;
        }
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for (&x, &y) in d.xs.iter().zip(&d.ys) {
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let cell = |v: f64, lo: f64, hi: f64| (((v - lo) / (hi - lo).max(1.0)) * JK_GRID as f64) as usize;
        let mut id = [u32::MAX; JK_GRID * JK_GRID];
        let mut of_seg = vec![u32::MAX; d.n_segs];
        let mut n = 0u32;
        for (s, &(sx, sy, c)) in sum.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let (cx, cy) = (sx / c as f64, sy / c as f64);
            let k = cell(cy, y0, y1).min(JK_GRID - 1) * JK_GRID + cell(cx, x0, x1).min(JK_GRID - 1);
            if id[k] == u32::MAX {
                id[k] = n;
                n += 1;
            }
            of_seg[s] = id[k];
        }
        if (n as usize) < JK_MIN_GROUPS {
            // Too compact for areas: segments one by one.
            n = 0;
            for (s, &(_, _, c)) in sum.iter().enumerate() {
                if c > 0 {
                    of_seg[s] = n;
                    n += 1;
                }
            }
        }
        Groups { of_seg, n: n as usize }
    }
}

/// Linearised delete-a-group jackknife of the fit `par`: for every group
/// of segments, the parameters refitted without it (one Gauss–Newton step
/// from `par` with that group's share of the normal equations removed).
/// None with too few groups; Some(empty) when leaving a group out leaves
/// the model undetermined (that group alone holds some of its parameters).
fn jackknife(d: &Data, dem: &DemMosaic, par: &Params, groups: &Groups) -> Option<Vec<Params>> {
    if groups.n < JK_MIN_GROUPS {
        return None;
    }
    let np = par.model.n();
    let e = evaluate_grouped(d, dem, par, LOSS_SCALE_M, groups);
    let mut reps = Vec::with_capacity(groups.n);
    for k in 0..groups.n {
        if e.group_ata[k].iter().all(|r| r.iter().all(|&v| v == 0.0)) {
            continue; // no sample of this group was used
        }
        let mut m: Vec<Vec<f64>> = (0..np)
            .map(|a| {
                let mut row: Vec<f64> = (0..np).map(|b| e.ata[a][b] - e.group_ata[k][a][b]).collect();
                row[a] += 1e-9 * e.ata[a][a].abs() + 1e-12;
                row.push(-(e.atb[a] - e.group_atb[k][a]));
                row
            })
            .collect();
        let Some(delta) = crate::geo::solve_dense(&mut m) else {
            return Some(Vec::new());
        };
        let mut p = *par;
        for (v, dv) in p.p.iter_mut().zip(&delta) {
            *v += dv;
        }
        reps.push(p);
    }
    (reps.len() >= JK_MIN_GROUPS).then_some(reps)
}

/// The jackknife's spread at the corners of `extent` (pixels: x0, y0, x1,
/// y1): the largest RMS displacement of a corner over the replicates
/// (Mercator metres), with the jackknife's (k − 1)/k variance factor.
/// ∞ when the model is undetermined without some group, NaN without an
/// estimate.
fn corner_spread(reps: Option<&[Params]>, extent: &[f64; 4]) -> f64 {
    let Some(reps) = reps else {
        return f64::NAN;
    };
    if reps.is_empty() {
        return f64::INFINITY;
    }
    let k = reps.len() as f64;
    corners(extent)
        .iter()
        .map(|&(x, y)| {
            let pts: Vec<(f64, f64)> = reps.iter().map(|p| p.apply(x, y)).collect();
            let mx = pts.iter().map(|p| p.0).sum::<f64>() / k;
            let my = pts.iter().map(|p| p.1).sum::<f64>() / k;
            let ss = pts.iter().map(|p| (p.0 - mx).powi(2) + (p.1 - my).powi(2)).sum::<f64>();
            ((k - 1.0) / k * ss).sqrt()
        })
        .fold(0.0, f64::max)
}

/// Corners TL, TR, BR, BL of an extent (x0, y0, x1, y1).
fn corners(e: &[f64; 4]) -> [(f64, f64); 4] {
    [(e[0], e[1]), (e[2], e[1]), (e[2], e[3]), (e[0], e[3])]
}

/// Largest displacement (Mercator metres) between two fits at the corners of `extent`.
fn corner_shift(a: &Params, b: &Params, extent: &[f64; 4]) -> f64 {
    corners(extent)
        .iter()
        .map(|&(x, y)| {
            let (p, q) = (a.apply(x, y), b.apply(x, y));
            (p.0 - q.0).hypot(p.1 - q.1)
        })
        .fold(0.0, f64::max)
}

/// Fraction of the cells of an 8 × 8 grid over `extent` that hold samples.
fn extent_cover(d: &Data, extent: &[f64; 4]) -> f64 {
    const G: usize = 8;
    let (w, h) = ((extent[2] - extent[0]).max(1.0), (extent[3] - extent[1]).max(1.0));
    let mut hit = [false; G * G];
    for (&x, &y) in d.xs.iter().zip(&d.ys) {
        let (u, v) = ((x - extent[0]) / w, (y - extent[1]) / h);
        if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
            hit[(v * G as f64) as usize * G + (u * G as f64) as usize] = true;
        }
    }
    hit.iter().filter(|&&b| b).count() as f64 / (G * G) as f64
}

/// Levenberg–Marquardt from `start`. Returns (params, cost, iterations, converged).
fn lm(
    d: &Data,
    dem: &DemMosaic,
    start: Params,
    c: f64,
    max_iter: usize,
    log_scale_bounds: (f64, f64),
) -> (Params, f64, usize, bool) {
    let np = start.model.n();
    let mut par = start;
    let mut cur = evaluate_params(d, dem, &par, c, true);
    let mut lambda = 1e-3;
    let mut converged = false;
    let mut it = 0;
    while it < max_iter {
        it += 1;
        let mut m: Vec<Vec<f64>> = (0..np)
            .map(|a| {
                let mut row = cur.ata[a][..np].to_vec();
                row[a] += lambda * cur.ata[a][a].max(1e-12) + 1e-12;
                row.push(-cur.atb[a]);
                row
            })
            .collect();
        let delta = match crate::geo::solve_dense(&mut m) {
            Some(x) => x,
            None => {
                lambda *= 10.0;
                if lambda > 1e8 {
                    break;
                }
                continue;
            }
        };
        let mut cand = par;
        for (p, dv) in cand.p.iter_mut().zip(&delta) {
            *p += dv;
        }
        if matches!(cand.model, Model::Similarity | Model::Aniso) {
            cand.p[0] = cand.p[0].clamp(log_scale_bounds.0, log_scale_bounds.1);
        }
        if cand.model == Model::Aniso {
            cand.p[4] = cand.p[4].clamp(-MAX_LOG_ANISO, MAX_LOG_ANISO);
        }
        let next = evaluate_params(d, dem, &cand, c, true);
        if next.cost < cur.cost {
            let gain = (cur.cost - next.cost) / cur.cost.max(1e-12);
            // Step size in Mercator metres at the map extent.
            let step_m = match par.model {
                Model::Similarity => delta[2].hypot(delta[3]) + 3000.0 * (delta[0].abs() + delta[1].abs()),
                Model::Aniso => delta[2].hypot(delta[3]) + 3000.0 * (delta[0].abs() + delta[1].abs() + delta[4].abs()),
                Model::Affine => delta[4].hypot(delta[5]) + 1500.0 * delta[..4].iter().map(|v| v.abs()).sum::<f64>(),
                Model::Projective => {
                    let extent = 0.5 * (par.p[0].abs() + par.p[4].abs());
                    delta[2].hypot(delta[5])
                        + [0, 1, 3, 4].iter().map(|&k| delta[k].abs()).sum::<f64>()
                        + extent * (delta[6].abs() + delta[7].abs())
                }
            };
            par = cand;
            cur = next;
            lambda = (lambda / 3.0).max(1e-9);
            if gain < 1e-5 || step_m < 0.05 {
                converged = true;
                break;
            }
        } else {
            lambda *= 4.0;
            if lambda > 1e8 {
                converged = true;
                break;
            }
        }
    }
    (par, cur.cost, it, converged)
}

/// Options of `refine_prepared_with`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefineOptions {
    /// The image is a photo of a paper map flattened by its page borders:
    /// residual perspective and aspect errors are expected, so the affine
    /// stage may be strongly anisotropic and a projective stage follows.
    pub photo: bool,
    /// The pixel box (x0, y0, x1, y1) the transform must hold, usually the
    /// whole image: `RefineResult::spread_m` is measured at its corners
    /// (None: the samples' bounding box).
    pub extent: Option<[u32; 4]>,
}

/// Levenberg–Marquardt on similarity then affine parameters, starting at `init`.
/// `dem` is a fine mosaic (zoom 13) covering the map footprint plus margin.
pub fn refine(samples: &ContourSamples, dem: &DemMosaic, init: &Affine) -> RefineResult {
    refine_prepared(samples, &PreparedDem::new(dem), init)
}

/// `refine` with a DEM prepared once (reuse it across candidates).
pub fn refine_prepared(samples: &ContourSamples, dem: &PreparedDem, init: &Affine) -> RefineResult {
    refine_prepared_with(samples, dem, init, &RefineOptions::default())
}

/// `refine_prepared` with options (photo mode: anisotropic affine, then projective).
pub fn refine_prepared_with(
    samples: &ContourSamples,
    dem: &PreparedDem,
    init: &Affine,
    opts: &RefineOptions,
) -> RefineResult {
    let mut full = Data::new(samples);
    if opts.photo {
        if full.xs.len() > MAX_SAMPLES_FINE_PHOTO {
            full = full.subset(full.xs.len().div_ceil(MAX_SAMPLES_FINE_PHOTO));
        }
    } else if full.xs.len() > MAX_SAMPLES_FINE {
        full = full.thin_segments(full.xs.len().div_ceil(MAX_SAMPLES_FINE));
    }
    if samples.pts.is_empty() || dem.levels.is_empty() {
        return metrics(&full, dem, &Fit::Affine(*init), 0, false, false);
    }
    let start = Params::from_affine(init, Model::Similarity, &full);
    let bounds = (start.p[0] - MAX_LOG_SCALE_CHANGE, start.p[0] + MAX_LOG_SCALE_CHANGE);
    let n_levels = dem.levels.len();
    let coarse_stride = full.xs.len().div_ceil(MAX_SAMPLES_COARSE).max(1);
    let level_data = |lvl: usize| -> Data {
        // Finest level uses everything; coarser ones a strided subset.
        let stride = if lvl == 0 { 1 } else { coarse_stride.min(1 << lvl) };
        full.subset(stride)
    };
    let loss = |lvl: usize| LOSS_SCALE_M * (1u32 << lvl) as f64;
    let mut iterations = 0;
    // Coarsest level: cost on a translation grid, then LM from the best few.
    let lvl = n_levels - 1;
    let dc = level_data(lvl);
    let mut grid: Vec<(f64, Params)> = Vec::new();
    let n_steps = (GRID_HALF_M / GRID_STEP_M).round() as i32;
    // The coarse search places a small-scale map to a fraction of its
    // (coarser) DEM cells: the grid spans `GRID_HALF_PX` of its pixels when
    // that is more than `GRID_HALF_M`.
    let step = GRID_STEP_M.max(GRID_HALF_PX * start.p[0].exp() / n_steps.max(1) as f64);
    for iy in -n_steps..=n_steps {
        for ix in -n_steps..=n_steps {
            let mut s = start;
            s.p[2] += ix as f64 * step;
            s.p[3] += iy as f64 * step;
            grid.push((evaluate_params(&dc, &dem.levels[lvl], &s, loss(lvl), false).cost, s));
        }
    }
    grid.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut best: Option<(Params, f64)> = None;
    for (_, s) in grid.iter().take(GRID_STARTS) {
        let (p, cost, it, _) = lm(&dc, &dem.levels[lvl], *s, loss(lvl), 15, bounds);
        iterations += it;
        if best.as_ref().is_none_or(|b| cost < b.1) {
            best = Some((p, cost));
        }
    }
    let mut par = best.map_or(start, |b| b.0);
    // Finer levels.
    let mut converged = n_levels == 1;
    let fine_data = level_data(0);
    for lvl in (0..n_levels - 1).rev() {
        let dl = if lvl == 0 { None } else { Some(level_data(lvl)) };
        let (p, _, it, conv) = lm(dl.as_ref().unwrap_or(&fine_data), &dem.levels[lvl], par, loss(lvl), 25, bounds);
        par = p;
        iterations += it;
        converged = conv;
    }
    let fine = &dem.levels[0];
    let sim_cost = evaluate_params(&fine_data, fine, &par, LOSS_SCALE_M, false).cost;
    let judge = Judge::new(&fine_data, fine, opts.extent);
    if opts.photo {
        let stages =
            PhotoStages { full: &full, fine_data: &fine_data, dem, level_data: &level_data, loss: &loss, bounds };
        return stages.run(par, sim_cost, iterations, converged, &judge);
    }
    let mut stages = vec![judge.stage("similarity", &par, sim_cost, None, false)];
    // The page's aspect ratio: a map rendered or scanned at another aspect
    // ratio than its ground (a Gaia PDF page: 1.6 %; scanners: tenths of a
    // percent). No shear: no map is drawn sheared, and a free affine fitted
    // to contours over part of the map takes up whatever the DEM and the
    // drawing disagree on there as shear, which grows towards the far side
    // (`MODEL_MIN_Z`).
    let ani_start = Params::from_affine(&par.to_affine(), Model::Aniso, &full);
    let (ani, ani_cost, it, ani_conv) = lm(&fine_data, fine, ani_start, LOSS_SCALE_M, 25, bounds);
    iterations += it;
    let mut st = judge.stage("aniso", &ani, ani_cost, Some(&par), true);
    let (mut kept, mut kept_cost, mut is_affine) = (par, sim_cost, false);
    if ani_cost < sim_cost * (1.0 - ANISO_MIN_GAIN)
        && ani.p[4].abs() < 0.99 * MAX_LOG_ANISO
        && (ani.p[0] - par.p[0]).abs() < 0.05
        && st.supported()
    {
        (kept, kept_cost, is_affine, converged) = (ani, ani_cost, true, ani_conv);
        stages[0].kept = false;
    } else {
        st.kept = false;
    }
    stages.push(st);
    let out = kept.to_affine();
    // A large map: a little perspective for its projection.
    let (_, my) = out.apply(full.cx, full.cy);
    let span_m = 2.0 * full.sn * out.mean_scale() * crate::geo::merc_to_latlon(0.0, my).0.to_radians().cos();
    if span_m > REGIONAL_SPAN_M {
        let start = Params::from_affine(&out, Model::Projective, &full);
        let (proj, proj_cost, it, proj_conv) = lm(&fine_data, fine, start, LOSS_SCALE_M, 25, bounds);
        iterations += it;
        let mut st = judge.stage("projective", &proj, proj_cost, Some(&kept), true);
        if proj_cost < kept_cost * (1.0 - REGIONAL_MIN_GAIN) && plausible_regional(&proj, &out) && st.supported() {
            for s in stages.iter_mut() {
                s.kept = false;
            }
            stages.push(st);
            let r = metrics(&full, dem, &Fit::Projective(proj.to_homography()), iterations, proj_conv, true);
            return judge.finish(r, stages, &proj);
        }
        st.kept = false;
        stages.push(st);
    }
    judge.finish(metrics(&full, dem, &Fit::Affine(out), iterations, converged, is_affine), stages, &kept)
}

/// Judges the fits of one refinement: their spread at the corners of the
/// extent (`jackknife`), how far apart two of them are there, how much of
/// the extent the samples cover.
struct Judge<'a> {
    data: &'a Data,
    dem: &'a DemMosaic,
    groups: Groups,
    extent: [f64; 4],
}

impl<'a> Judge<'a> {
    fn new(data: &'a Data, dem: &'a DemMosaic, extent: Option<[u32; 4]>) -> Judge<'a> {
        let extent = extent.map(|e| e.map(f64::from)).unwrap_or_else(|| {
            let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
            for (&x, &y) in data.xs.iter().zip(&data.ys) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
            if x0 <= x1 {
                [x0, y0, x1, y1]
            } else {
                [0.0; 4]
            }
        });
        Judge { data, dem, groups: Groups::new(data), extent }
    }

    /// Ground metres per Mercator metre at the extent's centre under `p`.
    fn cos(&self, p: &Params) -> f64 {
        let (_, my) = p.apply(0.5 * (self.extent[0] + self.extent[2]), 0.5 * (self.extent[1] + self.extent[3]));
        crate::geo::merc_to_latlon(0.0, my).0.to_radians().cos()
    }

    /// `RefineResult::spread_m` of `p`.
    fn spread(&self, p: &Params) -> f64 {
        corner_spread(jackknife(self.data, self.dem, p, &self.groups).as_deref(), &self.extent) * self.cos(p)
    }

    /// A stage fitted from `prev`; its spread only when `judged` (a model
    /// the selection tests: `StageFit::supported`), else NaN until kept.
    fn stage(&self, model: &'static str, p: &Params, cost: f64, prev: Option<&Params>, judged: bool) -> StageFit {
        StageFit {
            model,
            homography: p.to_homography(),
            cost,
            spread_m: if judged { self.spread(p) } else { f64::NAN },
            shift_m: prev.map_or(0.0, |q| corner_shift(p, q, &self.extent) * self.cos(p)),
            kept: true,
        }
    }

    /// `r` with the spread of its kept stage (`kept`, judged now if it was
    /// not and the fit could be reported at all), the cover and the stages.
    fn finish(&self, mut r: RefineResult, mut stages: Vec<StageFit>, kept: &Params) -> RefineResult {
        let hopeless = fit_quality(&r) == 0.0;
        if let Some(st) = stages.iter_mut().rev().find(|s| s.kept) {
            if st.spread_m.is_nan() && !hopeless {
                st.spread_m = self.spread(kept);
            }
            r.spread_m = st.spread_m;
        }
        r.extent_cover = extent_cover(self.data, &self.extent);
        r.stages = stages;
        r
    }
}

impl StageFit {
    /// Do the samples support this model over the previous one: does it
    /// move the extent's corners clearly further than its own jackknife
    /// spread there (`MODEL_MIN_Z`)? Its extra parameters must be held by
    /// the whole map, not by one area of it.
    fn supported(&self) -> bool {
        self.spread_m.is_finite() && self.shift_m >= MODEL_MIN_Z * self.spread_m
    }
}

/// A regional projection's perspective: tiny terms, the local affine part
/// near-conformal and at the scale of `t`.
fn plausible_regional(p: &Params, t: &Affine) -> bool {
    let (g, h) = (p.p[6], p.p[7]);
    let local = p.to_affine();
    g.is_finite()
        && h.is_finite()
        && g.abs() <= REGIONAL_MAX_PERSPECTIVE
        && h.abs() <= REGIONAL_MAX_PERSPECTIVE
        && local.det() * t.det() > 0.0
        && anisotropy(&local) < AFFINE_MAX_ANISOTROPY
        && (local.mean_scale() / t.mean_scale()).ln().abs() < 0.05
}

/// Photo mode: affine from `PHOTO_AFFINE_LEVEL` down, then projective from
/// `PHOTO_PROJECTIVE_LEVEL` down (coarse-to-fine, so the residual
/// perspective of a flattened photo, a few hundred metres at the corners, is
/// inside the basin of convergence). Each stage is kept only if it lowers the
/// finest-level robust cost meaningfully and stays plausible.
struct PhotoStages<'a> {
    full: &'a Data,
    fine_data: &'a Data,
    dem: &'a PreparedDem,
    level_data: &'a dyn Fn(usize) -> Data,
    loss: &'a dyn Fn(usize) -> f64,
    bounds: (f64, f64),
}

impl PhotoStages<'_> {
    /// LM from `start`, coarse-to-fine from level `top`.
    /// Returns (params, finest-level cost, iterations, converged).
    fn descend(&self, start: Params, top: usize) -> (Params, f64, usize, bool) {
        let top = top.min(self.dem.levels.len() - 1);
        let (mut par, mut iterations, mut converged) = (start, 0, false);
        for lvl in (0..=top).rev() {
            let dl = if lvl == 0 { None } else { Some((self.level_data)(lvl)) };
            let data = dl.as_ref().unwrap_or(self.fine_data);
            let (p, _, it, conv) = lm(data, &self.dem.levels[lvl], par, (self.loss)(lvl), 25, self.bounds);
            par = p;
            iterations += it;
            converged = conv;
        }
        let cost = evaluate_params(self.fine_data, &self.dem.levels[0], &par, LOSS_SCALE_M, false).cost;
        (par, cost, iterations, converged)
    }

    fn run(
        &self,
        sim: Params,
        sim_cost: f64,
        mut iterations: usize,
        mut converged: bool,
        judge: &Judge,
    ) -> RefineResult {
        let ts = sim.to_affine();
        let mut stages = vec![judge.stage("similarity", &sim, sim_cost, None, false)];
        // Anisotropic affine (a document scanner guesses the page aspect
        // ratio), from the similarity as is and stretched either way: the
        // plausible result with the lowest cost.
        let plausible = |t: &Affine| {
            anisotropy(t) < PHOTO_MAX_ANISOTROPY
                && (t.mean_scale() / ts.mean_scale()).ln().abs() < PHOTO_MAX_LOG_SCALE_CHANGE
        };
        let start = Params::from_affine(&ts, Model::Affine, self.full);
        let runs: Vec<(Params, f64, usize, bool)> = [0.0, PHOTO_AFFINE_STRETCH, -PHOTO_AFFINE_STRETCH]
            .iter()
            .map(|&k| self.descend(start.stretched(k), PHOTO_AFFINE_LEVEL))
            .collect();
        iterations += runs.iter().map(|r| r.2).sum::<usize>();
        let (mut best, mut best_cost, mut is_affine) = (sim, sim_cost, false);
        if let Some(&(aff, aff_cost, _, aff_conv)) =
            runs.iter().filter(|r| plausible(&r.0.to_affine())).min_by(|a, b| a.1.total_cmp(&b.1))
        {
            let mut st = judge.stage("affine", &aff, aff_cost, Some(&sim), false);
            if aff_cost < sim_cost * (1.0 - AFFINE_MIN_GAIN) {
                (best, best_cost, is_affine, converged) = (aff, aff_cost, true, aff_conv);
                stages[0].kept = false;
            } else {
                st.kept = false;
            }
            stages.push(st);
        }
        // Projective: the perspective left by imperfect page corners.
        let start = Params::from_affine(&best.to_affine(), Model::Projective, self.full);
        let (proj, proj_cost, it, proj_conv) = self.descend(start, PHOTO_PROJECTIVE_LEVEL);
        iterations += it;
        let mut st = judge.stage("projective", &proj, proj_cost, Some(&best), false);
        if proj_cost < best_cost * (1.0 - PROJECTIVE_MIN_GAIN) && self.plausible_projective(&proj, &ts) {
            for s in stages.iter_mut() {
                s.kept = false;
            }
            stages.push(st);
            let h = proj.to_homography();
            let r = metrics(self.full, self.dem, &Fit::Projective(h), iterations, proj_conv, true);
            return judge.finish(r, stages, &proj);
        }
        st.kept = false;
        stages.push(st);
        let r = metrics(self.full, self.dem, &Fit::Affine(best.to_affine()), iterations, converged, is_affine);
        judge.finish(r, stages, &best)
    }

    /// Small projective terms, no fold over the samples' extent, a plausible
    /// local affine part.
    fn plausible_projective(&self, p: &Params, sim: &Affine) -> bool {
        let (g, h) = (p.p[6], p.p[7]);
        if !(g.is_finite() && h.is_finite()) || g.abs() > PHOTO_MAX_PERSPECTIVE || h.abs() > PHOTO_MAX_PERSPECTIVE {
            return false;
        }
        // Denominator over the corners of the normalised sample extent (|u|, |v| ≤ 1).
        let q_min = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .iter()
            .map(|&(u, v)| 1.0 + g * u + h * v)
            .fold(f64::INFINITY, f64::min);
        let local = p.to_affine();
        q_min > 0.5
            && local.det() * sim.det() > 0.0
            && anisotropy(&local) < PHOTO_MAX_ANISOTROPY
            && (local.mean_scale() / sim.mean_scale()).ln().abs() < PHOTO_MAX_LOG_SCALE_CHANGE
    }
}

/// Ratio of the larger to the smaller singular value of the linear part.
fn anisotropy(t: &Affine) -> f64 {
    let (a, b, c, d) = (t.a, t.b, t.d, t.e);
    let s1 = a * a + b * b + c * c + d * d;
    let det = (a * d - b * c).abs();
    let disc = (s1 * s1 - 4.0 * det * det).max(0.0).sqrt();
    let smax = ((s1 + disc) / 2.0).sqrt();
    let smin = ((s1 - disc) / 2.0).max(0.0).sqrt();
    if smin > 0.0 {
        smax / smin
    } else {
        f64::INFINITY
    }
}

/// Evaluate a transform without optimising (used to verify grid results).
pub fn evaluate(samples: &ContourSamples, dem: &DemMosaic, t: &Affine) -> RefineResult {
    evaluate_prepared(samples, &PreparedDem::new(dem), t)
}

/// `evaluate` with a prepared DEM.
pub fn evaluate_prepared(samples: &ContourSamples, dem: &PreparedDem, t: &Affine) -> RefineResult {
    metrics(&Data::new(samples), dem, &Fit::Affine(*t), 0, false, false)
}

/// `evaluate_prepared` for any pixel → Mercator homography (photo-mode grid fits).
pub fn evaluate_prepared_homography(samples: &ContourSamples, dem: &PreparedDem, h: &Homography) -> RefineResult {
    let fit = if h.is_affine() { Fit::Affine(h.to_affine()) } else { Fit::Projective(*h) };
    metrics(&Data::new(samples), dem, &fit, 0, false, false)
}

/// One contour sample under a fitted transform: the data a smooth
/// correction on top of the transform (the photo `warp`) is fitted to. A
/// sample belongs where the DEM has its segment's level: moving it by `d`
/// changes its residual by about `g·d`, so a residual `r` on a slope `g`
/// asks for `−r·g/|g|²` across the contour (along it the residual says
/// nothing). The levels are free per segment and absorb whatever offset a
/// whole segment shares, so a correction is fitted jointly with them, like
/// the refinement does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SampleFit {
    /// Sample position (pixels) and its contour segment.
    pub px: [f32; 2],
    pub seg: u32,
    /// Mercator position under the transform.
    pub merc: (f64, f64),
    /// The segment's robust elevation level and the sample's DEM elevation
    /// minus it (metres).
    pub level_m: f64,
    pub residual_m: f64,
    /// DEM gradient at `merc` (metres of elevation per Mercator metre).
    pub grad: (f64, f64),
}

/// Every contour sample under the pixel → Mercator homography `h` on the
/// finest level of `dem` (samples off the DEM, or on segments with too few
/// samples on it, are left out).
pub fn sample_fits(samples: &ContourSamples, dem: &PreparedDem, h: &Homography) -> Vec<SampleFit> {
    let d = Data::new(samples);
    let (Some(par), Some(fine)) = (Params::from_homography(h, &d), dem.levels.first()) else {
        return Vec::new();
    };
    let e = evaluate_params(&d, fine, &par, LOSS_SCALE_M, false);
    sample_all(&d, fine, &par)
        .into_iter()
        .enumerate()
        .filter(|(i, _)| e.resid[*i].is_finite())
        .map(|(i, [elev, gx, gy, mx, my])| SampleFit {
            px: samples.pts[i],
            seg: samples.seg[i],
            merc: (mx, my),
            level_m: elev - e.resid[i],
            residual_m: e.resid[i],
            grad: (gx, gy),
        })
        .collect()
}

fn metrics(d: &Data, dem: &PreparedDem, fit: &Fit, iterations: usize, converged: bool, affine: bool) -> RefineResult {
    let (transform, homography) = match fit {
        Fit::Affine(t) => (*t, None),
        Fit::Projective(h) => (h.local_affine(d.cx, d.cy), Some(*h)),
    };
    // Ground metres per map pixel and per finest DEM cell at the samples' centroid.
    let cos = {
        let (_, my) = transform.apply(d.cx, d.cy);
        crate::geo::merc_to_latlon(0.0, my).0.to_radians().cos()
    };
    let px_ground_m = transform.mean_scale() * cos;
    let dem_cell_m = dem.levels.first().map_or(0.0, |l| l.res * cos);
    let empty = RefineResult {
        transform,
        homography,
        cost_m: f64::INFINITY,
        median_seg_std_m: f64::INFINITY,
        inlier_fraction: 0.0,
        dem_coverage: 0.0,
        iterations,
        converged,
        explained_variance: 0.0,
        relief_m: 0.0,
        n_samples: 0,
        n_segments: 0,
        affine,
        contour_interval_m: 0.0,
        interval_snr: 0.0,
        px_ground_m,
        dem_cell_m,
        gradient_alignment: f64::NAN,
        spread_m: f64::NAN,
        extent_cover: f64::NAN,
        stages: Vec::new(),
    };
    let Some(fine) = dem.levels.first() else {
        return empty;
    };
    if d.xs.is_empty() {
        return empty;
    }
    let par = match fit {
        Fit::Affine(t) => Params::from_affine(t, Model::Affine, d),
        Fit::Projective(h) => match Params::from_homography(h, d) {
            Some(p) => p,
            None => return empty,
        },
    };
    let e = evaluate_params(d, fine, &par, LOSS_SCALE_M, false);
    let n = d.xs.len();
    let on_dem = (0..n)
        .filter(|&i| {
            let (mx, my) = fit.apply(d.xs[i], d.ys[i]);
            fine.sample(mx, my).is_finite()
        })
        .count();
    let used: Vec<usize> = (0..n).filter(|&i| e.resid[i].is_finite()).collect();
    if used.is_empty() {
        return RefineResult { dem_coverage: on_dem as f64 / n as f64, ..empty };
    }
    let nu = used.len() as f64;
    let cost_m = used.iter().map(|&i| e.resid[i].abs().min(RESIDUAL_CLIP_M)).sum::<f64>() / nu;
    let inliers = used.iter().filter(|&&i| e.resid[i].abs() < INLIER_M).count() as f64 / nu;
    let mean_h = used.iter().map(|&i| e.elev[i]).sum::<f64>() / nu;
    let var_h = used.iter().map(|&i| (e.elev[i] - mean_h).powi(2)).sum::<f64>() / nu;
    let var_r = used.iter().map(|&i| e.resid[i].abs().min(RESIDUAL_CLIP_M).powi(2)).sum::<f64>() / nu;
    let explained_variance = if var_h > 0.0 { 1.0 - var_r / var_h } else { 0.0 };
    // Per-segment standard deviations.
    let mut s1 = vec![0.0f64; d.n_segs];
    let mut s2 = vec![0.0f64; d.n_segs];
    let mut cnt = vec![0usize; d.n_segs];
    for &i in &used {
        let s = d.seg[i] as usize;
        s1[s] += e.elev[i];
        s2[s] += e.elev[i] * e.elev[i];
        cnt[s] += 1;
    }
    let mut stds: Vec<f64> = (0..d.n_segs)
        .filter(|&s| cnt[s] >= MIN_SEG_SAMPLES)
        .map(|s| {
            let m = s1[s] / cnt[s] as f64;
            (s2[s] / cnt[s] as f64 - m * m).max(0.0).sqrt()
        })
        .collect();
    let n_segments = stds.len();
    let median_seg_std_m = median(&mut stds).unwrap_or(f64::INFINITY);
    let mut abs_r: Vec<f64> = used.iter().map(|&i| e.resid[i].abs()).collect();
    let sigma_r = 1.4826 * median(&mut abs_r).unwrap_or(f64::INFINITY);
    let contour_interval_m = interval_estimate(d, &e, &used);
    let interval_snr = if sigma_r > 0.0 { contour_interval_m / sigma_r } else { 0.0 };
    let gradient_alignment = gradient_alignment(d, fine, &par, &transform, &used);
    RefineResult {
        transform,
        cost_m,
        median_seg_std_m,
        inlier_fraction: inliers,
        dem_coverage: on_dem as f64 / n as f64,
        iterations,
        converged,
        explained_variance,
        relief_m: var_h.sqrt(),
        n_samples: used.len(),
        n_segments,
        affine,
        contour_interval_m,
        interval_snr,
        homography,
        px_ground_m,
        dem_cell_m,
        gradient_alignment,
        spread_m: f64::NAN,
        extent_cover: f64::NAN,
        stages: Vec::new(),
    }
}

/// How much the contours cross the terrain's slope under the fit: Σ|g·t̂| /
/// Σ|g| over the samples (g the DEM gradient, t̂ the contour's direction there,
/// mapped to Mercator). Contour lines run across the slope: a right fit stays
/// well under the 2/π ≈ 0.64 of lines lying any which way, and flat ground,
/// where any placement keeps a line's level, weighs little. Few and short
/// segments (a sparse mask) can keep their levels on wrong terrain by
/// lying along gentle slopes; they still cross the steep ones.
fn gradient_alignment(d: &Data, dem: &DemMosaic, par: &Params, t: &Affine, used: &[usize]) -> f64 {
    let samp = sample_all(d, dem, par);
    let (mut along, mut total) = (0.0f64, 0.0f64);
    for &i in used {
        let s = d.seg[i];
        let prev = if i > 0 && d.seg[i - 1] == s { i - 1 } else { i };
        let next = if i + 1 < d.xs.len() && d.seg[i + 1] == s { i + 1 } else { i };
        if prev == next {
            continue;
        }
        let (tx, ty) = t.apply_vec(d.xs[next] - d.xs[prev], d.ys[next] - d.ys[prev]);
        let tn = tx.hypot(ty);
        let [_, gx, gy, _, _] = samp[i];
        let gn = gx.hypot(gy);
        if tn <= 0.0 || !gn.is_finite() {
            continue;
        }
        along += (gx * tx + gy * ty).abs() / tn;
        total += gn;
    }
    if total > 0.0 {
        along / total
    } else {
        f64::NAN
    }
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let k = v.len() / 2;
    Some(*v.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)).1)
}

/// Queries used for the contour-interval estimate.
const INTERVAL_QUERIES: usize = 4000;

/// Median elevation difference between each sampled contour and the nearest
/// other contour across it (within ~1/60 of the image diagonal, at > 60° to
/// the line so pieces of the same line are not taken as neighbours).
fn interval_estimate(d: &Data, e: &Eval, used: &[usize]) -> f64 {
    use std::collections::HashMap;
    if used.len() < 10 {
        return 0.0;
    }
    let (mut xmin, mut xmax, mut ymin, mut ymax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &i in used {
        xmin = xmin.min(d.xs[i]);
        xmax = xmax.max(d.xs[i]);
        ymin = ymin.min(d.ys[i]);
        ymax = ymax.max(d.ys[i]);
    }
    let radius = ((xmax - xmin).hypot(ymax - ymin) / 60.0).max(10.0);
    let key = |i: usize| ((d.xs[i] / radius).floor() as i64, (d.ys[i] / radius).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for &i in used {
        grid.entry(key(i)).or_default().push(i);
    }
    let seg_mean = |i: usize| e.elev[i] - e.resid[i];
    let stride = used.len().div_ceil(INTERVAL_QUERIES).max(1);
    let mut diffs: Vec<f64> = used
        .par_iter()
        .step_by(stride)
        .filter_map(|&i| {
            let s = d.seg[i];
            // Tangent from the neighbouring samples of the same segment.
            let prev = if i > 0 && d.seg[i - 1] == s { i - 1 } else { i };
            let next = if i + 1 < d.xs.len() && d.seg[i + 1] == s { i + 1 } else { i };
            let (tx, ty) = (d.xs[next] - d.xs[prev], d.ys[next] - d.ys[prev]);
            let tn = tx.hypot(ty);
            if tn == 0.0 {
                return None;
            }
            let (kx, ky) = key(i);
            let mut best: Option<(f64, usize)> = None;
            for gy in ky - 1..=ky + 1 {
                for gx in kx - 1..=kx + 1 {
                    for &j in grid.get(&(gx, gy)).map(|v| v.as_slice()).unwrap_or(&[]) {
                        if d.seg[j] == s {
                            continue;
                        }
                        let (vx, vy) = (d.xs[j] - d.xs[i], d.ys[j] - d.ys[i]);
                        let dist = vx.hypot(vy);
                        if dist > radius || dist == 0.0 || (vx * tx + vy * ty).abs() > 0.5 * dist * tn {
                            continue;
                        }
                        if best.is_none_or(|b| dist < b.0) {
                            best = Some((dist, j));
                        }
                    }
                }
            }
            best.map(|(_, j)| (seg_mean(i) - seg_mean(j)).abs())
        })
        .collect();
    median(&mut diffs).unwrap_or(0.0)
}

/// Smooth ramp 0 → 1 between `lo` and `hi`.
fn ramp(x: f64, lo: f64, hi: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How well a refined transform explains the map's contours (0..1), from the
/// fine fit alone (no search evidence). Used to verify grid results too.
/// Every factor must hold (they multiply):
///
/// * explained variance: ≥ 0.97 → 1, ≤ 0.80 → 0 (right fits explain ~99.9 %
///   of the elevation variation along contours, wrong ones well under 90 %);
/// * median within-segment std: ≤ s → 1, ≥ 3·s → 0, s = `seg_std_floor_m`:
///   4 m on zoom-13 tiles (DEM noise is ~2–3 m), more on the coarser tiles
///   and for the coarser pixels of small-scale maps (a 1:250 000 sheet at
///   25 m/px fits its true place at 11–25 m on zoom 10, wrong places at
///   40–80 m);
/// * contour-interval SNR (neighbouring lines must differ by one interval,
///   above the residual noise): ≥ 2.0 → 1, ≤ 1.0 → 0. Grootkloof (5 m
///   contours, SRTM) gives 2.8; wrong fits give < 0.8, because they squeeze
///   many lines into little relief or scatter them over unrelated terrain;
/// * a plausible estimated contour interval: ≥ 1.6 m → 1, ≤ 0.8 m → 0
///   (topographic maps use 1–50 m; wrong fits on smooth terrain imply ~0.5 m);
/// * DEM coverage: ≥ 0.8 → 1, ≤ 0.3 → 0;
/// * enough evidence: ≥ 20 segments and ≥ 30 m relief, else scaled down.
pub fn fit_confidence(r: &RefineResult) -> f64 {
    let cov = ramp(r.dem_coverage, 0.3, 0.8);
    (fit_quality(r) * cov * fit_evidence(r)).clamp(0.0, 1.0)
}

/// The factors of `fit_confidence` that say how well the contours fit where
/// they landed on the DEM (explained variance, segment std, interval SNR,
/// interval), 0..1, whatever the coverage and amount of evidence.
pub fn fit_quality(r: &RefineResult) -> f64 {
    if r.n_samples == 0 || !r.cost_m.is_finite() {
        return 0.0;
    }
    let ev = ramp(r.explained_variance, 0.80, 0.97);
    let floor = seg_std_floor_m(r);
    let sd = 1.0 - ramp(r.median_seg_std_m, floor, 3.0 * floor);
    let snr = ramp(r.interval_snr, 1.0, 2.0);
    let interval = ramp(r.contour_interval_m, 0.8, 1.6);
    let across = if r.gradient_alignment.is_finite() {
        let (good, bad) = alignment_limits(r.n_segments);
        1.0 - ramp(r.gradient_alignment, good, bad)
    } else {
        1.0
    };
    (ev * sd * snr * interval * across).clamp(0.0, 1.0)
}

/// Median within-segment elevation spread (metres) a right fit stays under:
/// 4 m on zoom-13 tiles (~17–19 m cells), growing with the square root of
/// the DEM cell (interpolating a coarser DEM across steep ground: 8 m on
/// zoom 11, 11 m on zoom 10) and with the map's pixel (a line drawn 0.35 px
/// off on a 1:40 slope: 9 m at 25 m/px, 17.5 m at 50 m/px). 300-dpi sheets
/// of 1:50 000 and larger refine on zoom 13 at under 12 m/px: 4 m, as always.
pub fn seg_std_floor_m(r: &RefineResult) -> f64 {
    let dem = if r.dem_cell_m.is_finite() && r.dem_cell_m > 0.0 {
        SEG_STD_FLOOR_M * (r.dem_cell_m / ZOOM13_CELL_M).sqrt()
    } else {
        0.0
    };
    let px = if r.px_ground_m.is_finite() { SEG_STD_PER_PX * r.px_ground_m } else { 0.0 };
    SEG_STD_FLOOR_M.max(dem).max(px)
}

/// `gradient_alignment` from which a fit counts as crossing the terrain
/// (factor 1 → 0 of `fit_quality`), by the number of segments: the fewer,
/// the more a wrong fit can lay them along the slope. Right fits: 0.11–0.26
/// on the benchmark's maps and the real sheets whatever their segments
/// (0.27–0.31 on flat ground, 0.36 for a 1:250 000 sheet of 1 745 segments
/// on zoom-10 tiles). Wrong fits (the refined candidates of searches 90–110 km
/// from 76 maps): 0.34 at the least with 15–21 segments (sparse masks on
/// Gaia-like pages; one fitted its levels at 0.70), 0.46 at the least with
/// 900 segments or more.
fn alignment_limits(n_segments: usize) -> (f64, f64) {
    let t = ((n_segments.max(1) as f64 / 50.0).ln() / (400.0f64 / 50.0).ln()).clamp(0.0, 1.0);
    let good = 0.26 + t * (0.38 - 0.26);
    (good, good + 0.07 + t * 0.01)
}

/// `seg_std_floor_m` on zoom-13 tiles, and their ground cell at the equator.
const SEG_STD_FLOOR_M: f64 = 4.0;
const ZOOM13_CELL_M: f64 = 19.1;
/// …and per ground metre of the map's pixel.
const SEG_STD_PER_PX: f64 = 0.35;

/// The factors of `fit_confidence` that say how much there was to judge by
/// (0..1): contour segments that landed on the DEM, the terrain relief
/// under them and how much of the map they span (`span_evidence`). Near 0,
/// a low fit confidence says nothing (a map without contours, flat ground,
/// no terrain data).
pub fn fit_evidence(r: &RefineResult) -> f64 {
    if r.n_samples == 0 || !r.cost_m.is_finite() {
        return 0.0;
    }
    (ramp(r.n_segments as f64, 5.0, 20.0) * ramp(r.relief_m, 10.0, 30.0) * span_evidence(r)).clamp(0.0, 1.0)
}

/// How much of the map the samples span (`RefineResult::extent_cover`;
/// 1 when unknown): full from `SPAN_FULL` of its 8 × 8 cells, falling to
/// `SPAN_FLOOR` at `SPAN_NONE`. A fit to contours left in a corner of the
/// map places that corner, and says little about the rest: a Gaia raster at
/// JPEG quality 25 and lowered contrast kept 18 short segments in its west
/// (16 % of the cells) and fitted them 115 m off at 0.97.
fn span_evidence(r: &RefineResult) -> f64 {
    if r.extent_cover.is_finite() {
        SPAN_FLOOR + (1.0 - SPAN_FLOOR) * ramp(r.extent_cover, SPAN_NONE, SPAN_FULL)
    } else {
        1.0
    }
}

/// `span_evidence` limits.
const SPAN_NONE: f64 = 0.1;
const SPAN_FULL: f64 = 0.35;
const SPAN_FLOOR: f64 = 0.25;

/// Overall confidence (0..1) that a contour match is right, combining the
/// coarse search's distinctiveness with the fine fit.
///
/// * search: peak_ratio (significance of the best location over the best
///   other location ≥ 2 km away) ≥ 1.6 → 1, ≤ 1.15 → 0; true matches score
///   ≈ 2, a map outside the search area ≈ 1.0–1.1;
/// * fit: `fit_confidence`.
///
/// The result is 0.35·search + 0.65·fit, but never more than fit + 0.1 or
/// 2·search + 0.1: both pieces of evidence must agree. Thresholds: ≥ 0.7 is
/// a confident match, ≤ 0.3 means "not found".
pub fn confidence(c: &Candidate, r: &RefineResult) -> f64 {
    combine(search_evidence(c), fit_confidence(r))
}

fn search_evidence(c: &Candidate) -> f64 {
    ramp(c.peak_ratio as f64, 1.15, 1.6)
}

fn combine(search: f64, fit: f64) -> f64 {
    (0.35 * search + 0.65 * fit).min(fit + 0.1).min(2.0 * search + 0.1).clamp(0.0, 1.0)
}

/// Pick the best of several refined candidates (e.g. the coarse top-k, each
/// refined on its own fine DEM) and return (index, confidence).
///
/// The winner is the best fit (ties go to the earlier, more significant
/// candidate). When its fit beats every other candidate's by a clear margin
/// (fit-confidence gap ≥ 0.7 → full, ≤ 0.3 → none), that counts as search
/// evidence too, so a right match in repetitive terrain (ambiguous coarse
/// peak ratio) is still accepted once the alternatives were refined and failed.
pub fn select_refined(results: &[(Candidate, RefineResult)]) -> Option<(usize, f64)> {
    if results.is_empty() {
        return None;
    }
    let fits: Vec<f64> = results.iter().map(|(_, r)| fit_confidence(r)).collect();
    let best = fits.iter().enumerate().fold(0, |b, (i, &f)| if f > fits[b] + 1e-9 { i } else { b });
    let mut search = search_evidence(&results[best].0);
    if results.len() > 1 {
        let second = fits.iter().enumerate().filter(|&(i, _)| i != best).map(|(_, &f)| f).fold(0.0, f64::max);
        search = search.max(ramp(fits[best] - second, 0.3, 0.7));
    }
    Some((best, combine(search, fits[best])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_roundtrip() {
        let t = Affine::pixel_to_merc_similarity(2.1, 0.05, 1750.0, 2480.0, 3.05e6, -2.98e6);
        let s = ContourSamples { pts: vec![[0.0, 0.0], [2000.0, 4000.0]], seg: vec![0, 0], n_segs: 1 };
        let d = Data::new(&s);
        assert_eq!((d.cx, d.cy), (1000.0, 2000.0));
        assert!((d.sn - 0.5 * 2000f64.hypot(4000.0)).abs() < 1e-9);
        let p = Params::from_affine(&t, Model::Similarity, &d);
        let back = p.to_affine();
        for (x, y) in [(0.0, 0.0), (3500.0, 4900.0)] {
            let (a, b) = (t.apply(x, y), back.apply(x, y));
            assert!((a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
        }
        // Aniso: a similarity round-trips with u = 0; a page stretched along
        // image x by 1.5 % round-trips with u = ln(1.015) / 2.
        let a = Params::from_affine(&t, Model::Aniso, &d);
        assert!(a.p[4].abs() < 1e-12 && (a.p[1] - 0.05).abs() < 1e-12);
        let wide = Affine { a: t.a * 1.015, d: t.d * 1.015, ..t };
        let a = Params::from_affine(&wide, Model::Aniso, &d);
        assert!((a.p[4] - 0.5 * 1.015f64.ln()).abs() < 1e-9, "{a:?}");
        for (x, y) in [(0.0, 0.0), (3500.0, 4900.0)] {
            let (p, q) = (wide.apply(x, y), a.to_affine().apply(x, y));
            assert!((p.0 - q.0).abs() < 1e-6 && (p.1 - q.1).abs() < 1e-6);
        }
        let q = Params::from_affine(&t, Model::Affine, &d).to_affine();
        assert!((q.a - t.a).abs() < 1e-12 && (q.f - t.f).abs() < 1e-6);
        assert!((anisotropy(&t) - 1.0).abs() < 1e-9);
        // Projective: from an affine it is that affine; homography round trip.
        let pr = Params::from_affine(&t, Model::Projective, &d);
        for (x, y) in [(0.0, 0.0), (3500.0, 4900.0)] {
            let (a, (mx, my, _)) = (t.apply(x, y), pr.project(x, y));
            let b = pr.to_homography().apply(x, y);
            assert!((a.0 - mx).abs() < 1e-6 && (a.1 - my).abs() < 1e-6);
            assert!((a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
        }
        let mut pr2 = pr;
        pr2.p[6] = 0.03;
        pr2.p[7] = -0.05;
        pr2.p[2] = 120.0;
        let h = pr2.to_homography();
        let back = Params::from_homography(&h, &d).unwrap();
        for (x, y) in [(0.0, 0.0), (3500.0, 4900.0), (1000.0, 2000.0)] {
            let ((ax, ay, _), b) = (pr2.project(x, y), back.to_homography().apply(x, y));
            let c = h.apply(x, y);
            assert!((ax - b.0).abs() < 1e-5 && (ay - b.1).abs() < 1e-5, "{x} {y}");
            assert!((ax - c.0).abs() < 1e-5 && (ay - c.1).abs() < 1e-5, "{x} {y}");
        }
    }

    #[test]
    fn ramp_bounds() {
        assert_eq!(ramp(0.0, 1.0, 2.0), 0.0);
        assert_eq!(ramp(3.0, 1.0, 2.0), 1.0);
        assert!((ramp(1.5, 1.0, 2.0) - 0.5).abs() < 1e-12);
    }

    /// DEM tiles (zoom 13) of a synthetic surface over `fp`, and its 10 m
    /// contours traced in a 2000×2400 px image through `truth` (pixel →
    /// Mercator), by marching along each 6th row and recording level crossings.
    fn synthetic_case(truth: &dyn Fn(f64, f64) -> (f64, f64), fp: &MercBox) -> (ContourSamples, DemMosaic) {
        use crate::dem::{DemStore, TILE_SIZE};
        let z = 13u8;
        let elev = |mx: f64, my: f64| -> f64 {
            let (u, v) = (mx / 900.0, my / 700.0);
            300.0 * (u.sin() * v.cos()) + 80.0 * ((mx + 2.0 * my) / 400.0).sin()
        };
        let mut store = DemStore::new();
        for t in crate::dem::tiles_covering(z, fp) {
            let b = crate::dem::tile_bounds(t);
            let res = crate::dem::resolution(z);
            let mut e = vec![0.0f32; TILE_SIZE * TILE_SIZE];
            for r in 0..TILE_SIZE {
                for c in 0..TILE_SIZE {
                    let (mx, my) = (b.min_x + (c as f64 + 0.5) * res, b.max_y - (r as f64 + 0.5) * res);
                    e[r * TILE_SIZE + c] = elev(mx, my) as f32;
                }
            }
            store.add_elevations(t, e);
        }
        let dem = store.mosaic(z, fp);
        let mut s = ContourSamples::default();
        let mut seg_of_level = std::collections::HashMap::new();
        for y in (0..2400).step_by(6) {
            let mut prev = None;
            for x in 0..2000 {
                let (mx, my) = truth(x as f64 + 0.5, y as f64 + 0.5);
                let lvl = (elev(mx, my) / 10.0).floor() as i64;
                if let Some(p) = prev {
                    if p != lvl {
                        let key = (lvl.max(p), y / 240, x / 250);
                        let n = seg_of_level.len() as u32;
                        let id = *seg_of_level.entry(key).or_insert(n);
                        s.pts.push([x as f32, y as f32 + 0.5]);
                        s.seg.push(id);
                    }
                }
                prev = Some(lvl);
            }
        }
        s.n_segs = seg_of_level.len();
        (s, dem)
    }

    /// The fine level's cap thins each segment by the stride but keeps at
    /// least `FINE_MIN_SEG_SAMPLES` of it: no segment is lost.
    #[test]
    fn thin_segments_keeps_every_segment() {
        let mut s = ContourSamples::default();
        for (k, n) in [3usize, 10, 100, 41].into_iter().enumerate() {
            for i in 0..n {
                s.pts.push([i as f32, k as f32 * 10.0]);
                s.seg.push(k as u32);
            }
        }
        s.n_segs = 4;
        let d = Data::new(&s).thin_segments(5);
        let count = |k: u32| d.seg.iter().filter(|&&g| g == k).count();
        assert_eq!([count(0), count(1), count(2), count(3)], [3, 10, 20, 9]);
        assert!(d.seg.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(d.xs.len(), d.ys.len());
    }

    /// Contours across a plane's slope (straight lines of one level): along
    /// the slope's contours the alignment is ~0, turned across them ~1; the
    /// limits depend on the number of segments; the spread a right fit keeps
    /// grows with the DEM cell and the map pixel.
    #[test]
    fn gradient_alignment_and_noise_floor() {
        use crate::dem::{DemStore, TILE_SIZE};
        let z = 13u8;
        let centre = (3.0e6, -2.98e6);
        let fp = MercBox::around(centre.0, centre.1, 3000.0);
        let mut store = DemStore::new();
        for t in crate::dem::tiles_covering(z, &fp) {
            let b = crate::dem::tile_bounds(t);
            let res = crate::dem::resolution(z);
            let e: Vec<f32> = (0..TILE_SIZE * TILE_SIZE)
                .map(|i| (0.1 * (b.min_x + ((i % TILE_SIZE) as f64 + 0.5) * res - centre.0)) as f32)
                .collect();
            store.add_elevations(t, e);
        }
        let dem = PreparedDem::new(&store.mosaic(z, &fp));
        // 30 vertical lines in the image: contours of the plane under `along`.
        let mut s = ContourSamples::default();
        for k in 0..30u32 {
            for y in (0..1000).step_by(4) {
                s.pts.push([100.0 + 50.0 * k as f32, y as f32]);
                s.seg.push(k);
            }
        }
        s.n_segs = 30;
        let along = Affine::pixel_to_merc_similarity(2.0, 0.0, 800.0, 500.0, centre.0, centre.1);
        let across =
            Affine::pixel_to_merc_similarity(2.0, std::f64::consts::FRAC_PI_2, 800.0, 500.0, centre.0, centre.1);
        let a = evaluate_prepared(&s, &dem, &along);
        let b = evaluate_prepared(&s, &dem, &across);
        assert!(a.gradient_alignment < 0.02, "{a:?}");
        assert!(b.gradient_alignment > 0.98, "{b:?}");
        assert!(a.median_seg_std_m < 0.5 && b.median_seg_std_m > 10.0);
        // Few segments: stricter limits than many.
        let (few, many) = (alignment_limits(15), alignment_limits(2000));
        assert!(few.0 < many.0 && few.1 < many.1 && few.1 <= 0.34 && many.0 >= 0.37, "{few:?} {many:?}");
        // Zoom 13 at 1.9 m/px: 4 m; zoom 10 at 25 m/px: ~11 m; 50 m/px: 17.5 m.
        let floor =
            |cell: f64, px: f64| seg_std_floor_m(&RefineResult { dem_cell_m: cell, px_ground_m: px, ..a.clone() });
        assert_eq!(floor(17.3, 1.9), 4.0);
        assert!((floor(133.0, 25.0) - 10.56).abs() < 0.1, "{}", floor(133.0, 25.0));
        assert!((floor(133.0, 50.0) - 17.5).abs() < 1e-9);
    }

    /// Synthetic terrain: contours of a known surface, sampled through a known
    /// transform, are recovered from a 300 m offset start.
    #[test]
    fn recovers_synthetic_offset() {
        let truth = Affine::pixel_to_merc_similarity(2.0, 0.02, 1000.0, 1200.0, 3.0e6, -2.98e6);
        let (s, dem) = synthetic_case(&|x, y| truth.apply(x, y), &refine_dem_box(&truth, 2000, 2400));
        let init = Affine::pixel_to_merc_similarity(2.04, 0.0, 1000.0, 1200.0, 3.0e6 + 250.0, -2.98e6 - 200.0);
        let r = refine(&s, &dem, &init);
        let (a, b) = (r.transform.apply(1000.0, 1200.0), truth.apply(1000.0, 1200.0));
        let err = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
        assert!(err < 6.0, "centre error {err} m, {r:?}");
        assert!((r.transform.mean_scale() / 2.0 - 1.0).abs() < 0.005, "{r:?}");
        assert!(r.explained_variance > 0.99, "{r:?}");
        assert!(r.homography.is_none());
    }

    /// Photo mode: a flattened photo's residual perspective and aspect error
    /// (here 6 % and up to ~4 % scale change across the image) are recovered
    /// by the projective stage; scan mode cannot represent them.
    #[test]
    fn recovers_synthetic_perspective() {
        let sim = Affine::pixel_to_merc_similarity(2.0, 0.02, 1000.0, 1200.0, 3.0e6, -2.98e6);
        // Pixel-space perspective about the image centre.
        let (cx, cy, g, h) = (1000.0, 1200.0, 1.2e-5, -1.5e-5);
        let t = |a: f64, b: f64| Homography { m: [1.0, 0.0, a, 0.0, 1.0, b, 0.0, 0.0, 1.0] };
        let k = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.06, 0.0, g, h, 1.0] };
        let truth = sim.to_homography().mul(&t(cx, cy).mul(&k).mul(&t(-cx, -cy))).normalized();
        let (s, dem) = synthetic_case(&|x, y| truth.apply(x, y), &refine_dem_box(&sim, 2000, 2400));
        let max_err = |m: &Homography| {
            let mut worst = 0.0f64;
            for i in 0..5 {
                for j in 0..5 {
                    let (x, y) = (2000.0 * i as f64 / 4.0, 2400.0 * j as f64 / 4.0);
                    let (a, b) = (m.apply(x, y), truth.apply(x, y));
                    worst = worst.max((a.0 - b.0).hypot(a.1 - b.1));
                }
            }
            worst
        };
        let init = Affine::pixel_to_merc_similarity(2.05, 0.0, 1000.0, 1200.0, 3.0e6 + 150.0, -2.98e6 - 120.0);
        let prep = PreparedDem::new(&dem);
        let photo = refine_prepared_with(&s, &prep, &init, &RefineOptions { photo: true, ..Default::default() });
        let scan = refine_prepared(&s, &prep, &init);
        let hp = photo.homography.expect("projective stage kept");
        let (e_photo, e_scan) = (max_err(&hp), max_err(&scan.transform.to_homography()));
        assert!(e_photo < 8.0, "photo max error {e_photo:.1} m, {photo:?}");
        assert!(e_scan > 3.0 * e_photo && e_scan > 20.0, "scan {e_scan:.1} m vs photo {e_photo:.1} m");
        assert!(photo.explained_variance > 0.99 && fit_confidence(&photo) > 0.9, "{photo:?}");
        // `transform` is the projective fit's affine approximation at the samples' centroid.
        let d = Data::new(&s);
        let (a, b) = (photo.transform.apply(d.cx, d.cy), hp.apply(d.cx, d.cy));
        assert!((a.0 - b.0).hypot(a.1 - b.1) < 1e-6);
        // Evaluating the homography reproduces the refined metrics.
        let ev = evaluate_prepared_homography(&s, &prep, &hp);
        assert!((ev.cost_m - photo.cost_m).abs() < 1e-9 && ev.homography == Some(hp));
    }

    /// Per-sample fits: small residuals under the true transform, larger
    /// ones under a rotated one; levels, residuals and gradients agree with
    /// the DEM.
    #[test]
    fn sample_fits_match_the_dem() {
        let truth = Affine::pixel_to_merc_similarity(2.0, 0.02, 1000.0, 1200.0, 3.0e6, -2.98e6);
        let (s, dem) = synthetic_case(&|x, y| truth.apply(x, y), &refine_dem_box(&truth, 2000, 2400));
        let prep = PreparedDem::new(&dem);
        let median = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.total_cmp(b));
            v[v.len() / 2]
        };
        let exact = sample_fits(&s, &prep, &truth.to_homography());
        assert!(exact.len() > s.len() * 9 / 10, "{} of {}", exact.len(), s.len());
        let (x, y) = (exact[0].px[0] as f64, exact[0].px[1] as f64);
        let m = truth.apply(x, y);
        assert!((exact[0].merc.0 - m.0).hypot(exact[0].merc.1 - m.1) < 1e-6);
        assert!((exact[0].level_m / 10.0 - (exact[0].level_m / 10.0).round()).abs() < 0.1, "{:?}", exact[0]);
        let exact_r = median(exact.iter().map(|f| f.residual_m.abs()).collect());
        assert!(exact_r < 1.0, "{exact_r}");
        // Turned by 1° about the image centre: up to ~50 m off, differently
        // along each segment.
        let turned = Affine::pixel_to_merc_similarity(2.0, 0.02 + 1f64.to_radians(), 1000.0, 1200.0, 3.0e6, -2.98e6);
        let fits = sample_fits(&s, &prep, &turned.to_homography());
        let turned_r = median(fits.iter().map(|f| f.residual_m.abs()).collect());
        assert!(turned_r > 3.0 * exact_r.max(0.3), "{turned_r} vs {exact_r}");
        let fine = &prep.levels[0];
        for f in fits.iter().step_by(97) {
            let (mx, my) = f.merc;
            assert!((f.level_m + f.residual_m - fine.sample(mx, my) as f64).abs() < 1e-3, "{f:?}");
            let gx = (fine.sample(mx + 5.0, my) - fine.sample(mx - 5.0, my)) as f64 / 10.0;
            let gy = (fine.sample(mx, my + 5.0) - fine.sample(mx, my - 5.0)) as f64 / 10.0;
            assert!((gx - f.grad.0).hypot(gy - f.grad.1) < 0.2 * f.grad.0.hypot(f.grad.1) + 0.02, "{f:?} vs {gx} {gy}");
        }
    }

    /// Largest corner error (Mercator metres) of `m` against `truth` over a 2000 × 2400 image.
    fn corner_error(m: &Affine, truth: &Affine) -> f64 {
        [(0.0, 0.0), (2000.0, 0.0), (2000.0, 2400.0), (0.0, 2400.0)]
            .iter()
            .map(|&(x, y)| {
                let (a, b) = (m.apply(x, y), truth.apply(x, y));
                (a.0 - b.0).hypot(a.1 - b.1)
            })
            .fold(0.0, f64::max)
    }

    /// A page drawn 1.5 % wider than its ground (the Gaia PDF pages: 1.6 %)
    /// is recovered by the anisotropic similarity, which the jackknife
    /// supports; its spread is small with contours all over the image.
    #[test]
    fn recovers_page_aspect_ratio() {
        let sim = Affine::pixel_to_merc_similarity(2.0, 0.02, 1000.0, 1200.0, 3.0e6, -2.98e6);
        let (sx, cx, cy) = (1.015, 1000.0, 1200.0);
        let (tx, ty) = sim.apply(cx, cy);
        let (a, d) = (sim.a * sx, sim.d * sx);
        let truth = Affine { a, d, c: tx - a * cx - sim.b * cy, f: ty - d * cx - sim.e * cy, ..sim };
        let (s, dem) = synthetic_case(&|x, y| truth.apply(x, y), &refine_dem_box(&truth, 2000, 2400));
        let init = Affine::pixel_to_merc_similarity(2.03, 0.0, 1000.0, 1200.0, 3.0e6 + 200.0, -2.98e6 - 150.0);
        let opts = RefineOptions { extent: Some([0, 0, 2000, 2400]), ..Default::default() };
        let r = refine_prepared_with(&s, &PreparedDem::new(&dem), &init, &opts);
        let models: Vec<(&str, bool)> = r.stages.iter().map(|s| (s.model, s.kept)).collect();
        assert_eq!(models, [("similarity", false), ("aniso", true)], "{:?}", r.stages);
        assert!(r.affine && r.homography.is_none());
        let err = corner_error(&r.transform, &truth);
        assert!(err < 8.0, "corner error {err:.1} m");
        assert!(r.spread_m > 0.0 && r.spread_m < 5.0, "{}", r.spread_m);
        assert!(r.extent_cover > 0.95, "{}", r.extent_cover);
        // The similarity alone is ~15 m off at the corners: the stage's shift.
        assert!(r.stages[1].shift_m > 10.0, "{:?}", r.stages[1]);
    }

    /// Contours on the western 40 % of the image only: the fit holds there,
    /// but its corners swing further (the far side is extrapolated) and the
    /// samples cover less of the image, which `span_evidence` and the
    /// spread report.
    #[test]
    fn partial_coverage_widens_the_spread() {
        let truth = Affine::pixel_to_merc_similarity(2.0, 0.02, 1000.0, 1200.0, 3.0e6, -2.98e6);
        let (s, dem) = synthetic_case(&|x, y| truth.apply(x, y), &refine_dem_box(&truth, 2000, 2400));
        let mut west = ContourSamples { n_segs: s.n_segs, ..Default::default() };
        for (p, &g) in s.pts.iter().zip(&s.seg) {
            if p[0] < 800.0 {
                west.pts.push(*p);
                west.seg.push(g);
            }
        }
        let init = Affine::pixel_to_merc_similarity(2.04, 0.0, 1000.0, 1200.0, 3.0e6 + 250.0, -2.98e6 - 200.0);
        let opts = RefineOptions { extent: Some([0, 0, 2000, 2400]), ..Default::default() };
        let prep = PreparedDem::new(&dem);
        let all = refine_prepared_with(&s, &prep, &init, &opts);
        let part = refine_prepared_with(&west, &prep, &init, &opts);
        assert!(corner_error(&part.transform, &truth) < 10.0, "{part:?}");
        assert!(part.spread_m > 1.5 * all.spread_m, "spread {} vs {}", part.spread_m, all.spread_m);
        assert!((0.35..=0.5).contains(&part.extent_cover), "{}", part.extent_cover);
        assert!(span_evidence(&part) == 1.0 && span_evidence(&all) == 1.0);
        let thin = RefineResult { extent_cover: 0.15, ..part.clone() };
        assert!(span_evidence(&thin) < 0.4 && fit_confidence(&thin) < 0.5 * fit_confidence(&part));
    }

    /// Photo mode on an exactly similar map: no spurious projective terms.
    #[test]
    fn photo_mode_keeps_similarity_maps_similar() {
        let truth = Affine::pixel_to_merc_similarity(2.0, 0.02, 1000.0, 1200.0, 3.0e6, -2.98e6);
        let (s, dem) = synthetic_case(&|x, y| truth.apply(x, y), &refine_dem_box(&truth, 2000, 2400));
        let init = Affine::pixel_to_merc_similarity(2.04, 0.0, 1000.0, 1200.0, 3.0e6 + 250.0, -2.98e6 - 200.0);
        let r = refine_prepared_with(
            &s,
            &PreparedDem::new(&dem),
            &init,
            &RefineOptions { photo: true, ..Default::default() },
        );
        let m = r.homography.unwrap_or_else(|| r.transform.to_homography());
        for (x, y) in [(0.0, 0.0), (2000.0, 0.0), (2000.0, 2400.0), (0.0, 2400.0), (1000.0, 1200.0)] {
            let (a, b) = (m.apply(x, y), truth.apply(x, y));
            assert!((a.0 - b.0).hypot(a.1 - b.1) < 8.0, "({x}, {y}): {r:?}");
        }
    }
}
