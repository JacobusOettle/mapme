//! A small map on a large page (`content.rs`): the user's Gaia GPS export as
//! its native 1 118 × 791 px raster, placed unscaled on an A4 sheet rendered
//! at 300 dpi (2 480 × 3 508 px) under a title, with lines of text under it,
//! a frame round the sheet and the rest of it blank (a PDF that holds a map
//! smaller than its page). The contour path must size its search by the map,
//! not the page: located within the benchmark's tolerance from the map
//! centre with the fixture's tiles, the map's rectangle in the diagnostics.
//! A blank page and a page of text still fail at once. Paragraphs of text
//! beside the map (a guidebook's route description) are no part of it.
//! `cargo test --release --test map_on_page -- --nocapture` prints the errors.

mod common;
use common::time_budget_ms;
use mapme_core::dem::TileId;
use mapme_core::geo::{haversine_m, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::{GeorefRequest, GeorefResponse, GpsFix, Session, Status};
use mapme_core::image::Rgb8;
use std::path::{Path, PathBuf};
use std::time::Instant;

const PAGE: (usize, usize) = (2480, 3508);
const AT: (usize, usize) = (681, 400);

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/gaia-langalibalele")
}

fn load_png(path: &Path) -> Rgb8 {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let ch = info.color_type.samples();
    let rgb: Vec<u8> = buf[..info.buffer_size()]
        .chunks_exact(ch)
        .flat_map(|p| if ch >= 3 { [p[0], p[1], p[2]] } else { [p[0]; 3] })
        .collect();
    Rgb8::from_rgb(info.width as usize, info.height as usize, rgb).unwrap()
}

/// A white A4 page at 300 dpi with a frame round it and `map` at `AT`, a
/// title over it and lines of "text" (dashes) under it.
fn page_with(map: Option<&Rgb8>, text_lines: usize) -> Rgb8 {
    let (w, h) = PAGE;
    let mut data = vec![250u8; w * h * 3];
    let mut set = |x: usize, y: usize, c: [u8; 3]| data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
    for x in 24..w - 24 {
        for y in [24, 25, h - 26, h - 25] {
            set(x, y, [20, 20, 20]);
        }
    }
    for y in 24..h - 24 {
        for x in [24, 25, w - 26, w - 25] {
            set(x, y, [20, 20, 20]);
        }
    }
    if let Some(m) = map {
        for y in 0..m.h {
            for x in 0..m.w {
                set(AT.0 + x, AT.1 + y, m.rgb(x, y));
            }
        }
        // Title: letter-sized blocks over the map.
        for y in AT.1 - 90..AT.1 - 40 {
            for x in (AT.0..AT.0 + 700).filter(|x| x % 40 < 28) {
                set(x, y, [25, 25, 25]);
            }
        }
    }
    let top = map.map_or(300, |m| AT.1 + m.h + 60);
    for k in 0..text_lines {
        let y0 = top + 55 * k;
        for y in y0..(y0 + 26).min(h - 40) {
            for x in (AT.0..(AT.0 + 1100).min(w - 40)).filter(|x| x % 23 < 15) {
                set(x, y, [30, 30, 30]);
            }
        }
    }
    Rgb8::from_rgb(w, h, data).unwrap()
}

fn case_json() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(fixture().join("case_native.json")).unwrap()).unwrap()
}

/// Contours only, the GPS at the map centre, through the need_dem loop with
/// the fixture's tiles (the rest missing, as offline).
fn run(img: Rgb8) -> (GeorefResponse, f64) {
    let c = case_json();
    let g = &c["gps_variants"]["centre"];
    let req = GeorefRequest {
        gps: Some(GpsFix { lat: g[0].as_f64().unwrap(), lon: g[1].as_f64().unwrap(), accuracy_m: Some(10.0) }),
        methods: vec!["contours".into()],
        dpi: Some(300.0),
        ..Default::default()
    };
    let session = Session::new(img);
    let dem = fixture().join("dem");
    let mut core_ms = 0.0;
    for _ in 0..300 {
        let t = Instant::now();
        let resp = session.georeference(&req);
        core_ms += t.elapsed().as_secs_f64() * 1e3;
        if resp.status != Status::NeedDem {
            return (resp, core_ms);
        }
        for &t in &resp.need_tiles {
            let path = dem.join(format!("{}_{}_{}.png", t.z, t.x, t.y));
            match std::fs::read(path) {
                Ok(png) => session.add_dem_tile(t, &png).unwrap(),
                Err(_) => session.mark_dem_tile_missing(TileId { z: t.z, x: t.x, y: t.y }).unwrap(),
            }
        }
    }
    panic!("still need DEM tiles after 300 rounds");
}

#[test]
fn gaia_native_raster_on_an_a4_page() {
    let map = load_png(&fixture().join("native.png"));
    assert_eq!((map.w, map.h), (1118, 791));
    let (resp, ms) = run(page_with(Some(&map), 1));
    let c = case_json();
    let h = Homography { m: resp.pixel_to_mercator.expect(&resp.message) };
    let mut e: Vec<f64> = c["truth_points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let (x, y) = (p["x"].as_f64().unwrap() + AT.0 as f64, p["y"].as_f64().unwrap() + AT.1 as f64);
            let (mx, my) = h.apply(x, y);
            let (la, lo) = merc_to_latlon(mx, my);
            haversine_m(LatLon::new(la, lo), LatLon::new(p["lat"].as_f64().unwrap(), p["lon"].as_f64().unwrap()))
        })
        .collect();
    e.sort_by(f64::total_cmp);
    let area = &resp.diagnostics["contours"]["map_area"];
    let s = format!(
        "{:?} conf {:?}, errors median {:.1} m max {:.1} m, map area {area}, {ms:.0} ms: {}",
        resp.status,
        resp.confidence,
        e[e.len() / 2],
        e[e.len() - 1],
        resp.message
    );
    println!("{s}\n{}", resp.diagnostics["refined"]);
    assert_eq!(resp.status, Status::Ok, "{s}");
    assert!(resp.confidence.unwrap() >= 0.7, "{s}");
    let mpp = c["m_per_px"].as_f64().unwrap();
    assert!(e[e.len() / 2] <= 30f64.max(2.0 * mpp) && e[e.len() - 1] <= 75f64.max(4.0 * mpp), "{s}");
    // The map's rectangle: the raster (and its title), not the page.
    let r: Vec<f64> = area.as_array().expect(&s).iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(r[0] <= AT.0 as f64 && r[1] <= AT.1 as f64, "{s}");
    assert!(r[2] >= (AT.0 + map.w) as f64 && r[3] >= (AT.1 + map.h) as f64, "{s}");
    assert!((r[2] - r[0]) * (r[3] - r[1]) < 0.25 * (PAGE.0 * PAGE.1) as f64, "{s}");
}

#[test]
fn blank_and_text_pages_fail_at_once() {
    for (what, img) in [("blank", page_with(None, 0)), ("text", page_with(None, 50))] {
        let (resp, ms) = run(img);
        println!("{what} page: {:?} in {ms:.0} ms: {}", resp.status, resp.message);
        assert_eq!(resp.status, Status::Failed, "{what}: {}", resp.message);
        assert!(resp.message.contains("No contour lines"), "{what}: {}", resp.message);
        assert!(ms < time_budget_ms(2000.0), "{what}: {ms:.0} ms");
    }
}

// ---------------------------------------------------------------------------
// A guidebook page: the map with paragraphs of text above, below or beside it
// ---------------------------------------------------------------------------

/// Ink of one glyph-like shape `size` px tall (its x-height 0.6 of it) at
/// (x, baseline y): letters of a sans serif text at that size, 4 px strokes
/// at 42 px (10 pt at 300 dpi), some with an ascender or a descender.
fn glyph(data: &mut [u8], w: usize, x: usize, y: usize, size: usize, kind: u64) -> usize {
    let (gw, xh, st) = (size * 11 / 20, size * 3 / 5, (size / 10).max(2));
    let mut set = |x: usize, y: usize| {
        let i = (y * w + x) * 3;
        if i + 3 <= data.len() {
            data[i..i + 3].copy_from_slice(&[25, 25, 25]);
        }
    };
    let mut rect = |x0: usize, y0: usize, x1: usize, y1: usize| {
        for yy in y0..y1 {
            for xx in x0..x1 {
                set(xx, yy);
            }
        }
    };
    let top = y - xh;
    match kind % 6 {
        // o: a bowl.
        0 => {
            rect(x, top, x + gw, top + st);
            rect(x, y - st, x + gw, y);
            rect(x, top, x + st, y);
            rect(x + gw - st, top, x + gw, y);
        }
        // l, d: a stem with an ascender (and a bowl).
        1 | 2 => {
            rect(x + gw - st, y - size * 4 / 5, x + gw, y);
            if kind % 6 == 2 {
                rect(x, top, x + gw, top + st);
                rect(x, y - st, x + gw, y);
                rect(x, top, x + st, y);
            }
        }
        // n: a stem and an arch.
        3 => {
            rect(x, top, x + st, y);
            rect(x, top, x + gw, top + st);
            rect(x + gw - st, top, x + gw, y);
        }
        // e: a bowl open at the bottom right, its bar.
        4 => {
            rect(x, top, x + gw, top + st);
            rect(x, y - st, x + gw, y);
            rect(x, top, x + st, y);
            rect(x + gw - st, top, x + gw, top + xh / 2);
            rect(x, top + xh / 2 - st / 2, x + gw, top + xh / 2 + st / 2);
        }
        // p: a stem with a descender and a bowl.
        _ => {
            rect(x, top, x + st, y + size / 4);
            rect(x, top, x + gw, top + st);
            rect(x, y - st, x + gw, y);
            rect(x + gw - st, top, x + gw, y);
        }
    }
    gw
}

/// `lines` lines of words across `width` px from (x, y), a line every 1.4
/// `size`: a paragraph of text at `size` px.
fn paragraph(img: &mut Rgb8, x: usize, y: usize, width: usize, lines: usize, size: usize, seed: u64) {
    let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        r
    };
    let w = img.w;
    for k in 0..lines {
        let base = y + size + k * size * 7 / 5;
        if base + size / 3 >= img.h {
            break;
        }
        let mut cx = x;
        while cx + size * 4 < x + width {
            for _ in 0..2 + next() % 7 {
                cx += glyph(&mut img.data, w, cx, base, size, next()) + size / 7;
            }
            cx += size / 3;
        }
    }
}

/// The Gaia raster `map` (fixture `fixture`) on a white page of `page` px at
/// `at`, with paragraphs (x, y, width, lines) of 42 px text, located with the
/// GPS at the map centre: the response, the median and largest error at the
/// fixture's truth points (on the map), core ms.
fn guidebook(
    fixture: &str,
    page: (usize, usize),
    at: (usize, usize),
    paragraphs: &[(usize, usize, usize, usize)],
) -> (GeorefResponse, Option<(f64, f64)>, f64) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures").join(fixture);
    let map = load_png(&dir.join("native.png"));
    let mut img = Rgb8::from_rgb(page.0, page.1, vec![250u8; page.0 * page.1 * 3]).unwrap();
    for y in 0..map.h {
        let (s, d) = (y * map.w * 3, ((at.1 + y) * page.0 + at.0) * 3);
        img.data[d..d + map.w * 3].copy_from_slice(&map.data[s..s + map.w * 3]);
    }
    for (k, &(x, y, w, n)) in paragraphs.iter().enumerate() {
        paragraph(&mut img, x, y, w, n, 42, k as u64 + 1);
    }
    let c: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("case_native.json")).unwrap()).unwrap();
    let g = &c["gps_variants"]["centre"];
    let req = GeorefRequest {
        gps: Some(GpsFix { lat: g[0].as_f64().unwrap(), lon: g[1].as_f64().unwrap(), accuracy_m: Some(10.0) }),
        methods: vec!["contours".into()],
        dpi: Some(300.0),
        ..Default::default()
    };
    let session = Session::new(img);
    let dem = dir.join("dem");
    let t0 = Instant::now();
    let mut resp = session.georeference(&req);
    for _ in 0..300 {
        if resp.status != Status::NeedDem {
            break;
        }
        for &t in &resp.need_tiles {
            match std::fs::read(dem.join(format!("{}_{}_{}.png", t.z, t.x, t.y))) {
                Ok(png) => session.add_dem_tile(t, &png).unwrap(),
                Err(_) => session.mark_dem_tile_missing(TileId { z: t.z, x: t.x, y: t.y }).unwrap(),
            }
        }
        resp = session.georeference(&req);
    }
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    let errs = resp.pixel_to_mercator.map(|m| {
        let h = Homography { m };
        let mut e: Vec<f64> = c["truth_points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let (x, y) = (p["x"].as_f64().unwrap() + at.0 as f64, p["y"].as_f64().unwrap() + at.1 as f64);
                let (la, lo) = merc_to_latlon(h.apply(x, y).0, h.apply(x, y).1);
                haversine_m(LatLon::new(la, lo), LatLon::new(p["lat"].as_f64().unwrap(), p["lon"].as_f64().unwrap()))
            })
            .collect();
        e.sort_by(f64::total_cmp);
        (e[e.len() / 2], e[e.len() - 1])
    });
    (resp, errs, ms)
}

/// Located within the benchmark's tolerance (the raster's `mpp`), the map's
/// rectangle the raster's, not the text's, and the ± within `max_rms`.
fn assert_guidebook(
    what: &str,
    out: &(GeorefResponse, Option<(f64, f64)>, f64),
    mpp: f64,
    min_conf: f64,
    max_rms: f64,
) {
    let (resp, errs, ms) = out;
    let s = format!(
        "{what}: {:?} conf {:?} ±{:?} m, errors (median, max) {errs:?}, map area {}, {ms:.0} ms: {}",
        resp.status, resp.confidence, resp.rms_m, resp.diagnostics["contours"]["map_area"], resp.message
    );
    println!("{s}");
    assert_eq!(resp.status, Status::Ok, "{s}");
    let (med, max) = errs.unwrap();
    assert!(med <= 30f64.max(2.0 * mpp) && max <= 75f64.max(4.0 * mpp), "{s}");
    assert!(max <= resp.rms_m.unwrap(), "{s}");
    assert!(resp.confidence.unwrap() >= min_conf && resp.rms_m.unwrap() <= max_rms, "{s}");
}

/// Map A (9.1 m/px) at the top of a page, 32 lines of text under it: the
/// text's busy cells survived the opening that removes lines of text and
/// were taken for map (the map 15 % of the rectangle: ±177 m at 0.70).
#[test]
fn gaia_raster_above_a_paragraph() {
    let out = guidebook("gaia-langalibalele", PAGE, (300, 300), &[(300, 1180, 1880, 32)]);
    assert_guidebook("map A, text below", &out, 9.1, 0.9, 60.0);
    let r: Vec<f64> =
        out.0.diagnostics["contours"]["map_area"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(r[3] < 1250.0, "the text is no map: {r:?}");
}

/// Map A with paragraphs above and below it, and a column of text beside it
/// (wrapped round the map, 50 px from its edge: closing joins them).
#[test]
fn gaia_raster_between_paragraphs_and_beside_a_column() {
    let out = guidebook("gaia-langalibalele", PAGE, (150, 1300), &[(150, 150, 2100, 15), (150, 2180, 2100, 18)]);
    assert_guidebook("map A, text above and below", &out, 9.1, 0.9, 60.0);
    let out = guidebook("gaia-langalibalele", PAGE, (120, 300), &[(1290, 300, 1060, 13), (120, 1150, 2200, 30)]);
    assert_guidebook("map A, text wrapped round it", &out, 9.1, 0.9, 60.0);
}

/// Map B (23 m/px) on a landscape page with 25 lines of text under it: not
/// found at all (the text joined the map: fit confidence 0.27).
#[test]
fn gaia_b_raster_above_a_paragraph() {
    let out = guidebook("gaia-giants-castle", (PAGE.1, PAGE.0), (200, 250), &[(200, 1120, 3000, 25)]);
    assert_guidebook("map B, text below", &out, 23.0, 0.7, 120.0);
}

/// Two maps on one page, one over the other 120 px apart (closing joins
/// them into one rectangle): A (9.1 m/px) above B (23 m/px), the GPS at B's
/// centre, which A covers too. The regional stages (more than 40 km of
/// samples) were fitted to both maps at once: a projective fit kept, the
/// located map 55/348 m off with ±225 m. The fit keeps the samples of the
/// map it places and is judged over it. (Its DEM is planned for the whole
/// rectangle: zoom 11 here, where the fixture has no zoom-12 tiles, so map
/// A alone on a page fits better, 4/8 m against 30/35 m.)
#[test]
fn two_maps_on_a_page() {
    let fx = |f: &str| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures").join(f);
    let (a, b) = (
        load_png(&fx("gaia-langalibalele").join("native.png")),
        load_png(&fx("gaia-giants-castle").join("native.png")),
    );
    let (at_a, at_b) = ((150usize, 300usize), (150usize, 1211usize));
    let mut img = Rgb8::from_rgb(PAGE.0, PAGE.1, vec![250u8; PAGE.0 * PAGE.1 * 3]).unwrap();
    for (m, at) in [(&a, at_a), (&b, at_b)] {
        for y in 0..m.h {
            let (s, d) = (y * m.w * 3, ((at.1 + y) * PAGE.0 + at.0) * 3);
            img.data[d..d + m.w * 3].copy_from_slice(&m.data[s..s + m.w * 3]);
        }
    }
    let case = |f: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(fx(f).join("case_native.json")).unwrap()).unwrap()
    };
    let (ca, cb) = (case("gaia-langalibalele"), case("gaia-giants-castle"));
    let g = &cb["gps_variants"]["centre"];
    let req = GeorefRequest {
        gps: Some(GpsFix { lat: g[0].as_f64().unwrap(), lon: g[1].as_f64().unwrap(), accuracy_m: Some(10.0) }),
        methods: vec!["contours".into()],
        dpi: Some(300.0),
        ..Default::default()
    };
    let session = Session::new(img);
    let dems = [fx("gaia-langalibalele").join("dem"), fx("gaia-giants-castle").join("dem")];
    let mut resp = session.georeference(&req);
    for _ in 0..300 {
        if resp.status != Status::NeedDem {
            break;
        }
        for &t in &resp.need_tiles {
            match dems.iter().find_map(|d| std::fs::read(d.join(format!("{}_{}_{}.png", t.z, t.x, t.y))).ok()) {
                Some(png) => session.add_dem_tile(t, &png).unwrap(),
                None => session.mark_dem_tile_missing(TileId { z: t.z, x: t.x, y: t.y }).unwrap(),
            }
        }
        resp = session.georeference(&req);
    }
    assert_eq!(resp.status, Status::Ok, "{}", resp.message);
    let h = Homography { m: resp.pixel_to_mercator.unwrap() };
    // Errors at a map's truth points: (median, max).
    let errs = |c: &serde_json::Value, at: (usize, usize)| -> (f64, f64) {
        let mut e: Vec<f64> = c["truth_points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let (x, y) = (p["x"].as_f64().unwrap() + at.0 as f64, p["y"].as_f64().unwrap() + at.1 as f64);
                let (la, lo) = merc_to_latlon(h.apply(x, y).0, h.apply(x, y).1);
                haversine_m(LatLon::new(la, lo), LatLon::new(p["lat"].as_f64().unwrap(), p["lon"].as_f64().unwrap()))
            })
            .collect();
        e.sort_by(f64::total_cmp);
        (e[e.len() / 2], e[e.len() - 1])
    };
    let (ea, eb) = (errs(&ca, at_a), errs(&cb, at_b));
    let kept: Vec<String> = resp.diagnostics["refined"]["stages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kept"] == true)
        .map(|s| s["model"].as_str().unwrap().to_string())
        .collect();
    let s = format!(
        "two maps: conf {:?} ±{:?} m, errors on A {ea:?}, on B {eb:?}, kept {kept:?}, extent {}",
        resp.confidence, resp.rms_m, resp.diagnostics["contours"]["map_extent"]
    );
    println!("{s}");
    // The map the fit placed is within the ± everywhere, with a planar model.
    let (_, max) = if ea.0 < eb.0 { ea } else { eb };
    assert!(max <= resp.rms_m.unwrap() && max <= 75.0, "{s}");
    assert!(kept == ["similarity"] || kept == ["aniso"], "{s}");
}
