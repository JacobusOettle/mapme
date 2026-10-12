//! Grid path under phone-OCR damage, on the three MCSA Magaliesberg sheets
//! (fixtures/grootkloof, fixtures/castle-gorge, fixtures/mhlabatini).
//!
//! Every trial starts from the sheet's printed graticule labels (values and
//! line positions from truth.json, boxes where the labels are printed) and
//! the sheet's other OCR text, then damages the labels the way a phone OCR
//! does (seeded, reproducible):
//!
//! * only some labels survive (at least `min_keep` per axis), some read twice
//!   (overlapping passes) with independent damage;
//! * marks misread as other symbols, letters or digits, or lost; zeros read
//!   as O or Q; a lowercase or misread hemisphere letter, or none; stray spaces;
//! * a read cut off at a tile edge (part of the text, a shorter box);
//! * the westmost longitude label run into the "GIS V1 Sept 2017- Eras Gous"
//!   credit; the box of a label off its line by a few pixels;
//! * one label read with a wrong digit, or a stray coordinate somewhere else.
//!
//! The app's request is replayed (dpi 300, GPS 60 km away, grid only) and the
//! answer scored at the true graticule intersections. A trial is "confident
//! and wrong" when the answer is off by more than 50 m with confidence ≥ 0.5;
//! there must be none. With at least two correct labels on each axis the
//! sheet must be located (≥ 95 %) within 5 m.
//!
//! `cargo test --release --test stress -- --nocapture` prints the table.

mod common;
use common::*;
use mapme_core::geo::Homography;
use mapme_core::georef::{GeorefRequest, GpsFix, Method, Session, Status};
use mapme_core::labels::OcrObservation;
use std::fmt::Write as _;

struct Sheet {
    dir: &'static str,
    /// 60 km east of the map: contours could not help.
    far: (f64, f64),
}

const SHEETS: [Sheet; 3] = [
    Sheet { dir: "grootkloof", far: (-25.8499, 27.9988) },
    Sheet { dir: "castle-gorge", far: (-25.8153, 28.1913) },
    Sheet { dir: "mhlabatini", far: (-25.8288, 27.9299) },
];

/// The imprint along the bottom edge, x 436…773 on every sheet of the series.
const CREDIT: &str = "GIS V1 Sept 2017- Eras Gous";
const CREDIT_BBOX: [f64; 4] = [436.0, 4806.0, 337.0, 28.0];

/// A printed graticule label: its value, axis and box.
#[derive(Clone)]
struct Printed {
    lat: bool,
    value: f64,
    bbox: [f64; 4],
}

fn printed_labels(dir: &std::path::Path) -> Vec<Printed> {
    let t: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("truth.json")).unwrap()).unwrap();
    let mut out = Vec::new();
    for l in t["lat_lines"].as_array().unwrap() {
        let y = l["y"].as_f64().unwrap();
        out.push(Printed { lat: true, value: l["lat"].as_f64().unwrap(), bbox: [119.0, y - 84.0, 36.0, 168.0] });
    }
    for l in t["lon_lines"].as_array().unwrap() {
        let x = l["x"].as_f64().unwrap();
        out.push(Printed { lat: false, value: l["lon"].as_f64().unwrap(), bbox: [x - 80.0, 4803.0, 160.0, 31.0] });
    }
    out
}

/// The sheet's OCR text other than its graticule labels and the credit
/// (left label strip and bottom label row), as the RapidOCR stand-in for
/// Apple Vision reads it (fixtures/*/ocr.json; Grootkloof's fixture OCR is
/// hand-built and clean, so its RapidOCR read is in tests/data).
fn clutter(dir: &std::path::Path) -> Vec<OcrObservation> {
    let rapid = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/grootkloof-rapidocr.json");
    let ocr = if dir.ends_with("grootkloof") {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(rapid).unwrap()).unwrap();
        serde_json::from_value(v["observations"].clone()).unwrap()
    } else {
        load_ocr(dir)
    };
    ocr.into_iter()
        .filter(|o| {
            let (cx, cy) = o.center();
            cx >= 200.0 && cy <= 4790.0
        })
        .collect()
}

fn pick<'a, T>(rng: &mut Rng, v: &'a [T]) -> &'a T {
    &v[((rng.next_f64() * v.len() as f64) as usize).min(v.len() - 1)]
}

fn chance(rng: &mut Rng, p: f64) -> bool {
    rng.next_f64() < p
}

/// "25°49'00\"S" with its marks, zeros and hemisphere damaged.
fn damaged_text(rng: &mut Rng, value: f64, lat: bool) -> String {
    let a = value.abs();
    let (d, m) = (a.trunc() as u32, ((a - a.trunc()) * 60.0).round() as u32);
    let hemi = if lat { 'S' } else { 'E' };
    let mark = |rng: &mut Rng, printed: &str, symbols: &[&str], others: &[&str]| -> String {
        let r = rng.next_f64();
        if r < 0.5 {
            printed.to_string()
        } else if r < 0.72 {
            pick(rng, symbols).to_string()
        } else if r < 0.78 {
            pick(rng, others).to_string()
        } else {
            String::new()
        }
    };
    let deg = mark(rng, "°", &["*", "º", "˚", "-", "/", "%", "\"", "'"], &["o", "9", "0"]);
    let min = mark(rng, "'", &["/", "’", "`", "´", ",", "~", "|", "\\"], &["l", "I", "1"]);
    let sec = mark(rng, "\"", &["~", "*", "”", "''", "/", "\\", "\"\"", "'"], &["c", "11", "4"]);
    let h = {
        let r = rng.next_f64();
        if r < 0.6 {
            hemi.to_string()
        } else if r < 0.88 {
            hemi.to_ascii_lowercase().to_string()
        } else if r < 0.94 && lat {
            pick(rng, &["$", "5"]).to_string()
        } else {
            String::new()
        }
    };
    let space = |rng: &mut Rng| if chance(rng, 0.08) { " " } else { "" };
    let zeros = |rng: &mut Rng, s: String| -> String {
        s.chars().map(|c| if c == '0' && chance(rng, 0.04) { *pick(rng, &['O', 'Q', 'o']) } else { c }).collect()
    };
    let digits = zeros(rng, format!("{d:02}"));
    let mins = zeros(rng, format!("{m:02}"));
    let secs = zeros(rng, "00".to_string());
    let (s1, s2, s3) = (space(rng), space(rng), space(rng));
    format!("{digits}{deg}{s1}{mins}{min}{s2}{secs}{sec}{s3}{h}")
}

/// One label read with a wrong digit: another minute value (or degree).
fn wrong_text(rng: &mut Rng, value: f64, lat: bool) -> String {
    let a = value.abs();
    let (d, m) = (a.trunc() as i32, ((a - a.trunc()) * 60.0).round() as i32);
    let (d, m) = if chance(rng, 0.8) {
        // A minute digit misread (3↔8, 5↔6, 0↔8, 1↔7, …): never the true value.
        let mut w = m;
        while w == m {
            w = (m / 10 * 10 + (rng.next_f64() * 10.0) as i32).rem_euclid(60);
            if chance(rng, 0.5) {
                w = ((rng.next_f64() * 6.0) as i32 * 10 + m % 10).rem_euclid(60);
            }
        }
        (d, w)
    } else {
        (d + if chance(rng, 0.5) { 1 } else { -1 }, m)
    };
    format!("{d}°{m:02}'00\"{}", if lat { 'S' } else { 'E' })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// ≥ 2 correct labels on each axis, nothing wrong.
    Clean,
    /// ≥ 2 correct labels on each axis plus one wrong label or stray coordinate.
    OneWrong,
    /// An axis has fewer than 2 correct labels read whole (+ maybe a wrong one): not enough to outvote.
    Short,
}

struct Trial {
    kind: Kind,
    ocr: Vec<OcrObservation>,
    /// How the labels were damaged (printed on failures).
    log: String,
}

fn trial(rng: &mut Rng, labels: &[Printed], clutter: &[OcrObservation], short: bool) -> Trial {
    let mut ocr: Vec<OcrObservation> = clutter.to_vec();
    let mut log = String::new();
    let mut correct = [0usize; 2];
    let mut wrong = false;
    let short_axis = chance(rng, 0.5);
    for lat in [true, false] {
        let mut axis: Vec<&Printed> = labels.iter().filter(|l| l.lat == lat).collect();
        // Shuffle, then keep a random number of them (at least 2, or 1 on the short axis).
        for i in (1..axis.len()).rev() {
            let j = (rng.next_f64() * (i + 1) as f64) as usize;
            axis.swap(i, j.min(i));
        }
        let min_keep = if short && lat == short_axis { 1 } else { 2 };
        let max_keep = if short && lat == short_axis { 1 } else { axis.len() };
        let keep = min_keep + (rng.next_f64() * (max_keep - min_keep + 1) as f64) as usize;
        for l in axis.iter().take(keep.min(max_keep)) {
            let reads = if chance(rng, 0.35) { 2 } else { 1 };
            // A label survives when one of its reads is whole (not cut off at a tile edge).
            let mut whole = false;
            for _ in 0..reads {
                let mut text = damaged_text(rng, l.value, l.lat);
                let mut bbox = l.bbox;
                // The box a little off (OCR boxes are loose).
                bbox[0] += 3.0 * rng.normal();
                bbox[1] += 3.0 * rng.normal();
                bbox[2] *= 1.0 + 0.05 * rng.normal();
                bbox[3] *= 1.0 + 0.05 * rng.normal();
                // Cut off at a tile edge: the first or last part of the text.
                let cut = chance(rng, 0.08);
                whole |= !cut;
                if cut {
                    let chars: Vec<char> = text.chars().collect();
                    let k = 4 + (rng.next_f64() * (chars.len() - 4) as f64) as usize;
                    let frac = k as f64 / chars.len() as f64;
                    let along = if l.lat { 3 } else { 2 };
                    let from_start = chance(rng, 0.5);
                    text = if from_start {
                        chars[..k].iter().collect()
                    } else {
                        chars[chars.len() - k..].iter().collect()
                    };
                    // Latitude labels read bottom to top: their text starts at the bottom of the box.
                    if from_start == l.lat {
                        bbox[along - 2] += bbox[along] * (1.0 - frac);
                    }
                    bbox[along] *= frac;
                }
                // The westmost longitude label run into the credit.
                if !l.lat && l.bbox[0] < 1100.0 && chance(rng, 0.4) {
                    let [cx, cy, cw, ch] = CREDIT_BBOX;
                    let (x0, y0) = (bbox[0].min(cx), bbox[1].min(cy));
                    let (x1, y1) = ((bbox[0] + bbox[2]).max(cx + cw), (bbox[1] + bbox[3]).max(cy + ch));
                    let sep = if chance(rng, 0.5) { " " } else { "" };
                    let credit = if chance(rng, 0.5) { CREDIT.to_string() } else { CREDIT.replace(' ', "") };
                    text = if bbox[0] < cx { format!("{text}{sep}{credit}") } else { format!("{credit}{sep}{text}") };
                    bbox = [x0, y0, x1 - x0, y1 - y0];
                }
                let _ = write!(log, "{}{text:?} ", if cut { "CUT " } else { "" });
                ocr.push(OcrObservation { text, bbox, confidence: 0.5 + 0.5 * rng.next_f64() as f32 });
            }
            correct[usize::from(!lat)] += usize::from(whole);
        }
        // One label of this axis read wrong (instead of being dropped), sometimes.
        if keep < axis.len() && !wrong && chance(rng, if short { 0.5 } else { 0.25 }) {
            let l = axis[keep];
            let text = wrong_text(rng, l.value, l.lat);
            let _ = write!(log, "WRONG {text:?} ");
            ocr.push(OcrObservation { text, bbox: l.bbox, confidence: 0.9 });
            wrong = true;
        }
    }
    // The credit itself, when no label was run into it.
    if !ocr.iter().any(|o| o.text.contains("Gous")) {
        ocr.push(OcrObservation { text: CREDIT.into(), bbox: CREDIT_BBOX, confidence: 0.7 });
    }
    // A stray coordinate inside the map (a misread spot height, a legend line).
    if !short && !wrong && chance(rng, 0.15) {
        let lat = chance(rng, 0.5);
        let v = labels.iter().find(|l| l.lat == lat).unwrap().value;
        let text = wrong_text(rng, v, lat);
        let bbox = [400.0 + 2600.0 * rng.next_f64(), 400.0 + 4000.0 * rng.next_f64(), 160.0, 31.0];
        let _ = write!(log, "STRAY {text:?} ");
        ocr.push(OcrObservation { text, bbox, confidence: 0.8 });
        wrong = true;
    }
    let kind = match (correct.iter().all(|&c| c >= 2), wrong) {
        (false, _) => Kind::Short,
        (true, false) => Kind::Clean,
        (true, true) => Kind::OneWrong,
    };
    Trial { kind, ocr, log }
}

#[derive(Default)]
struct Tally {
    n: usize,
    located: usize,
    max_err: f64,
    confident_wrong: usize,
    low_conf: usize,
}

#[test]
fn grid_survives_phone_ocr_damage() {
    let mut table = String::new();
    let mut failures = Vec::new();
    let mut confident_wrong = Vec::new();
    let mut totals: Vec<(String, Kind, Tally)> = Vec::new();
    // STRESS_TRIALS / STRESS_SEED run more trials or other ones.
    let env = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (trials, seed) = (env("STRESS_TRIALS", 300), env("STRESS_SEED", 0x5EED));
    for (si, sheet) in SHEETS.iter().enumerate() {
        let dir = sheet_dir(sheet.dir);
        let session = Session::new(load_jpeg_file(&dir.join("map.jpg")));
        let truth = graticule_truth(&dir);
        let labels = printed_labels(&dir);
        let clutter = clutter(&dir);
        let mut rng = Rng::new(seed + si as u64);
        let mut tallies = [Tally::default(), Tally::default(), Tally::default()];
        let t0 = std::time::Instant::now();
        for k in 0..trials {
            let tr = trial(&mut rng, &labels, &clutter, k % 5 == 4);
            let req = GeorefRequest {
                gps: Some(GpsFix { lat: sheet.far.0, lon: sheet.far.1, accuracy_m: Some(10.0) }),
                ocr: tr.ocr,
                dpi: Some(300.0),
                methods: vec!["grid".into()],
                ..GeorefRequest::default()
            };
            if let Ok(dir) = std::env::var("STRESS_DUMP") {
                // Developer aid: the request of every trial, to replay one.
                let path = std::path::Path::new(&dir).join(format!("{}-{k}.json", sheet.dir));
                std::fs::write(path, serde_json::to_vec(&serde_json::json!({ "observations": req.ocr })).unwrap())
                    .unwrap();
            }
            let r = session.georeference(&req);
            let err = r.pixel_to_mercator.map_or(f64::INFINITY, |m| max_graticule_error_m(&Homography { m }, &truth));
            let conf = r.confidence.unwrap_or(0.0);
            let t = &mut tallies[tr.kind as usize];
            t.n += 1;
            let ok = r.status == Status::Ok && matches!(r.method, Some(Method::Grid)) && err <= 5.0;
            if ok {
                t.located += 1;
                t.max_err = t.max_err.max(err);
            }
            if r.status == Status::Ok && conf < 0.5 {
                t.low_conf += 1;
            }
            if r.status == Status::Ok && err > 50.0 && conf >= 0.5 {
                t.confident_wrong += 1;
                confident_wrong.push(format!(
                    "[{} #{k} {:?}] {err:.0} m off, conf {conf:.2}: {} | {}",
                    sheet.dir, tr.kind, r.message, tr.log
                ));
            }
            if !ok && tr.kind != Kind::Short && failures.len() < 40 {
                failures.push(format!(
                    "[{} #{k} {:?}] {:?} {:?} err {err:.1} m: {} {} | {}",
                    sheet.dir,
                    tr.kind,
                    r.status,
                    r.method,
                    r.message,
                    r.diagnostics.get("grid").map(|g| g.to_string()).unwrap_or_default(),
                    tr.log
                ));
            }
        }
        println!("[{}] {trials} trials in {:.1} s", sheet.dir, t0.elapsed().as_secs_f64());
        for (kind, t) in [Kind::Clean, Kind::OneWrong, Kind::Short].into_iter().zip(tallies) {
            totals.push((sheet.dir.to_string(), kind, t));
        }
    }
    let _ = writeln!(
        table,
        "{:<13} {:<9} {:>6} {:>8} {:>9} {:>11} {:>9} {:>16}",
        "sheet", "case", "trials", "located", "rate", "max err m", "conf<0.5", "confident wrong"
    );
    for (dir, kind, t) in &totals {
        let _ = writeln!(
            table,
            "{dir:<13} {:<9} {:>6} {:>8} {:>8.1}% {:>11.1} {:>9} {:>16}",
            format!("{kind:?}"),
            t.n,
            t.located,
            100.0 * t.located as f64 / t.n.max(1) as f64,
            t.max_err,
            t.low_conf,
            t.confident_wrong
        );
    }
    println!("{table}");
    for f in &failures {
        println!("not located: {f}");
    }
    for f in &confident_wrong {
        println!("CONFIDENT AND WRONG: {f}");
    }
    assert!(confident_wrong.is_empty(), "{} confident wrong answers", confident_wrong.len());
    for (dir, kind, t) in &totals {
        if *kind == Kind::Short {
            continue;
        }
        let rate = t.located as f64 / t.n.max(1) as f64;
        assert!(rate >= 0.95, "{dir} {kind:?}: located {:.1} %", 100.0 * rate);
        assert!(t.max_err <= 5.0, "{dir} {kind:?}: max error {:.1} m", t.max_err);
    }
}
