//! C ABI. See include/mapme_core.h.
//!
//! Every entry point catches panics (the crate is built with
//! `panic = "unwind"`) and turns them, like any error, into a failed JSON
//! response or an error return value. Only shared references to the session
//! are ever created, so `mm_session_progress` / `mm_session_cancel` may run
//! on other threads while `mm_session_georeference` works.

use crate::dem::TileId;
use crate::georef::{failed_json, fit_control_points_json, Session};
use crate::image::{Rgb8, RgbaView};
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Opaque session handle (`MMSession` in C).
pub struct MMSession {
    inner: Session,
}

/// Largest raster accepted (pixels per side and in total).
const MAX_SIDE: u32 = 32_768;
const MAX_PIXELS: u64 = 400_000_000;

/// Return codes of the tile functions.
pub const MM_OK: i32 = 0;
pub const MM_ERR_DECODE: i32 = -1;
pub const MM_ERR_ARGUMENT: i32 = -2;
pub const MM_ERR_PANIC: i32 = -3;

fn panic_message(e: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

/// Hand a Rust string to C (the caller frees it with `mm_string_free`).
fn into_c_string(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        // serde_json escapes NUL, so this is unreachable in practice.
        Err(_) => {
            CString::new(failed_json("response contained a NUL byte")).map_or(std::ptr::null_mut(), |c| c.into_raw())
        }
    }
}

/// Run `f`, converting a panic into a failed JSON response.
fn json_call(f: impl FnOnce() -> String) -> *mut c_char {
    let out = catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|e| failed_json(&format!("Internal error in the georeferencing core: {}", panic_message(&*e))));
    into_c_string(out)
}

/// Borrow a C string as UTF-8.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string that outlives the borrow.
unsafe fn c_str<'a>(p: *const c_char, what: &str) -> Result<&'a str, String> {
    if p.is_null() {
        return Err(format!("{what} is NULL"));
    }
    CStr::from_ptr(p).to_str().map_err(|_| format!("{what} is not valid UTF-8"))
}

/// Create a session from an 8-bit RGBA raster (alpha ignored), rows
/// `bytes_per_row` apart. The pixels are copied. Returns NULL on invalid input.
///
/// # Safety
/// `rgba` must be NULL or point to at least `bytes_per_row * (height - 1) + width * 4` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn mm_session_new(
    rgba: *const u8,
    width: u32,
    height: u32,
    bytes_per_row: u32,
) -> *mut MMSession {
    if rgba.is_null()
        || width == 0
        || height == 0
        || width > MAX_SIDE
        || height > MAX_SIDE
        || width as u64 * height as u64 > MAX_PIXELS
        || (bytes_per_row as u64) < width as u64 * 4
    {
        return std::ptr::null_mut();
    }
    let (w, h, stride) = (width as usize, height as usize, bytes_per_row as usize);
    let Some(len) = stride.checked_mul(h - 1).and_then(|n| n.checked_add(w * 4)) else {
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| {
        let data = std::slice::from_raw_parts(rgba, len);
        let Some(view) = RgbaView::new(data, w, h, stride) else {
            return std::ptr::null_mut();
        };
        let image: Rgb8 = Rgb8::from_rgba(&view);
        Box::into_raw(Box::new(MMSession { inner: Session::new(image) }))
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Destroy a session. NULL is ignored.
///
/// # Safety
/// `session` must be NULL or a pointer returned by `mm_session_new` that was
/// not freed yet, with no call on it still running.
#[no_mangle]
pub unsafe extern "C" fn mm_session_free(session: *mut MMSession) {
    if session.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(session))));
}

/// Add a Terrarium PNG DEM tile. Returns 0 on success, -1 if the PNG could
/// not be decoded (the tile is then treated as missing), -2 on invalid
/// arguments, -3 on an internal error.
///
/// # Safety
/// `session` must be NULL or a live session; `png` must be NULL or point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn mm_session_add_dem_tile(
    session: *mut MMSession,
    z: u8,
    x: u32,
    y: u32,
    png: *const u8,
    len: usize,
) -> i32 {
    if session.is_null() || png.is_null() || len == 0 {
        return MM_ERR_ARGUMENT;
    }
    let s = &(*session).inner;
    let t = TileId { z, x, y };
    catch_unwind(AssertUnwindSafe(|| {
        let bytes = std::slice::from_raw_parts(png, len);
        match s.add_dem_tile(t, bytes) {
            Ok(()) => MM_OK,
            // Both refuse invalid tile coordinates; a PNG that fails to decode is already marked missing.
            Err(_) if s.mark_dem_tile_missing(t).is_err() => MM_ERR_ARGUMENT,
            Err(_) => MM_ERR_DECODE,
        }
    }))
    .unwrap_or(MM_ERR_PANIC)
}

/// Record that a DEM tile is unavailable. Returns 0, or -2 on invalid arguments.
///
/// # Safety
/// `session` must be NULL or a live session.
#[no_mangle]
pub unsafe extern "C" fn mm_session_mark_dem_tile_missing(session: *mut MMSession, z: u8, x: u32, y: u32) -> i32 {
    if session.is_null() {
        return MM_ERR_ARGUMENT;
    }
    let s = &(*session).inner;
    catch_unwind(AssertUnwindSafe(|| match s.mark_dem_tile_missing(TileId { z, x, y }) {
        Ok(()) => MM_OK,
        Err(_) => MM_ERR_ARGUMENT,
    }))
    .unwrap_or(MM_ERR_PANIC)
}

/// Run georeferencing; returns a response JSON string (never NULL).
///
/// # Safety
/// `session` must be NULL or a live session; `request_json` must be NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mm_session_georeference(session: *mut MMSession, request_json: *const c_char) -> *mut c_char {
    if session.is_null() {
        return into_c_string(failed_json("session is NULL"));
    }
    let s = &(*session).inner;
    let req = match c_str(request_json, "request_json") {
        Ok(r) => r,
        Err(e) => return into_c_string(failed_json(&format!("Invalid request: {e}"))),
    };
    json_call(|| s.georeference_json(req))
}

/// Progress of the running georeference call, 0..1. Safe from any thread.
///
/// # Safety
/// `session` must be NULL or a live session.
#[no_mangle]
pub unsafe extern "C" fn mm_session_progress(session: *const MMSession) -> f32 {
    if session.is_null() {
        return 0.0;
    }
    (*session).inner.progress()
}

/// Ask the running georeference call (or the next one) to stop. Safe from any thread.
///
/// # Safety
/// `session` must be NULL or a live session.
#[no_mangle]
pub unsafe extern "C" fn mm_session_cancel(session: *mut MMSession) {
    if !session.is_null() {
        (*session).inner.cancel();
    }
}

/// Fit a transform to manual control points; returns a response JSON string (never NULL).
///
/// # Safety
/// `request_json` must be NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mm_fit_control_points(request_json: *const c_char) -> *mut c_char {
    match c_str(request_json, "request_json") {
        Ok(req) => json_call(|| fit_control_points_json(req)),
        Err(e) => into_c_string(failed_json(&format!("Invalid request: {e}"))),
    }
}

/// `{"version": "0.1.0", "build": "release aarch64-ios"}`.
#[no_mangle]
pub extern "C" fn mm_version() -> *mut c_char {
    json_call(|| {
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
        serde_json::json!({
            "version": crate::VERSION,
            "build": format!("{profile} {}-{}", std::env::consts::ARCH, std::env::consts::OS),
        })
        .to_string()
    })
}

/// Free a string returned by this library. NULL is ignored.
///
/// # Safety
/// `s` must be NULL or a string returned by this library that was not freed yet.
#[no_mangle]
pub unsafe extern "C" fn mm_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}
