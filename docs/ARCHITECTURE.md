# MapMe architecture

MapMe puts your live GPS position on any map you give it: a PDF, an image or a
screenshot (photos of paper maps come later). It works out where the map is on Earth by
itself (georeferencing), then shows a red dot, your heading, your track and nearby points
of interest.

```
┌────────────────────────── iOS app (Swift) ──────────────────────────┐
│ SwiftUI screens · UIKit tiled map canvas · CoreLocation · Vision OCR │
│ PDFKit/CoreGraphics rendering · URLSession (DEM tiles, OSM POIs)     │
└───────────────┬──────────────────────────────────────────────────────┘
                │ C ABI (include/mapme_core.h), JSON in / JSON out
┌───────────────▼──────────── mapme-core (Rust) ───────────────────────┐
│ grid/label georeferencing · contour-vs-DEM matching · control-point   │
│ fitting · geodesy. Pure Rust, no platform deps, reused by Android.    │
└──────────────────────────────────────────────────────────────────────┘
```

## Why Rust + Swift (and not Go)

* **Swift/SwiftUI/UIKit for the UI.** Native UI is the snappiest option on iPhone.
  Any cross-platform UI layer adds a runtime and frame-time overhead.
* **Rust for the heavy lifting.** Georeferencing is numeric work (FFT
  correlation, image filtering, least squares). Rust compiles to tight native
  code with no garbage collector or runtime, links as a small static library,
  and builds for both iOS and Android, so the same core is reused for Android.
  Go (gomobile) ships a GC runtime, makes bigger binaries and has a weaker iOS
  toolchain story.

## Coordinate conventions (everyone must follow these)

* **Image pixels**: origin top-left, x right, y down, unit = 1 pixel of the
  *base image* (the full-resolution raster the app renders once at import,
  `MapDocument.pixelWidth × pixelHeight`). Pixel centres are at integer + 0.5
  only where noted; transforms map continuous pixel coordinates.
* **Ground frame**: Web Mercator (EPSG:3857) metres, `x = R·λ`,
  `y = R·ln(tan(π/4 + φ/2))`, `R = 6378137`. North is +y, so it is *up*, the
  opposite of image y. Ground distance ≈ Mercator distance × cos(φ).
* **Georeference** = 3×3 homography `pixel_to_mercator` (row-major, 9 doubles):
  `[X, Y, W]ᵀ = H · [x, y, 1]ᵀ`, `merc = (X/W, Y/W)`. Affine fits have the last
  row `[0, 0, 1]`.
* **DEM**: Terrarium PNG tiles (AWS Open Data, no key):
  `https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png`,
  256×256, elevation = `R·256 + G + B/256 − 32768` metres, slippy-map XYZ
  tiling in Web Mercator. Zoom 11 (≈76 m Mercator px) is used for the coarse
  search and zoom 13 (≈19 m) for refinement and verification.

## Georeferencing pipeline (mapme-core)

The request carries the OCR text the platform found (Apple Vision on iOS,
ML Kit on Android), the GPS fix if any, and options. Methods are tried in this
order:

1. **Grid / graticule** (`grid.rs`, `labels.rs`). Parse coordinate labels
   (DMS `25°49'00"S`, decimal degrees, and UTM eastings/northings with the zone
   taken from GPS). Detect long straight grid lines (near-horizontal or
   near-vertical, at most ±5°) with a line-projection Hough. Pair labels with lines,
   then robustly fit (RANSAC) value = a + b·position for each axis, enforcing
   equal spacing and monotonic values. Grid-line intersections become ground
   control points, and an affine (≥3 non-collinear GCPs) or homography (≥4,
   reserved for photos) is fit. Typical accuracy is a few metres. If labels
   exist but no lines are found, label centres are used directly as a coarse
   fit, which is then refined with contours when a DEM is available.
2. **Contour matching against a DEM** (`contour.rs`, `skeleton.rs`,
   `search.rs`, `refine.rs`). This needs a GPS position (the map is assumed to
   lie within `search_radius_m` of you):
   * *Contour mask*: classify pixels by colour (default: the brown/tan/orange
     contour hues used by most topo maps; the user can override by picking a
     contour colour).
   * *Coarse search*: build a doubled-angle orientation field from the mask
     (structure tensor of the blurred mask; contour lines run perpendicular to
     the terrain gradient) and the same field from DEM gradients. Then do an
     FFT-based normalised complex cross-correlation over translation, for every
     candidate scale (geometric steps of about 4%) and rotation (0° by default,
     or a sweep if `allow_rotation`). This finds where the map's contour pattern
     matches the real terrain. The prototype found the Grootkloof map
     unambiguously in a 70×80 km window: peak 0.77 vs 0.28 for the next distinct
     location, about 40 m error.
   * *Refinement* (the "3D match"): skeletonise the mask, split it into contour
     segments at junctions, and sample DEM elevations along every segment. If
     the georeference is right, each contour segment has constant elevation.
     Gauss-Newton / Levenberg–Marquardt with a robust loss minimises the
     within-segment elevation variance over similarity (4 parameters), then
     affine (6), with DEM gradients giving the Jacobian. The prototype
     converged to about 14 m max error over the whole sheet (the SRTM noise
     floor) from starts up to 470 m off, in under a second.
   * *Confidence*: coarse peak ratio (best vs best distinct location) +
     refined robust cost + inlier fraction. If the map is not in the search
     area, the result is a low score and `failed` status, never a confident
     wrong answer.
3. **Manual control points** (`control_points.rs`): the user taps matching
   points on the map and on an Apple satellite map (or "I'm here" at their
   GPS position). 2 points give a similarity, 3 an affine, 4+ an affine
   (homography for photos).

The core is pull-based for DEM data. When it needs tiles it does not have, it
answers `need_dem` with the tile list. The app downloads them (cached on disk),
adds them, and calls again. Tiles that cannot be fetched (offline) are marked
missing so the core proceeds without them.

## C ABI (core/include/mapme_core.h)

```c
typedef struct MMSession MMSession;
MMSession *mm_session_new(const uint8_t *rgba, uint32_t width, uint32_t height, uint32_t bytes_per_row);
void       mm_session_free(MMSession *s);
int32_t    mm_session_add_dem_tile(MMSession *s, uint8_t z, uint32_t x, uint32_t y, const uint8_t *png, size_t len);
int32_t    mm_session_mark_dem_tile_missing(MMSession *s, uint8_t z, uint32_t x, uint32_t y);
char      *mm_session_georeference(MMSession *s, const char *request_json);
float      mm_session_progress(const MMSession *s);   /* 0..1, thread-safe */
void       mm_session_cancel(MMSession *s);           /* thread-safe */
char      *mm_fit_control_points(const char *request_json);
char      *mm_version(void);
void       mm_string_free(char *s);
```

All strings are UTF-8 JSON and owned by the caller after return; free them with
`mm_string_free`. Functions never panic across the FFI boundary: panics are
caught and turned into `{"status":"failed","message":...}`. A session is not
thread-safe except for `progress`/`cancel`. RGBA input is 8-bit, alpha
ignored, rows `bytes_per_row` apart, copied by the session.

### Georeference request

```json
{
  "gps": {"lat": -25.84, "lon": 27.40, "accuracy_m": 8.0},
  "ocr": [{"text": "25°49'00\"S", "bbox": [116, 449, 36, 185], "confidence": 0.92}],
  "dpi": 300.0,
  "contour_rgb": null,
  "search_radius_m": 20000,
  "allow_rotation": false,
  "methods": ["grid", "contours"],
  "verify": false
}
```

Every field is optional; defaults are as shown, except `gps`, `dpi` and
`contour_rgb`, which default to `null`, and `ocr`, which defaults to `[]`.
`bbox` is `[x, y, w, h]` in base-image pixels. `dpi` is the raster's pixels per
inch when known (PDF render), which turns an OCR'd scale like `1:25 000` into a
scale prior.

### Georeference response

```json
{
  "status": "ok",
  "method": "grid",
  "pixel_to_mercator": [2.1180, 0.0, 3046439.16, 0.0, -2.1173, -2975246.24, 0.0, 0.0, 1.0],
  "confidence": 0.97,
  "rms_m": 1.1,
  "scale_m_per_px": 1.906,
  "rotation_deg": 0.0,
  "corners": [[-25.8074, 27.3666], [-25.8074, 27.4334], [-25.8923, 27.4334], [-25.8923, 27.3666]],
  "need_tiles": [],
  "pois": [{"name": "KWAMI LODGE", "kind": "place", "elevation_m": null, "x": 2944.0, "y": 4499.0}],
  "message": "Located from 5 latitude and 3 longitude grid labels (±1 m).",
  "diagnostics": {},
  "timings_ms": {"grid": 85.0}
}
```

* `status` is `"ok" | "need_dem" | "failed"`. With `need_dem`, `need_tiles` is
  `[{"z":11,"x":1179,"y":1175}, …]`; add them and call again with the same
  request.
* `method` is `"grid" | "labels" | "contours" | null`.
* `scale_m_per_px` is in *ground* metres.
* `rotation_deg` is the clockwise angle of map-up from true north.
* `corners` are the image corners TL, TR, BR, BL as `[lat, lon]`.
* `pois` are map labels classified as place names, peaks or spot heights, in
  base-image pixels.

### Control point request/response

```json
{"points": [{"x": 878, "y": 541, "lat": -25.8167, "lon": 27.3833}], "model": "auto"}
→ {"status": "ok", "pixel_to_mercator": [...9], "rms_m": 2.0, "residuals_m": [1.2, ...], "model": "affine"}
```

## Repository layout

```
core/                 Rust crate mapme-core (lib + staticlib + C header)
  src/geo.rs          Web Mercator, haversine, bearing, UTM, Affine/Homography fits
  src/image.rs        GrayF32 / Mask buffers, area resize, Gaussian blur, Sobel, bilinear
  src/fft.rs          2-D complex FFT helpers on rustfft
  src/dem.rs          Terrarium decode, tile math, DemStore, DemMosaic sampling
  src/contour.rs      contour colour model + mask extraction
  src/skeleton.rs     thinning, junction removal, segment extraction, sampling
  src/search.rs       coarse orientation-field FFT search
  src/refine.rs       contour-constancy least-squares refinement + scoring
  src/labels.rs       coordinate / scale / POI label parsing
  src/grid.rs         grid-line detection, label↔line association, GCP fit
  src/control_points.rs manual calibration fits
  src/georef.rs       Session + orchestrator (request → response)
  src/ffi.rs          C ABI
  include/mapme_core.h
  tests/              integration tests on fixtures/grootkloof
fixtures/grootkloof/  the real map (PDF + extracted JPEG), DEM tiles, OCR fixture, truth
ios/                  XcodeGen project (project.yml), app, unit tests, UI tests
scripts/              build-core-ios.sh (XCFramework), package-ipa.sh, ...
.github/workflows/    core.yml (Linux), ios.yml (macOS build, test, IPA)
```

## iOS app structure (ios/MapMe)

* `App/` has `MapMeApp` (entry, `onOpenURL` import) and `AppModel` (root
  `@Observable` state: library, settings, location, track recorder).
* `Core/` has `MapMeCore.swift`, a thin Swift wrapper over the C ABI
  (`GeorefSession`, `fitControlPoints`), plus `CoreTypes.swift` with Codable
  request/response types.
* `Geo/` has `Geo.swift`: `LatLon`, `WebMercator`, `Homography` math,
  distance/bearing, coordinate formatting (DD, DMS, UTM).
* `Model/` has `MapDocument` (Codable metadata + georeference + POIs),
  `MapLibrary` (on-disk store), `Track` (+ GPX export) and `Waypoint`.
* `Services/` has `LocationService` (CoreLocation location + heading, mock mode
  for tests), `TrackRecorder`, `MapImporter` (PDF/image → base raster → tile
  pyramid → thumbnail), `TileStore`, `OCRService` (Vision, multi-orientation),
  `DEMService` (Terrarium fetch + disk cache), `GeoreferenceService` (OCR → core
  session → DEM loop, with progress stages), `POIService` (map labels +
  waypoints + OpenStreetMap Overpass).
* `Views/` holds the Library, Import progress, the Map screen (`MapCanvasUIView`:
  CATiledLayer pyramid, pan/pinch/rotate gestures, camera with north-up /
  heading-up / free modes, overlays for the GPS dot, accuracy, heading cone,
  dotted track, POIs and waypoints), HUD, Nearby sheet, Calibration (MapKit) and
  Settings.

Map files live in `Application Support/Maps/<uuid>/` as `meta.json`, the original
source, `tiles/{level}/{col}_{row}.jpg` (512 px tiles, level 0 = full
resolution) and `thumb.jpg`.

## Performance budget

* Import + render + tile pyramid for an A3 300-dpi PDF: < 3 s on an A15.
* Grid georeference (OCR + core): < 3 s. Contour georeference: < 4 s plus DEM
  download.
* Map interaction at 60/120 fps, because tiles are drawn off the main thread
  by CATiledLayer and overlays are cheap CAShapeLayers.
* Release core: `opt-level=3`, LTO, `codegen-units=1`, `panic=unwind` (needed
  to catch panics at the FFI boundary).
