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
 * cached in the session, so the search continues where it left off. One
 * answer asks for at most "max_tiles_per_round" tiles (default 64); a wide
 * search (search_radius_m beyond 50 km, up to 2 000 km) takes one round per
 * chunk of the area, nearest first: keep answering until "ok" or "failed"
 * (the response's "search" object reports the progress), or cancel.
 */
char *mm_session_georeference(MMSession *session, const char *request_json);

/*
 * Estimate a search before running it (stateless, fast). Request:
 *   {"radius_m": 1000000, "gps": {"lat": -25.75, "lon": 28.19} | null,
 *    "hints": [{"lat","lon","radius_m"}], "max_tiles_per_round": 64, "scale_known": false,
 *    "image_size": [w, h], "scale_m_per_px": [lo, hi] | "scale_bar_m_per_px": [lo, hi]}
 * (every field optional; radius clamped to 1 000 ... 2 000 000 m, default 50 000; without
 * "image_size" a typical 300-dpi A3 sheet; for a map on a larger page its rectangle, as a
 * search's diagnostics.contours.map_area reports it; the scale ranges as a search's
 * diagnostics.scale_range reports them, a printed scale's or a scale bar's).
 * Response: {"status":"ok","radius_m","coarse_zoom","tiles","approx_bytes","approx_seconds",
 *   "approx_compute_seconds","approx_download_seconds","approx_megabytes","rounds","chunks",
 *   "search_work","search_work_wide","tiles_by_zoom":[{"z","tiles"}]} (see docs/ARCHITECTURE.md,
 * "Wide search", Estimate), or {"status":"failed","message"}. Never NULL.
 */
char *mm_search_estimate(const char *request_json);

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

/*
 * Find the sheet of paper (the map page) in a photo, for flattening. 8-bit
 * RGBA, rows bytes_per_row apart (copied). Returns
 * {"status":"ok","quad":[[x,y],[x,y],[x,y],[x,y]],"confidence":0..1,"coverage":0..1}
 * with the corners TL, TR, BR, BL in pixels of the given raster (coverage =
 * quad area / image area), or {"status":"failed","message":...} when no page
 * stands out from its surroundings. Never NULL.
 */
char *mm_detect_page(const uint8_t *rgba, uint32_t width, uint32_t height, uint32_t bytes_per_row);

/*
 * Dark map rendering (see docs/ARCHITECTURE.md, "Dark map"). A paper map is shown dark by
 * flattening its paper (lighting, shadows, paper tint) and inverting lightness in OKLab while
 * keeping each feature's hue, so ink becomes light and paper becomes the app's night paper.
 *
 * mm_dark_profile analyses an overview of the WHOLE map (e.g. the coarsest pyramid level or the
 * thumbnail, long side 256..2048 px; 8-bit RGBA, alpha ignored, rows bytes_per_row apart) and
 * returns the map's profile as JSON, which the app stores with the map:
 *   {"status":"ok","version":1,"ink":..,"dead":..,"gain":..,
 *    "paper":{"cols":C,"rows":R,"rgb":[r,g,b, ...]}}   (paper white per cell, sRGB 0..1, row-major)
 * plus, for a photo whose page edges show the table, "edge":{"left":[..],"right":[..],"top":[..],
 * "bottom":[..]} (band depth per edge as a fraction of the map's width or height, sampled along
 * the edge; rendered as the night paper), or {"status":"failed","message":...}. Never NULL.
 */
char *mm_dark_profile(const uint8_t *rgba, uint32_t width, uint32_t height, uint32_t bytes_per_row);

/* Opaque, immutable dark style built from a profile (its colour lookup table is precomputed).
 * Thread-safe: any number of threads may render with one style at once. */
typedef struct MMDarkStyle MMDarkStyle;

/* NULL when the JSON is not a valid profile. Free with mm_dark_style_free (NULL, a style freed
 * already and any pointer that is not a live style are ignored; a style must not be freed while a
 * render on it is running). */
MMDarkStyle *mm_dark_style_new(const char *profile_json);
void mm_dark_style_free(MMDarkStyle *style);

/*
 * Render one tile of the map dark, in place (8-bit RGBA, alpha untouched). The tile covers the
 * map's normalised rectangle [u0, u0 + du] x [v0, v0 + dv] (fractions of the full map's width and
 * height, so the paper grid lines up across tiles and pyramid levels; pixel centres are at
 * u0 + (x + 0.5) / width * du). Returns MM_OK, or MM_ERR_ARGUMENT for NULL pointers, empty or
 * inconsistent sizes and non-finite or non-positive rectangles.
 */
int32_t mm_dark_render(const MMDarkStyle *style, uint8_t *rgba, uint32_t width, uint32_t height,
                       uint32_t bytes_per_row, double u0, double v0, double du, double dv);

/*
 * ---------------------------------------------------------------------------------------------
 * Terrain: elevations, route profiles, 3D height grids, contour rendering and tile plans for the
 * explorer features (docs/design/explorer.md; JSON fields in docs/ARCHITECTURE.md, "C ABI",
 * Terrain). Separate from MMSession, whose DEM tiles serve one georeference search.
 * ---------------------------------------------------------------------------------------------
 */

/* Returned by an entry point whose implementation is not in this build yet (int functions; the
 * JSON functions answer {"status":"failed","message":"not implemented: <function>"}). */
#define MM_ERR_NOT_IMPLEMENTED (-4)

/*
 * Opaque store of decoded Terrarium tiles that the app feeds. Every mm_terrain_* function on one
 * store may run on any thread at any time, concurrently (CATiledLayer drawing threads, the 3D
 * builder, route statistics): the store locks internally and never blocks for long.
 *
 * It never downloads anything. A query that needs tiles the store neither holds nor has been told
 * are missing answers "status":"need_dem" with "need_tiles":[{"z","x","y"}, ...] (at most
 * "max_tiles", default 64, nearest the query's centre first) and still answers from what it holds
 * (partial results: null / NaN / background where elevations are unknown). The app adds those
 * tiles (or marks them missing) and asks again; "ok" means nothing more can be had.
 *
 * Memory: at most max_bytes of decoded elevations are held (each tile counts what it really takes,
 * 256 KiB as f32); past that, the least recently used tiles are dropped and count as unknown again
 * (a later query lists them in need_tiles). max_bytes 0 = 64 MiB; otherwise clamped to
 * 4 MiB ... 1 GiB. Missing marks are not counted (at most 100 000 are kept, oldest dropped).
 * Keeping tiles on disk (offline regions, trailhead caches) is the app's business: the store is a
 * cache in memory only.
 *
 * Zoom choice (every query): "max_zoom" (default 13, at most 15) and "min_zoom" (default
 * max_zoom - 3) bound the DEM zooms used. Each query wants the finest zoom it can use within them
 * (elevations and profiles: max_zoom; a grid: the coarsest zoom whose cell is at most half the
 * node spacing; a render: the coarsest whose cell is at most two output pixels), falls back to
 * coarser zooms where tiles of the wanted one are marked missing, and lists in need_tiles the
 * unknown tiles of the zoom it would use next. Responses report the "zoom" used (the finest one
 * that contributed).
 */
typedef struct MMTerrain MMTerrain;

/* NULL only on an internal error. Free with mm_terrain_free. */
MMTerrain *mm_terrain_new(uint64_t max_bytes);

/* NULL, a store freed already and any pointer that is not a live store are ignored. No call on
 * the store may still be running. */
void mm_terrain_free(MMTerrain *terrain);

/*
 * Add a Terrarium PNG tile (as downloaded; replaces a tile held, clears a missing mark). Returns
 * MM_OK, MM_ERR_DECODE (the tile is then marked missing), MM_ERR_ARGUMENT (NULL pointers, len 0,
 * z > 15 or x/y outside the zoom's 2^z range) or MM_ERR_PANIC.
 */
int32_t mm_terrain_add_tile(MMTerrain *terrain, uint8_t z, uint32_t x, uint32_t y, const uint8_t *png, size_t len);

/* Record that a tile is unavailable (offline, not downloadable): queries fall back to coarser
 * zooms instead of asking for it again. MM_OK or MM_ERR_ARGUMENT. */
int32_t mm_terrain_mark_missing(MMTerrain *terrain, uint8_t z, uint32_t x, uint32_t y);

/* Forget every missing mark (the network is back): queries list those tiles again. */
void mm_terrain_clear_missing(MMTerrain *terrain);

/* {"status":"ok","tiles":N,"missing":M,"bytes":B,"max_bytes":C,"evicted":E}. Never NULL. */
char *mm_terrain_stats(const MMTerrain *terrain);

/*
 * Elevations at points.
 * Request:  {"points":[[lat,lon], ...], "max_zoom":13, "min_zoom":null, "max_tiles":64}
 * Response: {"status":"ok"|"need_dem"|"failed","elevations_m":[e|null, ...],"zoom":13,
 *            "need_tiles":[...]}   (bilinear on the DEM; null where unknown)
 * Never NULL.
 */
char *mm_terrain_elevations(const MMTerrain *terrain, const char *request_json);

/*
 * Statistics and elevation profile along a polyline (a planned route, a GPX track, the recorded
 * trail). The line is densified ("spacing_m", default half a DEM cell of the zoom used), sampled
 * bilinearly, smoothed along its length (Gaussian, sigma "smoothing_m", default one DEM cell) and
 * its climbs counted with a hysteresis ("hysteresis_m", default 3: a rise counts once it exceeds
 * it, like the recorded track's ascent).
 * Request:  {"points":[[lat,lon], ...] (at least 1), "max_zoom":13, "min_zoom":null,
 *            "spacing_m":null, "smoothing_m":null, "hysteresis_m":3.0, "max_samples":1000,
 *            "max_tiles":64}
 * Response: {"status","distance_m","ascent_m","descent_m","min_m","max_m",
 *            "samples":[[distance_m, elevation_m|null], ...]   (at most max_samples, evenly spaced,
 *                                                                 first at 0, last at distance_m),
 *            "vertex_distance_m":[...], "vertex_elevation_m":[e|null, ...]   (one per input point),
 *            "zoom","missing_fraction","need_tiles"}
 * distance_m is the geodesic length through the points (haversine, mean Earth radius
 * 6 371 008.8 m); ascent_m/descent_m/min_m/max_m are null when no elevation is known at all.
 * Never NULL.
 */
char *mm_terrain_profile(const MMTerrain *terrain, const char *request_json);

/*
 * Heights on a regular grid over a Web Mercator rectangle (the 3D view's terrain mesh). heights
 * receives cols x rows floats (metres, not exaggerated), row-major, row 0 along max_y (north),
 * node (i, j) at (min_x + i (max_x - min_x) / (cols - 1), max_y - j (max_y - min_y) / (rows - 1));
 * NaN where unknown. cols and rows are 2 ... 4096 and cols x rows <= heights_len.
 * Request:  {"rect":[min_x, min_y, max_x, max_y], "max_zoom":13, "min_zoom":null, "max_tiles":64}
 * Response: {"status","zoom","min_m","max_m","missing_fraction","need_tiles"}
 * Never NULL.
 */
char *mm_terrain_grid(const MMTerrain *terrain, const char *request_json, float *heights, size_t heights_len,
                      uint32_t cols, uint32_t rows);

/*
 * Draw contour lines (index lines every "index_every" intervals, thicker) and optional hillshade
 * into an 8-bit RGBA buffer (sRGB, premultiplied alpha, R G B A byte order, rows bytes_per_row
 * apart; every pixel is written).
 * Request:
 *   {"space":"map"|"mercator",
 *    "rect":[x0, y0, x1, y1],   map: base-image pixels (y down) covered by the whole buffer, pixel
 *                               (i, j) centred at (x0 + (i + 0.5)(x1 - x0) / width,
 *                               y0 + (j + 0.5)(y1 - y0) / height); mercator: [min_x, min_y, max_x,
 *                               max_y] in Web Mercator metres, row 0 along max_y
 *    "pixel_to_mercator":[9 doubles], "warp":{...}|null   (space "map": the map's georeference, as
 *                               in the georeference response; the warp is applied exactly)
 *    "style":"dark"|"paper",
 *    "interval_m":null,         null: the smallest of 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000 m
 *                               at least 4 x scale_m_per_pt
 *    "scale_m_per_pt":null,     ground metres per point the interval and line widths are chosen for
 *                               (null: from the rect's centre); pass one value for every tile of a
 *                               level so tiles agree
 *    "index_every":5, "hillshade":0.0 (strength 0 ... 1), "line_scale":1.0 (buffer pixels per point),
 *    "background":[r, g, b, a]|null   (0 ... 255, straight alpha; null: the style's paper, opaque;
 *                               [0,0,0,0]: transparent, lines only),
 *    "max_zoom":13, "min_zoom":null, "max_tiles":64}
 * Response: {"status","interval_m","index_interval_m","zoom","missing_fraction","need_tiles"}
 * A rectangle drawn as one buffer or as any grid of tiles gives the same pixels. Areas without
 * elevations are left as background; below 0 m only the 0 m line (the coast) is drawn. Never NULL.
 */
char *mm_terrain_render(const MMTerrain *terrain, const char *request_json, uint8_t *rgba, uint32_t width,
                        uint32_t height, uint32_t bytes_per_row);

/*
 * Which terrain tiles cover some areas at some zooms, and roughly what they weigh (stateless,
 * fast): offline regions and the terrain cached around a located map ("trailhead").
 * Request:
 *   {"areas":[{"bbox":[south, west, north, east], "zooms":[8,9,10,11,12]},   (west > east: across
 *                                                                              the antimeridian)
 *             {"circle":[lat, lon, radius_m], "zooms":[9,10]},
 *             {"quad":[[lat,lon] x 4], "margin_m":3000, "zooms":[12,13]}],
 *    "trailhead":{"corners":[[lat,lon] x 4 TL, TR, BR, BL]} | null}   (adds the trailhead areas:
 *                                                                      docs/design/explorer.md)
 * Response: {"status":"ok","tiles":N,"bytes":B,"bytes_high":H,
 *            "zooms":[{"z","tiles","bytes","bytes_high","ranges":[[x0, x1, y0, y1], ...]}],
 *            "dropped":[{"z","tiles","reason"}]}
 * Tiles are the union over the areas per zoom; ranges are disjoint inclusive tile rectangles
 * covering exactly them. bytes uses the mean tile size of SRTM-only terrain, bytes_high that of
 * the high-resolution sources (US 3DEP, European DEMs: 2-3x larger at zoom 11-13).
 * Or {"status":"failed","message"}. Never NULL.
 */
char *mm_tile_plan(const char *request_json);

/* Library version, e.g. {"version":"0.1.0","build":"release aarch64-ios"}. */
char *mm_version(void);

/* Free a string returned by this library. NULL is ignored. */
void mm_string_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* MAPME_CORE_H */
