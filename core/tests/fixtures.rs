mod common;
use common::*;
use mapme_core::geo::merc_to_latlon;

#[test]
fn fixtures_load_and_truth_is_consistent() {
    let m = load_map();
    assert_eq!((m.w, m.h), (3507, 4960));
    let t = truth();
    // Graticule intersection (878, 541) is 25°49'S 27°23'E.
    let (mx, my) = t.apply(878.0, 541.0);
    let (lat, lon) = merc_to_latlon(mx, my);
    assert!((lat - (-(25.0 + 49.0 / 60.0))).abs() < 2e-5, "{lat}");
    assert!((lon - (27.0 + 23.0 / 60.0)).abs() < 2e-5, "{lon}");
    assert!(max_error_m(&t.to_homography()) < 1e-6);
    let s = dem_store();
    assert_eq!(s.len(), 188);
    assert!(ocr_fixture().len() > 20);
}
