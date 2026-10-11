/*
 * mapme_core.h: C ABI of the MapMe georeferencing core (Rust).
 *
 * All strings are NUL-terminated UTF-8 JSON. Strings returned by the library
 * are owned by the caller and must be released with mm_string_free().
 * See docs/ARCHITECTURE.md for the request/response JSON schemas.
 *
 * No function panics or aborts across this boundary: internal errors become
 * {"status":"failed","message":...} responses or negative return codes.
 * One georeference call may run per session at a time; mm_session_progress
 * and mm_session_cancel may be called from any thread meanwhile.
 */
#ifndef MAPME_CORE_H
#define MAPME_CORE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque georeferencing session bound to one map raster. */
typedef struct MMSession MMSession;

/*
 * Create a session from an 8-bit RGBA raster (alpha ignored). Rows are
 * bytes_per_row apart. The pixels are copied; the caller may free them after
 * the call. Returns NULL on invalid input.
 */
MMSession *mm_session_new(const uint8_t *rgba, uint32_t width, uint32_t height, uint32_t bytes_per_row);

/* Destroy a session. NULL is ignored. */
void mm_session_free(MMSession *session);

/* Return codes of the DEM tile functions. */
#define MM_OK 0
#define MM_ERR_DECODE (-1)   /* PNG could not be decoded; the tile now counts as missing */
#define MM_ERR_ARGUMENT (-2) /* NULL pointer, empty data or tile coordinates out of range */
#define MM_ERR_PANIC (-3)    /* internal error */

/*
 * Add a Terrarium-encoded DEM tile (PNG bytes as downloaded from
 * https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png).
 * Returns MM_OK, or a negative MM_ERR_* code.
 */
int32_t mm_session_add_dem_tile(MMSession *session, uint8_t z, uint32_t x, uint32_t y, const uint8_t *png, size_t len);

/*
 * Record that a DEM tile is unavailable (e.g. offline) so it is not requested
 * again; georeferencing proceeds without it. Returns MM_OK or MM_ERR_ARGUMENT.
 */
int32_t mm_session_mark_dem_tile_missing(MMSession *session, uint8_t z, uint32_t x, uint32_t y);

/*
 * Run georeferencing (blocking; call off the main thread). Returns a response
 * JSON string, never NULL. Status "need_dem" means add (or mark missing) the
 * listed tiles and call again with the same request; work done so far is
 * cached in the session.
 */
char *mm_session_georeference(MMSession *session, const char *request_json);

/* Progress of the running georeference call, 0..1. Safe from any thread. */
float mm_session_progress(const MMSession *session);

/*
 * Ask the running georeference call to stop early; it returns
 * {"status":"failed","message":"cancelled"}. Requested while no call runs
 * (e.g. between need_dem rounds), it cancels the next call. The request is
 * cleared when a call returns "ok" or "failed". Safe from any thread.
 */
void mm_session_cancel(MMSession *session);

/*
 * Fit a transform from manual control points. Stateless. Returns
 * {"status","pixel_to_mercator","rms_m","residuals_m","model","scale_m_per_px",
 * "rotation_deg","message"}, never NULL.
 */
char *mm_fit_control_points(const char *request_json);

/* Library version, e.g. {"version":"0.1.0","build":"release aarch64-ios"}. */
char *mm_version(void);

/* Free a string returned by this library. NULL is ignored. */
void mm_string_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* MAPME_CORE_H */
