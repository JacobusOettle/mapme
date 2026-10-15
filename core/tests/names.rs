//! Names on the map (`labels::classify_pois`, `hints::hint_queries`) on every
//! fixture's OCR: Apple Vision's reads (`vision-ocr.json`, iOS 26.2
//! simulator) and the RapidOCR stand-ins (`ocr.json`).

mod common;

use mapme_core::hints::hint_queries;
use mapme_core::labels::{classify_pois, OcrObservation, PoiKind};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn ocr_file(path: &Path) -> Vec<OcrObservation> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    serde_json::from_value(v.get("observations").cloned().unwrap_or(v)).unwrap()
}

/// Every OCR file of the fixtures, labelled by its path under `fixtures/`.
fn every_ocr_file() -> Vec<(String, PathBuf)> {
    let root = fixtures();
    let mut out = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(d) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("ocr.json")) {
                out.push((p.strip_prefix(&root).unwrap().display().to_string(), p));
            }
        }
    }
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    for n in ["castle-gorge-ocr.json", "grootkloof-rapidocr.json", "mhlabatini-ocr.json"] {
        out.push((format!("core/tests/data/{n}"), data.join(n)));
    }
    out.sort();
    out
}

/// Apple Vision on the user's Gaia GPS exports (italic sans lettering: "Costle", "Poss",
/// "Songebethu" for Sangebethu, "Dumford" for Durnford): the app looks the first four hint
/// queries up when the map is not found near the user, so they must be prominent places,
/// spelled as printed.
#[test]
fn gaia_vision_hint_queries_start_with_prominent_places_spelled_right() {
    let want: [(&str, [&str; 4]); 2] = [
        ("gaia-giants-castle", ["Giant's Castle", "Langalibalele Pass", "Ntubeni Pass", "Bannerman's Pass"]),
        ("gaia-langalibalele", ["Mount Durnford", "Langalibalele Pass", "Main Cave", "Grysbok Bush"]),
    ];
    for (dir, first) in want {
        let obs = ocr_file(&fixtures().join(dir).join("vision-ocr.json"));
        let q = hint_queries(&obs);
        println!("{dir}: {q:?}");
        assert_eq!(&q[..4], &first, "{dir}: {q:?}");
        // Misreads Vision makes of the italic lettering, and pieces of small print: never asked.
        for bad in [
            "Poss",
            "POss",
            "Costle",
            "Castie",
            "Bushy",
            "nDệ",
            "pickı",
            "picku",
            "lain Cave",
            "ance",
            "ince",
            "LAA",
            "Oribl",
            "Ntloano",
            "Gaia",
            "Mercator",
        ] {
            assert!(!q.iter().any(|x| x.contains(bad)), "{dir}: {bad} in {q:?}");
        }
    }
}

/// The Nearby list of the Gaia pages as Apple Vision reads them: the names printed, wrapped
/// lines joined, the feature words mended, the summits with their heights (printed in feet,
/// "=10876" and "$10807" with the summit's mark, "108Q7", "10305L"), and nothing else. What
/// stays misread has no second read to tell: "Songebethu" (Sangebethu), "Mount Dumford" (map B;
/// map A reads it right), "KaMasihlengo Pass" (KaMasihlenga), "Lango-le-Balele" (Langa-le-),
/// "Legheku" (Leqheku).
#[test]
fn gaia_vision_pois_are_the_names_printed() {
    /// A name and its elevation (metres).
    type Named<'a> = (&'a str, Option<f64>);
    let want: [(&str, &[Named]); 2] = [
        (
            "gaia-giants-castle",
            &[
                ("Bannerman's Pass", None),
                ("Bannerman Hut", None),
                ("Songebethu", Some(3301.0)),
                ("Langalibalele Pass", None),
                ("Grysbok Bush", None),
                ("Oribi Ridge Rte", None),
                ("Mount Dumford", Some(3294.0)),
                ("Thaba-Putsoa", None),
                ("Giant's Castle", Some(3315.0)),
                ("Giant's Pass", None),
                ("Legheku", Some(3329.9)),
                ("Ntloana-Ts'oana", Some(3262.0)),
                ("Ntubeni Pass", None),
                ("KaMasihlengo Pass", None),
            ],
        ),
        (
            "gaia-langalibalele",
            &[
                ("Langalibalele Pass", None),
                ("Main Cave", None),
                ("Grysbok Bush", None),
                ("Erskine", Some(3148.0)),
                ("Bond", Some(3155.9)),
                ("Lango-le-Balele", None),
                ("Potterill", Some(3141.0)),
                ("Mount Durnford", Some(3294.0)),
                ("Kambule", Some(3154.1)),
                ("Katana", Some(3076.0)),
                ("Umtshezi", None),
            ],
        ),
    ];
    for (dir, names) in want {
        let pois = classify_pois(&ocr_file(&fixtures().join(dir).join("vision-ocr.json")));
        let got: Vec<(&str, Option<f64>)> = pois
            .iter()
            .filter(|p| p.kind != PoiKind::SpotHeight)
            .map(|p| (p.name.as_str(), p.elevation_m.map(|e| (e * 10.0).round() / 10.0)))
            .collect();
        assert_eq!(got, names, "{dir}");
    }
}

/// The POIs and hint queries of every fixture, printed (`--nocapture`) for review.
#[test]
fn names_of_every_fixture() {
    for (label, path) in every_ocr_file() {
        let obs = ocr_file(&path);
        let pois = classify_pois(&obs);
        let q = hint_queries(&obs);
        println!("== {label}");
        println!("   hint_queries: {q:?}");
        let names: Vec<String> = pois
            .iter()
            .filter(|p| p.kind != PoiKind::SpotHeight)
            .map(|p| match p.elevation_m {
                Some(e) => format!("{} ({:?} {e} m)", p.name, p.kind),
                None => format!("{} ({:?})", p.name, p.kind),
            })
            .collect();
        println!("   names: {names:?}");
        let heights: Vec<&str> =
            pois.iter().filter(|p| p.kind == PoiKind::SpotHeight).map(|p| p.name.as_str()).collect();
        println!("   heights: {heights:?}");
        assert!(q.len() <= 12, "{label}: {q:?}");
    }
}

/// A dense sheet's reads as Apple Vision gives them: a credit line and 2 000 names and heights.
fn dense_sheet_reads() -> Vec<OcrObservation> {
    let syl = ["ka", "ma", "tho", "ba", "lu", "ngo", "si", "the", "pu", "tso", "le", "ra", "mbu", "ze", "di", "kwa"];
    let feats = ["Pass", "Peak", "Hut", "Cave", "River", "Kloof", "Valley", "", "", ""];
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut obs = vec![OcrObservation {
        text: "Gaia Topo (feet): © Gaia GPS, OpenStreetMap".into(),
        bbox: [100.0, 20.0, 560.0, 28.0],
        confidence: 1.0,
    }];
    for k in 0..2000 {
        let text = if k % 7 == 0 {
            format!("{}", 3001 + (next() % 6000) * 2)
        } else {
            let n = 2 + next() % 3;
            let mut w: String = (0..n).map(|_| syl[(next() % syl.len() as u64) as usize]).collect();
            w[..1].make_ascii_uppercase();
            match feats[(next() % feats.len() as u64) as usize] {
                "" => w,
                f => format!("{w} {f}"),
            }
        };
        let (x, y) = (100.0 + 400.0 * (k % 50) as f64, 100.0 + 120.0 * (k / 50) as f64);
        obs.push(OcrObservation {
            text: text.clone(),
            bbox: [x, y, 15.0 * text.chars().count() as f64, 24.0],
            confidence: 1.0,
        });
    }
    obs
}

/// A dense sheet read by Apple Vision holds a couple of thousand reads: the names and the hint
/// queries of one take no longer than the rest of the import.
#[test]
fn names_of_a_dense_sheet_are_quick() {
    let obs = dense_sheet_reads();
    let t = std::time::Instant::now();
    let pois = classify_pois(&obs);
    let q = hint_queries(&obs);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(pois.len() > 1500 && q.len() == 12, "{} {q:?}", pois.len());
    assert!(ms < common::time_budget_ms(1500.0), "{ms:.0} ms");
}

/// Every response to a request with OCR text lists its hint queries, need_dem rounds too: a
/// session works them out once, not on every round (0.5 s a round for 2 000 names; a 2 000 km
/// search runs ~90 rounds).
#[test]
fn hint_queries_are_worked_out_once_a_session() {
    use mapme_core::georef::{GeorefRequest, GpsFix, Session, Status};
    let obs = dense_sheet_reads();
    let img = mapme_core::image::Rgb8::from_rgb(800, 600, vec![250u8; 800 * 600 * 3]).unwrap();
    let session = Session::new(img);
    let req = GeorefRequest {
        gps: Some(GpsFix { lat: -25.85, lon: 27.4, accuracy_m: Some(10.0) }),
        methods: vec!["contours".into()],
        ocr: obs,
        ..Default::default()
    };
    let mut first: Option<Vec<String>> = None;
    for round in 0..4 {
        // No tile is ever sent: every round is a bare need_dem.
        let t = std::time::Instant::now();
        let resp = session.georeference(&req);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(resp.status, Status::NeedDem, "{}", resp.message);
        let q = resp.hint_queries.expect("hint_queries");
        assert_eq!(q.len(), 12, "{q:?}");
        match &first {
            None => first = Some(q),
            Some(f) => {
                assert_eq!(&q, f);
                assert!(ms < common::time_budget_ms(100.0), "round {round}: {ms:.0} ms");
            }
        }
    }
}
