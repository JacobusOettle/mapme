//! Contour-line pixel classification.
//!
//! Contours on topographic maps are drawn in brown / tan / orange: muted tan
//! on many club and hiking maps (Grootkloof ~ (168,152,136)), mid brown on
//! USGS quads (~ (186,140,100)) and saturated orange-brown on Swiss / OS maps
//! (~ (210,140,70)). A pixel is a contour pixel when its HSV colour lies in
//! the model's box. The default box covers all of these while rejecting black
//! text, grey roads (low saturation), green vegetation, blue water, yellow
//! roads (hue > 45°) and the white / cream paper.

use crate::image::{rgb_to_hsv, Mask, Rgb8};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Colour model deciding which pixels belong to contour lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContourColorModel {
    /// Hue range in degrees (may wrap if min > max).
    pub hue_min: f32,
    pub hue_max: f32,
    pub sat_min: f32,
    pub sat_max: f32,
    pub val_min: f32,
    pub val_max: f32,
}

impl ContourColorModel {
    /// True if the colour belongs to this model.
    #[inline]
    pub fn matches(&self, rgb: [u8; 3]) -> bool {
        let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
        if s < self.sat_min || s > self.sat_max || v < self.val_min || v > self.val_max {
            return false;
        }
        if self.hue_min <= self.hue_max {
            h >= self.hue_min && h <= self.hue_max
        } else {
            h >= self.hue_min || h <= self.hue_max
        }
    }
}

/// Default model: brown / tan / orange contours (most topographic maps).
pub fn default_model() -> ContourColorModel {
    ContourColorModel { hue_min: 10.0, hue_max: 45.0, sat_min: 0.08, sat_max: 0.75, val_min: 0.35, val_max: 0.92 }
}

/// Muted tan / brown only (rejects saturated orange fills and roads).
pub fn muted_model() -> ContourColorModel {
    ContourColorModel { hue_min: 10.0, hue_max: 45.0, sat_min: 0.08, sat_max: 0.55, val_min: 0.35, val_max: 0.92 }
}

/// Saturated orange-brown only (rejects pale beige / sand area tints).
pub fn saturated_model() -> ContourColorModel {
    ContourColorModel { hue_min: 12.0, hue_max: 45.0, sat_min: 0.25, sat_max: 0.92, val_min: 0.40, val_max: 0.97 }
}

/// The models `auto_mask` chooses from, most general first.
pub fn candidate_models() -> Vec<ContourColorModel> {
    vec![default_model(), muted_model(), saturated_model()]
}

/// Model centred on a user-picked contour colour.
///
/// The box is tolerant: anti-aliased line pixels are lighter and less
/// saturated than the line core, and hue is noisy at low saturation. A grey
/// or black pick (saturation < 0.06) gives a hue-free model around its value.
pub fn model_from_rgb(rgb: [u8; 3]) -> ContourColorModel {
    let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
    let val_min = (v - 0.25).max(0.0);
    let val_max = (v + 0.2).min(0.97).max(v);
    if s < 0.06 {
        return ContourColorModel {
            hue_min: 0.0,
            hue_max: 360.0,
            sat_min: 0.0,
            sat_max: (s + 0.08).min(1.0),
            val_min: (v - 0.15).max(0.0),
            val_max: (v + 0.12).min(0.97).max(v),
        };
    }
    // Wider hue tolerance for muted colours, whose hue is poorly defined.
    let tol = 14.0 + 22.0 * ((0.3 - s) / 0.3).clamp(0.0, 1.0);
    let wrap = |x: f32| x.rem_euclid(360.0);
    ContourColorModel {
        hue_min: wrap(h - tol),
        hue_max: wrap(h + tol),
        sat_min: (s * 0.45).max(0.04),
        sat_max: (s * 1.6 + 0.1).min(1.0),
        val_min,
        val_max,
    }
}

/// Classify every pixel; 1 = contour.
pub fn extract_mask(img: &Rgb8, model: &ContourColorModel) -> Mask {
    let mut m = Mask::new(img.w, img.h);
    if img.w == 0 {
        return m;
    }
    m.data.par_chunks_mut(img.w).enumerate().for_each(|(y, row)| {
        let src = &img.data[y * img.w * 3..(y + 1) * img.w * 3];
        for (o, p) in row.iter_mut().zip(src.chunks_exact(3)) {
            *o = model.matches([p[0], p[1], p[2]]) as u8;
        }
    });
    m
}

/// How contour-like each candidate's mask is (higher is better). Exposed for
/// diagnostics; `auto_mask` keeps the best.
#[derive(Debug, Clone)]
pub struct MaskStats {
    pub fraction: f64,
    /// Fraction of mask pixels that survive an erosion (area fills, not lines).
    pub solid_fraction: f64,
    pub score: f64,
}

/// Try a few built-in models and keep the one whose mask looks most like
/// contour lines. Returns the mask and the model used.
///
/// Contour lines are thin, so a good mask has many pixels that vanish under
/// an erosion (radius ~ line half-width) and few that survive it (area tints,
/// fills). Score = thin pixels − 3 × solid pixels, discounted when the
/// coverage is implausible (< 0.2 % or > 40 % of the image).
pub fn auto_mask(img: &Rgb8) -> (Mask, ContourColorModel) {
    let models = candidate_models();
    let (bits, stats) = model_bits(img, &models);
    let best = stats
        .iter()
        .enumerate()
        .fold((0usize, f64::NEG_INFINITY), |acc, (i, s)| if s.score > acc.1 { (i, s.score) } else { acc })
        .0;
    let mut m = Mask::new(img.w, img.h);
    let bit = 1u8 << best;
    m.data.par_iter_mut().zip(bits.par_iter()).for_each(|(o, &b)| *o = (b & bit != 0) as u8);
    (m, models[best].clone())
}

/// Per-pixel bit set of matching models (bit i = models[i]) plus their stats.
pub fn model_bits(img: &Rgb8, models: &[ContourColorModel]) -> (Vec<u8>, Vec<MaskStats>) {
    assert!(models.len() <= 8);
    let (w, h) = (img.w, img.h);
    let mut bits = vec![0u8; w * h];
    if w == 0 || h == 0 {
        return (bits, Vec::new());
    }
    bits.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &img.data[y * w * 3..(y + 1) * w * 3];
        for (o, p) in row.iter_mut().zip(src.chunks_exact(3)) {
            let (hh, s, v) = rgb_to_hsv(p[0], p[1], p[2]);
            let mut b = 0u8;
            for (i, m) in models.iter().enumerate() {
                let hue_ok = if m.hue_min <= m.hue_max {
                    hh >= m.hue_min && hh <= m.hue_max
                } else {
                    hh >= m.hue_min || hh <= m.hue_max
                };
                if hue_ok && s >= m.sat_min && s <= m.sat_max && v >= m.val_min && v <= m.val_max {
                    b |= 1 << i;
                }
            }
            *o = b;
        }
    });
    let r = erosion_radius(w, h);
    let eroded = erode_bits(&bits, w, h, r);
    let count = |plane: &[u8]| -> [usize; 8] {
        plane
            .par_chunks(w)
            .map(|row| {
                let mut c = [0usize; 8];
                for &b in row {
                    if b != 0 {
                        for (i, ci) in c.iter_mut().enumerate() {
                            *ci += ((b >> i) & 1) as usize;
                        }
                    }
                }
                c
            })
            .reduce(
                || [0usize; 8],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b) {
                        *x += y;
                    }
                    a
                },
            )
    };
    let total = count(&bits);
    let solid = count(&eroded);
    let n = (w * h) as f64;
    let stats = (0..models.len())
        .map(|i| {
            let c = total[i] as f64;
            let sld = solid[i] as f64;
            let fraction = c / n;
            let mut score = (c - sld) - 3.0 * sld;
            if !(0.002..=0.4).contains(&fraction) {
                score = score.min(0.0) * 10.0 + score.max(0.0) * 0.1;
            }
            MaskStats { fraction, solid_fraction: if c > 0.0 { sld / c } else { 0.0 }, score }
        })
        .collect();
    (bits, stats)
}

/// Erosion radius ~ the half-width of a thick (index) contour line.
fn erosion_radius(w: usize, h: usize) -> usize {
    ((w.max(h) as f64 / 2480.0).round() as usize).clamp(1, 4)
}

/// Bitwise erosion with a (2r+1)² square: a bit survives only if it is set
/// in the whole neighbourhood (outside the image counts as unset).
fn erode_bits(bits: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let mut horiz = vec![0u8; w * h];
    horiz.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &bits[y * w..(y + 1) * w];
        for x in r..w.saturating_sub(r) {
            let mut b = 0xffu8;
            for v in &src[x - r..=x + r] {
                b &= v;
            }
            row[x] = b;
        }
    });
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y < r || y + r >= h {
            return;
        }
        row.copy_from_slice(&horiz[(y - r) * w..(y - r + 1) * w]);
        for yy in (y - r + 1)..=(y + r) {
            for (o, v) in row.iter_mut().zip(&horiz[yy * w..(yy + 1) * w]) {
                *o &= v;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROOTKLOOF: [u8; 3] = [168, 152, 136];
    const USGS: [u8; 3] = [186, 140, 100];
    const SWISS: [u8; 3] = [210, 140, 70];

    #[test]
    fn default_accepts_common_contour_palettes() {
        let m = default_model();
        for c in [GROOTKLOOF, USGS, SWISS, [150, 110, 70], [200, 170, 140], [232, 154, 90]] {
            assert!(m.matches(c), "{c:?} should be a contour colour");
        }
    }

    #[test]
    fn default_rejects_other_map_colours() {
        let m = default_model();
        let others: [[u8; 3]; 14] = [
            [0, 0, 0],       // black text
            [60, 60, 60],    // dark grey
            [150, 150, 150], // grey road
            [200, 198, 195], // light grey
            [255, 255, 255], // white paper
            [250, 245, 230], // cream paper
            [100, 160, 60],  // green vegetation
            [180, 210, 120], // light green
            [60, 120, 200],  // blue water
            [150, 200, 240], // light blue
            [240, 220, 60],  // yellow road
            [255, 230, 0],   // saturated yellow
            [230, 40, 40],   // red route
            [240, 160, 170], // pink boundary
        ];
        for c in others {
            assert!(!m.matches(c), "{c:?} should not be a contour colour");
        }
    }

    #[test]
    fn picked_colour_model() {
        for c in [GROOTKLOOF, USGS, SWISS, [120, 80, 50]] {
            let m = model_from_rgb(c);
            assert!(m.matches(c));
            // Slightly lighter (anti-aliased) and darker variants.
            let lighter = [c[0].saturating_add(15), c[1].saturating_add(15), c[2].saturating_add(15)];
            let darker = [c[0] - 20, c[1] - 20, c[2] - 20];
            assert!(m.matches(lighter) && m.matches(darker), "{c:?} {m:?}");
            assert!(!m.matches([60, 120, 200]) && !m.matches([255, 255, 255]) && !m.matches([0, 0, 0]));
            assert!(!m.matches([100, 160, 60]));
        }
        // Red contours wrap around hue 0.
        let m = model_from_rgb([200, 40, 50]);
        assert!(m.hue_min > m.hue_max);
        assert!(m.matches([200, 40, 50]) && m.matches([200, 50, 40]));
        assert!(!m.matches([60, 120, 200]));
        // Grey contours: hue-free model.
        let m = model_from_rgb([128, 128, 128]);
        assert!(m.matches([128, 128, 128]) && m.matches([135, 130, 128]));
        assert!(!m.matches([255, 255, 255]) && !m.matches([0, 0, 0]) && !m.matches([210, 140, 70]));
    }

    fn synthetic(line: [u8; 3], fill: Option<[u8; 3]>) -> Rgb8 {
        let (w, h) = (600usize, 400usize);
        let mut data = vec![255u8; w * h * 3];
        let mut put = |x: usize, y: usize, c: [u8; 3]| data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
        if let Some(f) = fill {
            for y in 250..390 {
                for x in 20..580 {
                    put(x, y, f);
                }
            }
        }
        // Wavy 2-px lines.
        for k in 0..12 {
            for x in 0..w {
                let yc = 15.0 + k as f64 * 30.0 + 8.0 * (x as f64 / 40.0 + k as f64).sin();
                for dy in 0..2 {
                    let y = yc as usize + dy;
                    if y < h {
                        put(x, y, line);
                    }
                }
            }
        }
        Rgb8::from_rgb(w, h, data).unwrap()
    }

    #[test]
    fn extract_mask_on_lines() {
        let img = synthetic(GROOTKLOOF, None);
        let m = extract_mask(&img, &default_model());
        assert!(m.fraction() > 0.05 && m.fraction() < 0.1, "{}", m.fraction());
        let (am, model) = auto_mask(&img);
        assert_eq!(model, default_model());
        assert_eq!(am.count(), m.count());
    }

    #[test]
    fn auto_mask_avoids_area_tints() {
        // Saturated orange contours over a pale beige area tint: the default
        // model also grabs the tint; the saturated model only the lines.
        let img = synthetic(SWISS, Some([232, 218, 195]));
        assert!(default_model().matches([232, 218, 195]));
        let (m, model) = auto_mask(&img);
        assert_eq!(model, saturated_model());
        assert!(!m.get(300, 380), "tint must be excluded");
        let line_px = (0..img.w).filter(|&x| m.get(x, (15.0 + 8.0 * (x as f64 / 40.0).sin()) as usize)).count();
        assert!(line_px > img.w * 9 / 10, "{line_px}");
    }
}
