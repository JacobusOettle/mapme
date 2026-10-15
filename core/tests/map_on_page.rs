//! A small map on a large page (`content.rs`): the user's Gaia GPS export as
//! its native 1 118 × 791 px raster, placed unscaled on an A4 sheet rendered
//! at 300 dpi (2 480 × 3 508 px) under a title, with lines of text under it,
//! a frame round the sheet and the rest of it blank (a PDF that holds a map
//! smaller than its page). The contour path must size its search by the map,
//! not the page: located within the benchmark's tolerance from the map
//! centre with the fixture's tiles, the map's rectangle in the diagnostics.
//! A blank page and a page of text still fail at once.
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
