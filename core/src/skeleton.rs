//! Thinning and contour segment extraction.
//!
//! The contour mask is thinned to a one-pixel-wide 8-connected skeleton
//! (parallel Zhang–Suen, then a sequential pass that removes the redundant
//! corner pixels of staircases), short spurs are pruned, junction pixels
//! (more than two skeleton neighbours) are removed so every remaining
//! component is a simple path or loop, and each path long enough to be a
//! contour segment is traced in order and sampled.

use crate::image::Mask;
use rayon::prelude::*;

/// Points sampled along contour segments, in base-image pixel coordinates.
/// `seg[i]` is the segment id of `pts[i]`; ids are 0..n_segs.
#[derive(Debug, Clone, Default)]
pub struct ContourSamples {
    pub pts: Vec<[f32; 2]>,
    pub seg: Vec<u32>,
    pub n_segs: usize,
}

impl ContourSamples {
    pub fn len(&self) -> usize {
        self.pts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }
}

/// Neighbour ring order: N, NE, E, SE, S, SW, W, NW (bit i = ring[i]).
const RING: [(isize, isize); 8] = [(0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1)];

/// Zhang–Suen deletion tables for the two sub-iterations, indexed by the
/// 8-bit neighbourhood code.
fn zs_luts() -> ([bool; 256], [bool; 256]) {
    let mut a = [false; 256];
    let mut b = [false; 256];
    for code in 0..256usize {
        let p = |i: usize| (code >> i) & 1;
        let n = code.count_ones();
        let trans = (0..8).filter(|&i| p(i) == 0 && p((i + 1) % 8) == 1).count();
        if !(2..=6).contains(&n) || trans != 1 {
            continue;
        }
        let (n_, e, s, w) = (p(0), p(2), p(4), p(6));
        a[code] = n_ * e * s == 0 && e * s * w == 0;
        b[code] = n_ * e * w == 0 && n_ * s * w == 0;
    }
    (a, b)
}

/// Yokoi 8-connectivity number of the neighbourhood (number of 8-connected
/// neighbour groups that the centre pixel joins).
#[inline]
fn yokoi8(code: u8) -> u32 {
    let c = |i: usize| ((code >> (i % 8)) & 1) ^ 1;
    let mut n = 0u32;
    for k in [0usize, 2, 4, 6] {
        n += (c(k) - c(k) * c(k + 1) * c(k + 2)) as u32;
    }
    n
}

/// Zero-padded working raster (1-pixel border) so neighbours never go out of bounds.
struct Padded {
    pw: usize,
    ph: usize,
    data: Vec<u8>,
}

impl Padded {
    fn from_mask(m: &Mask) -> Padded {
        let (pw, ph) = (m.w + 2, m.h + 2);
        let mut data = vec![0u8; pw * ph];
        data.par_chunks_mut(pw).enumerate().for_each(|(py, row)| {
            if py >= 1 && py <= m.h {
                let src = &m.data[(py - 1) * m.w..py * m.w];
                for (o, &v) in row[1..=m.w].iter_mut().zip(src) {
                    *o = (v != 0) as u8;
                }
            }
        });
        Padded { pw, ph, data }
    }

    fn to_mask(&self) -> Mask {
        let (w, h) = (self.pw - 2, self.ph - 2);
        let mut m = Mask::new(w, h);
        m.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let src = &self.data[(y + 1) * self.pw + 1..(y + 1) * self.pw + 1 + w];
            for (o, &v) in row.iter_mut().zip(src) {
                *o = (v != 0) as u8;
            }
        });
        m
    }

    #[inline]
    fn code(&self, i: usize) -> u8 {
        let pw = self.pw as isize;
        let mut c = 0u8;
        for (k, (dx, dy)) in RING.iter().enumerate() {
            let j = (i as isize + dy * pw + dx) as usize;
            c |= ((self.data[j] != 0) as u8) << k;
        }
        c
    }

    /// Indices of set pixels in raster order.
    fn set_indices(&self) -> Vec<usize> {
        let pw = self.pw;
        self.data
            .par_chunks(pw)
            .enumerate()
            .map(|(py, row)| {
                row.iter().enumerate().filter(|(_, &v)| v != 0).map(|(x, _)| py * pw + x).collect::<Vec<_>>()
            })
            .flatten()
            .collect()
    }
}

/// Zhang–Suen thinning in place (sub-iterations are parallel over rows).
fn zhang_suen(p: &mut Padded) {
    let (sub1, sub2) = zs_luts();
    let pw = p.pw;
    let ph = p.ph;
    let mut next = p.data.clone();
    // A pixel's fate under a sub-iteration's rule can only change if its 3×3
    // neighbourhood changed since that rule was last applied, i.e. in one of
    // the previous two sub-iterations. Rows are skipped otherwise.
    let mut ch1 = vec![true; ph];
    let mut ch2 = vec![true; ph];
    let mut quiet = 0;
    for iter in 0..400 {
        let lut = if iter % 2 == 0 { &sub1 } else { &sub2 };
        let cur = &p.data;
        let (c1, c2) = (&ch1, &ch2);
        let changed: Vec<bool> = next
            .par_chunks_mut(pw)
            .enumerate()
            .map(|(py, row)| {
                if py == 0 || py + 1 == ph {
                    return false;
                }
                let mid = &cur[py * pw..(py + 1) * pw];
                if !(py - 1..=py + 1).any(|r| c1[r] || c2[r]) {
                    row.copy_from_slice(mid);
                    return false;
                }
                let up = &cur[(py - 1) * pw..py * pw];
                let dn = &cur[(py + 1) * pw..(py + 2) * pw];
                let mut any = false;
                row[0] = 0;
                row[pw - 1] = 0;
                for x in 1..pw - 1 {
                    if mid[x] == 0 {
                        row[x] = 0;
                        continue;
                    }
                    let code = (up[x] != 0) as usize
                        | ((up[x + 1] != 0) as usize) << 1
                        | ((mid[x + 1] != 0) as usize) << 2
                        | ((dn[x + 1] != 0) as usize) << 3
                        | ((dn[x] != 0) as usize) << 4
                        | ((dn[x - 1] != 0) as usize) << 5
                        | ((mid[x - 1] != 0) as usize) << 6
                        | ((up[x - 1] != 0) as usize) << 7;
                    if lut[code] {
                        row[x] = 0;
                        any = true;
                    } else {
                        row[x] = 1;
                    }
                }
                any
            })
            .collect();
        std::mem::swap(&mut p.data, &mut next);
        let any = changed.iter().any(|&c| c);
        ch2 = std::mem::replace(&mut ch1, changed);
        if any {
            quiet = 0;
        } else {
            quiet += 1;
            if quiet >= 2 {
                break;
            }
        }
    }
}

/// Remove the redundant corner pixels Zhang–Suen leaves on staircases, so the
/// skeleton is minimal in 8-connectivity (line pixels have ≤ 2 neighbours).
fn remove_staircases(p: &mut Padded, idx: &[usize]) {
    for &i in idx {
        if p.data[i] == 0 {
            continue;
        }
        let code = p.code(i);
        if code.count_ones() >= 2 && yokoi8(code) == 1 {
            p.data[i] = 0;
        }
    }
}

/// Thin a binary mask to a one-pixel-wide 8-connected skeleton.
pub fn thin(mask: &Mask) -> Mask {
    let mut p = Padded::from_mask(mask);
    zhang_suen(&mut p);
    let idx = p.set_indices();
    remove_staircases(&mut p, &idx);
    p.to_mask()
}

/// Skeleton segments as ordered pixel paths (pixel indices x, y), each at
/// least `min_len` pixels long.
pub fn contour_segments(mask: &Mask, min_len: usize) -> Vec<Vec<(u32, u32)>> {
    if mask.w == 0 || mask.h == 0 {
        return Vec::new();
    }
    let mut p = Padded::from_mask(mask);
    zhang_suen(&mut p);
    let idx = p.set_indices();
    remove_staircases(&mut p, &idx);
    let mut idx: Vec<usize> = idx.into_iter().filter(|&i| p.data[i] != 0).collect();
    prune_spurs(&mut p, &mut idx, 4);
    trace_paths(&mut p, &idx, min_len.max(2))
}

/// Delete spurs: branches that run from an endpoint to a junction in at most
/// `max_len` pixels (thinning artefacts of bumpy line edges). Real line ends
/// are untouched because they do not reach a junction that quickly.
fn prune_spurs(p: &mut Padded, idx: &mut Vec<usize>, max_len: usize) {
    let pw = p.pw as isize;
    let offs: Vec<isize> = RING.iter().map(|&(dx, dy)| dy * pw + dx).collect();
    for _ in 0..2 {
        let data = &p.data;
        let degree = |i: usize| offs.iter().filter(|&&o| data[(i as isize + o) as usize] != 0).count();
        let del: Vec<usize> = idx
            .par_iter()
            .copied()
            .filter(|&i| degree(i) == 1)
            .flat_map_iter(|e| {
                let mut path = vec![e];
                let mut cur = e;
                loop {
                    let next: Vec<usize> = offs
                        .iter()
                        .map(|&o| (cur as isize + o) as usize)
                        .filter(|&j| data[j] != 0 && !path.contains(&j))
                        .collect();
                    if next.len() != 1 {
                        return Vec::new();
                    }
                    let j = next[0];
                    if degree(j) >= 3 {
                        return path;
                    }
                    if path.len() >= max_len {
                        return Vec::new();
                    }
                    path.push(j);
                    cur = j;
                }
            })
            .collect();
        if del.is_empty() {
            break;
        }
        for &i in &del {
            p.data[i] = 0;
        }
        idx.retain(|&i| p.data[i] != 0);
    }
}

/// Pixels over which the local line direction is measured while tracing.
const DIR_WINDOW: usize = 8;
/// Pixels followed ahead of each candidate step to judge its direction
/// (single steps are distorted where the skeleton bends towards a junction).
const LOOKAHEAD: usize = 6;
/// Continue only along a neighbour whose look-ahead direction is within this
/// angle of the current direction…
const MAX_TURN_DEG: f64 = 65.0;
/// …and, at a branch point, at least this much straighter than the next best.
const MIN_TURN_MARGIN_DEG: f64 = 20.0;

/// Trace the skeleton into ordered paths. Dense contours touch in the mask,
/// so their skeletons are joined by short "rungs"; instead of cutting at every
/// junction pixel, a path continues through a junction along its own
/// direction when that continuation is unambiguous, and stops otherwise. Every
/// pixel belongs to at most one path; rungs end up as short paths and are
/// dropped by `min_len`.
fn trace_paths(p: &mut Padded, idx: &[usize], min_len: usize) -> Vec<Vec<(u32, u32)>> {
    let pw = p.pw;
    let offs: Vec<(isize, isize, isize)> = RING.iter().map(|&(dx, dy)| (dx, dy, dy * pw as isize + dx)).collect();
    let degree: Vec<u32> = idx.par_iter().map(|&i| p.code(i).count_ones()).collect();
    let xy = |i: usize| ((i % pw) as f64, (i / pw) as f64);
    let cos_max = MAX_TURN_DEG.to_radians().cos();
    let cosine = |a: (f64, f64), b: (f64, f64)| {
        let n = ((a.0 * a.0 + a.1 * a.1) * (b.0 * b.0 + b.1 * b.1)).sqrt();
        if n > 0.0 {
            (a.0 * b.0 + a.1 * b.1) / n
        } else {
            -1.0
        }
    };
    // Follow unvisited pixels from `c` (a neighbour of `cur`) for a few steps,
    // always taking the one straightest along `u`; return where it ends.
    let look = |data: &[u8], cur: usize, c: usize, u: (f64, f64)| -> usize {
        let mut seen = [usize::MAX; LOOKAHEAD + 2];
        seen[0] = cur;
        seen[1] = c;
        let mut p = c;
        let (cx, cy) = xy(cur);
        for k in 0..LOOKAHEAD {
            let mut best = (f64::NEG_INFINITY, usize::MAX);
            for &(_, _, o) in &offs {
                let q = (p as isize + o) as usize;
                if data[q] == 1 && !seen[..k + 2].contains(&q) {
                    let (qx, qy) = xy(q);
                    let cs = cosine((qx - cx, qy - cy), u);
                    if cs > best.0 {
                        best = (cs, q);
                    }
                }
            }
            if best.1 == usize::MAX {
                break;
            }
            p = best.1;
            seen[k + 2] = p;
        }
        p
    };
    // Walk from the last pixel of `path` while the continuation is clear.
    let walk = |data: &mut [u8], path: &mut Vec<usize>| {
        let mut cands: Vec<(f64, usize)> = Vec::with_capacity(8);
        loop {
            let cur = *path.last().unwrap();
            let u = if path.len() >= 2 {
                let back = path[path.len().saturating_sub(DIR_WINDOW + 1)];
                let (cx, cy) = xy(cur);
                let (bx, by) = xy(back);
                Some((cx - bx, cy - by))
            } else {
                None
            };
            cands.clear();
            for &(dx, dy, o) in &offs {
                let j = (cur as isize + o) as usize;
                if data[j] == 1 {
                    let score = match u {
                        Some(u) => {
                            let e = look(data, cur, j, u);
                            let ((ex, ey), (cx, cy)) = (xy(e), xy(cur));
                            cosine((ex - cx, ey - cy), u)
                        }
                        // No direction yet: prefer 4-neighbours.
                        None if dx == 0 || dy == 0 => 1.0,
                        None => 0.9,
                    };
                    cands.push((score, j));
                }
            }
            if cands.is_empty() {
                break;
            }
            cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let (best_cos, best) = cands[0];
            if path.len() >= 2 {
                if best_cos < cos_max {
                    break;
                }
                if cands.len() > 1 {
                    let a0 = best_cos.clamp(-1.0, 1.0).acos();
                    let a1 = cands[1].0.clamp(-1.0, 1.0).acos();
                    // Two neighbours of the same continuing branch (adjacent to
                    // each other) are not an ambiguity.
                    let (b0x, b0y) = xy(best);
                    let (b1x, b1y) = xy(cands[1].1);
                    let adjacent = (b0x - b1x).abs() <= 1.0 && (b0y - b1y).abs() <= 1.0;
                    if !adjacent && (a1 - a0).to_degrees() < MIN_TURN_MARGIN_DEG {
                        break;
                    }
                }
            }
            data[best] = 2;
            path.push(best);
        }
    };
    let mut out = Vec::new();
    let mut emit = |path: &[usize]| {
        if path.len() >= min_len {
            out.push(path.iter().map(|&i| ((i % pw - 1) as u32, (i / pw - 1) as u32)).collect::<Vec<_>>());
        }
    };
    // Endpoints first (whole open lines), then whatever is left (loops, and
    // lines whose ends were junctions), traced both ways from the start pixel.
    for pass in 0..2 {
        for (&i, &d) in idx.iter().zip(&degree) {
            if p.data[i] != 1 || (pass == 0 && d != 1) {
                continue;
            }
            p.data[i] = 2;
            let mut fwd = vec![i];
            walk(&mut p.data, &mut fwd);
            let mut bwd = vec![i];
            walk(&mut p.data, &mut bwd);
            if bwd.len() > 1 {
                bwd.reverse();
                bwd.pop();
                bwd.extend_from_slice(&fwd);
                emit(&bwd);
            } else {
                emit(&fwd);
            }
        }
    }
    out
}

/// Skeletonise the mask, break it at junctions, keep segments with at least
/// `min_len` pixels and sample every `step`-th pixel.
pub fn contour_samples(mask: &Mask, min_len: usize, step: usize) -> ContourSamples {
    let segs = contour_segments(mask, min_len);
    samples_from_segments(&segs, step)
}

/// Sample every `step`-th pixel of each segment (pixel centres).
pub fn samples_from_segments(segs: &[Vec<(u32, u32)>], step: usize) -> ContourSamples {
    let step = step.max(1);
    let mut s = ContourSamples::default();
    for path in segs {
        let id = s.n_segs as u32;
        for &(x, y) in path.iter().step_by(step) {
            s.pts.push([x as f32 + 0.5, y as f32 + 0.5]);
            s.seg.push(id);
        }
        s.n_segs += 1;
    }
    s
}

/// Segment length / sampling step suited to the image size: about 1/40 of
/// the image diagonal (150 px on a 300-dpi A3 sheet) and every 3rd pixel there.
pub fn default_sampling(w: usize, h: usize) -> (usize, usize) {
    let diag = ((w * w + h * h) as f64).sqrt();
    let min_len = ((diag / 40.0).round() as usize).max(30);
    let step = ((diag / 2000.0).round() as usize).clamp(1, 6);
    (min_len, step)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neighbours(m: &Mask, x: usize, y: usize) -> usize {
        let mut n = 0;
        for (dx, dy) in RING {
            let (xx, yy) = (x as isize + dx, y as isize + dy);
            if xx >= 0 && yy >= 0 && (xx as usize) < m.w && (yy as usize) < m.h && m.get(xx as usize, yy as usize) {
                n += 1;
            }
        }
        n
    }

    #[test]
    fn yokoi_numbers() {
        assert_eq!(yokoi8(0b0101_0000), 1); // W + S
        assert_eq!(yokoi8(0b0100_0100), 2); // E + W
        assert_eq!(yokoi8(0b0000_0010), 1); // NE only
        assert_eq!(yokoi8(0xff), 0);
    }

    #[test]
    fn thick_bar_thins_to_one_line() {
        let mut m = Mask::new(120, 40);
        for y in 15..22 {
            for x in 10..110 {
                m.set(x, y, true);
            }
        }
        let t = thin(&m);
        // Every column in the middle has exactly one skeleton pixel.
        for x in 20..100 {
            let n = (0..40).filter(|&y| t.get(x, y)).count();
            assert_eq!(n, 1, "column {x}");
            let y = (0..40).find(|&y| t.get(x, y)).unwrap();
            assert!((17..=19).contains(&y), "{y}");
        }
        let segs = contour_segments(&m, 10);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].len() > 80);
    }

    #[test]
    fn diagonal_and_curved_lines_are_minimal() {
        // A thick circle: skeleton is a single loop where every pixel has exactly 2 neighbours.
        let mut m = Mask::new(200, 200);
        for y in 0..200 {
            for x in 0..200 {
                let r = ((x as f64 - 100.0).powi(2) + (y as f64 - 100.0).powi(2)).sqrt();
                if (60.0..64.0).contains(&r) {
                    m.set(x, y, true);
                }
            }
        }
        let t = thin(&m);
        let mut n_pix = 0;
        for y in 0..200 {
            for x in 0..200 {
                if t.get(x, y) {
                    n_pix += 1;
                    assert_eq!(neighbours(&t, x, y), 2, "({x},{y})");
                }
            }
        }
        // Circumference of r≈62 in 8-connected steps ≈ 2πr·(0.9..1.0).
        assert!((330..=400).contains(&n_pix), "{n_pix}");
        let segs = contour_segments(&m, 50);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].len(), n_pix);
        // Consecutive path pixels are 8-adjacent.
        for w in segs[0].windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!((a.0 as i32 - b.0 as i32).abs() <= 1 && (a.1 as i32 - b.1 as i32).abs() <= 1);
        }
    }

    #[test]
    fn crossing_lines_one_passes_through() {
        let mut m = Mask::new(200, 200);
        for i in 20..180 {
            for d in 0..3 {
                m.set(i, 99 + d, true);
                m.set(99 + d, i, true);
            }
        }
        let segs = contour_segments(&m, 20);
        let mut lens: Vec<usize> = segs.iter().map(|s| s.len()).collect();
        lens.sort();
        assert_eq!(lens.len(), 3, "{lens:?}");
        assert!(lens[0] > 70 && lens[1] < 82 && lens[2] > 150, "{lens:?}");
        // The through line is straight: it never jumps to the other one.
        let long = segs.iter().find(|s| s.len() > 150).unwrap();
        let xs: Vec<u32> = long.iter().map(|p| p.0).collect();
        let ys: Vec<u32> = long.iter().map(|p| p.1).collect();
        let span = |v: &[u32]| v.iter().max().unwrap() - v.iter().min().unwrap();
        assert!(span(&xs) <= 2 || span(&ys) <= 2);
        let samples = contour_samples(&m, 20, 3);
        assert_eq!(samples.n_segs, 3);
        assert_eq!(samples.pts.len(), samples.seg.len());
        assert!(samples.seg.iter().all(|&s| (s as usize) < samples.n_segs));
        let s0 = samples.seg.iter().filter(|&&s| s == 0).count();
        assert_eq!(s0, segs[0].len().div_ceil(3));
    }

    #[test]
    fn touching_parallel_lines_stay_separate() {
        // Two wavy lines whose masks touch through 1-px bridges ("rungs").
        let mut m = Mask::new(400, 60);
        for x in 5..395 {
            let yc = 20.0 + 3.0 * (x as f64 / 25.0).sin();
            for d in 0..3 {
                m.set(x, yc as usize + d, true);
                m.set(x, yc as usize + 6 + d, true);
            }
            if x % 37 == 0 {
                for d in 3..6 {
                    m.set(x, yc as usize + d, true);
                }
            }
        }
        let segs = contour_segments(&m, 40);
        let long: Vec<&Vec<(u32, u32)>> = segs.iter().filter(|s| s.len() > 300).collect();
        assert_eq!(long.len(), 2, "{:?}", segs.iter().map(|s| s.len()).collect::<Vec<_>>());
        for s in long {
            // Each long path stays on one line: its offset from the wave is constant.
            let offs: Vec<f64> = s.iter().map(|&(x, y)| y as f64 - (20.0 + 3.0 * (x as f64 / 25.0).sin())).collect();
            let lo = offs.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = offs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            assert!(hi - lo < 3.5, "{lo} {hi}");
        }
    }

    #[test]
    fn short_bits_and_spurs_are_dropped() {
        let mut m = Mask::new(300, 60);
        // Long line with a 2-pixel bump (spur source) and some short text-like blobs.
        for x in 10..290 {
            for d in 0..3 {
                m.set(x, 30 + d, true);
            }
        }
        m.set(150, 29, true);
        m.set(150, 28, true);
        for x in 20..30 {
            for y in 5..9 {
                m.set(x, y, true);
            }
        }
        let segs = contour_segments(&m, 40);
        assert_eq!(segs.len(), 1, "{:?}", segs.iter().map(|s| s.len()).collect::<Vec<_>>());
        assert!(segs[0].len() > 270);
    }

    #[test]
    fn sampling_defaults() {
        assert_eq!(default_sampling(3507, 4960), (152, 3));
        let (l, s) = default_sampling(800, 600);
        assert!(l >= 30 && s >= 1);
    }
}
