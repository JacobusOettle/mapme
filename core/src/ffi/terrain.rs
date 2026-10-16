//! C ABI of the terrain store and tile plans (`MMTerrain`; include/mapme_core.h, "Terrain").
//!
//! Like the rest of the ABI: arguments are checked here, panics are caught, JSON functions never
//! return NULL. Every function on one store may run on many threads at once: only shared
//! references to the store are ever created, and `TerrainStore` locks internally.

use super::{
    c_str, into_c_string, json_call, MAX_PIXELS, MAX_SIDE, MM_ERR_ARGUMENT, MM_ERR_DECODE, MM_ERR_PANIC, MM_OK,
};
use crate::dem::TileId;
use crate::georef::failed_json;
use crate::terrain::{
    answer_json, parse_request, ElevationsRequest, GridRequest, ProfileRequest, RenderRequest, TerrainError,
    TerrainStore, GRID_SIDE,
};
use std::collections::BTreeSet;
use std::ffi::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard};

/// An entry point not implemented in this build (int functions).
pub const MM_ERR_NOT_IMPLEMENTED: i32 = -4;

/// Opaque terrain store handle (`MMTerrain` in C).
pub struct MMTerrain {
    inner: TerrainStore,
}

/// Stores `mm_terrain_new` made that are not freed yet: freeing anything else is ignored.
static LIVE_TERRAINS: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());

fn live_terrains() -> MutexGuard<'static, BTreeSet<usize>> {
    LIVE_TERRAINS.lock().unwrap_or_else(|e| e.into_inner())
}

fn error_code(e: TerrainError) -> i32 {
    match e {
        TerrainError::Argument(_) => MM_ERR_ARGUMENT,
        TerrainError::Decode(_) => MM_ERR_DECODE,
        TerrainError::NotImplemented => MM_ERR_NOT_IMPLEMENTED,
    }
}

/// Run a JSON query on a store.
///
/// # Safety
/// `terrain` must be NULL or a live store; `request_json` NULL or a NUL-terminated string.
unsafe fn query(
    terrain: *const MMTerrain,
    request_json: *const c_char,
    f: impl FnOnce(&TerrainStore, &str) -> String,
) -> *mut c_char {
    if terrain.is_null() {
        return into_c_string(failed_json("terrain is NULL"));
    }
    let store = &(*terrain).inner;
    match c_str(request_json, "request_json") {
        Ok(req) => json_call(|| f(store, req)),
        Err(e) => into_c_string(failed_json(&format!("Invalid request: {e}"))),
    }
}

/// A store holding at most `max_bytes` of decoded elevations (0: 64 MiB). NULL only on an
/// internal error.
#[no_mangle]
pub extern "C" fn mm_terrain_new(max_bytes: u64) -> *mut MMTerrain {
    catch_unwind(|| {
        let terrain = Box::into_raw(Box::new(MMTerrain { inner: TerrainStore::new(max_bytes) }));
        live_terrains().insert(terrain as usize);
        terrain
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Destroy a store. NULL, a store freed already and any pointer that is not a live store are
/// ignored.
///
/// # Safety
/// No call on `terrain` may still be running.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_free(terrain: *mut MMTerrain) {
    if terrain.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if live_terrains().remove(&(terrain as usize)) {
            drop(Box::from_raw(terrain));
        }
    }));
}

/// Add a Terrarium PNG tile. Returns 0, -1 (not decodable: marked missing), -2 (arguments),
/// -3 (internal error) or -4 (not implemented).
///
/// # Safety
/// `terrain` must be NULL or a live store; `png` NULL or `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_add_tile(
    terrain: *mut MMTerrain,
    z: u8,
    x: u32,
    y: u32,
    png: *const u8,
    len: usize,
) -> i32 {
    if terrain.is_null() || png.is_null() || len == 0 {
        return MM_ERR_ARGUMENT;
    }
    let store = &(*terrain).inner;
    catch_unwind(AssertUnwindSafe(|| {
        let bytes = std::slice::from_raw_parts(png, len);
        match store.add_png(TileId { z, x, y }, bytes) {
            Ok(()) => MM_OK,
            Err(e) => error_code(e),
        }
    }))
    .unwrap_or(MM_ERR_PANIC)
}

/// Record that a tile is unavailable. Returns 0, -2 (arguments) or -4 (not implemented).
///
/// # Safety
/// `terrain` must be NULL or a live store.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_mark_missing(terrain: *mut MMTerrain, z: u8, x: u32, y: u32) -> i32 {
    if terrain.is_null() {
        return MM_ERR_ARGUMENT;
    }
    let store = &(*terrain).inner;
    catch_unwind(AssertUnwindSafe(|| match store.mark_missing(TileId { z, x, y }) {
        Ok(()) => MM_OK,
        Err(e) => error_code(e),
    }))
    .unwrap_or(MM_ERR_PANIC)
}

/// Forget every missing mark.
///
/// # Safety
/// `terrain` must be NULL or a live store.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_clear_missing(terrain: *mut MMTerrain) {
    if terrain.is_null() {
        return;
    }
    let store = &(*terrain).inner;
    let _ = catch_unwind(AssertUnwindSafe(|| store.clear_missing()));
}

/// `{"status":"ok","tiles","missing","bytes","max_bytes","evicted"}`; never NULL.
///
/// # Safety
/// `terrain` must be NULL or a live store.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_stats(terrain: *const MMTerrain) -> *mut c_char {
    if terrain.is_null() {
        return into_c_string(failed_json("terrain is NULL"));
    }
    let store = &(*terrain).inner;
    json_call(|| answer_json(Ok(store.stats())))
}

/// Elevations at points; never NULL.
///
/// # Safety
/// `terrain` must be NULL or a live store; `request_json` NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_elevations(terrain: *const MMTerrain, request_json: *const c_char) -> *mut c_char {
    query(terrain, request_json, |store, json| {
        answer_json(parse_request::<ElevationsRequest>(json).and_then(|req| store.elevations(&req)))
    })
}

/// Statistics and elevation profile along a polyline; never NULL.
///
/// # Safety
/// `terrain` must be NULL or a live store; `request_json` NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_profile(terrain: *const MMTerrain, request_json: *const c_char) -> *mut c_char {
    query(terrain, request_json, |store, json| {
        answer_json(parse_request::<ProfileRequest>(json).and_then(|req| store.profile(&req)))
    })
}

/// Heights on a `cols × rows` grid over a Web Mercator rectangle into `heights`; never NULL.
///
/// # Safety
/// `terrain` must be NULL or a live store; `request_json` NULL or a NUL-terminated string;
/// `heights` NULL or `heights_len` writable floats not accessed by anyone else during the call.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_grid(
    terrain: *const MMTerrain,
    request_json: *const c_char,
    heights: *mut f32,
    heights_len: usize,
    cols: u32,
    rows: u32,
) -> *mut c_char {
    let (c, r) = (cols as usize, rows as usize);
    let side = GRID_SIDE.0..=GRID_SIDE.1;
    if heights.is_null() || !side.contains(&c) || !side.contains(&r) || c * r > heights_len {
        return into_c_string(failed_json(&format!(
            "Invalid grid: {cols}×{rows} nodes into {heights_len} floats (2…{} per side)",
            GRID_SIDE.1
        )));
    }
    query(terrain, request_json, |store, json| {
        let out = std::slice::from_raw_parts_mut(heights, c * r);
        answer_json(parse_request::<GridRequest>(json).and_then(|req| store.grid(&req, out, c, r)))
    })
}

/// Draw contours (and hillshade) into an RGBA buffer; never NULL.
///
/// # Safety
/// `terrain` must be NULL or a live store; `request_json` NULL or a NUL-terminated string;
/// `rgba` NULL or at least `bytes_per_row * (height - 1) + width * 4` writable bytes not accessed
/// by anyone else during the call.
#[no_mangle]
pub unsafe extern "C" fn mm_terrain_render(
    terrain: *const MMTerrain,
    request_json: *const c_char,
    rgba: *mut u8,
    width: u32,
    height: u32,
    bytes_per_row: u32,
) -> *mut c_char {
    if rgba.is_null()
        || width == 0
        || height == 0
        || width > MAX_SIDE
        || height > MAX_SIDE
        || width as u64 * height as u64 > MAX_PIXELS
        || (bytes_per_row as u64) < width as u64 * 4
    {
        return into_c_string(failed_json(&format!(
            "Invalid buffer: {width}×{height} pixels, {bytes_per_row} bytes per row"
        )));
    }
    let (w, h, stride) = (width as usize, height as usize, bytes_per_row as usize);
    let Some(len) = stride.checked_mul(h - 1).and_then(|n| n.checked_add(w * 4)) else {
        return into_c_string(failed_json("Invalid buffer: too large"));
    };
    query(terrain, request_json, |store, json| {
        let data = std::slice::from_raw_parts_mut(rgba, len);
        answer_json(parse_request::<RenderRequest>(json).and_then(|req| store.render(&req, data, w, h, stride)))
    })
}

/// Tiles covering areas at zooms, with their weight (stateless); never NULL.
///
/// # Safety
/// `request_json` must be NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mm_tile_plan(request_json: *const c_char) -> *mut c_char {
    match c_str(request_json, "request_json") {
        Ok(req) => json_call(|| crate::terrain::tile_plan_json(req)),
        Err(e) => into_c_string(failed_json(&format!("Invalid request: {e}"))),
    }
}
