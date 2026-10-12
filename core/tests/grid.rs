//! Grid / label georeferencing, label parsing and control points on the real
//! Grootkloof sheet (fixtures/grootkloof) plus a synthetic UTM grid.
mod common;
use common::*;
use mapme_core::control_points::{self, ControlPoint};
use mapme_core::geo::{latlon_to_merc, latlon_to_utm, merc_to_latlon, utm_to_latlon, Homography, LatLon, Utm};
use mapme_core::grid::{detect_lines, georeference_from_grid, GridLine, GridMethod, GridResult, LineAxis};
use mapme_core::image::Rgb8;
use mapme_core::labels::{classify_pois, parse_coordinate, OcrObservation, PoiKind};
use std::sync::OnceLock;
use std::time::Instant;

fn map() -> &'static Rgb8 {
    static M: OnceLock<Rgb8> = OnceLock::new();
    M.get_or_init(load_map)
}

fn lines() -> &'static [GridLine] {
    static L: OnceLock<Vec<GridLine>> = OnceLock::new();
    L.get_or_init(|| detect_lines(map()))
}

fn gps() -> Option<LatLon> {
    Some(LatLon::new(CENTER.0, CENTER.1))
}

fn report(name: &str, r: &GridResult) -> f64 {
    let err = max_error_m(&r.transform);
    println!(
        "{name}: {:?} max err {err:.2} m, rms {:.2} m, {} lat + {} lon labels, {} GCPs, used {:?}",
        r.method,
        r.rms_m,
        r.n_lat,
        r.n_lon,
        r.gcps.len(),
        r.used_labels
    );
    err
}

#[test]
fn detect_lines_finds_graticule_and_frame() {
    let ls = lines();
    let w = map().w as f64;
    let h = map().h as f64;
    let hs: Vec<f64> = ls.iter().filter(|l| l.axis == LineAxis::Horizontal).map(|l| l.pos_at(w / 2.0)).collect();
    let vs: Vec<f64> = ls.iter().filter(|l| l.axis == LineAxis::Vertical).map(|l| l.pos_at(h / 2.0)).collect();
    for l in ls {
        println!("{:?} pos {:.2} tilt {:.3}° cov {:.3}", l.axis, l.pos_at(w.min(h) / 2.0), l.tilt_deg(), l.coverage);
    }
    // Graticule lines exactly; the double frame line (thin line + border bar +
    // thin line) is reported once, at either of its edges.
    let expect_h: [&[f64]; 7] =
        [&[106.0, 118.0], &[541.0], &[1515.0], &[2488.0], &[3462.0], &[4436.0], &[4841.0, 4853.0]];
    let expect_v: [&[f64]; 5] = [&[106.0, 118.0], &[878.0], &[1754.0], &[2630.0], &[3388.0, 3400.0]];
    assert_eq!(hs.len(), expect_h.len(), "horizontal {hs:?}");
    assert_eq!(vs.len(), expect_v.len(), "vertical {vs:?}");
    for (got, want) in hs.iter().zip(expect_h).chain(vs.iter().zip(expect_v)) {
        assert!(want.iter().any(|w| (got - w).abs() <= 1.5), "line at {got:.2}, expected one of {want:?}");
    }
    for l in ls {
        assert!(l.tilt_deg().abs() < 0.05, "{l:?}");
        assert!(l.coverage >= 0.35 && l.coverage <= 1.0, "{l:?}");
    }
}

#[test]
fn detect_lines_is_fast() {
    let img = map();
    let mut best = f64::INFINITY;
    let mut n = 0;
    for _ in 0..3 {
        let t = Instant::now();
        n = detect_lines(img).len();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("detect_lines on {}x{}: {best:.1} ms (best of 3)", img.w, img.h);
    assert_eq!(n, 12);
    if !cfg!(debug_assertions) {
        assert!(best < 400.0, "detect_lines took {best:.1} ms");
    }
}

#[test]
fn grid_georeference_grootkloof() {
    let obs = ocr_fixture();
    for g in [None, gps()] {
        let t = Instant::now();
        let r = georeference_from_grid(map(), lines(), &obs, g).expect("grid result");
        println!("georeference_from_grid: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
        let err = report("fixture", &r);
        assert_eq!(r.method, GridMethod::Lines);
        assert_eq!((r.n_lat, r.n_lon), (5, 3));
        assert_eq!(r.used_labels, (0..8).collect::<Vec<_>>());
        assert_eq!(r.gcps.len(), 15);
        assert!(err < 8.0, "max error {err:.2} m");
        assert!(r.rms_m < 5.0, "rms {:.2} m", r.rms_m);
        assert!(r.transform.is_affine());
    }
}

#[test]
fn grid_georeference_robust_to_ocr_noise() {
    let mut obs = ocr_fixture();
    obs[0].text = "25°48'00\"S".into(); // wrong value on the 25°49' line
    obs[2].text = "25*51’00\"S".into();
    obs[4].text = "25°53'0O\"S".into();
    obs[5].text = "27*23’00”E".into();
    obs[7].text = "27º25′00″E".into();
    let bogus = OcrObservation { text: "25°59'00\"S".into(), bbox: [116.0, 2908.0, 36.0, 185.0], confidence: 0.9 };
    // Drop the 25°50' and 25°52' latitude labels and the 27°24' longitude label.
    let mut noisy: Vec<OcrObservation> =
        obs.iter().enumerate().filter(|(i, _)| ![1usize, 3, 6].contains(i)).map(|(_, o)| o.clone()).collect();
    noisy.push(bogus);
    let idx = |t: &str| noisy.iter().position(|o| o.text == t).unwrap();
    let (wrong, fake) = (idx("25°48'00\"S"), idx("25°59'00\"S"));
    for g in [None, gps()] {
        let r = georeference_from_grid(map(), lines(), &noisy, g).expect("grid result");
        let err = report("noisy", &r);
        assert_eq!(r.method, GridMethod::Lines);
        assert!(err < 8.0, "max error {err:.2} m");
        assert!(!r.used_labels.contains(&wrong) && !r.used_labels.contains(&fake), "{:?}", r.used_labels);
        assert_eq!((r.n_lat, r.n_lon), (2, 2));
        // Unlabelled graticule lines are recovered from their round values.
        assert_eq!(r.gcps.len(), 15);
    }
}

#[test]
fn grid_georeference_with_lots_of_ocr_junk() {
    let mut obs = ocr_fixture();
    let mut seed = 12345u64;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for k in 0..600 {
        let text = match k % 4 {
            0 => format!("{}°{:02}'", 10 + rnd(80), rnd(60)),
            1 => format!("25°{}'00\"S", 40 + rnd(20)),
            2 => format!("{}", 10 + rnd(90)),
            _ => format!("27°{}'E", 15 + rnd(20)),
        };
        let (x, y) = (rnd(3400) as f64, rnd(4850) as f64);
        obs.push(OcrObservation { text, bbox: [x, y, 120.0, 28.0], confidence: 0.5 });
    }
    for g in [None, gps()] {
        let t = Instant::now();
        let r = georeference_from_grid(map(), lines(), &obs, g).expect("grid result");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let err = report("junk", &r);
        println!("georeference_from_grid with {} observations: {ms:.1} ms", obs.len());
        assert_eq!(r.method, GridMethod::Lines);
        assert!(err < 8.0, "max error {err:.2} m");
        assert!((0..8).all(|i| r.used_labels.contains(&i)), "{:?}", r.used_labels);
        if !cfg!(debug_assertions) {
            assert!(ms < 250.0, "{ms:.1} ms");
        }
    }
}

#[test]
fn grid_georeference_without_hemisphere_letters() {
    let mut obs = ocr_fixture();
    for o in obs.iter_mut().take(8) {
        o.text = o.text.trim_end_matches(['S', 'E']).to_string();
    }
    for g in [None, gps()] {
        let r = georeference_from_grid(map(), lines(), &obs, g).expect("grid result");
        let err = report("no hemisphere", &r);
        assert_eq!(r.method, GridMethod::Lines);
        assert!(err < 8.0, "max error {err:.2} m");
    }
}

/// Max ground error of `h`, a georeference of a resampled copy of the map,
/// over the original map frame; `fwd` maps original pixels into the copy.
fn max_error_resampled(h: &Homography, fwd: &dyn Fn(f64, f64) -> (f64, f64)) -> f64 {
    let t = truth();
    let lat_cos = (-25.85f64).to_radians().cos();
    let mut worst = 0.0f64;
    for i in 0..5 {
        for j in 0..5 {
            let x = 107.0 + (3399.0 - 107.0) * i as f64 / 4.0;
            let y = 106.0 + (4841.0 - 106.0) * j as f64 / 4.0;
            let (u, v) = fwd(x, y);
            let a = h.apply(u, v);
            let b = t.apply(x, y);
            worst = worst.max((a.0 - b.0).hypot(a.1 - b.1) * lat_cos);
        }
    }
    worst
}

/// OCR boxes moved with the image (centre mapped, size scaled).
fn resample_obs(obs: &[OcrObservation], fwd: &dyn Fn(f64, f64) -> (f64, f64), scale: f64) -> Vec<OcrObservation> {
    obs.iter()
        .map(|o| {
            let (cx, cy) = fwd(o.center().0, o.center().1);
            let (w, h) = (o.bbox[2] * scale, o.bbox[3] * scale);
            OcrObservation { text: o.text.clone(), bbox: [cx - w / 2.0, cy - h / 2.0, w, h], confidence: o.confidence }
        })
        .collect()
}

fn rotate(m: &Rgb8, deg: f64) -> Rgb8 {
    let (s, c) = deg.to_radians().sin_cos();
    let (cx, cy) = ((m.w as f64 - 1.0) / 2.0, (m.h as f64 - 1.0) / 2.0);
    let mut out = vec![255u8; m.w * m.h * 3];
    for y in 0..m.h {
        for x in 0..m.w {
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            let (sx, sy) = (c * dx + s * dy + cx, -s * dx + c * dy + cy);
            if sx < 0.0 || sy < 0.0 || sx >= (m.w - 1) as f64 || sy >= (m.h - 1) as f64 {
                continue;
            }
            let (x0, y0) = (sx as usize, sy as usize);
            let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
            for ch in 0..3 {
                let p = |xx: usize, yy: usize| m.data[(yy * m.w + xx) * 3 + ch] as f64;
                let v = p(x0, y0) * (1.0 - fx) * (1.0 - fy)
                    + p(x0 + 1, y0) * fx * (1.0 - fy)
                    + p(x0, y0 + 1) * (1.0 - fx) * fy
                    + p(x0 + 1, y0 + 1) * fx * fy;
                out[(y * m.w + x) * 3 + ch] = v.round() as u8;
            }
        }
    }
    Rgb8::from_rgb(m.w, m.h, out).unwrap()
}

fn downscale(m: &Rgb8, f: usize) -> Rgb8 {
    let (w, h) = (m.w / f, m.h / f);
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for ch in 0..3 {
                let mut s = 0u32;
                for dy in 0..f {
                    for dx in 0..f {
                        s += m.data[((y * f + dy) * m.w + x * f + dx) * 3 + ch] as u32;
                    }
                }
                out[(y * w + x) * 3 + ch] = ((s + (f * f / 2) as u32) / (f * f) as u32) as u8;
            }
        }
    }
    Rgb8::from_rgb(w, h, out).unwrap()
}

#[test]
fn rotated_map() {
    let deg = 3.0f64;
    let img = rotate(map(), deg);
    let (s, c) = deg.to_radians().sin_cos();
    let (cx, cy) = ((img.w as f64 - 1.0) / 2.0, (img.h as f64 - 1.0) / 2.0);
    let fwd = move |x: f64, y: f64| (c * (x - cx) - s * (y - cy) + cx, s * (x - cx) + c * (y - cy) + cy);
    let ls = detect_lines(&img);
    assert_eq!(ls.len(), 12, "{ls:?}");
    for l in &ls {
        assert!((l.tilt_deg() - deg).abs() < 0.05, "{l:?} tilt {}", l.tilt_deg());
    }
    let obs = resample_obs(&ocr_fixture(), &fwd, 1.0);
    let r = georeference_from_grid(&img, &ls, &obs, gps()).expect("rotated result");
    let err = max_error_resampled(&r.transform, &fwd);
    println!("rotated {deg}°: {:?} max err {err:.2} m, rms {:.2} m", r.method, r.rms_m);
    assert_eq!(r.method, GridMethod::Lines);
    assert!(err < 8.0, "max error {err:.2} m");
}

#[test]
fn screenshot_scale_maps() {
    for f in [2usize, 3, 4] {
        let img = downscale(map(), f);
        let k = f as f64;
        let fwd = move |x: f64, y: f64| ((x - (k - 1.0) / 2.0) / k, (y - (k - 1.0) / 2.0) / k);
        let obs = resample_obs(&ocr_fixture(), &fwd, 1.0 / k);
        let ls = detect_lines(&img);
        let r = georeference_from_grid(&img, &ls, &obs, gps()).expect("downscaled result");
        let err = max_error_resampled(&r.transform, &fwd);
        println!("1/{f} scale {}x{}: {} lines, {:?} max err {err:.2} m", img.w, img.h, ls.len(), r.method);
        assert_eq!(r.method, GridMethod::Lines, "1/{f} scale");
        assert!(err < 3.0 * k, "1/{f} scale max error {err:.2} m");
    }
}

#[test]
fn labels_only_fallback() {
    let obs = ocr_fixture();
    let r = georeference_from_grid(map(), &[], &obs, None).expect("labels-only result");
    let err = report("labels only", &r);
    assert_eq!(r.method, GridMethod::LabelsOnly);
    assert!(err < 150.0, "max error {err:.2} m");
    assert!(r.rms_m > 1.0 && r.rms_m < 150.0, "rms {:.2}", r.rms_m);
}

#[test]
fn no_coordinate_labels_gives_none() {
    let obs: Vec<OcrObservation> = ocr_fixture().into_iter().filter(|o| parse_coordinate(&o.text).is_none()).collect();
    assert!(obs.len() > 15);
    assert!(georeference_from_grid(map(), lines(), &obs, None).is_none());
    assert!(georeference_from_grid(map(), lines(), &obs, gps()).is_none());
    assert!(georeference_from_grid(map(), &[], &obs, gps()).is_none());
}

#[test]
fn pois_on_fixture() {
    let pois = classify_pois(&ocr_fixture());
    for p in &pois {
        println!("{:?} {:?} {:?} ({:.0}, {:.0})", p.kind, p.name, p.elevation_m, p.x, p.y);
    }
    let places: Vec<&str> = pois.iter().filter(|p| p.kind == PoiKind::Place).map(|p| p.name.as_str()).collect();
    assert_eq!(places, vec!["KWAMI LODGE", "SUGARBUSH HILL COUNTRY COTTAGE"]);
    let mut heights: Vec<f64> =
        pois.iter().filter(|p| p.kind == PoiKind::SpotHeight).filter_map(|p| p.elevation_m).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    assert_eq!(heights, vec![1412.0, 1428.0, 1523.0]);
    assert_eq!(pois.len(), 5);
    let kwami = pois.iter().find(|p| p.name == "KWAMI LODGE").unwrap();
    assert!((kwami.x - 2944.0).abs() < 1.0 && (kwami.y - 4499.0).abs() < 1.0);
}

fn truth_point(x: f64, y: f64) -> ControlPoint {
    let (mx, my) = truth().apply(x, y);
    let (lat, lon) = merc_to_latlon(mx, my);
    ControlPoint { x, y, lat, lon }
}

#[test]
fn control_point_fits_against_truth() {
    let two = [truth_point(878.0, 541.0), truth_point(2630.0, 4436.0)];
    let f = control_points::fit(&two, "auto").unwrap();
    let e2 = max_error_m(&f.transform);
    assert_eq!(f.model, "similarity");
    assert!(e2 < 5.0, "2-point similarity max error {e2:.2} m");

    let three = [truth_point(878.0, 541.0), truth_point(2630.0, 1515.0), truth_point(1754.0, 4436.0)];
    let f = control_points::fit(&three, "auto").unwrap();
    let e3 = max_error_m(&f.transform);
    assert_eq!(f.model, "affine");
    assert!(e3 < 0.01, "3-point affine max error {e3:.4} m");

    // Five taps with a few metres of error each.
    let mut five: Vec<ControlPoint> =
        [(300.0, 300.0), (3200.0, 400.0), (1700.0, 2500.0), (400.0, 4600.0), (3100.0, 4500.0)]
            .iter()
            .map(|&(x, y)| truth_point(x, y))
            .collect();
    for (k, p) in five.iter_mut().enumerate() {
        p.x += [1.5, -1.0, 0.5, -1.5, 1.0][k];
        p.y += [-1.0, 1.5, -0.5, 1.0, -1.5][k];
    }
    let f = control_points::fit(&five, "auto").unwrap();
    let e5 = max_error_m(&f.transform);
    println!("control points: 2 → {e2:.2} m, 3 → {e3:.4} m, 5 → {e5:.2} m (rms {:.2} m)", f.rms_m);
    assert_eq!(f.model, "affine");
    assert!(e5 < 5.0, "5-point affine max error {e5:.2} m");
    assert!(f.rms_m > 0.5 && f.rms_m < 5.0);
    let h = control_points::fit(&five, "homography").unwrap();
    assert!(max_error_m(&h.transform) < 10.0);
}

/// Synthetic UTM grid: 1 km lines at 2 m/px around the Grootkloof GPS fix,
/// labelled with full ("539000mE", "7143000mN") and abbreviated ("40", "41")
/// values.
fn utm_fixture() -> (Rgb8, Vec<OcrObservation>, impl Fn(f64, f64) -> (f64, f64)) {
    let (w, h) = (2400usize, 2400usize);
    let mut data = vec![255u8; w * h * 3];
    let mut ink = |x: usize, y: usize| {
        let i = (y * w + x) * 3;
        data[i..i + 3].copy_from_slice(&[30, 30, 30]);
    };
    for k in 0..5 {
        let p = 200 + 500 * k;
        for s in 0..w {
            ink(s, p);
            ink(s, p + 1);
            ink(p, s);
            ink(p + 1, s);
        }
    }
    let img = Rgb8::from_rgb(w, h, data).unwrap();
    let ob = |t: &str, x: f64, y: f64, bw: f64, bh: f64| OcrObservation {
        text: t.into(),
        bbox: [x - bw / 2.0, y - bh / 2.0, bw, bh],
        confidence: 0.9,
    };
    let obs = vec![
        ob("539000mE", 200.5, 2340.0, 150.0, 28.0),
        ob("40", 700.5, 2340.0, 34.0, 28.0),
        ob("41", 1200.5, 2340.0, 34.0, 28.0),
        ob("542000mE", 1700.5, 2340.0, 150.0, 28.0),
        ob("7143000mN", 90.0, 200.5, 160.0, 28.0),
        ob("42", 60.0, 700.5, 34.0, 28.0),
        ob("41", 60.0, 1200.5, 34.0, 28.0),
        ob("1523", 900.0, 900.0, 54.0, 20.0),
        ob("250", 1500.0, 1900.0, 50.0, 20.0),
    ];
    // Pixel → UTM: line k at pixel 200.5 + 500k (2 px wide lines).
    let to_utm = |x: f64, y: f64| (539_000.0 + (x - 200.5) * 2.0, 7_143_000.0 - (y - 200.5) * 2.0);
    (img, obs, to_utm)
}

#[test]
fn utm_grid_with_abbreviated_labels() {
    let (img, obs, to_utm) = utm_fixture();
    let ls = detect_lines(&img);
    assert_eq!(ls.len(), 10, "{ls:?}");
    let g = gps().unwrap();
    let zone = latlon_to_utm(g.lat, g.lon);
    let r = georeference_from_grid(&img, &ls, &obs, Some(g)).expect("utm result");
    assert_eq!(r.method, GridMethod::Lines);
    assert_eq!(r.gcps.len(), 25);
    assert_eq!(r.used_labels, (0..7).collect::<Vec<_>>());
    let mut worst = 0.0f64;
    for (x, y) in [(0.0, 0.0), (2399.0, 0.0), (1200.0, 1200.0), (0.0, 2399.0), (2399.0, 2399.0), (700.0, 1900.0)] {
        let (e, n) = to_utm(x, y);
        let ll = utm_to_latlon(&Utm { zone: zone.zone, north: zone.north, easting: e, northing: n });
        let want = latlon_to_merc(ll.lat, ll.lon);
        let got = r.transform.apply(x, y);
        worst = worst.max((want.0 - got.0).hypot(want.1 - got.1) * ll.lat.to_radians().cos());
    }
    println!("UTM grid: max err {worst:.3} m, rms {:.3} m", r.rms_m);
    assert!(worst < 2.0, "max error {worst:.3} m");
    // Without a GPS fix the UTM zone is unknown.
    assert!(georeference_from_grid(&img, &ls, &obs, None).is_none());
}

#[test]
fn one_latitude_label_on_the_longitudes_scale() {
    // Only 25°51'00"S of the latitude labels (obs 2), all three longitudes.
    let obs: Vec<OcrObservation> =
        ocr_fixture().into_iter().enumerate().filter(|(i, _)| ![0usize, 1, 3, 4].contains(i)).map(|(_, o)| o).collect();
    for g in [None, gps()] {
        let r = georeference_from_grid(map(), lines(), &obs, g).expect("grid result");
        let err = report("one latitude label", &r);
        assert_eq!(r.method, GridMethod::Lines);
        assert!(r.lone_label);
        assert_eq!((r.n_lat, r.n_lon), (1, 3));
        assert_eq!(r.gcps.len(), 15, "every graticule line on its round value");
        assert!(err < 8.0, "max error {err:.2} m");
    }
    // Two latitude labels that disagree: neither can be trusted alone.
    let mut two = obs.clone();
    two.push(OcrObservation { text: "25°55'00\"S".into(), bbox: [116.0, 3376.0, 36.0, 185.0], confidence: 0.9 });
    assert!(georeference_from_grid(map(), lines(), &two, gps()).is_none());
    // Two labels on each axis never take the one-label path.
    let r = georeference_from_grid(map(), lines(), &ocr_fixture(), gps()).unwrap();
    assert!(!r.lone_label);
}
