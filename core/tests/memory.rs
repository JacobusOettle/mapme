//! Peak memory of a search that finds nothing: the near disc at 57° N, where
//! its zoom-11 frame is ~2 400 cells across and every whole-frame NCC
//! surface ~25 MB. Counted by a global allocator (live heap bytes), in a pool
//! of 4 threads. Run with `cargo test --release --test memory -- --nocapture`.

mod common;
use common::synth::*;
use mapme_core::geo::{destination, LatLon};
use mapme_core::georef::Session;
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    let live = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(live, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                grew(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// A 1:50 000 sheet 110 km from the fix at 57° N: the near search (the inner
/// 20 km, then the coarse scale band and the near zoom's over 50 km) finds
/// nothing. Its coarse search correlates templates in pairs, two whole-frame
/// surfaces each, while the work within a pair is parallel too: the pairs in
/// flight keep their surfaces within `search::SURFACE_BUDGET_BYTES`. With
/// a pair per thread the near search peaked higher than one surface per
/// template had (n50-scottish 110 km off: 0.92 → 1.0 GB of RSS at 4 threads,
/// 1.05 → 1.15 GB at 6; here 0.57 → 0.69 GB of heap).
#[test]
fn near_search_peak_heap() {
    let fix = LatLon::new(57.42, -3.87);
    let (img, ocr) = synthetic_sheet(destination(fix, 225.0, 110_000.0));
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let (base, out) = pool.install(|| {
        let s = Session::new(img);
        let req = json!({"gps": {"lat": fix.lat, "lon": fix.lon, "accuracy_m": 10.0}, "dpi": 300.0, "ocr": ocr,
                         "methods": ["contours"]});
        let base = LIVE.load(Relaxed);
        PEAK.store(base, Relaxed);
        (base, serve(&s, &req, 64, 60, &noise_terrain, &mut |_| {}))
    });
    let peak_mb = (PEAK.load(Relaxed) - base) as f64 / 1e6;
    eprintln!("near search 110 km off at 57° N: peak heap {peak_mb:.0} MB above the image; {}", out.log);
    assert_eq!(out.last()["status"], "failed", "{}", out.last()["message"]);
    // 571-577 MB with two pairs in flight, 694-697 MB with four.
    assert!(peak_mb < 630.0, "{peak_mb:.0} MB");
}
