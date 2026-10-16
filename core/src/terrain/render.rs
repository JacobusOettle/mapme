//! Contour and hillshade rendering (`mm_terrain_render`).
//!
//! Every output pixel is a function of its own position only, so a rectangle drawn whole or as
//! any grid of tiles gives the same pixels (CATiledLayer and MKTileOverlay draw tiles on many
//! threads at once, in any order):
//!
//! * the pixel's centre goes to Web Mercator (space `mercator`: linear; space `map`: the map's
//!   homography plus its warp, exactly as the georeference has them) and on to the global pixel
//!   coordinates of the DEM zoom used;
//! * the elevation and its gradient come from a cubic B-spline through the DEM cells (a C² surface:
//!   smooth contour curves, even line widths, no cell-boundary kinks when zoomed far in);
//! * the gradient in output pixels (the DEM gradient through the Jacobian of output pixel → DEM
//!   pixel, the latter from the mapping of the neighbouring pixels) gives each pixel's distance to
//!   the nearest contour level, `|h − level| / |∇h|`: an anti-aliased line of constant width in
//!   output pixels (`line_scale` × points), faded where contours crowd closer than ~2 widths;
//! * hillshade from the ground gradient (light from the north-west at 45°) as a black or white
//!   veil, so it works over any background, transparent included;
//! * index contours are labelled with their elevation: anchors on a lattice fixed in the map's
//!   (or Mercator's) own coordinates snap to the nearest index line where it runs straight and
//!   uncrowded; a tile draws every label that reaches into it, computed the same way by every tile.
//!
//! Cost: one B-spline sample (16 taps; fewer zoomed in or out, sample.rs) and one mapping per
//! pixel, single-threaded (the callers draw tiles on several threads already): 12–19 ms for a
//! 512 × 512 tile on one core of the Linux box the budgets were set on (docs/ARCHITECTURE.md,
//! "C ABI", Terrain).

use super::sample::{coarsest_zoom_with_cell, floor_i64, round_f64, zoom_bounds, Chain, RowBand, HALF_WORLD};
use super::store::Tiles;
use super::{
    auto_interval_m, RenderRequest, RenderResponse, RenderSpace, RenderStyle, Status, MAX_QUERY_TILES_PER_ZOOM,
};
use crate::geo::{Homography, EARTH_RADIUS};
use crate::warp::Warp;

/// Line widths in points: minor contours, and index contours (1.6× wider, docs/design/explorer.md).
pub const MINOR_WIDTH_PT: f64 = 0.7;
pub const INDEX_WIDTH_PT: f64 = 1.12;
/// Contour labels: digit height (points), lattice spacing of the label anchors (points), the
/// farthest an anchor may move to reach its contour (share of the spacing).
pub const LABEL_HEIGHT_PT: f64 = 8.5;
pub const LABEL_SPACING_PT: f64 = 220.0;
const LABEL_REACH: f64 = 0.3;
/// A label sits on an index line only where index lines are at least this far apart (points), and
/// the line strays at most `LABEL_STRAIGHT_PT` from straight over the label's length.
const LABEL_MIN_INDEX_SPACING_PT: f64 = 14.0;
const LABEL_STRAIGHT_PT: f64 = 3.0;
/// Labels are placed on the DEM this many zooms coarser than the render's (smoother, and a
/// sixteenth of the tiles over the margin they need beyond the buffer).
const LABEL_ZOOMS_COARSER: u8 = 2;
/// The farthest from its anchor a label's computation samples the terrain (points): its reach,
/// the label's length either side, a little more for the last Newton steps.
const LABEL_STRAY_PT: f64 = LABEL_REACH * LABEL_SPACING_PT + 2.0 * 3.2 * LABEL_HEIGHT_PT + 8.0;

/// Glyph metrics in ems (the digit height): advance, stroke width, gap kept clear around a label.
const GLYPH_ADVANCE: f64 = 0.66;
const GLYPH_WIDTH: f64 = 0.56;
const GLYPH_STROKE: f64 = 0.15;
const LABEL_PAD: f64 = 0.3;

/// Colours (sRGB 0…1) and opacities of a style.
struct Palette {
    paper: [f64; 3],
    minor: [f64; 3],
    minor_alpha: f64,
    index: [f64; 3],
    index_alpha: f64,
    label: [f64; 3],
    /// Hillshade veil per unit of strength: black on slopes facing away from the light, white on
    /// slopes facing it.
    shadow: f64,
    light: f64,
}

/// The app's dark map: `Theme.uiOffMapNightPaper`, contours in the dark map's tan #836545, index
/// lines lighter.
const DARK: Palette = Palette {
    paper: [0.105, 0.105, 0.115],
    minor: [131.0 / 255.0, 101.0 / 255.0, 69.0 / 255.0],
    minor_alpha: 0.85,
    index: [176.0 / 255.0, 140.0 / 255.0, 100.0 / 255.0],
    index_alpha: 0.95,
    label: [196.0 / 255.0, 160.0 / 255.0, 118.0 / 255.0],
    shadow: 1.6,
    light: 0.9,
};

/// Paper: `Theme.uiOffMapPaper`, brown contours like a printed topographic sheet.
const PAPER: Palette = Palette {
    paper: [0.91, 0.90, 0.87],
    minor: [172.0 / 255.0, 118.0 / 255.0, 70.0 / 255.0],
    minor_alpha: 0.8,
    index: [140.0 / 255.0, 88.0 / 255.0, 44.0 / 255.0],
    index_alpha: 0.95,
    label: [128.0 / 255.0, 78.0 / 255.0, 38.0 / 255.0],
    shadow: 1.2,
    light: 0.8,
};

/// Output pixel ↔ the request's frame (map pixels or Mercator) ↔ Web Mercator.
struct Frame {
    map: Option<(Homography, Option<Warp>)>,
    /// Frame coordinates of the buffer's top-left corner.
    x0: f64,
    y0: f64,
    /// Frame units per buffer pixel along x and y; `ydir` is +1 (map pixels, y down) or −1
    /// (Mercator, y up).
    sx: f64,
    sy: f64,
    ydir: f64,
    /// Sign of the homography's projective denominator over the map (at pixel (0, 0)).
    w_ref: f64,
}

impl Frame {
    /// Frame coordinates of buffer position `(bi, bj)` (pixel `(i, j)`'s centre is `(i + ½, j + ½)`).
    #[inline]
    fn frame(&self, bi: f64, bj: f64) -> (f64, f64) {
        (self.x0 + bi * self.sx, self.y0 + self.ydir * bj * self.sy)
    }

    /// Buffer position of frame coordinates.
    #[inline]
    fn buffer(&self, fx: f64, fy: f64) -> (f64, f64) {
        ((fx - self.x0) / self.sx, (fy - self.y0) * self.ydir / self.sy)
    }

    /// Web Mercator of frame coordinates; None beyond the map's horizon or the world's poles.
    #[inline]
    fn merc(&self, fx: f64, fy: f64) -> Option<(f64, f64)> {
        let (mx, my) = match &self.map {
            None => (fx, fy),
            Some((h, None)) if h.is_affine() => {
                let m = &h.m;
                (m[0] * fx + m[1] * fy + m[2], m[3] * fx + m[4] * fy + m[5])
            }
            Some((h, warp)) => {
                let m = &h.m;
                let w = m[6] * fx + m[7] * fy + m[8];
                let ratio = w / self.w_ref;
                if ratio.is_nan() || ratio <= 0.1 {
                    return None;
                }
                let (mut mx, mut my) = ((m[0] * fx + m[1] * fy + m[2]) / w, (m[3] * fx + m[4] * fy + m[5]) / w);
                if let Some(wp) = warp {
                    let (dx, dy) = wp.offset(fx, fy);
                    mx += dx;
                    my += dy;
                }
                (mx, my)
            }
        };
        (mx.is_finite() && my.is_finite() && my.abs() <= HALF_WORLD).then_some((mx, my))
    }

    /// Mercator metres per frame unit around a frame point (the mapping's mean scale).
    fn merc_per_frame(&self, fx: f64, fy: f64) -> Option<f64> {
        match &self.map {
            None => Some(1.0),
            Some((h, _)) => {
                let a = h.local_affine(fx, fy);
                let s = (a.a * a.e - a.b * a.d).abs().sqrt();
                (s.is_finite() && s > 0.0).then_some(s)
            }
        }
    }
}

/// Cosine of the latitude at a Mercator y.
#[inline]
fn cos_lat(my: f64) -> f64 {
    1.0 / (my / EARTH_RADIUS).cosh()
}

#[inline]
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Coverage of a line `width` pixels wide at distance `d` pixels from its centre.
#[inline]
fn line_coverage(d: f64, width: f64) -> f64 {
    if width >= 1.0 {
        (width * 0.5 + 0.5 - d).clamp(0.0, 1.0)
    } else {
        (1.0 - d).clamp(0.0, 1.0) * width
    }
}

/// Premultiplied "over".
#[inline]
fn over(dst: &mut [f64; 4], rgb: [f64; 3], a: f64) {
    if a <= 0.0 {
        return;
    }
    let k = 1.0 - a;
    dst[0] = rgb[0] * a + dst[0] * k;
    dst[1] = rgb[1] * a + dst[1] * k;
    dst[2] = rgb[2] * a + dst[2] * k;
    dst[3] = a + dst[3] * k;
}

/// One elevation label placed on an index contour.
struct Label {
    /// Centre in frame coordinates (absolute: the same for every tile).
    fx: f64,
    fy: f64,
    /// Reading direction in buffer pixels (unit), and the text.
    dir: (f64, f64),
    text: Vec<u8>,
}

pub fn render(
    tiles: &Tiles,
    req: &RenderRequest,
    rgba: &mut [u8],
    w: usize,
    h: usize,
    stride: usize,
) -> Result<RenderResponse, String> {
    let (z_max, z_min) = zoom_bounds(req.max_zoom, req.min_zoom)?;
    let [a0, a1, a2, a3] = req.rect;
    if !req.rect.iter().all(|v| v.is_finite()) || a2 <= a0 || a3 <= a1 {
        return Err(format!("Invalid rect {:?}: [x0, y0, x1, y1] with x1 > x0 and y1 > y0", req.rect));
    }
    if w == 0 || h == 0 || stride < w * 4 || rgba.len() < stride * (h - 1) + w * 4 {
        return Err("Invalid buffer".into());
    }
    if let Some(i) = req.interval_m {
        if !(i.is_finite() && i > 0.0) {
            return Err(format!("Invalid interval_m {i}"));
        }
    }
    if let Some(s) = req.scale_m_per_pt {
        if !(s.is_finite() && s > 0.0) {
            return Err(format!("Invalid scale_m_per_pt {s}"));
        }
    }
    if !req.line_scale.is_finite() || req.line_scale <= 0.0 {
        return Err(format!("Invalid line_scale {}", req.line_scale));
    }
    let line_scale = req.line_scale.clamp(0.1, 16.0);
    let hillshade = if req.hillshade.is_finite() { req.hillshade.clamp(0.0, 1.0) } else { 0.0 };
    let pal = match req.style {
        RenderStyle::Dark => &DARK,
        RenderStyle::Paper => &PAPER,
    };

    let frame = match req.space {
        RenderSpace::Mercator => Frame {
            map: None,
            x0: a0,
            y0: a3,
            sx: (a2 - a0) / w as f64,
            sy: (a3 - a1) / h as f64,
            ydir: -1.0,
            w_ref: 1.0,
        },
        RenderSpace::Map => {
            let m = req.pixel_to_mercator.ok_or("Space map needs pixel_to_mercator")?;
            let hom = Homography { m };
            if !m.iter().all(|v| v.is_finite()) || hom.inverse().is_none() || m[8] == 0.0 {
                return Err("Invalid pixel_to_mercator".into());
            }
            let warp = match &req.warp {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => {
                    let wp: Warp = serde_json::from_value(v.clone()).map_err(|e| format!("Invalid warp: {e}"))?;
                    // A malformed warp is ignored (zero offset), as the app does.
                    wp.is_valid().then_some(wp)
                }
            };
            Frame {
                map: Some((hom, warp)),
                x0: a0,
                y0: a1,
                sx: (a2 - a0) / w as f64,
                sy: (a3 - a1) / h as f64,
                ydir: 1.0,
                w_ref: m[8],
            }
        }
    };

    // Scale: ground metres per buffer pixel, and Mercator metres per buffer pixel for the zoom.
    let (cfx, cfy) = frame.frame(w as f64 / 2.0, h as f64 / 2.0);
    let centre = frame.merc(cfx, cfy);
    let centre_scale = centre.zip(frame.merc_per_frame(cfx, cfy)).map(|((_, my), s)| (s * frame.sx, cos_lat(my)));
    let ground_per_px = match req.scale_m_per_pt {
        Some(s) => s / line_scale,
        None => centre_scale.map_or(f64::NAN, |(m, c)| m * c),
    };
    let merc_per_px = match (&frame.map, req.scale_m_per_pt) {
        // Mercator tiles of one zoom are the same size: the zoom follows from that alone.
        (None, _) => frame.sx,
        // A map's tiles of one level share scale_m_per_pt; one latitude per map (its pixel (0, 0))
        // turns it into Mercator, so every tile of the level picks the same DEM zoom.
        (Some(_), Some(_)) => {
            let lat_ref = frame.merc(0.0, 0.0).or(centre).map_or(1.0, |(_, my)| cos_lat(my));
            ground_per_px / lat_ref
        }
        (Some(_), None) => centre_scale.map_or(f64::NAN, |(m, _)| m),
    };
    let interval = req.interval_m.unwrap_or_else(|| auto_interval_m(ground_per_px * line_scale));
    let index_every = req.index_every;

    // Zoom, and every tile the pixels may touch.
    let bbox = mapped_bbox(&frame, w as f64, h as f64, 2.0);
    let mut z_want = coarsest_zoom_with_cell(2.0 * merc_per_px, z_min, z_max);
    let mut z_lo = z_min;
    let mut chain = loop {
        let mut chain = Chain::new(z_want, z_lo);
        let fits = match bbox {
            None => true,
            Some(b) => {
                chain.levels.iter_mut().all(|l| l.want_rect(b[0], b[1], b[2], b[3], 3.0, MAX_QUERY_TILES_PER_ZOOM))
            }
        };
        if fits || z_want == 0 {
            break chain;
        }
        z_want -= 1;
        z_lo = z_lo.min(z_want);
    };
    // Labels come from the DEM two zooms coarser (within the chain): they reach beyond the
    // buffer, and there a sixteenth of the tiles covers the same ground.
    let labels_on = req.labels && index_every > 0;
    let mut label_chain = labels_on.then(|| {
        let lz = z_want.saturating_sub(LABEL_ZOOMS_COARSER).max(z_lo);
        Chain::new(lz, lz)
    });
    if let Some(lc) = &mut label_chain {
        let margin_px = (label_margin_pt() + LABEL_STRAY_PT + 2.0) * line_scale;
        let fits = match mapped_bbox(&frame, w as f64, h as f64, margin_px) {
            None => false,
            Some(b) => lc.levels[0].want_rect(b[0], b[1], b[2], b[3], 3.0, MAX_QUERY_TILES_PER_ZOOM),
        };
        if !fits {
            label_chain = None;
        }
    }
    {
        let mut snap = tiles.snapshot();
        chain.fill(&mut snap);
        if let Some(lc) = &mut label_chain {
            lc.fill(&mut snap);
        }
    }
    let res0 = chain.finest().res;

    let labels = match &mut label_chain {
        Some(lc) => place_labels(&frame, lc, w as f64, h as f64, line_scale, interval * index_every as f64),
        None => Vec::new(),
    };

    // Background, premultiplied.
    let bg: [f64; 4] = match req.background {
        Some([r, g, b, a]) => {
            let a = a as f64 / 255.0;
            [r as f64 / 255.0 * a, g as f64 / 255.0 * a, b as f64 / 255.0 * a, a]
        }
        None => [pal.paper[0], pal.paper[1], pal.paper[2], 1.0],
    };

    let minor_w = MINOR_WIDTH_PT * line_scale;
    let index_w = INDEX_WIDTH_PT * line_scale;
    let em = LABEL_HEIGHT_PT * line_scale;
    // Output pixels per DEM cell: zoomed in, cache each cell's polynomial (sample.rs).
    let cached = res0 / merc_per_px >= 2.5;
    let inv_interval = 1.0 / interval;
    let index_interval = interval * index_every as f64;
    let inv_index_interval = if index_every > 0 { 1.0 / index_interval } else { 0.0 };
    // No pixel farther from a level than this (in buffer pixels) has any line on it.
    let reach_px = index_w.max(minor_w).max(1.0) * 0.5 + 0.5;
    let frac_1_sqrt_2 = std::f64::consts::FRAC_1_SQRT_2;

    // Rows of DEM pixel coordinates (finest zoom) with a one-pixel apron, rolling.
    let wa = w + 2;
    let inv_res0 = 1.0 / res0;
    let row_uv = |j: isize, out: &mut Vec<(f64, f64)>| {
        out.clear();
        for i in -1..=(w as isize) {
            let (fx, fy) = frame.frame(i as f64 + 0.5, j as f64 + 0.5);
            out.push(match frame.merc(fx, fy) {
                Some((mx, my)) => ((mx + HALF_WORLD) * inv_res0, (HALF_WORLD - my) * inv_res0),
                None => (f64::NAN, f64::NAN),
            });
        }
    };
    // Central difference, one-sided next to unmapped pixels.
    let diff = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| -> (f64, f64) {
        match (a.0.is_finite(), b.0.is_finite()) {
            (true, true) => ((b.0 - a.0) * 0.5, (b.1 - a.1) * 0.5),
            (false, true) => (b.0 - c.0, b.1 - c.1),
            (true, false) => (c.0 - a.0, c.1 - a.1),
            (false, false) => (0.0, 0.0),
        }
    };
    let (mut prev, mut cur, mut next) = (Vec::with_capacity(wa), Vec::with_capacity(wa), Vec::with_capacity(wa));
    row_uv(-1, &mut prev);
    row_uv(0, &mut cur);
    let mut unknown = 0usize;
    // 1 / (ground metres per finest DEM pixel), per tile row (the latitude barely changes).
    let mut inv_cell_cache = (i64::MIN, 1.0);
    let mut row_labels: Vec<(&Label, f64, f64, f64)> = Vec::new();
    // Mercator rows share one v: the B-spline's vertical half once per row (sample.rs, RowBand),
    // when every cell is a new one (zoomed out). Same bits as pixel by pixel.
    let banded = frame.map.is_none() && !cached;
    let mut band = RowBand::default();
    for j in 0..h {
        row_uv(j as isize + 1, &mut next);
        let use_band = banded && {
            let (first, last) = (cur[1], cur[w]);
            first.0.is_finite()
                && last.0.is_finite()
                && chain.levels[0].row_band(
                    first.1,
                    floor_i64(first.0 - 0.5) - 1,
                    floor_i64(last.0 - 0.5) + 2,
                    &mut band,
                )
        };
        let out_row = &mut rgba[j * stride..j * stride + w * 4];
        // Labels touching this row: buffer centre and half length.
        row_labels.clear();
        for l in &labels {
            let (cx, cy) = frame.buffer(l.fx, l.fy);
            let half_len = label_half_length_em(l.text.len()) * em;
            if (j as f64 + 0.5 - cy).abs() <= half_len + LABEL_PAD * em + 1.0 {
                row_labels.push((l, cx, cy, half_len));
            }
        }
        for i in 0..w {
            let mut px = bg;
            let (u, v) = cur[i + 1];
            let sample = match use_band.then(|| band.eval(u)).flatten() {
                Some(hit) => Some(hit),
                None if u.is_finite() => chain.bspline_px(u, v, cached),
                None => None,
            };
            if let Some((hh, hu, hv)) = sample {
                // The gradient in buffer pixels: the DEM gradient through the Jacobian of buffer
                // pixel → DEM pixel, from the neighbours' mapping.
                let (du_di, dv_di) = diff(cur[i], cur[i + 2], cur[i + 1]);
                let (du_dj, dv_dj) = diff(prev[i + 1], next[i + 1], cur[i + 1]);
                let gi = hu * du_di + hv * dv_di;
                let gj = hu * du_dj + hv * dv_dj;
                let g = (gi * gi + gj * gj).sqrt();

                if hillshade > 0.0 && hh > 0.0 {
                    let ty = floor_i64(v).div_euclid(256);
                    if ty != inv_cell_cache.0 {
                        inv_cell_cache = (ty, 1.0 / (res0 * cos_lat(HALF_WORLD - (ty as f64 + 0.5) * 256.0 * res0)));
                    }
                    let (p, q) = (hu * inv_cell_cache.1, -hv * inv_cell_cache.1);
                    let shade = (0.5 * (p - q) + frac_1_sqrt_2) / (1.0 + p * p + q * q).sqrt();
                    let delta = shade - frac_1_sqrt_2;
                    if delta < 0.0 {
                        let a = (hillshade * pal.shadow * -delta).min(0.85);
                        for c in &mut px {
                            *c *= 1.0 - a;
                        }
                        px[3] += a;
                    } else {
                        over(&mut px, [1.0; 3], (hillshade * pal.light * delta).min(0.5));
                    }
                }

                // The gap around labels, and their text.
                let mut gap = 1.0f64;
                let mut text = 0.0f64;
                if !row_labels.is_empty() {
                    let (bx, by) = (i as f64 + 0.5, j as f64 + 0.5);
                    for &(l, cx, cy, half_len) in &row_labels {
                        let (qx, qy) = (bx - cx, by - cy);
                        let along = qx * l.dir.0 + qy * l.dir.1;
                        let up = qx * l.dir.1 - qy * l.dir.0;
                        let outside =
                            (along.abs() - (half_len + LABEL_PAD * em)).max(up.abs() - (0.5 + LABEL_PAD) * em);
                        gap = gap.min(outside.clamp(0.0, 1.0));
                        if outside < 0.0 {
                            text = text.max(glyph_coverage(&l.text, (along + half_len) / em, 0.5 - up / em, em));
                        }
                    }
                }

                // The nearest contour level; most pixels are too far from it for any line.
                let kf = round_f64(hh * inv_interval);
                let off = (hh - kf * interval).abs();
                if kf >= 0.0 && g > 1e-9 && off < reach_px * g {
                    let dist = off / g;
                    let is_index = index_every > 0 && round_f64(hh * inv_index_interval) * index_every as f64 == kf;
                    let (width, rgb, alpha, fade) = if is_index {
                        (index_w, pal.index, pal.index_alpha, smoothstep(index_w, 2.0 * index_w, index_interval / g))
                    } else {
                        (minor_w, pal.minor, pal.minor_alpha, smoothstep(1.2 * minor_w, 2.5 * minor_w, interval / g))
                    };
                    over(&mut px, rgb, line_coverage(dist, width) * alpha * fade * gap);
                }
                over(&mut px, pal.label, text);
            } else {
                unknown += 1;
            }
            let o = &mut out_row[i * 4..i * 4 + 4];
            for c in 0..4 {
                o[c] = (px[c] * 255.0 + 0.5) as u8;
            }
        }
        std::mem::swap(&mut prev, &mut cur);
        std::mem::swap(&mut cur, &mut next);
    }

    let (ncx, ncy) = centre.unwrap_or_else(|| bbox.map_or((0.0, 0.0), |b| ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0)));
    let mut chains = vec![&chain];
    chains.extend(label_chain.as_ref());
    let need_tiles = Chain::need_tiles_of(&chains, ncx, ncy, req.max_tiles);
    Ok(RenderResponse {
        status: if need_tiles.is_empty() { Status::Ok } else { Status::NeedDem },
        interval_m: interval,
        index_interval_m: interval * index_every as f64,
        zoom: chain.zoom_used(),
        missing_fraction: unknown as f64 / (w * h) as f64,
        need_tiles,
    })
}

/// Mercator bounding box of the buffer grown by `margin_px` buffer pixels (its edges sampled,
/// plus the warp's largest offset); None when nothing of it maps (beyond the horizon).
fn mapped_bbox(frame: &Frame, w: f64, h: f64, margin_px: f64) -> Option<[f64; 4]> {
    const STEPS: usize = 32;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut any = false;
    let (i0, i1, j0, j1) = (-margin_px, w + margin_px, -margin_px, h + margin_px);
    let mut add = |bi: f64, bj: f64| {
        let (fx, fy) = frame.frame(bi, bj);
        if let Some((mx, my)) = frame.merc(fx, fy) {
            b = [b[0].min(mx), b[1].min(my), b[2].max(mx), b[3].max(my)];
            any = true;
        }
    };
    for k in 0..=STEPS {
        let t = k as f64 / STEPS as f64;
        add(i0 + t * (i1 - i0), j0);
        add(i0 + t * (i1 - i0), j1);
        add(i0, j0 + t * (j1 - j0));
        add(i1, j0 + t * (j1 - j0));
        add(i0 + t * (i1 - i0), j0 + t * (j1 - j0));
    }
    if !any {
        return None;
    }
    // A warp moves points by at most its largest node offset (bilinear between nodes).
    let pad = match &frame.map {
        Some((_, Some(wp))) => wp.max_offset_m(),
        _ => 0.0,
    };
    Some([b[0] - pad, (b[1] - pad).max(-HALF_WORLD), b[2] + pad, (b[3] + pad).min(HALF_WORLD)])
}

// ---------------------------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------------------------

/// Half a label's length in ems.
fn label_half_length_em(chars: usize) -> f64 {
    (chars.max(1) as f64 - 1.0) * GLYPH_ADVANCE * 0.5 + GLYPH_WIDTH * 0.5
}

/// Elevation and its gradient in frame units at frame coordinates (None where unknown).
fn frame_sample(frame: &Frame, chain: &mut Chain, fx: f64, fy: f64) -> Option<(f64, f64, f64)> {
    let res0 = chain.finest().res;
    let uv = |fx: f64, fy: f64| frame.merc(fx, fy).map(|(mx, my)| ((mx + HALF_WORLD) / res0, (HALF_WORLD - my) / res0));
    let (u, v) = uv(fx, fy)?;
    let (h, hu, hv) = chain.bspline_px(u, v, false)?;
    // d(u, v)/d(frame) by central differences of one buffer pixel.
    let (ex, ey) = (frame.sx, frame.sy);
    let (ua, ub) = (uv(fx - ex, fy)?, uv(fx + ex, fy)?);
    let (uc, ud) = (uv(fx, fy - ey)?, uv(fx, fy + ey)?);
    let (du_dx, dv_dx) = ((ub.0 - ua.0) / (2.0 * ex), (ub.1 - ua.1) / (2.0 * ex));
    let (du_dy, dv_dy) = ((ud.0 - uc.0) / (2.0 * ey), (ud.1 - uc.1) / (2.0 * ey));
    Some((h, hu * du_dx + hv * dv_dx, hu * du_dy + hv * dv_dy))
}

/// Point-space helpers of a render: frame units per point along x and y (frame orientation, so
/// Mercator's y points north), the label lattice and the index interval.
struct LabelSpace {
    ux: f64,
    uy: f64,
    /// Lattice spacing along x and between rows (frame units): a hexagonal lattice, every other
    /// row shifted by half a spacing.
    sx: f64,
    sy: f64,
    index_interval: f64,
}

/// The labels that reach into the buffer. Every anchor of a hexagonal lattice fixed in frame
/// coordinates (`LABEL_SPACING_PT` apart) snaps to the nearest index contour within
/// `LABEL_REACH` of the spacing, where it runs straight and uncrowded. Anchors are a spacing
/// apart and move at most 0.3 of it, so labels stay 0.4 spacings apart; each depends only on its
/// anchor's surroundings, computed the same way by every tile, so the tiles agree.
fn place_labels(frame: &Frame, chain: &mut Chain, w: f64, h: f64, line_scale: f64, index_interval: f64) -> Vec<Label> {
    if !(index_interval.is_finite() && index_interval > 0.0) {
        return Vec::new();
    }
    let (ux, uy) = (frame.sx * line_scale, frame.sy * line_scale);
    let sp = LabelSpace { ux, uy, sx: LABEL_SPACING_PT * ux, sy: LABEL_SPACING_PT * uy * 0.866, index_interval };
    let margin_px = label_margin_pt() * line_scale;
    let (fa, fb) = (frame.frame(-margin_px, -margin_px), frame.frame(w + margin_px, h + margin_px));
    let (fx0, fx1) = (fa.0.min(fb.0), fa.0.max(fb.0));
    let (fy0, fy1) = (fa.1.min(fb.1), fa.1.max(fb.1));
    let (b0, b1) = ((fy0 / sp.sy).floor() as i64, (fy1 / sp.sy).ceil() as i64);
    if b1 - b0 > 200 || (fx1 - fx0) / sp.sx > 200.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for b in b0..=b1 {
        let shift = if b.rem_euclid(2) == 1 { 0.5 } else { 0.0 };
        let (a0, a1) = ((fx0 / sp.sx - shift).floor() as i64, (fx1 / sp.sx - shift).ceil() as i64);
        for a in a0..=a1 {
            let Some(l) = label_at(frame, chain, &sp, (a as f64 + shift) * sp.sx, b as f64 * sp.sy) else { continue };
            let (cx, cy) = frame.buffer(l.fx, l.fy);
            let r = (label_half_length_em(l.text.len()) + LABEL_PAD) * LABEL_HEIGHT_PT * line_scale + 2.0;
            if cx > -r && cx < w + r && cy > -r && cy < h + r {
                out.push(l);
            }
        }
    }
    out
}

/// How far beyond a buffer (points) the labels reaching into it may be anchored.
fn label_margin_pt() -> f64 {
    LABEL_REACH * LABEL_SPACING_PT + 2.0 * (label_half_length_em(5) + LABEL_PAD) * LABEL_HEIGHT_PT
}

/// The point of contour `level` nearest a frame point by Newton steps (in points, isotropic), and
/// the gradient there (metres per point, frame orientation).
fn project(
    frame: &Frame,
    chain: &mut Chain,
    sp: &LabelSpace,
    (mut px, mut py): (f64, f64),
    level: f64,
    anchor: (f64, f64),
) -> Option<(f64, f64, f64, f64)> {
    for _ in 0..8 {
        // Never sample beyond LABEL_STRAY_PT of the anchor: every tile that computes this label
        // holds the terrain out to there, so they all compute it alike.
        if ((px - anchor.0) / sp.ux).hypot((py - anchor.1) / sp.uy) > LABEL_STRAY_PT {
            return None;
        }
        let (hh, gx, gy) = frame_sample(frame, chain, px, py)?;
        let (gpx, gpy) = (gx * sp.ux, gy * sp.uy);
        let g2 = gpx * gpx + gpy * gpy;
        if g2.is_nan() || g2 <= 1e-18 {
            return None;
        }
        if (hh - level).abs() < 1e-3 * sp.index_interval {
            return Some((px, py, gpx, gpy));
        }
        // A step of at most 40 points: Newton overshoots where the slope changes fast.
        let k = ((hh - level) / g2).clamp(-40.0 / g2.sqrt(), 40.0 / g2.sqrt());
        px -= k * gpx * sp.ux;
        py -= k * gpy * sp.uy;
    }
    None
}

/// The label an anchor at frame `(ax, ay)` makes, if any.
fn label_at(frame: &Frame, chain: &mut Chain, sp: &LabelSpace, ax: f64, ay: f64) -> Option<Label> {
    let (h0, _, _) = frame_sample(frame, chain, ax, ay)?;
    let level = (h0 / sp.index_interval).round() * sp.index_interval;
    if level <= 0.0 {
        return None;
    }
    let (px, py, gpx, gpy) = project(frame, chain, sp, (ax, ay), level, (ax, ay))?;
    let pt = |dx: f64, dy: f64| (dx / sp.ux).hypot(dy / sp.uy);
    if pt(px - ax, py - ay) > LABEL_REACH * LABEL_SPACING_PT {
        return None;
    }
    let g = gpx.hypot(gpy);
    if sp.index_interval / g < LABEL_MIN_INDEX_SPACING_PT {
        return None;
    }
    let text = format!("{}", level.round() as i64).into_bytes();
    // The label's direction: the chord between the contour's points half a label to either side.
    let half = (label_half_length_em(text.len()) + LABEL_PAD) * LABEL_HEIGHT_PT;
    let (tx, ty) = (-gpy / g, gpx / g);
    let end = |s: f64, chain: &mut Chain| {
        project(frame, chain, sp, (px + s * half * tx * sp.ux, py + s * half * ty * sp.uy), level, (ax, ay))
    };
    let (q0, q1) = (end(-1.0, chain)?, end(1.0, chain)?);
    let (cx, cy) = ((q1.0 - q0.0) / sp.ux, (q1.1 - q0.1) / sp.uy);
    let chord = cx.hypot(cy);
    if chord < 1.6 * half {
        return None; // the contour turns back within the label
    }
    let (dx, dy) = (cx / chord, cy / chord);
    // Straight enough: the contour stays within LABEL_STRAIGHT_PT of the chord through the label's
    // centre over its length.
    for s in [-0.5, 0.5] {
        let q = end(s, chain)?;
        let off = (((q.0 - px) / sp.ux) * dy - ((q.1 - py) / sp.uy) * dx).abs();
        if off > LABEL_STRAIGHT_PT {
            return None;
        }
    }
    for q in [q0, q1] {
        let off = (((q.0 - px) / sp.ux) * dy - ((q.1 - py) / sp.uy) * dx).abs();
        if off > LABEL_STRAIGHT_PT {
            return None;
        }
    }
    // Reading direction in buffer pixels: upright (left to right; bottom to top when vertical).
    let (mut bx, mut by) = (dx, dy * frame.ydir);
    if bx < -1e-9 || (bx.abs() <= 1e-9 && by > 0.0) {
        bx = -bx;
        by = -by;
    }
    Some(Label { fx: px, fy: py, dir: (bx, by), text })
}

/// Coverage of a label's text at text coordinates (ems from the left end, ems down from the
/// top), `em` pixels per em.
fn glyph_coverage(text: &[u8], x: f64, y: f64, em: f64) -> f64 {
    if !(-GLYPH_STROKE..=1.0 + GLYPH_STROKE).contains(&y) {
        return 0.0;
    }
    let mut best = f64::INFINITY;
    for (k, &ch) in text.iter().enumerate() {
        let gx = x - k as f64 * GLYPH_ADVANCE;
        if !(-GLYPH_STROKE..=GLYPH_WIDTH + GLYPH_STROKE).contains(&gx) {
            continue;
        }
        for stroke in glyph(ch) {
            for s in stroke.windows(2) {
                best = best.min(segment_distance(gx, y, s[0], s[1]));
            }
        }
    }
    ((GLYPH_STROKE * 0.5 - best) * em + 0.5).clamp(0.0, 1.0)
}

#[inline]
fn segment_distance(x: f64, y: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let (wx, wy) = (x - a.0, y - a.1);
    let l2 = vx * vx + vy * vy;
    let t = if l2 > 0.0 { ((wx * vx + wy * vy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (wx - t * vx).hypot(wy - t * vy)
}

/// Stroke font for the digits and the minus sign: polylines in a 0.56 × 1 em box, y down.
fn glyph(ch: u8) -> &'static [&'static [(f64, f64)]] {
    const ZERO: &[(f64, f64)] = &[
        (0.28, 0.0),
        (0.43, 0.05),
        (0.52, 0.2),
        (0.55, 0.5),
        (0.52, 0.8),
        (0.43, 0.95),
        (0.28, 1.0),
        (0.13, 0.95),
        (0.04, 0.8),
        (0.01, 0.5),
        (0.04, 0.2),
        (0.13, 0.05),
        (0.28, 0.0),
    ];
    const ONE: &[(f64, f64)] = &[(0.1, 0.2), (0.34, 0.0), (0.34, 1.0)];
    const TWO: &[(f64, f64)] = &[
        (0.03, 0.2),
        (0.11, 0.06),
        (0.26, 0.0),
        (0.41, 0.04),
        (0.51, 0.16),
        (0.51, 0.32),
        (0.42, 0.47),
        (0.03, 1.0),
        (0.55, 1.0),
    ];
    const THREE_A: &[(f64, f64)] =
        &[(0.04, 0.1), (0.16, 0.01), (0.34, 0.0), (0.48, 0.07), (0.52, 0.22), (0.45, 0.37), (0.3, 0.45), (0.18, 0.45)];
    const THREE_B: &[(f64, f64)] =
        &[(0.3, 0.45), (0.47, 0.53), (0.55, 0.69), (0.52, 0.86), (0.4, 0.97), (0.24, 1.0), (0.1, 0.96), (0.01, 0.87)];
    const FOUR: &[(f64, f64)] = &[(0.42, 1.0), (0.42, 0.0), (0.0, 0.68), (0.56, 0.68)];
    const FIVE: &[(f64, f64)] = &[
        (0.5, 0.0),
        (0.1, 0.0),
        (0.05, 0.44),
        (0.2, 0.37),
        (0.36, 0.37),
        (0.5, 0.46),
        (0.55, 0.65),
        (0.51, 0.85),
        (0.39, 0.97),
        (0.24, 1.0),
        (0.1, 0.96),
        (0.01, 0.87),
    ];
    const SIX: &[(f64, f64)] = &[
        (0.5, 0.08),
        (0.4, 0.01),
        (0.25, 0.0),
        (0.11, 0.09),
        (0.03, 0.3),
        (0.02, 0.6),
        (0.05, 0.82),
        (0.15, 0.96),
        (0.28, 1.0),
        (0.42, 0.96),
        (0.52, 0.83),
        (0.54, 0.67),
        (0.5, 0.52),
        (0.4, 0.43),
        (0.27, 0.41),
        (0.14, 0.45),
        (0.05, 0.55),
        (0.03, 0.62),
    ];
    const SEVEN: &[(f64, f64)] = &[(0.01, 0.0), (0.55, 0.0), (0.2, 1.0)];
    const EIGHT_A: &[(f64, f64)] = &[
        (0.28, 0.0),
        (0.42, 0.04),
        (0.48, 0.14),
        (0.46, 0.3),
        (0.38, 0.41),
        (0.28, 0.46),
        (0.18, 0.41),
        (0.1, 0.3),
        (0.08, 0.14),
        (0.14, 0.04),
        (0.28, 0.0),
    ];
    const EIGHT_B: &[(f64, f64)] = &[
        (0.28, 0.46),
        (0.44, 0.52),
        (0.54, 0.65),
        (0.55, 0.8),
        (0.47, 0.94),
        (0.28, 1.0),
        (0.09, 0.94),
        (0.01, 0.8),
        (0.02, 0.65),
        (0.12, 0.52),
        (0.28, 0.46),
    ];
    const NINE: &[(f64, f64)] = &[
        (0.06, 0.92),
        (0.16, 0.99),
        (0.31, 1.0),
        (0.45, 0.91),
        (0.53, 0.7),
        (0.54, 0.4),
        (0.51, 0.18),
        (0.41, 0.04),
        (0.28, 0.0),
        (0.14, 0.04),
        (0.04, 0.17),
        (0.02, 0.33),
        (0.06, 0.48),
        (0.16, 0.57),
        (0.29, 0.59),
        (0.42, 0.55),
        (0.51, 0.45),
        (0.53, 0.38),
    ];
    const MINUS: &[(f64, f64)] = &[(0.08, 0.55), (0.48, 0.55)];
    match ch {
        b'0' => &[ZERO],
        b'1' => &[ONE],
        b'2' => &[TWO],
        b'3' => &[THREE_A, THREE_B],
        b'4' => &[FOUR],
        b'5' => &[FIVE],
        b'6' => &[SIX],
        b'7' => &[SEVEN],
        b'8' => &[EIGHT_A, EIGHT_B],
        b'9' => &[NINE],
        b'-' => &[MINUS],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_is_antialiased() {
        assert_eq!(line_coverage(0.0, 2.0), 1.0);
        assert_eq!(line_coverage(1.5, 2.0), 0.0);
        assert!((line_coverage(1.0, 2.0) - 0.5).abs() < 1e-12);
        assert!((line_coverage(0.0, 0.5) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn digits_have_ink() {
        for ch in b"0123456789" {
            let text = [*ch];
            let mut ink = 0.0;
            for j in 0..40 {
                for i in 0..24 {
                    ink += glyph_coverage(&text, i as f64 / 40.0, j as f64 / 40.0, 40.0);
                }
            }
            assert!(ink > 40.0, "{}: {ink}", *ch as char);
        }
        assert_eq!(glyph_coverage(b"1", 5.0, 0.5, 40.0), 0.0);
    }
}
