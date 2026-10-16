//! The terrain C ABI's contract as the app relies on it, whatever the implementation: argument
//! checking, JSON answers that are never NULL and always carry a known status, frees that ignore
//! NULL and stale pointers, and a store usable from many threads at once. Behaviour (profiles,
//! grids, renders, plans) is tested with the implementation.

use mapme_core::ffi::*;
use serde_json::{json, Value};
use std::ffi::{c_char, CStr, CString};
use std::ptr;

fn take_json(p: *mut c_char) -> Value {
    assert!(!p.is_null(), "a terrain function returned NULL");
    let s = unsafe { CStr::from_ptr(p) }.to_str().expect("UTF-8").to_owned();
    unsafe { mm_string_free(p) };
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

fn status(v: &Value) -> &str {
    let s = v["status"].as_str().unwrap_or_else(|| panic!("no status: {v}"));
    assert!(["ok", "need_dem", "failed"].contains(&s), "unknown status {s}");
    s
}

fn cstring(v: &Value) -> CString {
    CString::new(v.to_string()).unwrap()
}

struct Terrain(*mut MMTerrain);

impl Terrain {
    fn new() -> Terrain {
        let t = mm_terrain_new(0);
        assert!(!t.is_null());
        Terrain(t)
    }
}

impl Drop for Terrain {
    fn drop(&mut self) {
        unsafe { mm_terrain_free(self.0) };
    }
}

// Many threads share one store (CATiledLayer's drawing threads, the 3D builder, route stats).
struct Shared(*mut MMTerrain);
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

#[test]
fn stats_report_the_byte_budget() {
    for (asked, held) in [(0u64, 64u64 << 20), (1, 4 << 20), (256 << 20, 256 << 20), (u64::MAX, 1 << 30)] {
        let t = mm_terrain_new(asked);
        let v = take_json(unsafe { mm_terrain_stats(t) });
        assert_eq!(status(&v), "ok");
        assert_eq!(v["max_bytes"].as_u64(), Some(held), "asked {asked}");
        for key in ["tiles", "missing", "bytes", "evicted"] {
            assert!(v[key].as_u64().is_some(), "{key} in {v}");
        }
        unsafe { mm_terrain_free(t) };
    }
}

#[test]
fn null_and_stale_pointers_are_refused() {
    unsafe {
        mm_terrain_free(ptr::null_mut());
        mm_terrain_clear_missing(ptr::null_mut());
        assert_eq!(status(&take_json(mm_terrain_stats(ptr::null()))), "failed");
        let req = cstring(&json!({"points": [[-25.84, 27.4]]}));
        assert_eq!(status(&take_json(mm_terrain_elevations(ptr::null(), req.as_ptr()))), "failed");
        assert_eq!(status(&take_json(mm_terrain_profile(ptr::null(), req.as_ptr()))), "failed");
        assert_eq!(status(&take_json(mm_tile_plan(ptr::null()))), "failed");
        let png = [0x89u8, 0x50, 0x4E, 0x47];
        assert_eq!(mm_terrain_add_tile(ptr::null_mut(), 12, 1, 1, png.as_ptr(), png.len()), -2);
        assert_eq!(mm_terrain_mark_missing(ptr::null_mut(), 12, 1, 1), -2);

        // A freed store freed again, and a pointer that never was a store: ignored.
        let t = mm_terrain_new(0);
        mm_terrain_free(t);
        mm_terrain_free(t);
        let mut not_a_store = 0u64;
        mm_terrain_free(&mut not_a_store as *mut u64 as *mut MMTerrain);
    }
}

#[test]
fn tile_arguments_are_checked() {
    let t = Terrain::new();
    let png = [0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    unsafe {
        assert_eq!(mm_terrain_add_tile(t.0, 12, 1, 1, ptr::null(), 8), -2);
        assert_eq!(mm_terrain_add_tile(t.0, 12, 1, 1, png.as_ptr(), 0), -2);
        assert_eq!(mm_terrain_add_tile(t.0, 16, 0, 0, png.as_ptr(), png.len()), -2);
        assert_eq!(mm_terrain_add_tile(t.0, 3, 8, 0, png.as_ptr(), png.len()), -2);
        assert_eq!(mm_terrain_mark_missing(t.0, 3, 0, 8), -2);
        assert_eq!(mm_terrain_mark_missing(t.0, 20, 0, 0), -2);
        // A truncated PNG never decodes (or, before the store exists, is not implemented).
        let code = mm_terrain_add_tile(t.0, 12, 1, 1, png.as_ptr(), png.len());
        assert!(code == -1 || code == -4, "code {code}");
    }
}

#[test]
fn queries_answer_json_for_any_request() {
    let t = Terrain::new();
    let requests = [
        json!({"points": [[-25.84, 27.40], [-25.85, 27.41]]}),
        json!({"points": []}),
        json!({"points": "nowhere"}),
        json!({}),
        json!([1, 2, 3]),
    ];
    for req in &requests {
        let c = cstring(req);
        unsafe {
            status(&take_json(mm_terrain_elevations(t.0, c.as_ptr())));
            status(&take_json(mm_terrain_profile(t.0, c.as_ptr())));
            status(&take_json(mm_tile_plan(c.as_ptr())));
        }
    }
    unsafe {
        assert_eq!(status(&take_json(mm_terrain_profile(t.0, ptr::null()))), "failed");
        let garbage = CString::new("{not json").unwrap();
        assert_eq!(status(&take_json(mm_terrain_profile(t.0, garbage.as_ptr()))), "failed");
        assert_eq!(status(&take_json(mm_tile_plan(garbage.as_ptr()))), "failed");
    }
}

#[test]
fn grid_and_render_check_their_buffers() {
    let t = Terrain::new();
    let grid = cstring(&json!({"rect": [3_046_000.0, -2_978_000.0, 3_050_000.0, -2_974_000.0]}));
    let mut heights = vec![0f32; 16];
    unsafe {
        // NULL buffer, a side under 2, more nodes than floats: failed, buffer untouched.
        assert_eq!(status(&take_json(mm_terrain_grid(t.0, grid.as_ptr(), ptr::null_mut(), 16, 4, 4))), "failed");
        assert_eq!(status(&take_json(mm_terrain_grid(t.0, grid.as_ptr(), heights.as_mut_ptr(), 16, 1, 16))), "failed");
        assert_eq!(status(&take_json(mm_terrain_grid(t.0, grid.as_ptr(), heights.as_mut_ptr(), 15, 4, 4))), "failed");
        assert!(heights.iter().all(|&h| h == 0.0));
        status(&take_json(mm_terrain_grid(t.0, grid.as_ptr(), heights.as_mut_ptr(), 16, 4, 4)));
    }

    let render = cstring(&json!({"space": "mercator", "rect": [3_046_000.0, -2_978_000.0, 3_050_000.0, -2_974_000.0],
                                  "style": "dark"}));
    let mut rgba = vec![0u8; 64 * 64 * 4];
    unsafe {
        assert_eq!(status(&take_json(mm_terrain_render(t.0, render.as_ptr(), ptr::null_mut(), 64, 64, 256))), "failed");
        assert_eq!(
            status(&take_json(mm_terrain_render(t.0, render.as_ptr(), rgba.as_mut_ptr(), 0, 64, 256))),
            "failed"
        );
        assert_eq!(
            status(&take_json(mm_terrain_render(t.0, render.as_ptr(), rgba.as_mut_ptr(), 64, 64, 255))),
            "failed"
        );
        status(&take_json(mm_terrain_render(t.0, render.as_ptr(), rgba.as_mut_ptr(), 64, 64, 256)));
    }
}

#[test]
fn one_store_serves_many_threads() {
    let t = Terrain::new();
    let shared = Shared(t.0);
    let shared = &shared;
    std::thread::scope(|scope| {
        for i in 0..8 {
            scope.spawn(move || {
                let req = cstring(&json!({"points": [[-25.84 + i as f64 * 1e-3, 27.4], [-25.85, 27.41]]}));
                let mut rgba = vec![0u8; 32 * 32 * 4];
                let render = cstring(&json!({"space": "mercator", "rect": [3_046_000.0, -2_978_000.0, 3_047_000.0,
                                                                              -2_977_000.0], "style": "paper"}));
                for _ in 0..20 {
                    unsafe {
                        status(&take_json(mm_terrain_profile(shared.0, req.as_ptr())));
                        status(&take_json(mm_terrain_render(
                            shared.0,
                            render.as_ptr(),
                            rgba.as_mut_ptr(),
                            32,
                            32,
                            128,
                        )));
                        mm_terrain_mark_missing(shared.0, 12, 2359, 2352 + i);
                        status(&take_json(mm_terrain_stats(shared.0)));
                    }
                }
                unsafe { mm_terrain_clear_missing(shared.0) };
            });
        }
    });
}
