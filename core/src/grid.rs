//! Grid / graticule georeferencing from straight grid lines + OCR labels.
//!
//! OWNER: grid agent. Contract (may be extended, not broken):
//!
//! *Line detection*: every thin dark (or strongly coloured) run across a row or
//! column is reduced to one sub-pixel centre. A projection-profile Hough over
//! ±5° then finds long straight lines, refined at 0.02° and by least squares.
//! Runs of closely spaced coloured lines are fill hatching, not a grid.
//!
//! *Georeferencing*: coordinate labels (every reading of a garbled one,
//! `labels::coordinate_readings`) are paired with nearby lines of their
//! axis and an exhaustive RANSAC fits `value = a + b·position` per axis.
//! Unlabelled lines that land on round grid values join in; label values
//! sharing only a unit far finer than the line step are misreads. Line
//! intersections become GCPs for an affine fit. An axis with a single label
//! value may take the other axis' scale (`GridResult::lone_label`).
//!
//! *Photos* (`GridOptions::photo`, `detect_lines_photo`): lines are found on
//! a sharpened copy and against their local background, may converge, and
//! are traced knot by knot, so they may bend where the paper does not lie
//! flat (`GridLine::bend`). Per-axis fits may be projective, and with enough
//! GCPs a homography replaces the affine when it fits clearly better (the
//! perspective a flattened photo keeps). `GridResult::lines` returns the
//! identified lines with their values.

use crate::contour::photo_grey_ink;
use crate::geo::{
    haversine_m, latlon_to_merc, latlon_to_utm, merc_to_latlon, utm_to_latlon, Affine, Homography, LatLon, Utm,
    EARTH_RADIUS,
};
use crate::image::{unsharp_mask, Rgb8};
use crate::labels::{coordinate_readings, CoordValue, OcrObservation};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineAxis {
    /// Roughly horizontal in the image (parallels / northings).
    Horizontal,
    /// Roughly vertical in the image (meridians / eastings).
    Vertical,
}

/// A long straight line: points p with p·n = rho, n = (cos θ, sin θ).
/// θ is in radians: ≈ π/2 for horizontal lines, ≈ 0 for vertical ones.
///
/// On photos of paper that does not lie flat (fold creases, curl) a printed
/// grid line is not straight: `bend` then holds its deviation from this
/// straight chord, and `pos_at` / `intersect` follow the bent line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridLine {
    pub axis: LineAxis,
    pub theta: f64,
    pub rho: f64,
    /// Fraction of the image span covered by line pixels.
    pub coverage: f64,
    /// Deviation of a bent line from the chord (photos only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend: Option<Bend>,
}

/// Knots of `Bend::offsets`.
pub const BEND_KNOTS: usize = 24;

/// How a traced line deviates from its straight chord: offsets across the
/// line (pixels along image y for horizontal lines, x for vertical ones,
/// added to the chord's position) at `BEND_KNOTS` knots spread evenly over
/// the image (knot k at `s = k·ds` along the line: x for horizontal lines,
/// y for vertical ones), interpolated linearly between knots and constant
/// beyond the first and last.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bend {
    /// Knot spacing (pixels).
    pub ds: f64,
    pub offsets: [f32; BEND_KNOTS],
    /// Along-line range in which the line was actually seen (pixels).
    pub lo: f64,
    pub hi: f64,
}

impl Bend {
    /// Offset at `s` along the line.
    pub fn offset_at(&self, s: f64) -> f64 {
        if self.ds.is_nan() || self.ds <= 0.0 {
            return 0.0;
        }
        let u = (s / self.ds).clamp(0.0, (BEND_KNOTS - 1) as f64);
        let k = (u.floor() as usize).min(BEND_KNOTS - 2);
        let f = u - k as f64;
        self.offsets[k] as f64 * (1.0 - f) + self.offsets[k + 1] as f64 * f
    }

    /// Largest |offset| (pixels).
    pub fn max_abs(&self) -> f64 {
        self.offsets.iter().fold(0.0f64, |m, &o| m.max(o.abs() as f64))
    }
}

impl GridLine {
    /// Line `p = p0 + (s − mid)·slope`, where `s` runs along the line (x for
    /// horizontal lines, y for vertical ones) and `p` across it.
    pub fn from_slope(axis: LineAxis, mid: f64, p0: f64, slope: f64, coverage: f64) -> GridLine {
        let alpha = slope.atan();
        let (sa, ca) = alpha.sin_cos();
        let rho = ca * p0 - sa * mid;
        let theta = match axis {
            LineAxis::Horizontal => std::f64::consts::FRAC_PI_2 + alpha,
            LineAxis::Vertical => -alpha,
        };
        GridLine { axis, theta, rho, coverage, bend: None }
    }

    /// Position across the line at `s` along it: y at x = s for horizontal
    /// lines, x at y = s for vertical lines (following the bend, if any).
    pub fn pos_at(&self, s: f64) -> f64 {
        let (sn, cs) = self.theta.sin_cos();
        let chord = match self.axis {
            LineAxis::Horizontal => (self.rho - cs * s) / sn,
            LineAxis::Vertical => (self.rho - sn * s) / cs,
        };
        chord + self.bend.map_or(0.0, |b| b.offset_at(s))
    }

    /// Point of the line at `s` along it (image x, y).
    pub fn point_at(&self, s: f64) -> (f64, f64) {
        match self.axis {
            LineAxis::Horizontal => (s, self.pos_at(s)),
            LineAxis::Vertical => (self.pos_at(s), s),
        }
    }

    /// Intersection with another line (None when parallel). Bent lines are
    /// intersected where they actually cross (fixed-point iteration from the
    /// chords' intersection).
    pub fn intersect(&self, o: &GridLine) -> Option<(f64, f64)> {
        let (s1, c1) = self.theta.sin_cos();
        let (s2, c2) = o.theta.sin_cos();
        let det = c1 * s2 - s1 * c2;
        if det.abs() < 1e-9 {
            return None;
        }
        let p = ((self.rho * s2 - s1 * o.rho) / det, (c1 * o.rho - c2 * self.rho) / det);
        if (self.bend.is_none() && o.bend.is_none()) || self.axis == o.axis {
            return Some(p);
        }
        // Grid lines are within a few degrees of the image axes, so this
        // contracts fast.
        let (h, v) = if self.axis == LineAxis::Horizontal { (self, o) } else { (o, self) };
        let mut x = p.0;
        for _ in 0..20 {
            let nx = v.pos_at(h.pos_at(x));
            let done = (nx - x).abs() < 1e-4;
            x = nx;
            if done {
                break;
            }
        }
        let y = h.pos_at(x);
        (x.is_finite() && y.is_finite()).then_some((x, y))
    }

    /// Rotation from the image axis in degrees, positive = clockwise on
    /// screen. A map rotated by φ gives φ for both families.
    pub fn tilt_deg(&self) -> f64 {
        match self.axis {
            LineAxis::Horizontal => (self.theta - std::f64::consts::FRAC_PI_2).to_degrees(),
            LineAxis::Vertical => self.theta.to_degrees(),
        }
    }
}

// ---------------------------------------------------------------------------
// Line detection
// ---------------------------------------------------------------------------

/// Minimum fraction of the image span a dark line must cover.
pub const MIN_COVERAGE: f64 = 0.35;
/// Photos: dense contours blurred into a grid line hide more of it.
const PHOTO_MIN_COVERAGE: f64 = 0.25;
/// Coloured lines (blue UTM grids) must be longer: rivers and roads are coloured too.
const COLOUR_MIN_COVERAGE: f64 = 0.5;
/// Longest run across a line still treated as thin (pixels).
const MAX_RUN: usize = 7;
/// A thin run needs this many non-ink pixels to the next run on both sides.
const RUN_GAP: usize = 2;
const MAX_ANGLE_DEG: f64 = 5.0;
const COARSE_STEP_DEG: f64 = 0.25;
/// Profile window (± bins) absorbing the drift of a line between coarse angles.
const COARSE_WIN: usize = 4;
const FINE_RANGE_DEG: f64 = 0.3;
const FINE_STEP_DEG: f64 = 0.02;
/// Half-width of the band collected around a coarse line for refinement.
const BAND_PX: f64 = 14.0;
/// Points within this distance of a line are its inliers.
const INLIER_PX: f64 = 1.5;
/// Parallel lines closer than this are one line (double frame lines, thick borders).
const MERGE_PX: f64 = 16.0;
/// Photos: the darkest pixel of a grid-line crossing has at least this much
/// grey ink (`contour::photo_grey_ink`; on the Grootkloof photo this keeps
/// 85 % of the grid-line pixels and 16 % of the contour pixels).
const PHOTO_DARK_PEAK: u8 = 60;
/// A photo line whose median colourfulness (`line_tint`) exceeds this is a
/// run of contours, not a grey grid line.
const PHOTO_MAX_LINE_TINT: f64 = 0.15;
/// Photos: the coarse profile window is wider (a bent line strays from its chord)…
const PHOTO_COARSE_WIN: usize = 6;
/// …the band followed around it too…
const PHOTO_BAND_PX: f64 = 28.0;
/// …the line is followed knot by knot: its local direction may change by
/// this much between knots (fold creases bend it)…
const PHOTO_KNOT_TURN_DEG: f64 = 1.5;
/// …its position may differ this much from the previous knot's prediction…
const PHOTO_KNOT_WIN_PX: f64 = 3.0;
/// …and it is lost after this many knots without evidence.
const PHOTO_MAX_GAP_KNOTS: usize = 6;
/// Candidate local lines kept per knot, their direction range around the
/// coarse line, and the cost of a knot skipped where the line is hidden
/// (in units of the inliers a knot needs).
const PHOTO_KNOT_CANDIDATES: usize = 6;
const PHOTO_KNOT_SLOPE_DEG: f64 = 3.0;
const PHOTO_GAP_COST: f64 = 0.3;
/// Inlier distance of a traced line (blurred line centres are noisier).
const PHOTO_INLIER_PX: f64 = 1.75;
/// Largest deviation of a traced line from its chord (pixels).
const PHOTO_MAX_BEND_PX: f64 = 40.0;
/// Coloured lines closer than this (pixels, or fraction of the image across
/// them) to the next, in runs of at least `HATCH_MIN_LINES`, are a fill
/// pattern (`drop_hatching`): Mhlabatini's vegetation hatching is ~35 px apart.
const HATCH_SPACING_PX: f64 = 50.0;
const HATCH_SPACING_FRAC: f64 = 0.015;
const HATCH_MIN_LINES: usize = 3;
/// Largest hole in a line (fraction of the span), e.g. under a legend panel.
/// Separate boxes that happen to line up (legend and scale-bar frames) leave
/// a bigger one.
const MAX_GAP: f64 = 0.4;

#[derive(Clone, Copy)]
enum InkClass {
    /// Black / grey, low saturation.
    Dark,
    /// Black / grey on a normalised photo (`contour::photo_grey_ink`):
    /// contours are dense on photos (a few pixels apart) and, as dark as
    /// blurred grid lines, would otherwise crowd them out.
    PhotoDark,
    /// Strongly saturated colour (blue or magenta grids).
    Colour,
}

/// What makes a run of ink pixels a line crossing: every pixel at least
/// `weak`, the darkest at least `peak`, and the summed ink at least `mass`
/// (a thin anti-aliased line in a downscaled screenshot is a pair of light
/// grey pixels, but carries as much ink as one black pixel).
#[derive(Clone, Copy)]
struct RunSpec {
    weak: u8,
    peak: u8,
    mass: u32,
}

impl InkClass {
    fn spec(self) -> RunSpec {
        match self {
            InkClass::Dark => RunSpec { weak: 30, peak: 60, mass: 64 },
            InkClass::PhotoDark => RunSpec { weak: 30, peak: PHOTO_DARK_PEAK, mass: 64 },
            InkClass::Colour => RunSpec { weak: 30, peak: 90, mass: 150 },
        }
    }
}

/// Per-pixel ink level for a class (0 = background).
fn ink_plane(img: &Rgb8, class: InkClass) -> Vec<u8> {
    let mut out = vec![0u8; img.w * img.h];
    out.par_chunks_mut(img.w).enumerate().for_each(|(y, row)| {
        let src = &img.data[y * img.w * 3..(y + 1) * img.w * 3];
        for (o, px) in row.iter_mut().zip(src.chunks_exact(3)) {
            let mx = px[0].max(px[1]).max(px[2]);
            let mn = px[0].min(px[1]).min(px[2]);
            let chroma = (mx - mn) as u32;
            *o = match class {
                InkClass::Dark => {
                    // Dark pixels may be tinted by what they are printed over;
                    // light ones must be grey (brown contours are not).
                    let neutral = if mx <= 110 { chroma <= 60 } else { chroma * 10 <= mx as u32 + 80 };
                    if neutral {
                        255 - mx
                    } else {
                        0
                    }
                }
                InkClass::PhotoDark => photo_grey_ink([px[0], px[1], px[2]]),
                InkClass::Colour => {
                    if chroma >= 30 && chroma * 5 >= mx as u32 * 2 {
                        chroma as u8
                    } else {
                        0
                    }
                }
            };
        }
    });
    out
}

/// Sub-pixel centres of thin runs, grouped by scan line (CSR layout).
struct ScanPoints {
    start: Vec<usize>,
    pos: Vec<f32>,
}

impl ScanPoints {
    fn from_lists(lists: Vec<Vec<f32>>) -> ScanPoints {
        let total = lists.iter().map(Vec::len).sum();
        let mut start = Vec::with_capacity(lists.len() + 1);
        let mut pos = Vec::with_capacity(total);
        start.push(0);
        for l in lists {
            pos.extend_from_slice(&l);
            start.push(pos.len());
        }
        ScanPoints { start, pos }
    }

    fn n_scans(&self) -> usize {
        self.start.len() - 1
    }

    fn scan(&self, s: usize) -> &[f32] {
        &self.pos[self.start[s]..self.start[s + 1]]
    }
}

/// Centres (ink-weighted, including the faint neighbours) of isolated runs of
/// at most `MAX_RUN` pixels that satisfy `spec`.
fn thin_run_centres(v: &[u8], spec: RunSpec, runs: &mut Vec<(usize, usize, bool)>, out: &mut Vec<f32>) {
    runs.clear();
    let n = v.len();
    let mut i = 0;
    while i < n {
        if v[i] >= spec.weak {
            let s = i;
            let mut peak = 0;
            while i < n && v[i] >= spec.weak {
                peak = peak.max(v[i]);
                i += 1;
            }
            runs.push((s, i, peak >= spec.peak));
        } else {
            i += 1;
        }
    }
    for (k, &(s, e, strong)) in runs.iter().enumerate() {
        // Isolated from other real ink (faint specks nearby do not matter).
        if !strong
            || e - s > MAX_RUN
            || (k > 0 && runs[k - 1].2 && s < runs[k - 1].1 + RUN_GAP)
            || (k + 1 < runs.len() && runs[k + 1].2 && runs[k + 1].0 < e + RUN_GAP)
        {
            continue;
        }
        let mass: u32 = v[s..e].iter().map(|&x| x as u32).sum();
        if mass < spec.mass {
            continue;
        }
        let a = s.saturating_sub(1);
        let b = (e + 1).min(n);
        let (mut sw, mut swx) = (0.0f32, 0.0f32);
        for (j, &val) in v[a..b].iter().enumerate() {
            let wv = val as f32;
            sw += wv;
            swx += wv * (a + j) as f32;
        }
        out.push(swx / sw);
    }
}

/// Runs along each row (centres are x positions) → vertical lines.
fn row_points(ink: &[u8], w: usize, spec: RunSpec) -> ScanPoints {
    let lists: Vec<Vec<f32>> = ink
        .par_chunks(w)
        .map_init(Vec::new, |runs, row| {
            let mut out = Vec::new();
            thin_run_centres(row, spec, runs, &mut out);
            out
        })
        .collect();
    ScanPoints::from_lists(lists)
}

/// Runs down each column (centres are y positions) → horizontal lines.
/// Columns are processed in blocks transposed into a local buffer.
fn column_points(ink: &[u8], w: usize, h: usize, spec: RunSpec) -> ScanPoints {
    const B: usize = 32;
    let blocks: Vec<Vec<Vec<f32>>> = (0..w.div_ceil(B))
        .into_par_iter()
        .map(|bi| {
            let x0 = bi * B;
            let x1 = (x0 + B).min(w);
            let bw = x1 - x0;
            let mut col = vec![0u8; bw * h];
            for y in 0..h {
                for (j, &v) in ink[y * w + x0..y * w + x1].iter().enumerate() {
                    col[j * h + y] = v;
                }
            }
            let mut runs = Vec::new();
            (0..bw)
                .map(|j| {
                    let mut out = Vec::new();
                    thin_run_centres(&col[j * h..(j + 1) * h], spec, &mut runs, &mut out);
                    out
                })
                .collect()
        })
        .collect();
    ScanPoints::from_lists(blocks.into_iter().flatten().collect())
}

fn window_sum(acc: &[u32], r: usize) -> Vec<u32> {
    let n = acc.len();
    let mut pre = Vec::with_capacity(n + 1);
    pre.push(0u32);
    let mut s = 0u32;
    for &v in acc {
        s += v;
        pre.push(s);
    }
    (0..n).map(|i| pre[(i + r + 1).min(n)] - pre[i.saturating_sub(r)]).collect()
}

/// A line of one family: `p = p0 + (s − mid)·slope` (s = scan index,
/// `mid = (n_scans − 1)/2`), plus for traced photo lines the bend from that
/// chord (in scan coordinates, which are the image's along-line pixels).
#[derive(Debug, Clone, Copy)]
struct Found {
    p0: f64,
    slope: f64,
    coverage: f64,
    bend: Option<Bend>,
}

impl Found {
    fn pos(&self, mid: f64, s: f64) -> f64 {
        self.p0 + (s - mid) * self.slope + self.bend.map_or(0.0, |b| b.offset_at(s))
    }
}

/// Long straight lines of one family (on photos: lines that may bend,
/// followed by `trace_line`).
fn hough_lines(pts: &ScanPoints, extent: usize, min_cov: f64, photo: bool) -> Vec<Found> {
    let n = pts.n_scans();
    if n < 16 || extent < 16 {
        return Vec::new();
    }
    let coarse_win = if photo { PHOTO_COARSE_WIN } else { COARSE_WIN };
    let mid = (n as f64 - 1.0) / 2.0;
    let nk = (MAX_ANGLE_DEG / COARSE_STEP_DEG).round() as i64;
    let margin = (mid * MAX_ANGLE_DEG.to_radians().tan()).ceil() as i64 + 2 * MERGE_PX as i64;
    let nb = extent + 2 * margin as usize;
    let profiles: Vec<Vec<u32>> = (-nk..=nk)
        .into_par_iter()
        .map(|k| {
            let t = (k as f64 * COARSE_STEP_DEG).to_radians().tan();
            let mut acc = vec![0u32; nb];
            for s in 0..n {
                let off = (s as f64 - mid) * t - margin as f64;
                for &c in pts.scan(s) {
                    let b = (c as f64 - off).round();
                    if b >= 0.0 && b < nb as f64 {
                        acc[b as usize] += 1;
                    }
                }
            }
            window_sum(&acc, coarse_win)
        })
        .collect();
    let mut best = vec![0u32; nb];
    let mut best_k = vec![0i64; nb];
    for (ki, p) in profiles.iter().enumerate() {
        for (b, &v) in p.iter().enumerate() {
            if v > best[b] {
                best[b] = v;
                best_k[b] = ki as i64 - nk;
            }
        }
    }
    let thr = (0.7 * min_cov * n as f64) as u32;
    let r = MERGE_PX as usize;
    let mut peaks = Vec::new();
    for (b, &v) in best.iter().enumerate() {
        if v < thr.max(1) {
            continue;
        }
        let lo = b.saturating_sub(r);
        let hi = (b + r + 1).min(nb);
        if (lo..hi).all(|j| best[j] < v || (best[j] == v && j >= b)) {
            let t = (best_k[b] as f64 * COARSE_STEP_DEG).to_radians().tan();
            peaks.push((b as f64 - margin as f64, t));
        }
    }
    let mut lines: Vec<Found> = peaks
        .par_iter()
        .filter_map(|&(p0, t0)| {
            if photo {
                trace_line(pts, mid, p0, t0)
            } else {
                refine_line(pts, mid, p0, t0).map(|(p0, slope, coverage)| Found { p0, slope, coverage, bend: None })
            }
        })
        .filter(|l| l.coverage >= min_cov)
        .collect();
    lines.sort_by(|a, b| b.coverage.total_cmp(&a.coverage));
    let span = n as f64 - 1.0;
    let mut kept: Vec<Found> = Vec::new();
    for l in lines {
        let dup = kept.iter().any(|k| {
            if let (Some(lb), Some(kb)) = (&l.bend, &k.bend) {
                // Bent lines: compare where both were seen.
                let (lo, hi) = (lb.lo.max(kb.lo), lb.hi.min(kb.hi));
                hi - lo >= 0.1 * span
                    && (0..=8).all(|i| {
                        let s = lo + (hi - lo) * i as f64 / 8.0;
                        (l.pos(mid, s) - k.pos(mid, s)).abs() < MERGE_PX
                    })
            } else {
                let d0 = (l.p0 - mid * l.slope) - (k.p0 - mid * k.slope);
                let d1 = (l.p0 + (span - mid) * l.slope) - (k.p0 + (span - mid) * k.slope);
                d0.abs() < MERGE_PX && d1.abs() < MERGE_PX
            }
        });
        if !dup {
            kept.push(l);
        }
    }
    kept
}

/// Line points within `half_px` of a coarse line `p = p0 + (s − mid)·t0`,
/// as (s − mid, residual to the coarse line), ordered by scan.
fn band_points(pts: &ScanPoints, mid: f64, p0: f64, t0: f64, half_px: f64) -> Vec<(f32, f32)> {
    let mut band = Vec::new();
    for s in 0..pts.n_scans() {
        let ds = s as f64 - mid;
        let pred = p0 + ds * t0;
        let sc = pts.scan(s);
        let lo = sc.partition_point(|&c| (c as f64) < pred - half_px);
        for &c in &sc[lo..] {
            let q = c as f64 - pred;
            if q > half_px {
                break;
            }
            band.push((ds as f32, q as f32));
        }
    }
    band
}

/// Coverage of a line by its inlier scans: only solid stretches count
/// (isolated text strokes or symbols that happen to touch the line do not,
/// and neither do they bridge a hole between unrelated boxes that line up).
/// `inl` lists the inlier scans (relative to `mid`) in order. Returns the
/// covered fraction of the `n` scans, the largest hole between solid
/// stretches (scans) and the range they span (None when nothing is solid).
fn solid_coverage(inl: impl Iterator<Item = f32>, n: usize) -> (f64, f32, Option<(f32, f32)>) {
    let min_seg = (n as f64 / 150.0).max(8.0) as usize;
    // (first scan, last scan, inlier scans)
    let mut segs: Vec<(f32, f32, usize)> = Vec::new();
    for ds in inl {
        match segs.last_mut() {
            Some(s) if ds == s.1 => {}
            Some(s) if ds - s.1 <= 3.0 => {
                s.1 = ds;
                s.2 += 1;
            }
            _ => segs.push((ds, ds, 1)),
        }
    }
    segs.retain(|s| s.2 >= min_seg);
    let covered: usize = segs.iter().map(|s| s.2).sum();
    let max_gap = segs.windows(2).map(|w| w[1].0 - w[0].1).fold(0.0f32, f32::max);
    let range = segs.first().zip(segs.last()).map(|(a, b)| (a.0, b.1));
    (covered as f64 / n as f64, max_gap, range)
}

/// Candidate local lines at one knot: residual `q ≈ q0 + (s − sc)·dt`
/// through the `band` points within `half` scans of `sc` (`dt` within
/// ±`PHOTO_KNOT_SLOPE_DEG` of the coarse line). The best few distinct ones
/// (`q0` at least 2 px apart) with at least `min_count` inliers, as
/// `(q0, dt, inliers)`, best first.
fn knot_candidates(band: &[(f32, f32)], sc: f32, half: f32, min_count: u32) -> Vec<(f64, f64, u32)> {
    const BINS_PER_PX: f64 = 4.0;
    const STEP_DEG: f64 = 0.125;
    let lo = band.partition_point(|p| p.0 < sc - half);
    let hi = band.partition_point(|p| p.0 <= sc + half);
    let pts = &band[lo..hi];
    let span = PHOTO_BAND_PX + 2.0;
    let nbins = (2.0 * span * BINS_PER_PX) as usize;
    let w = (PHOTO_INLIER_PX * BINS_PER_PX) as usize;
    let steps = (PHOTO_KNOT_SLOPE_DEG / STEP_DEG).round() as i64;
    // Best (inliers, dt) of every position over all directions.
    let mut best = vec![(0u32, 0.0f64); nbins];
    let mut hist = vec![0u32; nbins];
    for k in -steps..=steps {
        let dt = (k as f64 * STEP_DEG).to_radians().tan();
        hist.fill(0);
        for &(s, q) in pts {
            let b = ((q as f64 - (s - sc) as f64 * dt + span) * BINS_PER_PX).floor();
            if b >= 0.0 && b < nbins as f64 {
                hist[b as usize] += 1;
            }
        }
        for (bi, &v) in window_sum(&hist, w).iter().enumerate() {
            if v > best[bi].0 || (v == best[bi].0 && dt.abs() < best[bi].1.abs()) {
                best[bi] = (v, dt);
            }
        }
    }
    let r = (2.0 * BINS_PER_PX) as usize;
    let mut out = Vec::new();
    for (bi, &(v, dt)) in best.iter().enumerate() {
        if v < min_count {
            continue;
        }
        // Plateaus of the window sum: their centre.
        let (lo, hi) = (bi.saturating_sub(r), (bi + r + 1).min(nbins));
        if !(lo..hi).all(|j| best[j].0 < v || (best[j].0 == v && j >= bi)) {
            continue;
        }
        let mut end = bi;
        while end + 1 < nbins && best[end + 1].0 == v {
            end += 1;
        }
        out.push(((bi + end) as f64 / 2.0 / BINS_PER_PX + 0.5 / BINS_PER_PX - span, dt, v));
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.2));
    out.truncate(PHOTO_KNOT_CANDIDATES);
    out
}

/// Photos: follow a line that may bend (paper not lying flat) from a coarse
/// straight estimate `p = p0 + (s − mid)·t0`. Candidate local lines are
/// found at evenly spaced knots along it, and dynamic programming picks the
/// chain of candidates with the most inliers that stays smooth (each knot
/// close to where its predecessor points, a bounded turn, a cost for knots
/// skipped where the line is hidden), so a contour or road running beside
/// the line for a while is not followed. Knot positions are then
/// re-estimated by local linear regression on the inliers of the
/// piecewise-linear curve, and the curve is split into its least-squares
/// chord plus the offsets from it.
fn trace_line(pts: &ScanPoints, mid: f64, p0: f64, t0: f64) -> Option<Found> {
    let n = pts.n_scans();
    let band = band_points(pts, mid, p0, t0, PHOTO_BAND_PX);
    if band.len() < 16 {
        return None;
    }
    let h = (n as f64 - 1.0) / (BEND_KNOTS - 1) as f64;
    let knot_s = |k: usize| (k as f64 * h - mid) as f32;
    let half = h as f32;
    // Enough evidence for a knot: a fifth of the scans in its window.
    let min_count = ((0.2 * 2.0 * h) as u32).max(6);
    let cands: Vec<Vec<(f64, f64, u32)>> =
        (0..BEND_KNOTS).map(|k| knot_candidates(&band, knot_s(k), half, min_count)).collect();
    // Dynamic programming over (knot, candidate): best chain ending there.
    let mc = min_count as f64;
    let mut value: Vec<Vec<f64>> = cands.iter().map(|c| c.iter().map(|a| a.2 as f64).collect()).collect();
    let mut back: Vec<Vec<Option<(usize, usize)>>> = cands.iter().map(|c| vec![None; c.len()]).collect();
    for k in 0..BEND_KNOTS {
        for (i, a) in cands[k].iter().enumerate() {
            for j in 1..=(PHOTO_MAX_GAP_KNOTS + 1).min(k) {
                let kb = k - j;
                for (ib, b) in cands[kb].iter().enumerate() {
                    let tol = PHOTO_KNOT_WIN_PX * (1.0 + 0.5 * (j - 1) as f64);
                    let miss = (a.0 - (b.0 + j as f64 * h * 0.5 * (a.1 + b.1))).abs();
                    let turn = (a.1.atan() - b.1.atan()).abs().to_degrees();
                    let max_turn = PHOTO_KNOT_TURN_DEG * (1.0 + 0.25 * (j - 1) as f64);
                    if miss > tol || turn > max_turn {
                        continue;
                    }
                    let pen = mc * (0.5 * miss / tol + 0.5 * turn / max_turn + PHOTO_GAP_COST * (j - 1) as f64);
                    let v = value[kb][ib] + a.2 as f64 - pen;
                    if v > value[k][i] {
                        value[k][i] = v;
                        back[k][i] = Some((kb, ib));
                    }
                }
            }
        }
    }
    let mut end: Option<(usize, usize, f64)> = None;
    for (k, vs) in value.iter().enumerate() {
        for (i, &v) in vs.iter().enumerate() {
            if end.is_none_or(|e| v > e.2) {
                end = Some((k, i, v));
            }
        }
    }
    let (mut k, mut i, _) = end?;
    // (q, dt, seen) per knot along the chain; skipped knots interpolated.
    let mut knots: Vec<(f64, f64, bool)> = vec![(0.0, 0.0, false); BEND_KNOTS];
    loop {
        let c = cands[k][i];
        knots[k] = (c.0, c.1, true);
        match back[k][i] {
            Some((kb, ib)) => {
                let cb = cands[kb][ib];
                for (m, knot) in knots.iter_mut().enumerate().take(k).skip(kb + 1) {
                    let f = (m - kb) as f64 / (k - kb) as f64;
                    *knot = (cb.0 * (1.0 - f) + c.0 * f, cb.1 * (1.0 - f) + c.1 * f, false);
                }
                (k, i) = (kb, ib);
            }
            None => break,
        }
    }
    // Knots beyond the seen ones continue straight from the last seen knot.
    let first = knots.iter().position(|k| k.2)?;
    let last = knots.iter().rposition(|k| k.2)?;
    for k in (0..first).rev() {
        knots[k].0 = knots[k + 1].0 - h * knots[first].1;
    }
    for k in last + 1..BEND_KNOTS {
        knots[k].0 = knots[k - 1].0 + h * knots[last].1;
    }
    let curve = |q: &[f64], s: f32| -> f64 {
        let u = ((s as f64 + mid) / h).clamp(0.0, (BEND_KNOTS - 1) as f64);
        let k = (u.floor() as usize).min(BEND_KNOTS - 2);
        let f = u - k as f64;
        q[k] * (1.0 - f) + q[k + 1] * f
    };
    let mut q: Vec<f64> = knots.iter().map(|k| k.0).collect();
    // Re-estimate each seen knot from the curve's inliers within a knot
    // spacing (triangular weights, local linear fit), with a shrinking
    // inlier distance (the chain's candidates are only good to a pixel or two).
    for tol in [3.5, 2.5, PHOTO_INLIER_PX, PHOTO_INLIER_PX] {
        let mut nq = q.clone();
        for (k, nqk) in nq.iter_mut().enumerate().take(last + 1).skip(first) {
            let sk = knot_s(k);
            let lo = band.partition_point(|p| p.0 < sk - half);
            let hi = band.partition_point(|p| p.0 <= sk + half);
            let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
            for &(s, r) in &band[lo..hi] {
                if (r as f64 - curve(&q, s)).abs() > tol {
                    continue;
                }
                let wgt = 1.0 - ((s - sk).abs() / half) as f64;
                let x = (s - sk) as f64;
                sw += wgt;
                sx += wgt * x;
                sy += wgt * r as f64;
                sxx += wgt * x * x;
                sxy += wgt * x * r as f64;
            }
            let den = sw * sxx - sx * sx;
            if sw >= 3.0 && den.abs() > 1e-9 {
                let b = (sw * sxy - sx * sy) / den;
                let a = (sy - b * sx) / sw;
                if (a - q[k]).abs() <= tol {
                    *nqk = a;
                }
            }
        }
        q = nq;
    }
    // Inliers of the final curve → coverage, chord.
    let inl: Vec<(f32, f32)> =
        band.iter().copied().filter(|&(s, r)| (r as f64 - curve(&q, s)).abs() <= PHOTO_INLIER_PX).collect();
    let (coverage, max_gap, range) = solid_coverage(inl.iter().map(|p| p.0), n);
    let (lo, hi) = range?;
    if max_gap as f64 > MAX_GAP * n as f64 {
        return None;
    }
    // Chord: least squares through the curve at the seen scans.
    let (mut sn, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for &(s, _) in &inl {
        let (x, y) = (s as f64, curve(&q, s));
        sn += 1.0;
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let den = sn * sxx - sx * sx;
    if sn < 16.0 || den.abs() < 1e-9 {
        return None;
    }
    let b = (sn * sxy - sx * sy) / den;
    let a = (sy - b * sx) / sn;
    let slope = t0 + b;
    let offsets: [f32; BEND_KNOTS] = std::array::from_fn(|k| {
        let s = knot_s(k) as f64;
        (q[k] - (a + b * s)) as f32
    });
    let bend = Bend { ds: h, offsets, lo: lo as f64 + mid, hi: hi as f64 + mid };
    if slope.atan().to_degrees().abs() > MAX_ANGLE_DEG + FINE_RANGE_DEG || bend.max_abs() > PHOTO_MAX_BEND_PX {
        return None;
    }
    Some(Found { p0: p0 + a, slope, coverage, bend: Some(bend) })
}

/// Fine angle search + least squares around a coarse line.
fn refine_line(pts: &ScanPoints, mid: f64, p0: f64, t0: f64) -> Option<(f64, f64, f64)> {
    let n = pts.n_scans();
    let band = band_points(pts, mid, p0, t0, BAND_PX);
    if band.len() < 16 {
        return None;
    }
    // Fine search: 0.25 px bins, ±INLIER_PX window.
    const BINS_PER_PX: f64 = 4.0;
    let half = BAND_PX + 4.0;
    let nbins = (2.0 * half * BINS_PER_PX) as usize;
    let win = (INLIER_PX * BINS_PER_PX) as usize;
    let a0 = t0.atan();
    let nf = (FINE_RANGE_DEG / FINE_STEP_DEG).round() as i64;
    let mut best = (0u32, 0.0f64, 0.0f64);
    let mut hist = vec![0u32; nbins];
    for k in -nf..=nf {
        let dt = (a0 + (k as f64 * FINE_STEP_DEG).to_radians()).tan() - t0;
        hist.fill(0);
        for &(ds, q) in &band {
            let b = ((q as f64 - ds as f64 * dt + half) * BINS_PER_PX).floor();
            if b >= 0.0 && b < nbins as f64 {
                hist[b as usize] += 1;
            }
        }
        let ws = window_sum(&hist, win);
        if let Some((bi, &v)) = ws.iter().enumerate().max_by_key(|&(i, &v)| (v, std::cmp::Reverse(i))) {
            if v > best.0 {
                best = (v, (bi as f64 + 0.5) / BINS_PER_PX - half, dt);
            }
        }
    }
    let (mut a, mut b) = (best.1, best.2);
    for _ in 0..3 {
        let (mut sn, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for &(ds, q) in &band {
            let (x, y) = (ds as f64, q as f64);
            if (y - (a + b * x)).abs() <= INLIER_PX {
                sn += 1.0;
                sx += x;
                sy += y;
                sxx += x * x;
                sxy += x * y;
            }
        }
        if sn < 16.0 {
            return None;
        }
        let den = sn * sxx - sx * sx;
        if den.abs() < 1e-9 {
            return None;
        }
        b = (sn * sxy - sx * sy) / den;
        a = (sy - b * sx) / sn;
    }
    let inliers =
        band.iter().filter(|&&(ds, q)| (q as f64 - (a + b * ds as f64)).abs() <= INLIER_PX).map(|&(ds, _)| ds);
    let (coverage, max_gap, _) = solid_coverage(inliers, n);
    let slope = t0 + b;
    if slope.atan().to_degrees().abs() > MAX_ANGLE_DEG + FINE_RANGE_DEG || max_gap as f64 > MAX_GAP * n as f64 {
        return None;
    }
    Some((p0 + a, slope, coverage))
}

fn detect_class(img: &Rgb8, class: InkClass, min_cov: f64, photo: bool) -> Vec<GridLine> {
    let ink = ink_plane(img, class);
    let spec = class.spec();
    let (hp, vp) = rayon::join(|| column_points(&ink, img.w, img.h, spec), || row_points(&ink, img.w, spec));
    drop(ink);
    let (hl, vl) = rayon::join(|| hough_lines(&hp, img.h, min_cov, photo), || hough_lines(&vp, img.w, min_cov, photo));
    let line = |axis: LineAxis, n: usize, f: Found| GridLine {
        bend: f.bend,
        ..GridLine::from_slope(axis, (n as f64 - 1.0) / 2.0, f.p0, f.slope, f.coverage)
    };
    let lines = hl
        .into_iter()
        .map(|f| line(LineAxis::Horizontal, img.w, f))
        .chain(vl.into_iter().map(|f| line(LineAxis::Vertical, img.h, f)));
    if matches!(class, InkClass::PhotoDark) {
        // A run of brown contours lined up for a while can pass for a grey
        // line; along a whole line the difference in colour is clear.
        lines.filter(|l| line_tint(img, l) <= PHOTO_MAX_LINE_TINT).collect()
    } else {
        lines.collect()
    }
}

/// Median colourfulness of a photo line: chroma / ink (max − min channel
/// over 255 − max channel) of the darkest pixel across the line, along the
/// stretch where it was seen.
fn line_tint(img: &Rgb8, l: &GridLine) -> f64 {
    let ext = along_extent(img.w as f64, img.h as f64, l.axis);
    let (lo, hi) = l.bend.map_or((0.0, ext - 1.0), |b| (b.lo, b.hi));
    let mut tints: Vec<f64> = Vec::new();
    let mut s = lo.max(0.0);
    while s <= hi.min(ext - 1.0) {
        let c = l.pos_at(s).round() as isize;
        let mut best: Option<[u8; 3]> = None;
        for d in -1..=1 {
            let (x, y) = match l.axis {
                LineAxis::Horizontal => (s as isize, c + d),
                LineAxis::Vertical => (c + d, s as isize),
            };
            if x < 0 || y < 0 || x as usize >= img.w || y as usize >= img.h {
                continue;
            }
            let p = img.rgb(x as usize, y as usize);
            if best.is_none_or(|b| p.iter().max() < b.iter().max()) {
                best = Some(p);
            }
        }
        if let Some(p) = best {
            let (mx, mn) = (*p.iter().max().unwrap(), *p.iter().min().unwrap());
            if mx < 230 {
                tints.push((mx - mn) as f64 / (255 - mx) as f64);
            }
        }
        s += 2.0;
    }
    median(tints).unwrap_or(f64::INFINITY)
}

fn along_extent(img_w: f64, img_h: f64, axis: LineAxis) -> f64 {
    match axis {
        LineAxis::Horizontal => img_w,
        LineAxis::Vertical => img_h,
    }
}

fn same_line(a: &GridLine, b: &GridLine, img_w: f64, img_h: f64) -> bool {
    if a.axis != b.axis {
        return false;
    }
    let e = along_extent(img_w, img_h, a.axis);
    (a.pos_at(0.0) - b.pos_at(0.0)).abs() < MERGE_PX && (a.pos_at(e) - b.pos_at(e)).abs() < MERGE_PX
}

/// Detect long straight thin lines within ±5° of horizontal/vertical.
///
/// Dark neutral lines covering ≥ 35 % of the image span, plus strongly
/// coloured lines (blue UTM grids) covering ≥ 50 %. Horizontal lines come
/// first, each family sorted by position.
pub fn detect_lines(img: &Rgb8) -> Vec<GridLine> {
    if img.w < 32 || img.h < 32 || img.data.len() != img.w * img.h * 3 {
        return Vec::new();
    }
    detect_lines_with(img, false)
}

fn detect_lines_with(img: &Rgb8, photo: bool) -> Vec<GridLine> {
    let (lines, colour) = rayon::join(
        || {
            let (class, cov) =
                if photo { (InkClass::PhotoDark, PHOTO_MIN_COVERAGE) } else { (InkClass::Dark, MIN_COVERAGE) };
            detect_class(img, class, cov, photo)
        },
        || detect_class(img, InkClass::Colour, COLOUR_MIN_COVERAGE, photo),
    );
    let colour = drop_hatching(colour, img.w as f64, img.h as f64);
    merge_lines(lines, colour, img.w as f64, img.h as f64)
}

/// Coloured fill patterns (the green hatching of vegetation, marsh) are long
/// parallel coloured lines too: a run of at least `HATCH_MIN_LINES` lines of
/// one family, each within `HATCH_SPACING_PX` (or `HATCH_SPACING_FRAC` of the
/// image across them) of the next, is hatching, not a grid.
fn drop_hatching(lines: Vec<GridLine>, w: f64, h: f64) -> Vec<GridLine> {
    let mut hatch = vec![false; lines.len()];
    for axis in [LineAxis::Horizontal, LineAxis::Vertical] {
        let (along, across) = match axis {
            LineAxis::Horizontal => (w, h),
            LineAxis::Vertical => (h, w),
        };
        let gap = HATCH_SPACING_PX.max(HATCH_SPACING_FRAC * across);
        let mut fam: Vec<(f64, usize)> =
            lines.iter().enumerate().filter(|(_, l)| l.axis == axis).map(|(i, l)| (l.pos_at(along / 2.0), i)).collect();
        fam.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut start = 0;
        for k in 1..=fam.len() {
            if k == fam.len() || fam[k].0 - fam[k - 1].0 > gap {
                if k - start >= HATCH_MIN_LINES {
                    for f in &fam[start..k] {
                        hatch[f.1] = true;
                    }
                }
                start = k;
            }
        }
    }
    lines.into_iter().zip(hatch).filter(|(_, h)| !h).map(|(l, _)| l).collect()
}

/// `lines` plus the lines of `more` that are not already there, horizontal
/// lines first, each family sorted by position.
fn merge_lines(mut lines: Vec<GridLine>, more: Vec<GridLine>, w: f64, h: f64) -> Vec<GridLine> {
    for l in more {
        if !lines.iter().any(|m| same_line(m, &l, w, h)) {
            lines.push(l);
        }
    }
    let key = |l: &GridLine| match l.axis {
        LineAxis::Horizontal => (0, l.pos_at(w / 2.0)),
        LineAxis::Vertical => (1, l.pos_at(h / 2.0)),
    };
    lines.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
    });
    lines
}

/// Radius (pixels) of the local-background windows used on photos: wider
/// than a blurred grid line, narrower than the gaps between grid lines.
pub const PHOTO_BACKGROUND_RADIUS: usize = 6;
/// Unsharp mask applied to photos before line detection (sigma px, amount).
const PHOTO_SHARPEN: (f64, f32) = (1.5, 1.2);

/// `detect_lines` for photographed maps (`img` normalised by
/// `contour::normalize_illumination`). A photo blurs the thin grid lines into
/// faint grey strokes, so the image is sharpened first. Lines are detected
/// on it directly and, for stretches over area tints, with each pixel
/// measured against its local background (`flatten_local_background`); the
/// two sets are merged.
pub fn detect_lines_photo(img: &Rgb8) -> Vec<GridLine> {
    if img.w < 32 || img.h < 32 || img.data.len() != img.w * img.h * 3 {
        return Vec::new();
    }
    let sharp = unsharp_mask(img, PHOTO_SHARPEN.0, PHOTO_SHARPEN.1);
    let (direct, flat) = rayon::join(
        || detect_lines_with(&sharp, true),
        || detect_lines_with(&flatten_local_background(&sharp, PHOTO_BACKGROUND_RADIUS), true),
    );
    merge_lines(direct, flat, img.w as f64, img.h as f64)
}

/// Contrast of each pixel against its local background, per channel:
/// `255 − (background − value)`. The background is the brightest value on
/// *both* sides of the pixel within `r` pixels, across the row or across the
/// column (whichever is brighter): a thin dark stroke keeps its contrast,
/// while tints, fills, shading and the edges between them (dark on one side
/// only) become white.
pub fn flatten_local_background(img: &Rgb8, r: usize) -> Rgb8 {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || img.data.len() != w * h * 3 {
        return img.clone();
    }
    let px = |x: usize, y: usize, c: usize| img.data[(y * w + x) * 3 + c];
    // min(max left, max right) along each row.
    let mut across_row = vec![0u8; w * h * 3];
    across_row.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            for c in 0..3 {
                let left = (x.saturating_sub(r)..=x).map(|xx| px(xx, y, c)).max().unwrap_or(0);
                let right = (x..=(x + r).min(w - 1)).map(|xx| px(xx, y, c)).max().unwrap_or(0);
                row[x * 3 + c] = left.min(right);
            }
        }
    });
    // Same across each column (row slices, so memory stays sequential),
    // combined with the row background.
    let line = |y: usize| &img.data[y * w * 3..(y + 1) * w * 3];
    let mut out = vec![0u8; w * h * 3];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let mut up = line(y).to_vec();
        for yy in y.saturating_sub(r)..y {
            up.iter_mut().zip(line(yy)).for_each(|(u, &v)| *u = (*u).max(v));
        }
        row.copy_from_slice(line(y));
        for yy in y + 1..=(y + r).min(h - 1) {
            row.iter_mut().zip(line(yy)).for_each(|(d, &v)| *d = (*d).max(v));
        }
        let across = &across_row[y * w * 3..(y + 1) * w * 3];
        for (((o, &u), &a), &v) in row.iter_mut().zip(&up).zip(across).zip(line(y)) {
            let bg = (*o).min(u).max(a);
            *o = 255 - (bg - v);
        }
    });
    Rgb8 { w, h, data: out }
}

// ---------------------------------------------------------------------------
// Georeferencing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gcp {
    pub x: f64,
    pub y: f64,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridMethod {
    /// Labels matched to detected grid lines (accurate).
    Lines,
    /// Labels only (no lines found): coarse, should be refined.
    LabelsOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridResult {
    pub transform: Homography,
    pub method: GridMethod,
    /// RMS residual of the GCP fit in ground metres.
    pub rms_m: f64,
    /// Distinct latitude (northing) and longitude (easting) values read
    /// from the labels used.
    pub n_lat: usize,
    pub n_lon: usize,
    pub gcps: Vec<Gcp>,
    /// OCR observations (indices) that were used as coordinate labels.
    pub used_labels: Vec<usize>,
    /// True when `transform` is a homography (photo mode), else an affine.
    #[serde(default)]
    pub projective: bool,
    /// The grid lines the fit identified, with their values (line fits
    /// only): `gcps` are their intersections. On photos the lines follow
    /// the paper's bends, so they also constrain a correction for paper
    /// that does not lie flat along their whole length.
    #[serde(default)]
    pub lines: Vec<ValuedLine>,
    /// Zone and hemisphere of a UTM grid (`ValuedLine::value` in metres);
    /// None for a graticule (degrees).
    #[serde(default)]
    pub utm_zone: Option<(u8, bool)>,
    /// One axis was fitted from a single label value, on the other axis'
    /// scale (`n_lat` or `n_lon` is 1): no other label could outvote a
    /// misread, so the answer needs checking (against the terrain, or by
    /// the user).
    #[serde(default)]
    pub lone_label: bool,
}

/// A grid line with its grid value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ValuedLine {
    pub line: GridLine,
    /// Latitude (horizontal lines) or longitude (vertical lines) in degrees
    /// on a graticule; northing or easting in metres on a UTM grid.
    pub value: f64,
    /// A label named the line (else it lies on a round value between them).
    pub labelled: bool,
}

const MIN_GROUND_M_PER_PX: f64 = 0.05;
const MAX_GROUND_M_PER_PX: f64 = 200.0;
/// Labels whose centre lies in this outer fraction of the image label the
/// lines of the nearer edge (left/right → latitude, top/bottom → longitude).
const EDGE_BAND: f64 = 0.12;
/// Full grid values must lie this close to the GPS position (metres).
const FULL_GRID_GATE_M: f64 = 150_000.0;
/// Abbreviated km labels resolve to the nearest value within this distance.
const ABBREV_GRID_GATE_M: f64 = 60_000.0;
/// Grid lines deviating more than this from their family's median tilt are ignored.
const MAX_FAMILY_TILT_DEG: f64 = 0.75;
/// Photos: residual perspective makes grid lines converge, so they may
/// deviate more from the family median…
const PHOTO_FAMILY_TILT_DEG: f64 = 2.5;
/// …their positions are a projective, not linear, function of the grid
/// value and paper that does not lie flat shifts them (tolerance as a
/// fraction of the pixels between grid values one label step apart)…
const PHOTO_AXIS_TOL: f64 = 0.06;
/// …and the page's aspect ratio is a guess, so the GCP fit need not be
/// conformal (tolerance on the axis-scale ratio, like the 0.06 of scans).
const PHOTO_CONFORMAL_TOL: f64 = 0.35;
/// Photos: an axis fit through at least this many labelled lines may be
/// projective (`projective_fit`).
const PHOTO_PROJECTIVE_AXIS_LINES: usize = 4;
/// Photos: the GCP fit RMS (ground metres) that costs as much as one more
/// line in the choice between fits.
const PHOTO_RMS_PER_SCORE: f64 = 8.0;
/// Photos: with at least this many GCPs a homography is fitted too…
pub const PHOTO_MIN_HOMOGRAPHY_GCPS: usize = 6;
/// …and used when its RMS (ground metres) is at most this fraction of the
/// affine's and at least `PHOTO_HOMOGRAPHY_MIN_GAIN_M` lower.
const PHOTO_HOMOGRAPHY_RMS_RATIO: f64 = 0.75;
const PHOTO_HOMOGRAPHY_MIN_GAIN_M: f64 = 0.5;
/// Bounds the exhaustive RANSAC (hypotheses grow with the square of this).
const MAX_HYPOTHESIS_PAIRS: usize = 240;
/// Distinct fits kept per axis and slope sign for the joint choice.
const TOP_FITS: usize = 6;
/// A line fit's step between adjacent grid lines may be at most this many of
/// the finest standard unit its label values share (one, or a few where
/// grid lines were missed).
const MAX_LINE_STEP_UNITS: f64 = 4.5;
/// Conformality tolerance of label-only fits (label centres are a few
/// pixels off their lines; a misread label is off by a whole interval).
const LABELS_CONFORMAL_TOL: f64 = 0.15;
/// One-label axes (`solve_axis_lone`): grid lines each axis needs (the
/// label's, and others on round values)…
const LONE_MIN_LINES: usize = 3;
/// …and how far from the GPS fix, when there is one, the map may be (km): a
/// label cut short reads as another degree ("7°35'" for 27°35').
const LONE_MAX_GPS_KM: f64 = 250.0;

/// Grid intervals for graticules, in degrees (DMS and decimal).
const GEO_UNITS: [f64; 27] = [
    10.0,
    5.0,
    2.0,
    1.0,
    0.5,
    1.0 / 3.0,
    0.25,
    0.2,
    1.0 / 6.0,
    0.1,
    1.0 / 12.0,
    0.05,
    1.0 / 30.0,
    0.025,
    0.02,
    1.0 / 60.0,
    0.01,
    1.0 / 120.0,
    0.005,
    1.0 / 180.0,
    1.0 / 240.0,
    1.0 / 360.0,
    0.002,
    1.0 / 720.0,
    0.001,
    1.0 / 1800.0,
    1.0 / 3600.0,
];
/// Grid intervals for projected grids, in metres.
const UTM_UNITS: [f64; 10] = [100_000.0, 50_000.0, 20_000.0, 10_000.0, 5_000.0, 2_000.0, 1_000.0, 500.0, 200.0, 100.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sys {
    /// Latitude / longitude graticule.
    Geo,
    /// UTM eastings / northings in the GPS zone.
    Utm,
}

/// X: the value changes along image x and is carried by vertical lines
/// (longitude / easting). Y: horizontal lines (latitude / northing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ax {
    X,
    Y,
}

#[derive(Debug, Clone)]
struct Cand {
    obs: usize,
    ax: Ax,
    /// Degrees (Geo) or metres (Utm).
    value: f64,
    /// Value in the axis' linear frame (Mercator metres or UTM metres).
    lin: f64,
    cx: f64,
    cy: f64,
    bw: f64,
    bh: f64,
    /// Abbreviated grid label: only trusted when it sits on a grid line.
    abbrev: bool,
}

impl Cand {
    /// (along the axis' lines, across them, extent across).
    fn geom(&self) -> (f64, f64, f64) {
        match self.ax {
            Ax::Y => (self.cx, self.cy, self.bh),
            Ax::X => (self.cy, self.cx, self.bw),
        }
    }

    fn text_height(&self) -> f64 {
        self.bw.min(self.bh)
    }
}

struct Ctx {
    w: f64,
    h: f64,
    gps: Option<LatLon>,
    utm: Option<Utm>,
    /// Photo mode (`GridOptions::photo`).
    photo: bool,
}

fn lin_of(sys: Sys, ax: Ax, v: f64) -> f64 {
    match (sys, ax) {
        (Sys::Geo, Ax::X) => EARTH_RADIUS * v.to_radians(),
        (Sys::Geo, Ax::Y) => latlon_to_merc(v, 0.0).1,
        (Sys::Utm, _) => v,
    }
}

fn value_of(sys: Sys, ax: Ax, lin: f64) -> f64 {
    match (sys, ax) {
        (Sys::Geo, Ax::X) => (lin / EARTH_RADIUS).to_degrees(),
        (Sys::Geo, Ax::Y) => merc_to_latlon(0.0, lin).0,
        (Sys::Utm, _) => lin,
    }
}

fn edge_axis(o: &OcrObservation, ctx: &Ctx) -> Option<Ax> {
    let (cx, cy) = o.center();
    let ex = (cx / ctx.w).min(1.0 - cx / ctx.w);
    let ey = (cy / ctx.h).min(1.0 - cy / ctx.h);
    match (ex < EDGE_BAND, ey < EDGE_BAND) {
        (true, false) => Some(Ax::Y),
        (false, true) => Some(Ax::X),
        // In a corner, text running up or down the side is a latitude label (a longitude label
        // at the end of the bottom row reads across); otherwise either.
        (true, true) if o.bbox[3].abs() >= 2.0 * o.bbox[2].abs() => Some(Ax::Y),
        _ => None,
    }
}

/// Readings of a degree value without hemisphere. The axis comes from the
/// label's position; an unsigned value may be either hemisphere (the line
/// fit keeps the sign that gives a non-mirrored map). Readings close to the
/// GPS fix win when there are any.
fn degree_readings(v: f64, o: &OcrObservation, ctx: &Ctx) -> Vec<(Ax, f64)> {
    let axes = match edge_axis(o, ctx) {
        Some(a) => vec![a],
        None => vec![Ax::X, Ax::Y],
    };
    let signs: Vec<f64> = if v > 0.0 { vec![v, -v] } else { vec![v] };
    let all: Vec<(Ax, f64)> = axes
        .iter()
        .flat_map(|&ax| signs.iter().map(move |&s| (ax, s)))
        .filter(|&(ax, s)| s.abs() <= if ax == Ax::Y { 90.0 } else { 180.0 })
        .collect();
    if let Some(g) = ctx.gps {
        let near: Vec<(Ax, f64)> = all
            .iter()
            .copied()
            .filter(|&(ax, s)| match ax {
                Ax::Y => (s - g.lat).abs() <= 3.0,
                Ax::X => (s - g.lon).abs() <= 3.0 / g.lat.to_radians().cos().max(0.1),
            })
            .collect();
        if !near.is_empty() {
            return near;
        }
    }
    all
}

/// Axis hint from a grid label's suffix: "…mE"/"…E" eastings, "…mN"/"…N" northings.
fn grid_axis_hint(text: &str) -> Option<Ax> {
    match text.trim().chars().last()?.to_ascii_uppercase() {
        'E' => Some(Ax::X),
        'N' => Some(Ax::Y),
        _ => None,
    }
}

/// Abbreviated grid label: 2–4 digits, optionally with "km", "mE", "mN", "E", "N".
fn parse_abbreviated(text: &str) -> Option<(f64, usize)> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut body = t.as_str();
    for suf in ["mE", "mN", "km", "E", "N"] {
        if let Some(b) = body.strip_suffix(suf) {
            body = b;
            break;
        }
    }
    if !(2..=4).contains(&body.len()) || !body.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((body.parse().ok()?, body.len()))
}

fn plausible_utm(ax: Ax, v: f64, u: &Utm, gate: f64) -> bool {
    match ax {
        Ax::X => (100_000.0..=900_000.0).contains(&v) && (v - u.easting).abs() <= gate,
        Ax::Y => (0.0..=10_000_000.0).contains(&v) && (v - u.northing).abs() <= gate,
    }
}

/// Coordinate label candidates for the graticule and UTM systems.
fn label_candidates(obs: &[OcrObservation], ctx: &Ctx) -> (Vec<Cand>, Vec<Cand>) {
    let mut geo = Vec::new();
    let mut utm = Vec::new();
    for (i, o) in obs.iter().enumerate() {
        if !o.bbox.iter().all(|v| v.is_finite()) {
            continue;
        }
        let (cx, cy) = o.center();
        let mk = |sys: Sys, ax: Ax, value: f64, abbrev: bool| Cand {
            obs: i,
            ax,
            value,
            lin: lin_of(sys, ax, value),
            cx,
            cy,
            bw: o.bbox[2].abs(),
            bh: o.bbox[3].abs(),
            abbrev,
        };
        let hint = grid_axis_hint(&o.text);
        let axes = hint.map_or(vec![Ax::X, Ax::Y], |a| vec![a]);
        // Every reading of a garbled label is a candidate: the fit keeps the one that agrees
        // with the other labels.
        let readings = coordinate_readings(&o.text);
        for r in &readings {
            match r.value {
                CoordValue::Lat(v) => geo.push(mk(Sys::Geo, Ax::Y, v, false)),
                CoordValue::Lon(v) => geo.push(mk(Sys::Geo, Ax::X, v, false)),
                CoordValue::Degrees(v) => {
                    for (ax, v) in degree_readings(v, o, ctx) {
                        geo.push(mk(Sys::Geo, ax, v, false));
                    }
                }
                CoordValue::Grid(v) => {
                    if let Some(u) = &ctx.utm {
                        for &ax in &axes {
                            if plausible_utm(ax, v, u, FULL_GRID_GATE_M) {
                                utm.push(mk(Sys::Utm, ax, v, false));
                            }
                        }
                    }
                }
            }
        }
        if readings.is_empty() {
            if let (Some(u), Some((n, digits))) = (&ctx.utm, parse_abbreviated(&o.text)) {
                let period = 10f64.powi(digits as i32 + 3);
                let base = n * 1000.0;
                for &ax in &axes {
                    let target = if ax == Ax::X { u.easting } else { u.northing };
                    let v = base + ((target - base) / period).round() * period;
                    if plausible_utm(ax, v, u, ABBREV_GRID_GATE_M) {
                        utm.push(mk(Sys::Utm, ax, v, true));
                    }
                }
            }
        }
    }
    (geo, utm)
}

/// One axis' fit `lin = a + b·u / (1 + c·u)`, `u = t − t0` (t = line
/// position at the image centre, or the label centre for label-only fits).
/// `c = t0 = 0` (linear) except for photos, whose residual perspective
/// spaces equally spaced grid values projectively.
#[derive(Debug, Clone)]
struct AxisFit {
    a: f64,
    b: f64,
    c: f64,
    t0: f64,
    from_lines: bool,
    /// (line index, grid value) for every line used…
    lines: Vec<(usize, f64)>,
    /// …and whether a label named it (else it lies on a round value).
    labelled: Vec<bool>,
    /// Inlier candidate indices.
    inliers: Vec<usize>,
    score: f64,
    cost: f64,
    /// RMS label residual (pixels) of label-only fits.
    rms_px: f64,
}

impl AxisFit {
    /// Position (t) of the grid value with linear value `lin`.
    fn t_at(&self, lin: f64) -> f64 {
        axis_t_at(self.a, self.b, self.c, self.t0, lin)
    }

    fn values(&self, cands: &[Cand]) -> Vec<f64> {
        let mut v: Vec<f64> = if self.from_lines {
            self.lines.iter().map(|l| l.1).collect()
        } else {
            self.inliers.iter().map(|&i| cands[i].value).collect()
        };
        sort_dedup(&mut v);
        v
    }

    fn better_than(&self, o: &AxisFit) -> bool {
        self.score > o.score || (self.score == o.score && self.cost < o.cost)
    }
}

/// The best few distinct fits of one axis, for slope b > 0 (`.0[0]`) and
/// b < 0 (`.0[1]`), best first. Several are kept because the best fit of one
/// axis alone can be a coincidence among many labels that the other axis
/// then contradicts (scale mismatch).
#[derive(Default)]
struct TopFits([Vec<AxisFit>; 2]);

impl TopFits {
    fn push(&mut self, f: AxisFit) {
        let list = &mut self.0[usize::from(f.b < 0.0)];
        match list.iter().position(|o| o.lines == f.lines && o.inliers == f.inliers) {
            Some(i) if f.better_than(&list[i]) => list[i] = f,
            Some(_) => return,
            None => list.push(f),
        }
        list.sort_by(|a, b| {
            if a.better_than(b) {
                std::cmp::Ordering::Less
            } else if b.better_than(a) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        list.truncate(TOP_FITS);
    }

    fn sign(&self, s: usize) -> &[AxisFit] {
        &self.0[s]
    }
}

fn sort_dedup(v: &mut Vec<f64>) {
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup_by(|a, b| (*a - *b).abs() <= 1e-9 * a.abs().max(1.0));
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// Largest standard grid interval dividing every value and not exceeding the
/// spacing of adjacent lines.
fn grid_unit(values: &[f64], step: f64, sys: Sys) -> Option<f64> {
    let (units, tol): (&[f64], f64) = match sys {
        Sys::Geo => (&GEO_UNITS, 0.2 / 3600.0),
        Sys::Utm => (&UTM_UNITS, 0.5),
    };
    units
        .iter()
        .copied()
        .filter(|&u| u <= 1.1 * step && values.iter().all(|&v| ((v / u).round() * u - v).abs() <= tol))
        .max_by(|a, b| a.total_cmp(b))
}

fn plausible_scale(b: f64, ground_per_lin: f64) -> bool {
    let g = b.abs() * ground_per_lin;
    g.is_finite() && (MIN_GROUND_M_PER_PX..=MAX_GROUND_M_PER_PX).contains(&g)
}

fn line_fit(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let (mx, my) = pts.iter().fold((0.0, 0.0), |s, p| (s.0 + p.0 / n, s.1 + p.1 / n));
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for p in pts {
        sxx += (p.0 - mx) * (p.0 - mx);
        sxy += (p.0 - mx) * (p.1 - my);
    }
    if sxx <= 1e-12 {
        return None;
    }
    let b = sxy / sxx;
    Some((my - b * mx, b))
}

/// `t` where `lin = a + b·u / (1 + c·u)`, `u = t − t0`.
fn axis_t_at(a: f64, b: f64, c: f64, t0: f64, lin: f64) -> f64 {
    let v = (lin - a) / b;
    t0 + v / (1.0 - c * v)
}

/// `lin = a + b·u / (1 + c·u)`, `u = t − t0`.
fn axis_lin_at(a: f64, b: f64, c: f64, t0: f64, t: f64) -> f64 {
    let u = t - t0;
    a + b * u / (1.0 + c * u)
}

/// Least-squares fit of `lin = a + b·u / (1 + c·u)` (`u = t − t0`, t0 the
/// mean position) to `(t, lin)` points: `c` by a coarse-to-fine search over
/// perspectives that keep `|c·u| ≤ 0.25`, `a` and `b` linear for each.
/// Returns `(a, b, c, t0)`.
fn projective_fit(pts: &[(f64, f64)]) -> Option<(f64, f64, f64, f64)> {
    if pts.len() < 3 {
        return None;
    }
    let t0 = pts.iter().map(|p| p.0).sum::<f64>() / pts.len() as f64;
    let umax = pts.iter().map(|p| (p.0 - t0).abs()).fold(0.0, f64::max);
    if umax <= 0.0 {
        return None;
    }
    let sse = |c: f64| -> Option<(f64, f64, f64)> {
        let xs: Vec<(f64, f64)> = pts.iter().map(|p| ((p.0 - t0) / (1.0 + c * (p.0 - t0)), p.1)).collect();
        let (a, b) = line_fit(&xs)?;
        let e = xs.iter().map(|&(x, l)| ((l - a - b * x) / b).powi(2)).sum::<f64>();
        Some((e, a, b))
    };
    let cmax = 0.25 / umax;
    let mut best: Option<(f64, f64, f64, f64)> = None;
    let mut step = cmax / 20.0;
    let mut centre = 0.0;
    for _ in 0..3 {
        for k in -20..=20 {
            let c = centre + k as f64 * step;
            if c.abs() > cmax {
                continue;
            }
            if let Some((e, a, b)) = sse(c) {
                if best.is_none_or(|bb| e < bb.0) {
                    best = Some((e, a, b, c));
                }
            }
        }
        centre = best?.3;
        step /= 10.0;
    }
    best.map(|(_, a, b, c)| (a, b, c, t0))
}

struct AxisLines<'a> {
    sys: Sys,
    ax: Ax,
    cands: &'a [Cand],
    /// Line positions at the image centre.
    ts: Vec<f64>,
    /// Candidate lines of every label with a placement weight: 1 for a label
    /// centred on the line, falling off as it sits further away.
    near: Vec<Vec<(usize, f64)>>,
    spacing: f64,
    tol: f64,
    /// Photo mode: refits may be projective…
    photo: bool,
    /// …and the inlier tolerance is a fraction of the pixels between grid
    /// values one label step (`unit_lin`, linear units) apart around `t_mid`.
    unit_lin: f64,
    t_mid: f64,
    /// Coverage of every line (which of two traces of one value to keep).
    coverage: Vec<f64>,
    /// Distinct label values a fit needs (1 for `solve_axis_lone`).
    min_values: usize,
}

impl AxisLines<'_> {
    fn evaluate(&self, a: f64, b: f64) -> Option<AxisFit> {
        self.evaluate_model(a, b, 0.0, 0.0)
    }

    fn evaluate_model(&self, a: f64, b: f64, c: f64, t0: f64) -> Option<AxisFit> {
        let t_at = |lin: f64| axis_t_at(a, b, c, t0, lin);
        let lin_at = |t: f64| axis_lin_at(a, b, c, t0, t);
        let tol = if self.photo && self.unit_lin > 0.0 {
            let l0 = lin_at(self.t_mid);
            let px = (t_at(l0 + self.unit_lin) - self.t_mid).abs();
            if px.is_finite() {
                (PHOTO_AXIS_TOL * px).max(3.0)
            } else {
                self.tol
            }
        } else {
            self.tol
        };
        let mut inl: Vec<(usize, usize)> = Vec::new();
        let (mut cost, mut weight) = (0.0, 0.0);
        for (ci, cl) in self.near.iter().enumerate() {
            let tp = t_at(self.cands[ci].lin);
            let best = if self.photo {
                // A label's line on a photo: of its lines within the (loose)
                // tolerance, the best placed under the label and seen over
                // most of its length (strokes traced beside a line are short).
                cl.iter()
                    .map(|&(li, w)| ((self.ts[li] - tp).abs(), li, w))
                    .filter(|x| x.0 < tol)
                    .max_by(|x, y| (x.2 * self.coverage[x.1]).total_cmp(&(y.2 * self.coverage[y.1])))
            } else {
                cl.iter().map(|&(li, w)| ((self.ts[li] - tp).abs(), li, w)).min_by(|x, y| x.0.total_cmp(&y.0))
            };
            if let Some((r, li, w)) = best {
                if r < tol {
                    inl.push((ci, li));
                    cost += r;
                    weight += w;
                }
            }
        }
        let mut vals: Vec<f64> = inl.iter().map(|&(ci, _)| self.cands[ci].value).collect();
        sort_dedup(&mut vals);
        if vals.len() < self.min_values.max(1) {
            return None;
        }
        // (grid value, labelled) per line.
        let mut line_val: Vec<Option<(f64, bool)>> = vec![None; self.ts.len()];
        for &(ci, li) in &inl {
            line_val[li].get_or_insert((self.cands[ci].value, true));
        }
        // Unlabelled lines on round grid values.
        let tm = self.ts.iter().sum::<f64>() / self.ts.len() as f64;
        let mut step =
            (value_of(self.sys, self.ax, lin_at(tm + self.spacing)) - value_of(self.sys, self.ax, lin_at(tm))).abs();
        if self.photo {
            // Strokes beside the lines (contours, the frame) shrink the median
            // line spacing on photos: no finer unit than the labels show.
            step = step.max(vals.windows(2).map(|w| w[1] - w[0]).fold(f64::INFINITY, f64::min));
        }
        let unit = grid_unit(&vals, step, self.sys);
        // Grid lines are drawn at a round interval and labelled with its multiples, so the
        // labels share a unit about as large as the step between lines. Labels that only
        // share a far finer one are misreads put together (24°00'11" and 27°23' on adjacent
        // meridians, 3.4° apart).
        if unit.is_some_and(|u| step > MAX_LINE_STEP_UNITS * u) {
            return None;
        }
        if let Some(u) = unit {
            let v0 = value_of(self.sys, self.ax, lin_at(tm));
            let u_px = (t_at(lin_of(self.sys, self.ax, v0 + u)) - t_at(lin_of(self.sys, self.ax, v0))).abs();
            if u_px >= 8.0 * tol {
                for (li, lv) in line_val.iter_mut().enumerate() {
                    if lv.is_some() {
                        continue;
                    }
                    let v = value_of(self.sys, self.ax, lin_at(self.ts[li]));
                    let vs = (v / u).round() * u;
                    let r = (t_at(lin_of(self.sys, self.ax, vs)) - self.ts[li]).abs();
                    if r < tol {
                        *lv = Some((vs, false));
                        cost += r;
                    }
                }
            }
        }
        // One line per grid value: of two traces of one line (or a line and
        // a stray stroke beside it, both within the tolerance), the one seen
        // over more of its length.
        let mut lines: Vec<(usize, f64)> = Vec::new();
        let mut labelled: Vec<bool> = Vec::new();
        for (li, (v, lab)) in line_val.iter().enumerate().filter_map(|(li, v)| v.map(|v| (li, v))) {
            match lines.iter().position(|l| (l.1 - v).abs() <= 1e-9 * v.abs().max(1.0)) {
                Some(k) => {
                    if self.coverage[li] > self.coverage[lines[k].0] {
                        lines[k].0 = li;
                        labelled[k] = lab;
                    }
                }
                None => {
                    lines.push((li, v));
                    labelled.push(lab);
                }
            }
        }
        let n_labelled = labelled.iter().filter(|&&l| l).count();
        // Photos: a line counts less when only its position (within the
        // looser photo tolerance) puts it on a round value: a frame line
        // must not outweigh a label.
        let line_score =
            if self.photo { n_labelled as f64 + 0.5 * (lines.len() - n_labelled) as f64 } else { lines.len() as f64 };
        let mut inliers: Vec<usize> = inl.iter().map(|x| x.0).collect();
        inliers.dedup();
        Some(AxisFit {
            a,
            b,
            c,
            t0,
            from_lines: true,
            score: weight + line_score,
            lines,
            labelled,
            inliers,
            cost,
            rms_px: 0.0,
        })
    }

    fn refit(&self, f: &AxisFit) -> Option<AxisFit> {
        let pts: Vec<(f64, f64)> = f.lines.iter().map(|&(li, v)| (self.ts[li], lin_of(self.sys, self.ax, v))).collect();
        if self.photo {
            // Projective only through labelled lines (unlabelled ones on
            // round values could bend the fit towards a frame line).
            let lab: Vec<(f64, f64)> = pts.iter().zip(&f.labelled).filter(|(_, &l)| l).map(|(p, _)| *p).collect();
            if lab.len() >= PHOTO_PROJECTIVE_AXIS_LINES {
                let (a, b, c, t0) = projective_fit(&lab)?;
                return self.evaluate_model(a, b, c, t0);
            }
        }
        let (a, b) = line_fit(&pts)?;
        self.evaluate(a, b)
    }
}

/// Position across one axis' lines at the image centre (where `ts` are measured).
fn centre_pos(ax: Ax, ctx: &Ctx) -> f64 {
    match ax {
        Ax::Y => ctx.h / 2.0,
        Ax::X => ctx.w / 2.0,
    }
}

/// The labels' step between adjacent grid values (linear units) and its
/// size in pixels: medians over labels adjacent in value, from their
/// positions across the axis' lines. None with fewer than two distinct values.
fn label_unit(cands: &[Cand]) -> Option<(f64, f64)> {
    let mut by_lin: Vec<(f64, f64)> = cands.iter().filter(|c| !c.abbrev).map(|c| (c.lin, c.geom().1)).collect();
    by_lin.sort_by(|a, b| a.0.total_cmp(&b.0));
    by_lin.dedup_by(|a, b| (a.0 - b.0).abs() <= 1e-6 * a.0.abs().max(1.0));
    if by_lin.len() < 2 {
        return None;
    }
    let unit = median(by_lin.windows(2).map(|w| w[1].0 - w[0].0).collect())?;
    let px = median(
        by_lin.windows(2).map(|w| (w[1].1 - w[0].1).abs() / ((w[1].0 - w[0].0) / unit).round().max(1.0)).collect(),
    )?;
    (unit > 0.0 && px > 2.0).then_some((unit, px))
}

/// The lines of one axis with the labels' candidate lines (`AxisLines`),
/// and every label's candidate lines with their distances.
#[allow(clippy::type_complexity)]
fn axis_lines<'a>(
    cands: &'a [Cand],
    lines: &[GridLine],
    ax: Ax,
    sys: Sys,
    ctx: &Ctx,
) -> Option<(AxisLines<'a>, Vec<Vec<(usize, f64)>>)> {
    let centre = match ax {
        Ax::Y => ctx.w / 2.0,
        Ax::X => ctx.h / 2.0,
    };
    let ts: Vec<f64> = lines.iter().map(|l| l.pos_at(centre)).collect();
    let mut sorted = ts.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mut spacing = median(sorted.windows(2).map(|w| w[1] - w[0]).collect()).unwrap_or(0.0);
    // Photos: strokes traced beside the grid lines (contours, the frame)
    // shrink the median line spacing; the labels' own spacing does not
    // depend on them.
    let unit = if ctx.photo { label_unit(cands) } else { None };
    if let Some((_, unit_px)) = unit {
        spacing = unit_px;
    }
    if spacing <= 2.0 {
        return None;
    }
    // Up to three candidate lines per label, as (line, distance), nearest first.
    let near_d: Vec<Vec<(usize, f64)>> = cands
        .iter()
        .map(|c| {
            let (along, across, ext) = c.geom();
            // Labels sit on or right next to their line (margin labels are centred on
            // the line end; inside labels run along it), never half a spacing away.
            let gate = (0.5 * ext + 2.0 * c.text_height() + 4.0).min(0.5 * spacing);
            let mut d: Vec<(usize, f64)> = lines
                .iter()
                .enumerate()
                .map(|(li, l)| (li, (l.pos_at(along) - across).abs()))
                .filter(|x| x.1 <= gate)
                .collect();
            d.sort_by(|x, y| x.1.total_cmp(&y.1));
            d.truncate(3);
            d
        })
        .collect();
    let near = near_d
        .iter()
        .zip(cands)
        .map(|(ls, c)| {
            let s = 0.5 * c.text_height() + 4.0;
            ls.iter().map(|&(li, d)| (li, 1.0 / (1.0 + (d / s).powi(2)))).collect()
        })
        .collect();
    let tol = if ctx.photo { PHOTO_AXIS_TOL * spacing } else { 0.005 * spacing };
    let al = AxisLines {
        sys,
        ax,
        cands,
        ts,
        near,
        spacing,
        tol: tol.max(3.0),
        photo: ctx.photo,
        unit_lin: unit.map_or(0.0, |u| u.0),
        t_mid: centre_pos(ax, ctx),
        coverage: lines.iter().map(|l| l.coverage).collect(),
        min_values: 2,
    };
    Some((al, near_d))
}

/// Exhaustive RANSAC over (label, line) pairs. Returns the best fit for
/// b > 0 and for b < 0.
fn solve_axis_lines(cands: &[Cand], lines: &[GridLine], ax: Ax, sys: Sys, ctx: &Ctx, ground_per_lin: f64) -> TopFits {
    let mut out = TopFits::default();
    if lines.len() < 2 || cands.len() < 2 {
        return out;
    }
    let Some((al, near_d)) = axis_lines(cands, lines, ax, sys, ctx) else { return out };
    let mut pairs: Vec<(usize, usize)> =
        near_d.iter().enumerate().flat_map(|(ci, ls)| ls.iter().map(move |&(li, _)| (ci, li))).collect();
    if pairs.len() > MAX_HYPOTHESIS_PAIRS {
        // Very many candidates (noisy OCR): hypotheses only from the labels
        // closest to a line, each with its nearest line.
        let mut firsts: Vec<(f64, usize, usize)> =
            near_d.iter().enumerate().filter_map(|(ci, ls)| ls.first().map(|&(li, d)| (d, ci, li))).collect();
        firsts.sort_by(|x, y| x.0.total_cmp(&y.0));
        pairs = firsts.into_iter().take(MAX_HYPOTHESIS_PAIRS).map(|(_, ci, li)| (ci, li)).collect();
    }
    for (k, &(ci, li)) in pairs.iter().enumerate() {
        for &(cj, lj) in &pairs[k + 1..] {
            if ci == cj || li == lj {
                continue;
            }
            let dl = cands[ci].lin - cands[cj].lin;
            let dt = al.ts[li] - al.ts[lj];
            if dl.abs() < 1e-6 || dt.abs() < 1.0 {
                continue;
            }
            let b = dl / dt;
            if !plausible_scale(b, ground_per_lin) {
                continue;
            }
            let a = cands[ci].lin - b * al.ts[li];
            if let Some(f) = al.evaluate(a, b) {
                out.push(f);
            }
        }
    }
    let mut refined = TopFits::default();
    for f in out.0.into_iter().flatten() {
        let mut best = f;
        for _ in 0..2 {
            match al.refit(&best) {
                Some(r) if (r.b > 0.0) == (best.b > 0.0) && !best.better_than(&r) => best = r,
                _ => break,
            }
        }
        refined.push(best);
    }
    refined
}

/// The one value the labels of an axis give, read on its edge of the map:
/// every label there must allow it (a label read several ways allows each
/// reading), and no other value may be read there. With it, the candidate
/// line the labels are centred on. None for no value, several values or no
/// centred line.
fn lone_label(cands: &[Cand], lines: &[GridLine], ax: Ax, ctx: &Ctx) -> Option<(f64, usize)> {
    let on_edge = |c: &Cand| {
        let (ex, ey) = ((c.cx / ctx.w).min(1.0 - c.cx / ctx.w), (c.cy / ctx.h).min(1.0 - c.cy / ctx.h));
        match ax {
            Ax::Y => ex < EDGE_BAND,
            Ax::X => ey < EDGE_BAND,
        }
    };
    let edge: Vec<&Cand> = cands.iter().filter(|c| !c.abbrev && on_edge(c)).collect();
    let mut obs: Vec<usize> = edge.iter().map(|c| c.obs).collect();
    obs.sort_unstable();
    obs.dedup();
    let mut common: Option<Vec<f64>> = None;
    for o in obs {
        let mut vals: Vec<f64> = edge.iter().filter(|c| c.obs == o).map(|c| c.value).collect();
        sort_dedup(&mut vals);
        common = Some(match common {
            None => vals,
            Some(prev) => {
                prev.into_iter().filter(|p| vals.iter().any(|v| (v - p).abs() <= 1e-9 * p.abs().max(1.0))).collect()
            }
        });
    }
    let common = common?;
    let &[v] = common.as_slice() else { return None };
    // Every label of that value sits centred on one line.
    let mut line: Option<usize> = None;
    for c in edge.iter().filter(|c| (c.value - v).abs() <= 1e-9 * v.abs().max(1.0)) {
        let (along, across, ext) = c.geom();
        let gate = (0.25 * ext).max(c.text_height());
        let near = lines
            .iter()
            .enumerate()
            .map(|(li, l)| (li, (l.pos_at(along) - across).abs()))
            .filter(|x| x.1 <= gate)
            .min_by(|x, y| x.1.total_cmp(&y.1))?;
        if line.is_some_and(|l| l != near.0) {
            return None;
        }
        line = Some(near.0);
    }
    Some((v, line?))
}

/// An axis whose labels give one value (`lone_label`), from the other
/// axis' fit `strong`: a map's scale in Mercator metres is the same along x
/// and y when it is conformal (UTM, Mercator) and 1/cos φ times more along y
/// when it is plain lat/lon, and x and y slopes have opposite signs (not
/// mirrored), so the slope follows; the label puts its value on its line and
/// the other lines take their round values by counting (at least one must,
/// within the usual tolerance). Then refitted through those lines.
fn solve_axis_lone(
    cands: &[Cand],
    lines: &[GridLine],
    ax: Ax,
    sys: Sys,
    ctx: &Ctx,
    strong: &AxisFit,
    lat: f64,
) -> Vec<AxisFit> {
    let mut out = Vec::new();
    // The other axis' scale must be the lines' own: three or more of its lines on their values
    // (two misread labels on two lines make a fit of any scale).
    let strong_ok = strong.lines.len() >= LONE_MIN_LINES;
    if lines.len() < LONE_MIN_LINES
        || cands.is_empty()
        || ctx.photo
        || !strong.from_lines
        || strong.c != 0.0
        || !strong_ok
    {
        return out;
    }
    let Some((v, li)) = lone_label(cands, lines, ax, ctx) else { return out };
    let Some((mut al, _)) = axis_lines(cands, lines, ax, sys, ctx) else { return out };
    al.min_values = 1;
    let ratios: &[f64] = match sys {
        Sys::Geo => &[1.0, 1.0 / lat.to_radians().cos().max(0.05)],
        Sys::Utm => &[1.0],
    };
    for &r in ratios {
        let b = match ax {
            Ax::Y => -strong.b * r,
            Ax::X => -strong.b / r,
        };
        let a = lin_of(sys, ax, v) - b * al.ts[li];
        let Some(f) = al.evaluate(a, b).filter(|f| f.lines.len() >= LONE_MIN_LINES) else { continue };
        if let Some(f) = al.refit(&f).filter(|g| g.lines.len() >= LONE_MIN_LINES && (g.b > 0.0) == (f.b > 0.0)) {
            out.push(f);
        }
    }
    out
}

/// Label-only fit on label centres (assumes an axis-aligned map).
fn solve_axis_labels(cands: &[Cand], ground_per_lin: f64) -> TopFits {
    let mut out = TopFits::default();
    let idx: Vec<usize> = (0..cands.len()).filter(|&i| !cands[i].abbrev).take(MAX_HYPOTHESIS_PAIRS).collect();
    if idx.len() < 2 {
        return out;
    }
    let pos = |c: &Cand| c.geom().1;
    let th = median(idx.iter().map(|&i| cands[i].text_height()).collect()).unwrap_or(20.0);
    let tol = (1.5 * th).max(8.0);
    let evaluate = |a: f64, b: f64| -> Option<AxisFit> {
        let mut inliers = Vec::new();
        let (mut cost, mut ss) = (0.0, 0.0);
        for &i in &idx {
            let r = (pos(&cands[i]) - (cands[i].lin - a) / b).abs();
            if r < tol {
                inliers.push(i);
                cost += r;
                ss += r * r;
            }
        }
        let mut vals: Vec<f64> = inliers.iter().map(|&i| cands[i].value).collect();
        sort_dedup(&mut vals);
        if vals.len() < 2 {
            return None;
        }
        let rms_px = (ss / inliers.len() as f64).sqrt();
        Some(AxisFit {
            a,
            b,
            c: 0.0,
            t0: 0.0,
            from_lines: false,
            lines: Vec::new(),
            labelled: Vec::new(),
            score: inliers.len() as f64,
            inliers,
            cost,
            rms_px,
        })
    };
    for (k, &i) in idx.iter().enumerate() {
        for &j in &idx[k + 1..] {
            let dl = cands[i].lin - cands[j].lin;
            let dt = pos(&cands[i]) - pos(&cands[j]);
            if dl.abs() < 1e-6 || dt.abs() < tol {
                continue;
            }
            let b = dl / dt;
            if !plausible_scale(b, ground_per_lin) {
                continue;
            }
            if let Some(f) = evaluate(cands[i].lin - b * pos(&cands[i]), b) {
                out.push(f);
            }
        }
    }
    let mut refined = TopFits::default();
    for f in out.0.into_iter().flatten() {
        let pts: Vec<(f64, f64)> = f.inliers.iter().map(|&i| (pos(&cands[i]), cands[i].lin)).collect();
        let r = line_fit(&pts).and_then(|(a, b)| evaluate(a, b)).filter(|r| (r.b > 0.0) == (f.b > 0.0));
        refined.push(match r {
            Some(r) if r.score >= f.score => r,
            _ => f,
        });
    }
    refined
}

fn to_latlon(sys: Sys, vx: f64, vy: f64, ctx: &Ctx) -> Option<LatLon> {
    match sys {
        Sys::Geo => Some(LatLon::new(vy, vx)),
        Sys::Utm => {
            let u = ctx.utm?;
            Some(utm_to_latlon(&Utm { zone: u.zone, north: u.north, easting: vx, northing: vy }))
        }
    }
}

struct Solution {
    affine: Affine,
    /// The fit used: `affine`, or a homography (photos).
    transform: Homography,
    method: GridMethod,
    rms_m: f64,
    n_lat: usize,
    n_lon: usize,
    gcps: Vec<Gcp>,
    used: Vec<usize>,
    lines: Vec<ValuedLine>,
    score: f64,
    /// One axis was fitted from a single label value (`solve_axis_lone`).
    lone: bool,
}

impl Solution {
    fn better_than(&self, o: &Solution) -> bool {
        let rank = |s: &Solution| u8::from(s.method == GridMethod::Lines);
        // North-up wins ties: unsigned degree labels also fit a map turned 180°.
        let up = |s: &Solution| u8::from(s.affine.e < 0.0);
        (rank(self), self.score, up(self), -self.rms_m) > (rank(o), o.score, up(o), -o.rms_m)
    }
}

#[allow(clippy::too_many_arguments)]
fn combine(
    sys: Sys,
    cx: &[Cand],
    cy: &[Cand],
    fx: &AxisFit,
    fy: &AxisFit,
    lines_x: &[GridLine],
    lines_y: &[GridLine],
    ctx: &Ctx,
) -> Option<Solution> {
    let from_lines = fx.from_lines && fy.from_lines;
    let mut src = Vec::new();
    let mut gcps = Vec::new();
    if from_lines {
        for &(lx, vx) in &fx.lines {
            for &(ly, vy) in &fy.lines {
                let Some(p) = lines_y[ly].intersect(&lines_x[lx]) else { continue };
                let ll = to_latlon(sys, vx, vy, ctx)?;
                src.push(p);
                gcps.push(Gcp { x: p.0, y: p.1, lat: ll.lat, lon: ll.lon });
            }
        }
    } else {
        for vx in fx.values(cx) {
            for vy in fy.values(cy) {
                let px = fx.t_at(lin_of(sys, Ax::X, vx));
                let py = fy.t_at(lin_of(sys, Ax::Y, vy));
                let ll = to_latlon(sys, vx, vy, ctx)?;
                src.push((px, py));
                gcps.push(Gcp { x: px, y: py, lat: ll.lat, lon: ll.lon });
            }
        }
    }
    if src.len() < 3 || !gcps.iter().all(|g| g.lat.abs() <= 85.0 && g.lon.abs() <= 180.0) {
        return None;
    }
    let dst: Vec<(f64, f64)> = gcps.iter().map(|g| latlon_to_merc(g.lat, g.lon)).collect();
    let affine = Affine::fit(&src, &dst)?;
    if affine.det() >= 0.0 {
        return None;
    }
    let lat_c = gcps.iter().map(|g| g.lat).sum::<f64>() / gcps.len() as f64;
    let g = lat_c.to_radians().cos();
    let scale = affine.mean_scale() * g;
    if !(MIN_GROUND_M_PER_PX..=MAX_GROUND_M_PER_PX).contains(&scale) {
        return None;
    }
    // Mercator metres per pixel along image y vs x: 1 for conformal maps
    // (Mercator, UTM/TM), 1/cos φ for plain lat/lon (equirectangular) maps.
    // Labels paired with the wrong lines break this.
    let ratio = affine.b.hypot(affine.e) / affine.a.hypot(affine.d);
    let tol = match (from_lines, ctx.photo) {
        (_, true) => PHOTO_CONFORMAL_TOL,
        (true, false) => 0.06,
        (false, false) => LABELS_CONFORMAL_TOL,
    };
    if !((ratio - 1.0).abs() <= tol || (ratio * g - 1.0).abs() <= tol) {
        return None;
    }
    let mut ss = 0.0;
    for ((p, d), gc) in src.iter().zip(&dst).zip(&gcps) {
        let q = affine.apply(p.0, p.1);
        let e = (q.0 - d.0).hypot(q.1 - d.1) * gc.lat.to_radians().cos();
        ss += e * e;
    }
    let mut rms_m = (ss / src.len() as f64).sqrt();
    let mut transform = affine.to_homography();
    if ctx.photo && from_lines && src.len() >= PHOTO_MIN_HOMOGRAPHY_GCPS {
        // Residual perspective of a flattened photo: a homography through
        // the line intersections, when it fits clearly better.
        if let Some(hm) = Homography::fit(&src, &dst) {
            let mut ss = 0.0;
            for ((p, d), gc) in src.iter().zip(&dst).zip(&gcps) {
                let q = hm.apply(p.0, p.1);
                let e = (q.0 - d.0).hypot(q.1 - d.1) * gc.lat.to_radians().cos();
                ss += e * e;
            }
            let rms_h = (ss / src.len() as f64).sqrt();
            let det = hm.local_affine(ctx.w / 2.0, ctx.h / 2.0).det();
            if rms_h.is_finite()
                && rms_h <= PHOTO_HOMOGRAPHY_RMS_RATIO * rms_m
                && rms_h <= rms_m - PHOTO_HOMOGRAPHY_MIN_GAIN_M
                && det * affine.det() > 0.0
            {
                transform = hm;
                rms_m = rms_h;
            }
        }
    }
    let method = if from_lines { GridMethod::Lines } else { GridMethod::LabelsOnly };
    if !from_lines {
        // Label placement uncertainty: residual scatter plus half a text height.
        let label_fits: Vec<&AxisFit> = [fx, fy].into_iter().filter(|f| !f.from_lines).collect();
        let th = median(
            label_fits
                .iter()
                .flat_map(|f| {
                    let cs = if std::ptr::eq(*f, fx) { cx } else { cy };
                    f.inliers.iter().map(move |&i| cs[i].text_height())
                })
                .collect(),
        )
        .unwrap_or(20.0);
        let res_px = (label_fits.iter().map(|f| f.rms_px * f.rms_px).sum::<f64>() / label_fits.len() as f64).sqrt();
        rms_m = (rms_m * rms_m + (res_px * scale).powi(2) + (0.5 * th * scale).powi(2)).sqrt();
    }
    let obs_of = |cs: &[Cand], f: &AxisFit| {
        let mut v: Vec<usize> = f.inliers.iter().map(|&i| cs[i].obs).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    // Labels read: distinct values (several passes may read one label).
    let n_labels = |cs: &[Cand], f: &AxisFit| {
        let mut v: Vec<f64> = f.inliers.iter().map(|&i| cs[i].value).collect();
        sort_dedup(&mut v);
        v.len()
    };
    let lon_obs = obs_of(cx, fx);
    let lat_obs = obs_of(cy, fy);
    let mut used: Vec<usize> = lon_obs.iter().chain(&lat_obs).copied().collect();
    used.sort_unstable();
    used.dedup();
    let valued = |f: &AxisFit, ls: &[GridLine]| -> Vec<ValuedLine> {
        f.lines
            .iter()
            .zip(&f.labelled)
            .map(|(&(li, value), &labelled)| ValuedLine { line: ls[li], value, labelled })
            .collect()
    };
    let lines = if from_lines { [valued(fx, lines_x), valued(fy, lines_y)].concat() } else { Vec::new() };
    Some(Solution {
        affine,
        transform,
        method,
        rms_m,
        n_lat: n_labels(cy, fy),
        n_lon: n_labels(cx, fx),
        gcps,
        used,
        lines,
        // Photos: a looser line tolerance lets a stray stroke stand in for a
        // grid line; the 2-D fit of the intersections tells them apart.
        score: fx.score + fy.score - if ctx.photo { rms_m / PHOTO_RMS_PER_SCORE } else { 0.0 },
        lone: false,
    })
}

fn solve_system(sys: Sys, cands: &[Cand], lines_x: &[GridLine], lines_y: &[GridLine], ctx: &Ctx) -> Option<Solution> {
    let cx: Vec<Cand> = cands.iter().filter(|c| c.ax == Ax::X).cloned().collect();
    let cy: Vec<Cand> = cands.iter().filter(|c| c.ax == Ax::Y).cloned().collect();
    if cx.is_empty() || cy.is_empty() || cx.len() + cy.len() < 3 {
        return None;
    }
    let ref_lat = match sys {
        Sys::Geo => median(cy.iter().map(|c| c.value).collect()).unwrap_or(0.0),
        Sys::Utm => ctx.gps.map_or(0.0, |g| g.lat),
    };
    let gpl = match sys {
        Sys::Geo => ref_lat.to_radians().cos(),
        Sys::Utm => 1.0,
    };
    let lx = solve_axis_lines(&cx, lines_x, Ax::X, sys, ctx, gpl);
    let ly = solve_axis_lines(&cy, lines_y, Ax::Y, sys, ctx, gpl);
    let bx = solve_axis_labels(&cx, gpl);
    let by = solve_axis_labels(&cy, gpl);
    let mut best: Option<Solution> = None;
    // Not mirrored: x and y slopes of opposite signs (north-up or south-up).
    // Line fits rank first; label-only fits stand in when no combination of
    // line fits passes the checks in `combine`.
    for (sx, sy) in [(0usize, 1usize), (1, 0)] {
        for fx in lx.sign(sx).iter().chain(bx.sign(sx)) {
            for fy in ly.sign(sy).iter().chain(by.sign(sy)) {
                if let Some(s) = combine(sys, &cx, &cy, fx, fy, lines_x, lines_y, ctx) {
                    if best.as_ref().is_none_or(|b| s.better_than(b)) {
                        best = Some(s);
                    }
                }
            }
        }
    }
    best.or_else(|| solve_lone(sys, &cx, &cy, (&lx, &ly), lines_x, lines_y, ctx, ref_lat))
}

/// No fit through two labels on each axis: one axis with a single label
/// value (`solve_axis_lone`) on the scale of the other axis' line fits.
/// Every such solution must put the map in the same place (one value, read
/// once, cannot be outvoted: the caller flags the answer for checking).
#[allow(clippy::too_many_arguments)]
fn solve_lone(
    sys: Sys,
    cx: &[Cand],
    cy: &[Cand],
    (lx, ly): (&TopFits, &TopFits),
    lines_x: &[GridLine],
    lines_y: &[GridLine],
    ctx: &Ctx,
    ref_lat: f64,
) -> Option<Solution> {
    let lat = match sys {
        Sys::Geo => lone_label(cy, lines_y, Ax::Y, ctx).map_or(ref_lat, |(v, _)| v),
        Sys::Utm => ref_lat,
    };
    let mut sols: Vec<Solution> = Vec::new();
    for (strong, weak_ax) in [(lx, Ax::Y), (ly, Ax::X)] {
        for fs in strong.0.iter().flatten() {
            let (cands, lines) = if weak_ax == Ax::Y { (cy, lines_y) } else { (cx, lines_x) };
            for fw in solve_axis_lone(cands, lines, weak_ax, sys, ctx, fs, lat) {
                let (fx, fy) = if weak_ax == Ax::Y { (fs, &fw) } else { (&fw, fs) };
                if let Some(mut s) = combine(sys, cx, cy, fx, fy, lines_x, lines_y, ctx) {
                    s.lone = true;
                    sols.push(s);
                }
            }
        }
    }
    // Near the GPS fix, when there is one.
    if let Some(g) = ctx.gps {
        sols.retain(|s| {
            let (mx, my) = s.affine.apply(ctx.w / 2.0, ctx.h / 2.0);
            let (lat, lon) = merc_to_latlon(mx, my);
            haversine_m(LatLon::new(lat, lon), g) <= 1000.0 * LONE_MAX_GPS_KM
        });
    }
    let best = sols.iter().enumerate().fold(None::<usize>, |b, (i, s)| match b {
        Some(j) if !s.better_than(&sols[j]) => Some(j),
        _ => Some(i),
    })?;
    // All of them nearly as well supported (within one line or label of the best) in one
    // place (within 2 px at the corners): a stretched aspect that puts only the frame line on
    // a round value does not count against the graticule.
    let corners = [(0.0, 0.0), (ctx.w, 0.0), (0.0, ctx.h), (ctx.w, ctx.h)];
    let px = |s: &Solution| -> Vec<(f64, f64)> { corners.iter().map(|&(x, y)| s.affine.apply(x, y)).collect() };
    let reference = px(&sols[best]);
    let scale = sols[best].affine.mean_scale();
    let floor = sols[best].score - 1.0;
    let agree = sols
        .iter()
        .filter(|s| s.score > floor)
        .all(|s| px(s).iter().zip(&reference).all(|(p, q)| (p.0 - q.0).hypot(p.1 - q.1) <= 2.0 * scale));
    agree.then(|| sols.swap_remove(best))
}

/// Lines of one family that are parallel to the family's consensus: grid
/// lines are, stray straight features (a fence, a road) usually are not.
fn grid_family(lines: &[GridLine], axis: LineAxis, max_tilt_deg: f64) -> Vec<GridLine> {
    let fam: Vec<GridLine> =
        lines.iter().filter(|l| l.axis == axis && l.theta.is_finite() && l.rho.is_finite()).copied().collect();
    if fam.len() < 3 {
        return fam;
    }
    let Some(med) = median(fam.iter().map(GridLine::tilt_deg).collect()) else { return fam };
    fam.into_iter().filter(|l| (l.tilt_deg() - med).abs() <= max_tilt_deg).collect()
}

/// Options of `georeference_from_grid_with`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridOptions {
    /// The image is a photo flattened by its page borders: grid lines may
    /// converge, line positions are projective in the grid values, the
    /// aspect ratio is approximate, and with enough GCPs a homography is fitted.
    pub photo: bool,
}

/// Full grid pipeline. `gps` resolves UTM zone / abbreviated grid labels and
/// the hemisphere of degree labels without one.
pub fn georeference_from_grid(
    img: &Rgb8,
    lines: &[GridLine],
    obs: &[OcrObservation],
    gps: Option<LatLon>,
) -> Option<GridResult> {
    georeference_from_grid_with(img, lines, obs, gps, &GridOptions::default())
}

/// `georeference_from_grid` with options (photo mode).
pub fn georeference_from_grid_with(
    img: &Rgb8,
    lines: &[GridLine],
    obs: &[OcrObservation],
    gps: Option<LatLon>,
    opts: &GridOptions,
) -> Option<GridResult> {
    let gps = gps.filter(|g| g.lat.is_finite() && g.lon.is_finite() && g.lat.abs() <= 84.0);
    let ctx = Ctx {
        w: img.w.max(1) as f64,
        h: img.h.max(1) as f64,
        gps,
        utm: gps.map(|g| latlon_to_utm(g.lat, g.lon)),
        photo: opts.photo,
    };
    let (geo, utm) = label_candidates(obs, &ctx);
    let max_tilt = if opts.photo { PHOTO_FAMILY_TILT_DEG } else { MAX_FAMILY_TILT_DEG };
    let lines_x = grid_family(lines, LineAxis::Vertical, max_tilt);
    let lines_y = grid_family(lines, LineAxis::Horizontal, max_tilt);
    let mut best: Option<(Sys, Solution)> = None;
    for (sys, cands) in [(Sys::Geo, &geo), (Sys::Utm, &utm)] {
        if let Some(s) = solve_system(sys, cands, &lines_x, &lines_y, &ctx) {
            if best.as_ref().is_none_or(|b| s.better_than(&b.1)) {
                best = Some((sys, s));
            }
        }
    }
    let (sys, s) = best?;
    Some(GridResult {
        projective: !s.transform.is_affine(),
        transform: s.transform,
        method: s.method,
        rms_m: s.rms_m,
        n_lat: s.n_lat,
        n_lon: s.n_lon,
        gcps: s.gcps,
        used_labels: s.used,
        lines: s.lines,
        utm_zone: match sys {
            Sys::Geo => None,
            Sys::Utm => ctx.utm.map(|u| (u.zone, u.north)),
        },
        lone_label: s.lone,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: usize, h: usize) -> Rgb8 {
        Rgb8::from_rgb(w, h, vec![255u8; w * h * 3]).unwrap()
    }

    fn put(img: &mut Rgb8, x: usize, y: usize, v: [u8; 3]) {
        let i = (y * img.w + x) * 3;
        img.data[i..i + 3].copy_from_slice(&v);
    }

    #[test]
    fn line_geometry() {
        let h = GridLine::from_slope(LineAxis::Horizontal, 500.0, 300.0, 0.01, 1.0);
        assert!((h.pos_at(500.0) - 300.0).abs() < 1e-9);
        assert!((h.pos_at(600.0) - 301.0).abs() < 1e-9);
        let v = GridLine::from_slope(LineAxis::Vertical, 400.0, 200.0, -0.02, 1.0);
        assert!((v.pos_at(400.0) - 200.0).abs() < 1e-9);
        assert!((v.pos_at(500.0) - 198.0).abs() < 1e-9);
        let (x, y) = h.intersect(&v).unwrap();
        assert!((h.pos_at(x) - y).abs() < 1e-9 && (v.pos_at(y) - x).abs() < 1e-9);
        assert!((h.tilt_deg() - 0.01f64.atan().to_degrees()).abs() < 1e-9);
    }

    #[test]
    fn detects_tilted_lines_and_ignores_thick_bars_and_diagonals() {
        let (w, h) = (1200, 900);
        let mut img = blank(w, h);
        let t = 1.5f64.to_radians().tan();
        for x in 0..w {
            // 2 px tilted horizontal line through (600, 300).
            let y = 300.0 + (x as f64 - 599.5) * t;
            for dy in 0..2 {
                put(&mut img, x, (y.floor() as usize) + dy, [20, 20, 20]);
            }
            // Thick 12 px bar (not a thin line).
            for yy in 600..612 {
                put(&mut img, x, yy, [0, 0, 0]);
            }
            // Diagonal at 30°.
            let yd = 50.0 + x as f64 * 0.577;
            if yd < h as f64 {
                put(&mut img, x, yd as usize, [0, 0, 0]);
            }
        }
        for y in 0..h {
            put(&mut img, 250, y, [10, 10, 10]);
            // Blue grid line.
            put(&mut img, 900, y, [20, 90, 230]);
            put(&mut img, 901, y, [20, 90, 230]);
        }
        let lines = detect_lines(&img);
        let hs: Vec<&GridLine> = lines.iter().filter(|l| l.axis == LineAxis::Horizontal).collect();
        let vs: Vec<&GridLine> = lines.iter().filter(|l| l.axis == LineAxis::Vertical).collect();
        assert_eq!(hs.len(), 1, "{lines:?}");
        assert_eq!(vs.len(), 2, "{lines:?}");
        assert!((hs[0].pos_at(599.5) - 300.0).abs() < 0.3, "{:?}", hs[0]);
        assert!((hs[0].tilt_deg() - 1.5).abs() < 0.05, "{}", hs[0].tilt_deg());
        assert!((vs[0].pos_at(450.0) - 250.0).abs() < 0.3);
        assert!((vs[1].pos_at(450.0) - 900.5).abs() < 0.3);
    }

    #[test]
    fn coloured_hatching_is_no_grid() {
        // Long green vegetation hatching (Mhlabatini's colour, 4 px lines 35 px apart over 85 %
        // of the height), a blue grid line and a black one.
        let (w, h) = (1400, 1800);
        let mut img = blank(w, h);
        for y in 100..1630 {
            for k in 0..22 {
                for dx in 0..4 {
                    put(&mut img, 300 + 35 * k + dx, y, [195, 221, 122]);
                }
            }
        }
        for y in 0..h {
            put(&mut img, 1200, y, [20, 90, 230]);
            put(&mut img, 1201, y, [20, 90, 230]);
            put(&mut img, 150, y, [10, 10, 10]);
        }
        let lines = detect_lines(&img);
        let vs: Vec<f64> =
            lines.iter().filter(|l| l.axis == LineAxis::Vertical).map(|l| l.pos_at(h as f64 / 2.0)).collect();
        assert_eq!(vs.len(), 2, "{vs:?}");
        assert!((vs[0] - 150.0).abs() < 1.0 && (vs[1] - 1200.5).abs() < 1.0, "{vs:?}");
    }

    #[test]
    fn local_background_flattening_keeps_thin_lines_only() {
        // Dark 2-px lines over a beige tint and over a shaded area (a photo).
        let (w, h) = (200, 120);
        let mut img = blank(w, h);
        for y in 0..h {
            for x in 0..w {
                let bg = if x < 100 { [200, 195, 185] } else { [150, 145, 140] };
                put(&mut img, x, y, bg);
            }
            for x in [60, 61] {
                put(&mut img, x, y, [90, 88, 83]);
            }
            for x in [150, 151] {
                put(&mut img, x, y, [68, 65, 63]);
            }
        }
        let f = flatten_local_background(&img, PHOTO_BACKGROUND_RADIUS);
        assert_eq!(f.rgb(20, 50), [255, 255, 255]);
        assert_eq!(f.rgb(180, 50), [255, 255, 255]);
        assert_eq!(f.rgb(60, 50), [145, 148, 153]);
        assert_eq!(f.rgb(151, 50), [173, 175, 178]);
        // The scan detector sees no thin line on the tints; the photo detector finds both.
        assert!(detect_lines(&img).iter().all(|l| l.axis != LineAxis::Vertical));
        let photo = detect_lines_photo(&img);
        let vs: Vec<f64> = photo.iter().filter(|l| l.axis == LineAxis::Vertical).map(|l| l.pos_at(60.0)).collect();
        assert_eq!(vs.len(), 2, "{photo:?}");
        assert!((vs[0] - 60.5).abs() < 0.5 && (vs[1] - 150.5).abs() < 0.5, "{vs:?}");
    }

    #[test]
    fn grid_units() {
        let m = 1.0 / 60.0;
        assert_eq!(grid_unit(&[25.0 + 49.0 * m, 25.0 + 51.0 * m], m * 1.0001, Sys::Geo), Some(GEO_UNITS[15]));
        assert_eq!(grid_unit(&[540_000.0, 542_000.0], 1000.0, Sys::Utm), Some(1000.0));
        // 25.8167° is 25°49'00.1": still a 30" multiple; 25.8123° is no round value.
        assert_eq!(grid_unit(&[25.8167], 0.01, Sys::Geo), Some(1.0 / 120.0));
        assert_eq!(grid_unit(&[25.8123], 0.01, Sys::Geo), None);
    }

    #[test]
    fn abbreviated_labels() {
        assert_eq!(parse_abbreviated("41"), Some((41.0, 2)));
        assert_eq!(parse_abbreviated("541 mE"), Some((541.0, 3)));
        assert_eq!(parse_abbreviated("7141N"), Some((7141.0, 4)));
        assert_eq!(parse_abbreviated("5"), None);
        assert_eq!(parse_abbreviated("KWAMI"), None);
    }
}
