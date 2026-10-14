//! Scale priors from a map's scale bar: the distance printed beside it ("1 mi",
//! "2 km", "500 ft", or a row of tick labels "0 1 2 km") and its length in the
//! image, so the ground metres per pixel are known without the raster's dpi
//! (a photo, a screenshot, an exported raster) and without a printed
//! representative fraction (screen maps print none).
//!
//! * One label with a distance and its unit: the bar is the longest dark
//!   horizontal line beside it (above, below or level with the label, within
//!   2.5 text heights), under the label or reaching to within 1.5 text
//!   heights of it, between 3 and 60 text heights long and thin (a line, not
//!   a box or the frame). Gaia GPS exports: "1 mi" centred over a 532 px
//!   line on a 300-dpi page (3.025 m/px; the truth is 3.03 × 3.08). A read
//!   taller than 8 % of the image's short side is no bar's label (its window
//!   would be most of the image).
//! * Tick labels: three numbers or more on one baseline in arithmetic
//!   progression ("0", "1", "2 km"; the unit on the last one or beside it):
//!   pixels per unit from their positions (least squares).
//!
//! The result is a prior for the contour search (`georef`), never the answer:
//! the bar's own accuracy is ~0.2 % (Gaia), its measurement a few pixels,
//! and a page drawn with another aspect ratio (Gaia's PDFs: 1.6 %) or a photo
//! skews it further.

use crate::image::Rgb8;
use crate::labels::OcrObservation;
use serde::Serialize;

/// Largest text height of a bar's label, as a share of the image's short
/// side (Gaia's "1 mi": 1 % of the page; a phone screenshot's "500 ft":
/// 2.6 %; a small crop around the bar more), and at least `MIN_LABEL_CAP`
/// pixels.
const MAX_LABEL_SHARE: f64 = 0.08;
const MIN_LABEL_CAP: f64 = 48.0;

/// A scale bar found on the map.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScaleBar {
    /// The label (or labels) read.
    pub label: String,
    /// Ground metres the bar (or one unit of the ticks) stands for…
    pub metres: f64,
    /// …and its length in pixels.
    pub px: f64,
    /// Ground metres per pixel.
    pub m_per_px: f64,
    /// "line" (a measured bar line) or "ticks" (tick label positions).
    pub source: &'static str,
}

/// Metres of a distance unit as printed ("mi", "km", "m", "ft", "yd", "nm").
fn unit_metres(u: &str) -> Option<f64> {
    let u = u.trim().trim_end_matches('.').to_lowercase();
    Some(match u.as_str() {
        "km" | "kms" | "kilometre" | "kilometres" | "kilometer" | "kilometers" => 1000.0,
        "m" | "metre" | "metres" | "meter" | "meters" => 1.0,
        "mi" | "mile" | "miles" => 1609.344,
        "ft" | "feet" | "foot" => 0.3048,
        "yd" | "yds" | "yard" | "yards" => 0.9144,
        "nm" | "nmi" => 1852.0,
        _ => return None,
    })
}

/// A number as printed in a scale bar ("1", "0.5", "0,5", "½", "1/2").
fn number(t: &str) -> Option<f64> {
    let t = t.trim();
    match t {
        "½" => return Some(0.5),
        "¼" => return Some(0.25),
        "¾" => return Some(0.75),
        _ => {}
    }
    if let Some((a, b)) = t.split_once('/') {
        let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        return (b > 0.0).then_some(a / b);
    }
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') {
        return None;
    }
    t.replace(',', ".").parse().ok()
}

/// "1 mi", "2km", "0.5 mi", "500 ft", "1000 m" → metres; None for anything
/// else. Scale bars are drawn for round distances (1, 2, 2.5 or 5 times a
/// power of ten): a height ("1428 m", "2015m") is no bar's label.
pub fn distance_label(text: &str) -> Option<f64> {
    let t = text.trim();
    let split = t.find(|c: char| c.is_alphabetic())?;
    let (num, unit) = t.split_at(split);
    let v = number(num)?;
    let m = unit_metres(unit)?;
    let d = v * m;
    let mantissa = v / 10f64.powf(v.log10().floor());
    let round = [1.0, 2.0, 2.5, 5.0].iter().any(|r| (mantissa - r).abs() < 1e-6) || [0.25, 0.75].contains(&v);
    (round && d > 0.0 && d <= 1_000_000.0).then_some(d)
}

/// Luminance of a pixel (0–255).
fn lum(img: &Rgb8, x: usize, y: usize) -> f64 {
    let [r, g, b] = img.rgb(x, y);
    0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64
}

/// The bar line beside a distance label: (x0, x1, row) of the longest dark
/// horizontal run that passes under the label or ends near it, x1 moved in
/// by the end ticks' width when the bar has them (x1 - x0 is then the
/// distance between the tick centres).
fn bar_line(img: &Rgb8, o: &OcrObservation) -> Option<(f64, f64, usize)> {
    let [bx, by, bw, bh] = o.bbox;
    let th = bh.min(bw).max(4.0);
    let (w, h) = (img.w as f64, img.h as f64);
    let x_lo = (bx + bw / 2.0 - 32.0 * th).max(0.0) as usize;
    let x_hi = (bx + bw / 2.0 + 32.0 * th).min(w - 1.0).max(0.0) as usize;
    let y_lo = (by - 2.5 * th).max(0.0) as usize;
    let y_hi = (by + bh + 2.5 * th).min(h - 1.0).max(0.0) as usize;
    if x_hi <= x_lo || y_hi <= y_lo {
        return None;
    }
    // Dark: well below the local paper (the window's bright level: the
    // 90th percentile of its luminance, from a histogram).
    let mut hist = [0usize; 256];
    for y in y_lo..=y_hi {
        for x in x_lo..=x_hi {
            hist[(lum(img, x, y).round() as usize).min(255)] += 1;
        }
    }
    let n = (x_hi - x_lo + 1) * (y_hi - y_lo + 1);
    let mut seen = 0;
    let paper = hist
        .iter()
        .position(|&k| {
            seen += k;
            seen > (n * 9) / 10
        })
        .unwrap_or(255) as f64;
    let dark = paper - 90.0;
    if dark <= 0.0 {
        return None;
    }
    let gap = 2usize;
    let mut best: Option<(f64, f64, usize)> = None;
    for y in y_lo..=y_hi {
        // Dark runs of this row, bridging gaps of up to `gap` px.
        let mut x = x_lo;
        while x <= x_hi {
            if lum(img, x, y) >= dark {
                x += 1;
                continue;
            }
            let start = x;
            let mut end = x;
            let mut miss = 0;
            while x <= x_hi {
                if lum(img, x, y) < dark {
                    end = x;
                    miss = 0;
                } else {
                    miss += 1;
                    if miss > gap {
                        break;
                    }
                }
                x += 1;
            }
            let (x0, x1) = (start as f64, end as f64 + 1.0);
            let len = x1 - x0;
            // Under the label, or ending near it; a line, not the frame.
            let near = (x0 <= bx + bw && x1 >= bx) || (x1 - bx).abs() <= 1.5 * th || (x0 - (bx + bw)).abs() <= 1.5 * th;
            let framed = start == x_lo && x_lo > 0 || end == x_hi && x_hi + 1 < img.w;
            if near && !framed && len >= 3.0 * th && len <= 60.0 * th && best.is_none_or(|b| len > b.1 - b.0) {
                best = Some((x0, x1, y));
            }
        }
    }
    let (x0, x1, y) = best?;
    // Thin: the dark run's extent holds for at most half a text height of rows.
    let line: Vec<bool> = (y_lo..=y_hi)
        .map(|yy| {
            let n = (x0 as usize..x1 as usize).filter(|&xx| lum(img, xx, yy) < dark).count();
            n as f64 >= 0.8 * (x1 - x0)
        })
        .collect();
    let rows = line.iter().filter(|&&l| l).count();
    if rows as f64 > (0.5 * th).max(3.0) {
        return None;
    }
    // End ticks: the distance runs between their centres, so a run that
    // includes both ticks is one tick width too long.
    let max_tick = (0.5 * th).max(rows as f64 + 2.0).ceil() as usize;
    let tick = |edge: usize, inward: bool| -> usize {
        (y_lo..=y_hi)
            .zip(&line)
            .filter(|(_, &l)| !l)
            .map(|(yy, _)| {
                (0..=max_tick)
                    .take_while(|&k| {
                        let xx = if inward { edge + k } else { edge.wrapping_sub(k) };
                        xx < img.w && lum(img, xx, yy) < dark
                    })
                    .count()
            })
            .max()
            .unwrap_or(0)
    };
    let (left, right) = (tick(x0 as usize, true), tick(x1 as usize - 1, false));
    let ticked = |w: usize| w >= 1 && w <= max_tick;
    let shorten = if ticked(left) && ticked(right) { (left + right) as f64 / 2.0 } else { 0.0 };
    Some((x0, x1 - shorten, y))
}

/// The map's scale bar, if one is read and measured: a row of tick labels
/// ("0 1 2 km", or "0" and "2 km" at the bar's ends), else a single distance
/// label with its bar line. The labels span the whole bar; the line beside
/// a label at the end of a bar of alternating blocks is one block.
pub fn find_scale_bar(img: &Rgb8, obs: &[OcrObservation]) -> Option<ScaleBar> {
    if let Some(t) = tick_labels(obs) {
        return Some(t);
    }
    let mut found: Vec<ScaleBar> = Vec::new();
    // A bar's label is small print: a box taller than this share of the
    // image is no such label (and its window would be the whole image).
    let max_text = (MAX_LABEL_SHARE * img.w.min(img.h) as f64).max(MIN_LABEL_CAP);
    for o in obs {
        if !o.bbox.iter().all(|v| v.is_finite()) || o.bbox[2] < o.bbox[3] || o.bbox[3] > max_text {
            continue;
        }
        let Some(m) = distance_label(&o.text) else { continue };
        let Some((x0, x1, _)) = bar_line(img, o) else { continue };
        let px = x1 - x0;
        found.push(ScaleBar { label: o.text.trim().to_string(), metres: m, px, m_per_px: m / px, source: "line" });
    }
    // Several reads of one label agree; take the median.
    if found.is_empty() {
        return None;
    }
    found.sort_by(|a, b| a.m_per_px.total_cmp(&b.m_per_px));
    Some(found.swap_remove(found.len() / 2))
}

/// Tick labels on one baseline ("0", "1", "2 km"; or just "0" and "2 km"):
/// pixels per unit from their positions.
fn tick_labels(obs: &[OcrObservation]) -> Option<ScaleBar> {
    // (centre x of the number, value, unit metres if printed, centre y, height)
    let mut nums: Vec<(f64, f64, Option<f64>, f64, f64)> = Vec::new();
    let mut units: Vec<(f64, f64, f64)> = Vec::new(); // (x, y, metres) of a lone unit word
                                                      // Across the map: a box wider than tall, or a short number ("0", "10").
    let across = |o: &OcrObservation| o.bbox[2] >= o.bbox[3] || o.text.trim().chars().count() <= 2;
    for o in obs.iter().filter(|o| o.bbox.iter().all(|v| v.is_finite()) && across(o)) {
        let t = o.text.trim();
        let (cx, cy) = o.center();
        if let Some(m) = unit_metres(t) {
            units.push((o.bbox[0], cy, m));
            continue;
        }
        let split = t.find(|c: char| c.is_alphabetic()).unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let Some(v) = number(num) else { continue };
        let unit = if unit.trim().is_empty() { None } else { Some(unit_metres(unit)?) };
        // The number's own centre: its share of the box.
        let n = t.chars().count().max(1) as f64;
        let x = o.bbox[0] + o.bbox[2] * (num.trim_end().chars().count() as f64 / n) / 2.0;
        nums.push((if unit.is_some() { x } else { cx }, v, unit, cy, o.bbox[3]));
    }
    let mut best: Option<(f64, ScaleBar)> = None; // (span px, bar)
    for &(_, _, _, cy, th) in &nums {
        let mut row: Vec<(f64, f64, Option<f64>)> =
            nums.iter().filter(|q| (q.3 - cy).abs() < 0.6 * th).map(|q| (q.0, q.1, q.2)).collect();
        if row.len() < 3 {
            continue;
        }
        row.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Values in arithmetic progression from left to right.
        let steps: Vec<f64> = row.windows(2).map(|p| p[1].1 - p[0].1).collect();
        let step = steps[0];
        if step <= 0.0 || steps.iter().any(|s| (s - step).abs() > 1e-6 * step.max(1.0)) {
            continue;
        }
        // The unit: on a label, or a unit word right of the last one.
        let last = row[row.len() - 1];
        let unit = row.iter().find_map(|r| r.2).or_else(|| {
            units
                .iter()
                .filter(|u| (u.1 - cy).abs() < 0.6 * th && u.0 >= last.0 && u.0 - last.0 <= 6.0 * th)
                .map(|u| u.2)
                .next()
        })?;
        // Least squares: x = a + k·value.
        let n = row.len() as f64;
        let (mv, mx) = (row.iter().map(|r| r.1).sum::<f64>() / n, row.iter().map(|r| r.0).sum::<f64>() / n);
        let (sxx, sxy) = row.iter().fold((0.0, 0.0), |(a, b), r| (a + (r.1 - mv).powi(2), b + (r.1 - mv) * (r.0 - mx)));
        if sxx <= 0.0 {
            continue;
        }
        let k = sxy / sxx; // px per value unit
        let span = (last.0 - row[0].0).abs();
        let resid = row.iter().map(|r| (r.0 - (mx + k * (r.1 - mv))).abs()).fold(0.0, f64::max);
        if k <= 0.0 || span < 3.0 * th || span > 60.0 * th || resid > 0.03 * span {
            continue;
        }
        let label = row.iter().map(|r| format!("{}", r.1)).collect::<Vec<_>>().join(" ");
        let sb = ScaleBar { label, metres: unit, px: k, m_per_px: unit / k, source: "ticks" };
        if best.as_ref().is_none_or(|b| span > b.0) {
            best = Some((span, sb));
        }
    }
    // Just "0" and a distance with its unit, at the two ends of a bar: the
    // zero over (or under) its left end, the distance beside its right end,
    // so within one and a half text heights of each other's centre line.
    for &(x, v, unit, cy, th) in &nums {
        let Some(unit) = unit else { continue };
        for &(x0, _, _, _, _) in nums.iter().filter(|q| q.1 == 0.0 && q.2.is_none() && (q.3 - cy).abs() < 1.5 * th) {
            let span = x - x0;
            if v <= 0.0 || span < 3.0 * th || span > 60.0 * th || best.as_ref().is_some_and(|b| b.0 >= span) {
                continue;
            }
            let k = span / v;
            let label = format!("0 {v}");
            best = Some((span, ScaleBar { label, metres: unit, px: k, m_per_px: unit / k, source: "ticks" }));
        }
    }
    best.map(|b| b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ob(text: &str, x: f64, y: f64, w: f64, h: f64) -> OcrObservation {
        OcrObservation { text: text.into(), bbox: [x, y, w, h], confidence: 0.9 }
    }

    #[test]
    fn distance_labels() {
        assert_eq!(distance_label("1 mi"), Some(1609.344));
        assert_eq!(distance_label("2 km"), Some(2000.0));
        assert_eq!(distance_label("0.5 mi"), Some(804.672));
        assert_eq!(distance_label("500 ft"), Some(152.4));
        assert_eq!(distance_label("1000 m"), Some(1000.0));
        assert_eq!(distance_label("1/2 mi"), Some(804.672));
        for junk in ["Mount Durnford", "10876", "1 mile hike", "km", "Web Mercator", "1428 m", "2015m"] {
            assert_eq!(distance_label(junk), None, "{junk}");
        }
    }

    /// A white page with a "1 mi" label centred over a bar (Gaia's layout),
    /// plus the map frame and other lines around: with 2 px end ticks the
    /// distance runs between the tick centres (300.5 .. 830.5), without ticks
    /// it is the whole line (300 .. 832).
    #[test]
    fn measures_a_gaia_bar() {
        let (w, h) = (1200usize, 400usize);
        let page = |ticks: bool| {
            let mut data = vec![255u8; w * h * 3];
            let mut put = |x: usize, y: usize| {
                let i = (y * w + x) * 3;
                data[i..i + 3].copy_from_slice(&[20, 20, 20]);
            };
            for x in 300..832 {
                put(x, 250);
                put(x, 251);
            }
            if ticks {
                for y in 240..252 {
                    for x in [300, 301, 830, 831] {
                        put(x, y);
                    }
                }
            }
            // The frame: a long line across the page below.
            for x in 0..w {
                put(x, 330);
                put(x, 331);
            }
            Rgb8::from_rgb(w, h, data).unwrap()
        };
        let obs =
            vec![ob("1 mi", 541.0, 220.0, 49.0, 25.0), ob("Web Mercator | EPSG: 3857", 430.0, 262.0, 266.0, 26.0)];
        for (ticks, px) in [(true, 530.0), (false, 532.0)] {
            let sb = find_scale_bar(&page(ticks), &obs).expect("bar");
            assert_eq!(sb.source, "line");
            assert!((sb.px - px).abs() <= 0.5, "{sb:?}");
            assert!((sb.m_per_px - 1609.344 / px).abs() < 0.002, "{sb:?}");
        }
        // No bar drawn: no prior from the label alone.
        let blank = Rgb8::from_rgb(w, h, vec![255u8; w * h * 3]).unwrap();
        assert_eq!(find_scale_bar(&blank, &obs), None);
        // A read as tall as the page is no bar's label (its window would be
        // the whole image, for every such read).
        let huge = vec![ob("1 mi", 0.0, 0.0, 1200.0, 380.0)];
        assert_eq!(find_scale_bar(&page(true), &huge), None);
    }

    #[test]
    fn tick_labels_give_pixels_per_unit() {
        // "0 1 2 3 km" with ticks every 236.2 px (1 km at 1:25 000, 600 dpi).
        let obs = vec![
            ob("0", 100.0, 500.0, 12.0, 20.0),
            ob("1", 336.0, 500.0, 12.0, 20.0),
            ob("2", 572.4, 500.0, 12.0, 20.0),
            ob("3 km", 808.6, 500.0, 50.0, 20.0),
        ];
        let img = Rgb8::from_rgb(10, 10, vec![255u8; 300]).unwrap();
        let sb = find_scale_bar(&img, &obs).expect("ticks");
        assert_eq!(sb.source, "ticks");
        assert!((sb.m_per_px - 1000.0 / 236.2).abs() < 0.05, "{sb:?}");
        // Without a unit the ticks say nothing.
        let bare: Vec<OcrObservation> = obs[..3].to_vec();
        assert_eq!(find_scale_bar(&img, &bare), None);
        // "0" and "2 km" at the ends of a bar of alternating blocks (the
        // synthetic USGS sheets at 3.5 m/px; the zero over the bar, so a
        // little higher): the labels' span, not a block.
        let ends = vec![ob("0", 2515.0, 3790.0, 10.0, 12.0), ob("2 km", 3072.0, 3803.0, 40.0, 12.0)];
        let sb = find_scale_bar(&img, &ends).expect("ends");
        assert_eq!(sb.source, "ticks");
        assert!((sb.m_per_px - 2000.0 / 557.0).abs() < 0.01, "{sb:?}");
        // Not without the zero, nor with another number first.
        assert_eq!(find_scale_bar(&img, &ends[1..]), None);
        let one = vec![ob("1", 2515.0, 3796.0, 10.0, 12.0), ends[1].clone()];
        assert_eq!(find_scale_bar(&img, &one), None);
    }
}
