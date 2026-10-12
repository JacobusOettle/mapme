# MapMe architecture

MapMe puts your live GPS position on any map you give it: a PDF, an image, a
screenshot or a photo of a paper map. It works out where the map is on Earth by
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
  search within 50 km of the GPS fix and around hints, zoom 10 (≈153 m) and
  zoom 9 (≈306 m) for the wide search beyond (see *Wide search*), and zoom 13
  (≈19 m) for refinement and verification. Mercator cells shrink on the
  ground by cos(latitude), so past 60° each coarse zoom is one coarser (two
  past 75.5°, three past 82.8°: `wide::zoom_at_latitude`), keeping the
  ground detail and the tile count of the low latitudes.

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
   control points, and an affine (≥3 non-collinear GCPs) is fit, or for
   photos a homography (see *Photo mode*). Typical accuracy is a few metres.
   If labels exist but no lines are found, label centres are used directly as
   a coarse fit, which is then refined with contours when a DEM is available.
2. **Contour matching against a DEM** (`contour.rs`, `skeleton.rs`,
   `search.rs`, `refine.rs`). This needs somewhere to look: the GPS position
   (the map is assumed to lie within `search_radius_m` of you, 50 km by
   default, up to 2 000 km) and/or hints (places named on the map that the app
   geocoded, the map's own coordinate labels). Hint areas are searched first,
   then the near disc around the fix, then the wide area beyond 50 km chunk by
   chunk (see *Wide search*):
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

### Photo mode (`"photo": true`)

A photo of a paper map is first flattened by the app from the page borders
(VisionKit document scanner / Vision rectangle detection, plus a corner editor)
and imported like any image. Two things survive that flattening and would
break a scan-style georeference: the page corners are never exact, so a
residual perspective remains (a 2.5 % corner error moves the map corners by
~100–300 m on a 1:25 000 sheet), and the output size comes from the quad's edge
lengths, so the aspect ratio is off by up to ~15 % at a 15–20° camera tilt.
Lighting is uneven (shadow, vignetting, colour cast) and thin lines are blurred
into the paper. With `photo: true` the request:

* **normalises the image** (`contour::normalize_illumination`): the paper
  colour is estimated per channel on a 48-block grid (each block's 90th
  percentile), closed with neighbouring blocks (ink-filled blocks borrow the
  paper level of their neighbours), padded by linear extrapolation, smoothed
  and divided out: paper becomes white and neutral in shadow and light. A
  contrast curve `v^1.5` restores the colour of blurred line pixels. Blocks that
  are not paper (other chromaticity or much darker) and connected to the image
  border, i.e. the table a flattening with corners outside the page leaves, are
  blanked together with the ring of blocks holding the page edge. This copy is
  used for grid lines and contours; the original stays in the session.
* **grid**: lines are detected on a sharpened copy (unsharp mask σ 1.5) both
  directly and against their local background (`grid::flatten_local_background`,
  a two-sided top-hat that keeps thin strokes and drops tints and edges), and
  merged; only grey ink counts (brown contours are dense and, blurred, as dark
  as a grid line). Each line is traced knot by knot (24 knots; dynamic
  programming over local line candidates, bounded turn per knot), so it may
  bend where the paper does not lie flat (`GridLine::bend`,
  `diagnostics.grid.max_bend_px`), and GCPs are the bent lines' intersections.
  Line families may converge (±2.5° around their median), per-axis line fits
  tolerate 6 % of the labels' spacing and may be projective in the grid value
  (through ≥ 4 labelled lines), and the conformality check accepts the
  scanner's aspect error. With ≥ 6 GCPs a homography is fitted too and used
  when its RMS is ≤ 75 % of the affine's and ≥ 0.5 m lower
  (`diagnostics.grid.model = "homography"`). A photo grid fit is accepted up
  to 60 m RMS (folds bend the grid). `GridResult::lines` keeps the identified
  lines with their values.
* **contours**: the mask is the luminance *valleys* of the normalised photo
  (across the line a pixel is no brighter than both neighbours and ≥ 5 levels
  darker than the paper beyond them) that are not black ink and lie on the
  relaxed default (or picked) contour colour, by a vote of ≥ 2 pixels of
  their 3×3 neighbourhood (the hue of faint lines is noisy). Unlike a colour
  threshold, this keeps dense contours ~2.5 px apart separate instead of
  blurring them into blobs whose skeleton follows no contour. The coarse
  search is unchanged (similarity templates tolerate the residual perspective
  and aspect error; the right location keeps a peak ratio ≈ 1.4–2.2). The
  refinement (`refine::RefineOptions { photo: true }`) runs similarity as
  usual, then an affine coarse-to-fine from pyramid level 2 that may be
  anisotropic (≤ 1.35), started from the similarity and from it stretched by
  ±3.5 % along x against y (the plausible result with the lowest cost wins:
  from the similarity alone it can stall in a near-isotropic minimum), then a
  projective stage from level 1:
  8 parameters about the samples' centroid in coordinates normalised by their
  half-diagonal, merc = rm + (a·u + b·v + c, d·u + e·v + f) / (1 + g·u + h·v),
  analytic Jacobian, the same robust variable-projection LM. It is kept when
  it lowers the robust cost by ≥ 3 % and stays sane (|g|, |h| ≤ 0.15, no fold
  over the samples' extent, plausible local scale and anisotropy);
  `diagnostics.refined.projective` tells. `refine::sample_fits` gives every
  contour sample's elevation residual and slope under a fitted transform.

* **warp** (`warp.rs`, the `warp` of the response): paper that does not lie
  flat (fold creases, curl) bends the map in ways no homography follows, so
  on top of the fitted homography `H` a smooth offset lattice `B` is
  estimated (20 nodes along the long side, ~115 px apart on a phone photo)
  by regularised least squares over the node offsets:
  * *grid lines*: every identified grid line is traced densely in the
    normalised photo (`warp::trace_printed_line`: thin grey crossings every
    2 px within ±40 px of its detected course, picked by a running median
    with a shrinking tolerance, then followed beyond where it was seen by
    extrapolation, so a fold that bends it ~25 px away is kept); every 8 px
    of it says that `H + B` puts it on its value (across the line: Mercator
    y for a parallel, x for a meridian, the gradient of northing or easting
    for a UTM line). The outermost other long straight line on each side
    (the map frame) must stay straight too: on a graticule it is a parallel
    or meridian of unknown value (a free offset), otherwise a straight line
    anywhere (free offset and slope); in the contour path all detected lines
    count that way. Lines that are short, ragged (jumping between features)
    or beside an identified line are left out.
  * *contours* (contour path; the grid path too when contours were requested:
    it then answers `need_dem` for the zoom-13 tiles under the sheet): the
    DEM elevation along every contour segment must be constant under `H + B`,
    with the segment's level a free variable eliminated in closed form
    (variable projection), like the refinement does, since it absorbs any
    offset a whole segment shares. Gauss–Newton with Levenberg damping and
    soft-L1 weights (3 m), from DEM pyramid level 1 to the finest.
  * *regularisation*: the bending energy (second differences of each offset
    component, expected ~3 % of a cell per cell) and a weak pull to zero
    (150 m). The normal equations are dense (2 × 300 unknowns) and solved by
    Cholesky.

  A warp is kept only if data it was not fitted to agree with it: each
  identified grid line, predicted by the warp fitted without it, must end up
  at most 85 % of its RMS distance under `H` alone (and ≥ 1 px closer), and
  each half of the contour segments (split by parity) must lose ≥ 6 % of its
  robust cost under the warp fitted without it. It must stay fold-free
  (pixel derivative ≤ 0.35× that of `H`) and plausible (≤ 300 m where the
  map's lines and contours are). Flat sheets get none (the synthetic photos
  of `tests/photo.rs`: held-out lines get worse, contour halves gain 2–5 %).
  Grid lines alone (offline, no DEM) do not pass on the real folded sheet:
  its 5 × 3 graticule lines are too far apart to predict each other through
  the folds (22 m held out vs 19 m from `H`), so it then gets no warp.
  `diagnostics.warp` reports `source`, `accepted` or `rejected` (why),
  `cols`/`rows`, `n_lines`, `n_straight_lines`, `n_line_points`,
  `n_segments`, `n_samples`, `rms_before_m`/`rms_after_m` (grid lines from
  their value under `H` and `H + B`), `holdout_before_m`/`holdout_rms_m`
  (the same for each line left out), `contour_cost_before_m`/`_after_m`
  (robust mean elevation residual), `contour_holdout_gain`,
  `median_seg_std_m`, `max_offset_m`, `derivative_ratio`, `iterations` and
  `ms`. With a warp, `rms_m` is the held-out line RMS (grid) or the contour
  estimate from the warped segments' spread.

The response's `pixel_to_mercator` is then a true homography (last row not
`[0, 0, 1]`); without a warp `scale_m_per_px` and `rotation_deg` are taken
from its local affine approximation at the image centre and `corners` from
the homography itself (with one, from the full model). On synthetic photos
of the Grootkloof sheet (`tests/photo.rs`: camera tilted ~15°, shadow
×0.55…1, warm cast, blur σ 1.2, noise; flattened from corners 0.5 % and
2.5 % off, inwards and outwards) the grid path is within ~2 m and the contour
path within ~10–30 m over the whole map frame (an affine from the same GCPs
is off by 20 m and 115 m), and no warp is kept. On the real phone photo of
the printed sheet, folded in four (`tests/realphoto.rs`, flattened from the
page corners and from corners 0.5–1 % off; median / p90 / max over the 670
truth points), `pixel_to_mercator` alone is at the flat-model floor (the best
single homography: 37 / 66 / 181 m): the grid path 31.5–31.8 / 88–90 /
293–296 m, the contour path 36–40 / 87–98 / 288–348 m. With the warp the
grid path is at 11.3–12.7 / 48–53 / 140–145 m and the contour path at
14.4–16.0 / 60–67 / 145–157 m, for ~0.45–0.6 s of fit (plus the contour
products, ~0.4 s, in the grid path and the line detection, ~0.5 s, in the
contour path). Inside the outermost graticule lines the grid path is at
~9 / 21 / 59 m; the tail comes from the strip below the last parallel
(legend and title: no grid line, few contours) where the paper curls by
100–170 m and only the frame's shape is known, and from the truth itself,
which is off the printed graticule by up to ~7 px (~30 m) near the creases.

The core is pull-based for DEM data. When it needs tiles it does not have, it
answers `need_dem` with the tile list (at most `max_tiles_per_round`, default
64; the next call asks for the rest). The app downloads them (cached on disk),
adds them, and calls again. Tiles that cannot be fetched (offline) are marked
missing so the core proceeds without them. Every result is cached in the
session, so each call continues where the last one stopped; a wide search
takes about one round per chunk, and the app keeps answering until `ok` or
`failed` (or the user cancels).

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
char      *mm_search_estimate(const char *request_json);  /* stateless */
char      *mm_detect_page(const uint8_t *rgba, uint32_t width, uint32_t height, uint32_t bytes_per_row);
char      *mm_dark_profile(const uint8_t *rgba, uint32_t width, uint32_t height, uint32_t bytes_per_row);
MMDarkStyle *mm_dark_style_new(const char *profile_json);   /* immutable, thread-safe */
void       mm_dark_style_free(MMDarkStyle *style);
int32_t    mm_dark_render(const MMDarkStyle *style, uint8_t *rgba, uint32_t width, uint32_t height,
                          uint32_t bytes_per_row, double u0, double v0, double du, double dv);
char      *mm_version(void);
void       mm_string_free(char *s);
```

All strings are UTF-8 JSON and owned by the caller after return; free them with
`mm_string_free`. Functions never panic across the FFI boundary: panics are
caught and turned into `{"status":"failed","message":...}`. A session is not
thread-safe except for `progress`/`cancel`. RGBA input is 8-bit, alpha
ignored, rows `bytes_per_row` apart, copied by the session.

`mm_search_estimate` says what a search will fetch and how long it takes,
before running it (stateless, microseconds; see *Wide search*).

`mm_detect_page` finds the sheet of paper in a photo (`page.rs`, stateless,
~20–60 ms for 12–24 MP): `{"status":"ok","quad":[[x,y]×4],"confidence","coverage"}`
with the corners TL, TR, BR, BL in the given raster's pixels, or
`{"status":"failed","message"}` when nothing stands out or the page fills the
frame (an already-flat scan). It works on a ≤ 800 px copy: brightness relative
to the local paper level times closeness to the paper colour, the largest solid
region away from the border, its best quadrilateral; then each side is refined
on full-resolution profiles (a printed frame just inside the edge is not taken
for it) with a robust line fit, and corners are the lines' intersections. On
the real photo (`fixtures/grootkloof/photo`) the corners are within 4 px
(0.12 % of the diagonal), and within 0.25 % when the photo is rotated, scaled,
cropped, darkened, blurred or re-tilted.

### Georeference request

```json
{
  "gps": {"lat": -25.84, "lon": 27.40, "accuracy_m": 8.0},
  "ocr": [{"text": "25°49'00\"S", "bbox": [116, 449, 36, 185], "confidence": 0.92}],
  "dpi": 300.0,
  "contour_rgb": null,
  "search_radius_m": 50000,
  "allow_rotation": false,
  "methods": ["grid", "contours"],
  "verify": false,
  "photo": false,
  "hints": [{"lat": -25.80, "lon": 27.36, "radius_m": 15000, "source": "place", "name": "Grootkloof"}],
  "max_tiles_per_round": 64
}
```

Every field is optional; defaults are as shown, except `gps`, `dpi` and
`contour_rgb`, which default to `null`, and `ocr` and `hints`, which default
to `[]`. `search_radius_m` is clamped to 1 000 … 2 000 000 (ground metres);
up to 50 000 it is the near disc searched at zoom 11, beyond that the rest is
the wide search. `hints` are candidate areas (`lat`, `lon` required;
`radius_m` defaults to 15 000 and is clamped to 1 000 … 50 000; `source` is
`"place"` (a name from `hint_queries` the app geocoded; the default, also for
unknown values), `"label"` (coordinates read on the map) or `"user"` (an area
the user picked); `name` is optional). With a GPS fix, hints farther than
`search_radius_m` from it are ignored (`diagnostics.hints_ignored`); without
one the hints alone say where to search. `max_tiles_per_round` (clamped to
1 … 4 096) caps every need_dem answer.
`bbox` is `[x, y, w, h]` in base-image pixels. `dpi` is the raster's pixels per
inch when known (PDF render), which turns an OCR'd scale like `1:25 000` into a
scale prior. `photo` marks a photo of a paper map that the app flattened by its
page borders (see *Photo mode*): the image is normalised for lighting and the
grid and contour fits may end in a homography. OCR boxes are in the flattened
image's pixels, like everything else.

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
* `hint_queries` (every response to a request with OCR text): names on the
  map worth geocoding into `hints`, best first, at most 12: the title and the
  series name (from furniture lines such as "MCSA MAGALIESBERG SHEET 3 OF 10"),
  then place, peak and water names by text size. Legend entries (columns of
  short labels, or legend, furniture and route-note words only, also when OCR
  merged them: "DryWaterCourse", "NOENTRYSIGN"), scale bars, coordinates, spot
  heights, credits ("GIS V1 Sept 2017 - …") and the same name read twice or
  partially are left out. Grootkloof: `["GROOTKLOOF", "HAMERKOP",
  "MAGALIESBERG", "SUGARBUSH HILL COUNTRY COTTAGE", "KWAMI LODGE"]`.
* `search` (whenever the contour search ran): its progress, for the app's
  "Searched 320 of 1 000 km" UI:
  `{"radius_m", "searched_radius_m", "coarse_zoom", "tiles_planned",
  "tiles_fetched", "hints_tried", "hints_planned", "stage"}`. `radius_m` is
  the clamped search radius (0 without a fix), `searched_radius_m` the radius
  around the fix searched completely so far (the near disc, then the nearest
  point of the wide chunks not searched yet; chunks are a few hundred km
  wide, so it grows in steps; it ends below `radius_m` when the map is too
  small to be searched that far), `coarse_zoom` that of the current coarse
  stage (11, 10 or 9; coarser past 60° of latitude), `tiles_planned`/`tiles_fetched` the coarse tiles of the whole
  search (hint and near areas, wide chunks; candidate checks come on top),
  `stage` one of `"hints" | "near" | "wide" | "refine" | "done"`.
* A failed contour search says what was searched: "No match within 1 000 km
  of you (searched 1 073 terrain tiles; tried 3 places named on the map).
  Search a wider area, pick the contour colour, or calibrate manually."
  (thousands grouped with a no-break space, U+00A0; "Search a wider area" is
  left out at 2 000 km). A map too small for the wide search's zooms: "No
  match within 150 km of you (…). This map shows too small an area to be
  searched farther than 150 km away. Pick the contour colour, or calibrate
  manually." With a labels' band the parenthesis adds "first along 26°51'S,
  printed on the map". Without a fix or hints: "… contour matching needs your
  location or a place to search …"; with no terrain downloaded at all: "Terrain
  data for the search area couldn't be downloaded …".
* `diagnostics.search_areas` lists every hint and near area searched (centre,
  radius, tiles, candidates, outcome), `diagnostics.label_hints` the hints read
  from coordinate labels and `diagnostics.wide` the wide search (zoom, chunks
  searched, `band_chunks`, the radius covered, candidates re-checked and
  refined with their numbers); `diagnostics.selected.source` is `"wide"` for
  a wide match, and `selected.recheck` the local re-check of a hint or near
  match that needed one; `diagnostics.dem_tiles_held` the DEM tiles the
  session holds (256 KB each).
* `pixel_to_mercator` is affine (last row `[0, 0, 1]`) except in photo mode,
  where it may be a full homography; always evaluate it projectively.
* `scale_m_per_px` is in *ground* metres.
* `rotation_deg` is the clockwise angle of map-up from true north.
* `corners` are the image corners TL, TR, BR, BL as `[lat, lon]`.
* `pois` are map labels classified as place names, peaks or spot heights, in
  base-image pixels. A spot height with fewer digits than
  nearly all others on the sheet and under half their median is dropped as a
  misread (e.g. "112" for a "1412" cut by a grid line).
* `warp` (photo mode only; absent or `null` otherwise) is a smooth correction
  for paper that does not lie flat (fold creases, curl). No single homography
  fits a folded sheet: on `fixtures/grootkloof/photo` (a phone photo of the
  printed sheet, folded in four) the best one is off by ~37 m median and
  ~180 m at the corners. With a warp the full model is

  ```
  merc(p) = H(p) + B(p)
  ```

  where `H` is `pixel_to_mercator` and `B(p)` is the bilinear interpolation of
  per-node offsets on a regular lattice over the image:

  ```json
  "warp": {
    "cols": 17, "rows": 23,
    "origin": [0.0, 0.0],
    "spacing": [120.5, 117.1],
    "offsets_m": [dx00, dy00, dx01, dy01, ...]
  }
  ```

  Node `(i, j)` (column `i`, row `j`) sits at pixel
  `origin + (i·spacing[0], j·spacing[1])`. Its offset is
  `offsets_m[2·(j·cols + i)]` (x) and `[… + 1]` (y), in Web-Mercator metres,
  the units `H` outputs. To evaluate `B(p)`, take the lattice coordinates
  `u = (p.x − origin[0]) / spacing[0]` and `v = (p.y − origin[1]) / spacing[1]`,
  clamp them to `[0, cols − 1] × [0, rows − 1]` (the warp is constant beyond
  the lattice), and interpolate the four surrounding nodes bilinearly. The
  lattice covers the whole image: `origin = (0, 0)` and
  `spacing = (width / (cols − 1), height / (rows − 1))`. The core keeps the
  warp fold-free: its pixel derivative stays below 0.35× that of `H`, so the
  inverse `mercator → pixel` converges by fixed-point iteration,
  `p₀ = H⁻¹(m)`, `pₖ₊₁ = H⁻¹(m − B(pₖ))` (6 iterations reach < 0.01 px).
  `rms_m`, `corners`, `scale_m_per_px` and `rotation_deg` include the warp.
  `pixel_to_mercator` alone is still the best homography, so a client that
  ignores `warp` gets the flat-model answer. `diagnostics.warp` says how the
  warp was fitted (`source`: `"grid"` or `"contours"`, constraint counts, RMS
  before and after, largest offset).

### Control point request/response

```json
{"points": [{"x": 878, "y": 541, "lat": -25.8167, "lon": 27.3833}], "model": "auto"}
→ {"status": "ok", "pixel_to_mercator": [...9], "rms_m": 2.0, "residuals_m": [1.2, ...], "model": "affine"}
```

## Wide search

The user's first search covers 50 km around the GPS fix (the near disc, as
before but with a larger default radius). When the map is not found there the
app can search further, up to 2 000 km (`search_radius_m`), and offer places
named on the map (`hint_queries`, geocoded into `hints`). A 1 000 km radius at
zoom 11 would be ~10 000 tiles (1 GB, 660 Mpx); the wide search instead works
on coarser tiles, chunk by chunk, and verifies candidates on finer ones.

**Order** (`georef.rs`, `georef/wide_search.rs`, `wide.rs`, `hints.rs`):

1. *Hints* (user, then label, then place hints, each in the order given; at
   most 8; a hint whose disc an earlier one covers is skipped), each searched
   like the near area: the zoom-11 coarse search within the hint's radius,
   then the zoom-13 refinement of its best candidates. The map's coordinate
   labels become a `label` hint when the grid fit cannot use them: one
   latitude and one longitude label (their medians), or a corner label
   holding both; the map centre lies along the offset from that labelled
   point to the image centre at the unknown scale, so the hint is the disc
   around that segment (scale range from an OCR'd "1:25 000" and the dpi, or
   0.4–12 m/px), plus 15 % of the offset for a map that is not north up and 2 km.
   Labels of one axis alone (a latitude label and the scale, say) put the map
   centre in a band of latitudes (or longitudes) the same way
   (`hints::label_band`, `diagnostics.label_band`): the wide search takes the
   band's placements first (a strip a few km wide: a few chunks and tiles
   instead of the disc), then the rest of the disc. The band never skips
   anything: one misread label ("26°51'" for "25°51'") must not hide a map
   next to the user, so the near disc is searched as always and a wide search
   that finds nothing in the band covers the whole radius.
2. *Near*: the disc of `min(search_radius_m, 50 km)` around the fix at zoom 11
   (`wide::near_zoom`; up to 50 km this is the whole search), the inner 20 km first: where the
   map usually is, a third of the cost (the 50 km disc's coarse search takes
   ~2 s on 64 tiles, the 20 km one ~0.8 s), so a user near the sheet waits no
   longer than with the old 20 km default; without a confident match there,
   the whole disc follows. In a hint or near area the fit gap among its three
   refined candidates counts as search evidence (`refine::select_refined`); a
   match that only this gap makes acceptable must also stand out locally
   like a wide match (its zoom-11 re-check below peaks at ≥ 2.2; see
   *Adversarial checks*: without it the phone photo was placed 137 and 145 km
   off at confidence 0.82 and 0.999).
3. *Wide* (`search_radius_m` > 50 km): the rest of the disc, north up, in
   chunks nearest first (`wide::plan_wide`): the coarse tile grid is cut into
   blocks of `s × s` tiles of placements (where the map centre may lie), each
   searched on a mosaic of those tiles plus the tiles the map's extent reaches
   (one ring at zoom 9/10), at most `floor(√max_tiles_per_round)` (3…8) tiles a
   side: 6 × 6 placements on an 8 × 8 mosaic by default. Chunks are ordered by
   the ground distance of their nearest point to the fix, so the search grows
   in rings; tiles a centre in the annulus cannot reach are not fetched.
   Distances count across the antimeridian (`search::nearest_in_box`), so a
   fix in Fiji reaches maps on the far side of 180°. The
   coarse search of a chunk (`search::Region`: its box, half-open so chunks
   never share a placement, minus the band's strip when that was searched
   first, and the annulus 50 km … R) keeps its six most
   significant candidates, of which the three with the highest correlation go
   on. Each is re-checked by a zoom-11 search 4 km around it, scale ±10 %
   (2–9 tiles); one that stands out locally (re-check peak ratio ≥ 2.2,
   correlation ≥ 0.2) is refined at zoom 13 and **accepted only when
   confident** (`refine::confidence` ≥ 0.7, with the re-check peak ratio as
   search evidence, where the near area accepts from 0.5) and its re-check
   correlation is ≥ 1.1 × that of every other re-checked candidate ≥ 2 km
   away. The search stops at the first accepted match. A chunk's tiles are
   released once no later chunk needs them, re-check tiles once used and a
   rejected candidate's zoom-13 tiles once refined (its outcome is cached;
   the near and hint areas' tiles stay), so memory stays at about two
   chunks' worth however wide the search (`diagnostics.dem_tiles_held`,
   `Session::dem_tiles_held`); one need_dem round fetches a chunk's re-check
   tiles together with the next chunk's tiles (not past the band's chunks).

Matches below 0.7 from a hint or the near area (accepted from 0.5) wait while
the remaining areas are searched; the most confident is reported at the end.

**Coarse zoom** (`wide::wide_reach`): zoom 10 up to 150 km, zoom 9 beyond (for
latitude: one coarser past 60°, `wide::zoom_at_latitude`; a 1 000 km search
from Tromsø plans 1 463 tiles instead of 5 601, from 84°N 392). A map too
small for zoom 9 (no scale of the range gives it a template of 24 cells: less
than ~6.6 km on its short side) is searched at zoom 10 out to 150 km only, one
too small for zoom 10 (under ~3.3 km) not beyond the near disc: zoom 11 over
1 000 km would be ~18 000 tiles. The answer then says how far was searched
("No match within 50 km of you (…). This map shows too small an area to be
searched farther than 50 km away."; `search.searched_radius_m` stops there).
At zoom 9 the 1:25 000 sheets (6.7 km wide) are just above that floor and
were found everywhere (below). Scales whose template would be smaller than
24 cells at the wide zoom are not searched there
(`diagnostics.wide.min_scale_m_per_px`).

**Measurements** (`tests/wide_measure.rs`, ignored tests on real Terrarium
tiles, cached; a 4-core 2.1 GHz Xeon shared with other jobs):

*Tile weights* (60 tiles per zoom spread over 2 300 km around the
Magaliesberg, land and the sea off Durban): mean 76.8 KB at zoom 8, 68.9 KB at
9, 45.9 KB at 10, 26.4 KB at 11 (sea tiles are 0.7 KB), 20.0 KB at 12, 15.9 KB at
13; decoding ~1.0–1.3 ms per tile. Every tile the measured searches fetched
(1 860 at zoom 9 from Windhoek and Cape Town to Durban, 586 at zoom 13 under
refinements, which are in rugged terrain) weighs more: 86.6 KB at zoom 9,
22.6 KB at 12, 26.3 KB at 13. The estimate uses those (with 68.9 KB a 1 000 km
search from Cape Town was estimated at 75 MB; it fetched 90.0 MB, now
estimated 89.6 MB).

*Direct coarse windows* (the Grootkloof scan, one mosaic, default scale range
0.4–12 m/px / with the scale prior 1.43–2.65 m/px; truth rank among distinct
candidates, its correlation and peak ratio):

| zoom | fix (distance) | R km | tiles | search s | truth | NCC | ratio | next NCC | +RSS MB |
|---|---|---|---|---|---|---|---|---|---|
| 11 | sheet (0 km) | 50 | 100 | 2.45 / 0.81 | #1 | 0.71 | 2.22 | 0.35 | 273 |
| 11 | Pretoria (80 km) | 110 | 289 | 11.9 / 3.1 | #1 | 0.71 | 2.15 | 0.42 | 727 |
| 10 | Pretoria (80 km) | 110 | 81 | 1.67 / 0.75 | #1 | 0.66 | 2.18 | 0.41 | 130 |
| 9 | Pretoria (80 km) | 110 | 25 | 0.48 / 0.11 | #1 | 0.69 | 1.77 | 0.31 | – |
| 9 | Polokwane (300 km) | 330 | 132 | 3.2 / 1.4 | #1 | 0.66 | 1.51 | 0.41 | 298 |
| 9 | Bloemfontein (383 km) | 413 | 196 | 7.9 / 2.9 | #1 | 0.69 | 1.59 | 0.32 | 618 |
| 9 | Durban (572 km) | 602 | 380 | 20.5 / 9.8 | #1 | 0.71 | 1.11 | 0.48 | 1 379 |

The true location stays the top candidate at every zoom and size, but the
peak ratio (significance over the best other location) decays with the area
(2.2 → 1.1), so over a wide area it is no evidence and an un-chunked window
costs 1.4 GB: hence chunks, and a re-check at zoom 11.

*Candidate checks* (the top ten candidates by correlation of the z10 window
from Pretoria and the z9 windows from Bloemfontein and Durban, each re-checked
at zoom 11 and the best five refined at zoom 13; five cases: the Grootkloof
scan, a synthetic photo of it, the real phone photo of it, Castle Gorge and
Mhlabatini):

* the truth is the #1 candidate by correlation in all 15 windows (coarse NCC
  0.52–0.75; the best false 0.29–0.51, the highest over the sea's bathymetry,
  which zoom 9 has and zoom 11 does not: those re-checks find nothing);
* re-check: truth NCC 0.45–0.65 at a local peak ratio of **2.56–5.39**; false
  candidates NCC ≤ 0.36 at **≤ 1.87** (2–9 tiles, 0.02 s each);
* refinement: the truth fits at 1.000 in every case; false candidates at 0.000
  for the scans, but photo refinement (homography, more freedom) fits a false
  candidate on the real photo at **0.635** (1 093 km away, re-check ratio
  0.83). Accepting a wide match on a refinement alone, or on its margin over
  junk candidates, would be a confident wrong answer; the re-check ratio
  separates them.

*The implemented search* (`wide_session`, `wide_session_real_photo`: the
session API, the Grootkloof scan with its "1:25 000" OCR'd unless noted, and
the real phone photo flattened by its page corners (photo mode, no OCR);
tiles through a ~2 MB/s proxy, partly cached; peak RSS of the whole test
process, which also holds its own copies of the map: 423 MB for a search
that finds the sheet in the inner 20 km, 563 MB for the full 50 km disc):

| from (distance) | R km | zoom | outcome | rounds | tiles z9/10 + z11 + z13 | MB | total s (core) | peak RSS MB |
|---|---|---|---|---|---|---|---|---|
| on the sheet | 50 | 11 | ok, inner 20 km | 3 | 16 + 12 | 0.9 | 1.0 (1.0) | 423 |
| Rustenburg (26 km) | 50 | 11 | ok, whole disc | 4 | 64 + 28 | 2.9 | 4.2 (3.4) | 563 |
| Pretoria (80 km) | 120 | 10 | ok, chunk 1 of 4 | 7 | 52 + 57 + 38 | 5.0 | 5.6 (5.4) | 638 |
| Polokwane (300 km) | 350 | 9 | ok, chunk 3 of 4 | 10 | 100 + 78 + 57 | 9.7 | 7.5 (6.3) | 622 |
| Bloemfontein (383 km) | 450 | 9 | ok, chunk 3 of 9 | 10 | 149 + 96 + 54 | 13.2 | 9.6 (7.5) | 666 |
| Durban (572 km) | 650 | 9 | ok, chunk 9 of 15 | 16 | 327 + 162 + 45 | 35.3 | 13.7 (11.4) | 734 |
| Durban, no scale | 650 | 9 | ok, chunk 9 of 15 | 17 | 353 + 193 + 30 | 38.1 | 25.6 (25.1) | 752 |
| Windhoek (~1 060 km) | 1 000 | 9 | failed, all 25 chunks | 31 | 675 + 317 + 56 | 55.5 | 34.6 (25.6) | 722 |
| photo, Bloemfontein | 450 | 9 | ok, chunk 3 of 9 | 10 | 152 + 100 + 51 | 13.4 | 18.4 (16.4) | 648 |
| photo, Durban | 650 | 9 | ok, chunk 9 of 15 | 16 | 333 + 164 + 34 | 36.1 | 31.1 (26.8) | 691 |

The wide search adds ~100–190 MB over the 50 km near search (one chunk's
mosaic, its search buffers and the zoom-9 tiles of the next ring), however
wide; the 1 000 km search that cannot succeed releases its 675 zoom-9 tiles
as it goes. The photo is accepted at a re-check ratio of 2.76–2.80 (NCC
margin 1.33–1.99), confidence 1.0, with the same transform as from on the
sheet. The estimate for these (scale unknown) is conservative: 1 000 km,
78.8 MB in 29 rounds and ~121 s on an iPhone vs 55.5 MB in 31 rounds and
25.6 s of core time here with the scale known; from Cape Town (the photo,
which fails) 89.6 MB in 36 rounds vs 90.0 MB in 39 rounds, ~80 s of iPhone
compute (53 s here) vs 61 s here.

Every scan match is the sheet within 11.6 m (max over the frame). Hermetic tests
(`tests/wide.rs`) cover the same on fixture tiles and synthetic terrain served
on demand: a 1:50 000 synthetic sheet 120 km from the fix found by the zoom-10
chunks (10 rounds, ~14 s), the same sheet 260 km away with R = 200 km an
honest failure after all six zoom-9 chunks (two false candidates passed the
re-check gate there, at ratios 2.54 and 2.27, and refined to 0.0: the gate is
a cost filter, the refinement and the confidence decide), and the same sheet
150 km away with one latitude label and its scale: the near disc is searched
(it fails), then the band's chunks (4.9 km of latitude: 2 chunks, 7 zoom-9
tiles instead of ~100) find it in 9 rounds.

*Adversarial checks* (review of the wide search; `tests/wide.rs`,
`tests/realphoto.rs`, and the ignored network runs above with
`WIDE_RUNS` / `WIDE_PHOTO_RUNS`):

* **Confident wrong answers on real terrain.** The phone photo (photo mode,
  no OCR: the default scale range) from eight places 70–170 km from the
  sheet with R = 50 km (Pilanesberg, Johannesburg, Suikerbosrand,
  Thabazimbi, Bronkhorstspruit, the western Magaliesberg, the Waterberg,
  Harrismith) was accepted twice at a wrong place: Thabazimbi at 0.822 (in
  the inner 20 km, so the old 20 km default did it too) and the Waterberg at
  0.999 (in the 50 km disc), 137 and 145 km from the sheet. Both refinements
  fitted wrong terrain at 0.967 / 0.999 (scale 4× and 2.4× off, turned 5°
  and 10°) with coarse peak ratios ~1.0: the confidence came from the fit
  gap among the three refined candidates (`select_refined`), which counts as
  search evidence. A hint or near match lifted to `ACCEPT` that way now also
  needs its zoom-11 re-check (4 km, ±10 % scale) to peak at ≥ 2.2 its
  runner-up, like a wide match: those two re-check at 1.27 and 1.37 and
  fail honestly; true matches from 12, 26 and 45 km (and every gate test)
  are unchanged. Their tiles are in `core/tests/data/dem-thabazimbi` and
  `dem-waterberg` (`realphoto_no_confident_wrong_answer_on_real_terrain`).
  The scan (with and without its scale label) gave no false match from any
  of the eight. The photo from Harrismith (R = 250 km) and Cape Town (R =
  1 000 km, 31 chunks, 39 rounds, 90.0 MB, 61 s of core time) fails
  honestly; no wide candidate even reached the refinement.
* **Decoys** (synthetic terrain holding a fraction α of the sheet's terrain
  170 km away, the true place outside the search, through a place hint and
  the wide search): α = 0.5 and 0.65 fail honestly (at 0.65 the decoy passes
  the re-check gate at ratio 4.5 but refines to 0.007); from α ≈ 0.8 the
  decoy fits at ±20 m and is accepted (it is then the same landscape).
* **Chunk boundaries**: a sheet centred on the corner where four zoom-9
  chunks meet, on a chunk edge, and 49.3 / 50.7 km from the fix (near disc /
  wide search) is found each time.
* **Cancel and resume**: five cancels in the middle of chunk searches; each
  call continues where the last stopped (no zoom-9 tile asked for twice, no
  chunk searched twice); a repeated call after `ok` or `failed` answers the
  same in ~0.02 s without asking for tiles.
* **Tile cap**: `max_tiles_per_round` 1 (a 70 km search: 218 rounds, one
  tile each, 0.036 s of core time per call), 0 (read as 1) and 10¹²
  (4 096: one round per area).
* **Memory**: the zoom-9 tiles of a 200 km search are all released by its
  end; at most 205 tiles (51 MB) were held at once.
* **Hints**: a hint with a 5 000 km radius (searched at 50 km) no longer
  hides the hints after it as "covered"; the radius is clamped first.

**Estimate** (`mm_search_estimate`, `wide::estimate`): the plan the search
would make (hint areas, at most 8 after ordering, and the near disc at zoom
11, the wide chunks; zooms for latitude) for a
typical 300-dpi A3 sheet at 0.4–12 m/px, plus measured allowances for
candidate checks (12 zoom-11 tiles per chunk, 16 zoom-13 tiles per
refinement, two refinements per area); bytes from the tile weights above;
compute from the measured 0.47 s per million mosaic cells over the default
scale range (0.17 s with `"scale_known": true`, a scale label read) and 0.6 s
per refinement, times 1.5 for an iPhone; download at 2 MB/s plus 0.5 s per
round. From Pretoria, scale unknown:

| R km | zoom | tiles | MB | rounds | chunks | ~s (compute + download) |
|---|---|---|---|---|---|---|
| 20 | 11 | 67 | 1.8 | 2 | 0 | 5 (3.4 + 1.9) |
| 50 | 11 | 128 | 3.5 | 3 | 0 | 11 (8.2 + 3.2) |
| 100 | 10 | 231 | 7.5 | 6 | 2 | 19 (12.6 + 6.7) |
| 250 | 9 | 261 | 11.3 | 8 | 4 | 24 (14.0 + 9.7) |
| 500 | 9 | 465 | 25.7 | 13 | 9 | 45 (25.3 + 19.3) |
| 1 000 | 9 | 1 197 | 78.8 | 29 | 25 | 121 (67.5 + 53.9) |
| 2 000 | 9 | 4 025 | 282.9 | 92 | 88 | 424 (236.3 + 187.5) |

(For 50 km and more the near disc counts twice: the inner 20 km first, the
whole disc if the map is not there. A whole search is planned; one that
finds the map stops early, so these are upper bounds.)

Known limits: the wide search is north up (a rotation sweep costs 36 times
as much; the hint and near areas honour `allow_rotation`); maps narrower than
~6.6 km are only searched to 150 km (zoom 10) and those under ~3.3 km only in
the near and hint areas (the answer says so); at zoom 9 scales whose
template would be under 24 cells are left out without a word (a phone
screenshot of a detailed map, say: `diagnostics.wide.min_scale_m_per_px`);
across the antimeridian the wide chunks reach maps on the far side, but the
near disc and maps straddling 180° are not searched there; far north and
south the per-latitude zoom is set at the fix, so a 1 000 km disc from
69.6°N still plans 1.8× the tiles of one from 26°S (its poleward part has
smaller tiles); the estimate assumes a typical sheet (a small-scale map adds
margin tiles, a scale label makes the search ~3× faster, a photo ~15 %
slower).

## Dark map

The app shows maps dark by default (Settings → Map appearance: Dark / Paper). A plain colour
inversion turns a topographic map muddy: yellow roads and pale green fills go dark brown, and a
photo's shading and paper tint invert into blotches. The core renders it instead
(`core/src/dark.rs`, C ABI `mm_dark_profile`, `mm_dark_style_new`, `mm_dark_render`):

1. **Profile** (once per map, from an overview of the whole map, long side ≤ 2048 px; a larger
   one is box-downsampled first):
   * *paper grid*: `cols = round(w / B)` × `rows = round(h / B)` cells (at least 1 × 1) of
     `B = max(8, long side / 32)` px, cell `i` covering pixels `[i·w/cols, (i+1)·w/cols)` so its
     centre is exactly at the sampling centre below; per cell and channel the 90th percentile
     (the paper white there); a 3 × 3 maximum filter (cells full of ink borrow the paper of
     their neighbours) then a 3 × 3 box blur, edges replicated; floored at 0.2; rounded to 1e-4.
     Stored as `cols × rows` sRGB triples; sampled bilinearly in normalised map coordinates with
     cell centres at `((i + 0.5) / cols, (j + 0.5) / rows)`, clamped at the edges. The grid
     position is quantised to 1/4096 of a cell, so every tiling of the map computes the same
     paper for the same map position.
   * *digital map* (a PDF's render, an exported image): one exact colour, its background, covers
     at least 5 % of the overview's pixels and shows up in at least a quarter of the grid's cells
     (the print's white: 29–33 % of its pixels at any overview scale; a photo's noise never
     repeats a colour that often: the real photo's commonest covers 0.3 %, the synthetic photo's
     0.04 %; a smooth, noise-free lighting gradient repeats colours, but each along a narrow
     band). Its paper is flat: one cell, the per-channel maximum of the cells' 90th percentiles.
     A solid fill wider than a cell or two (a sea, a lake, a forest tint) is then never taken for
     paper: with the grid it faded into the night paper away from its shore, with a glowing band
     along it (a pale sea's bare pixels at L′ 0.25 … 0.44, now all 0.41).
   * *flat field*: `x = min(rgb / paper(u, v), 1)` per channel (sRGB-encoded values). White
     paper becomes exactly white wherever it is, in shadow or not.
   * *print or photo*: on the flat-fielded overview in OKLab, the paper noise `dead` (below);
     a digital map, or one with `dead < 0.035`, is a print: `ink` 0.12, `gain` 1, no flare and
     no edge bands (a PDF's full-bleed content is never cropped). Neither estimate is reliable
     on a print: its darkest 0.7 % of pixels are not always black ink (a sparse map's are paper:
     "ink" 0.65, whose flare removal turned an orange track salmon; the Grootkloof print's give
     0.12 at 1/3 scale but 0.21 at 1/4, the overview the app uses), and its ink chroma depends
     on the scale and on what the ink is printed over (gain 1.0 at 1/3, 1.16 at 1/4, 1.7 with a
     sea under it). Here `dead` is 0.015 for the print, 0.059 for the real photo and 0.069 for
     the synthetic one; the rest of the profile is for photos.
   * a photo's darkest ink on the flat-fielded overview: `ink = clamp(P0.7 %(L), 0.12, 0.65)`
     (0.38 for the phone photo of the print).
   * *flare*: a photo's blacks are lifted by veiling light (lens flare, the camera's tone
     curve), which also washes out its colours. It is removed in linear light before anything
     else: `φ = max(ink³ − 0.12³, 0) / (1 − 0.12³)` (0.053 for the real photo),
     `lin′ = clamp((lin − φ) / (1 − φ), 0, 1)` per channel, after which the photo's darkest ink
     is at L 0.12 like a print's. (Stretching L between `ink` and paper instead left the
     photo's contours near white: half of them neutral grey, the hierarchy flat.)
   * on the flare-free values: paper noise from the pixels above the 55th percentile of L,
     `dead = clamp(1 − median + 3 · 1.4826 · MAD, 0.015, 0.12)` (paper grain and JPEG noise stay
     paper: 0.015 print, 0.062 photo); `gain = clamp(0.12 / median C, 1, 1.8)` over coloured
     ink pixels (`t ≥ 0.5`, `C ≥ 0.025`; 1 when fewer than 200), so a photo's washed-out colours
     come back. 0.12 is a print's typical ink chroma: the print's coloured ink has a median C of
     0.135 (gain 1), a synthetic photo of it (lighting, blur, noise) gain 1.11, the real phone
     photo gain 1.8 (its `t ≥ 0.5` pixels are mostly black text; the grey gate below keeps
     greys grey at any gain).
   * *edge bands* (photos only): where the flattened page's corners include a sliver of the
     table (the real photo: wood along both long edges, 1.2 % of the image, which rendered as a
     glowing orange frame).
     On the flat-fielded overview a pixel is paper when `t < 0.2` and `C · gain < 0.05`; per
     scanline across each edge the band's depth is the distance from the border to the first run
     of 3 paper pixels, capped at 3 % of the scanline; smoothed along the edge by a running median
     of 9 then a running maximum of 5 (mirrored at the ends, so corner scanlines do not spread
     the cap), plus one pixel where it is not 0 (the blurred border between table and paper);
     binned to at most 256 values per edge (each the largest depth within half a bin, so the
     linear interpolation between bins never falls below a scanline's own depth), as fractions
     of the map's width (`left`, `right`) or height (`top`, `bottom`), rounded up to 1e-4.
     Real photo: up to 3.2 % deep, 1.76 % of the map; synthetic photo 0.02 %; print: none.
   * JSON: `{"status":"ok","version":1,"ink","dead","gain","paper":{"cols","rows","rgb":[…]}}`
     plus `"edge":{"left","right","top","bottom"}` when there are edge bands; numbers with 4
     decimals (110 bytes for the print's flat paper, 23 KB for the photo's 25 × 33 cells); ~200 ms
     for the print's 1169 × 1653 overview (one pass over the pixels, and a sort of them for the
     digital test; a photo takes a second pass with the flare removed: ~150 ms for its
     850 × 1116 overview).
2. **Render** each pixel of the flat-fielded tile, flare removed (OKLab `L, a, b`,
   `C = |(a, b)|`):
   * `t = clamp(((1 − L) − dead) / ((1 − min(ink, 0.12)) − dead), 0, 1)` – how much ink, by
     lightness;
   * `gate = smoothstep(0.012, 0.03, C · clamp(L / 0.25, 0, 1))` – grey stays grey (no
     amplified colour casts); near black, where OKLab chroma is unstable, a colour must be
     proportionally stronger (JPEG noise around black text, e.g. sRGB (0, 0, 4) with C 0.034,
     would otherwise come out light blue or pink);
   * `s = clamp((C · gain · gate − 0.07) / 0.08, 0, 1)` – saturated colour is a feature even when
     light (yellow roads, routes, water lines), while pale fills (C < 0.07) stay background;
   * `sw = 0.45 + (0.8 − 0.45) · smoothstep(0.52, 0.64, S)`, `S` the HSV saturation
     `(max − min) / max` of the sRGB of `(L, gain · a, gain · b)` (clipped to the gamut) – a
     full-strength spot ink (S ≥ 0.62 on the print: yellow and orange roads, the red route, the
     cyan gate, blue water) is lifted fully, a tint (a light ink screened with white, S 0.47–0.55:
     the vegetation hatch and dots, the boundary's pink band) only to a mid tone. With one weight
     for both, the vegetation became acid lime as bright as the roads (8.8:1), and the boundary
     band the same salmon as the route (ΔE 0.025). A threshold on lightness instead would also
     dim the cyan gate symbol, which has the hatch's L and C;
   * `f = max(t, sw · s)`; `L′ = Lp + (0.90 − Lp) · f^0.78`, `Lp` = OKLab L of the night paper
     sRGB (0.13, 0.13, 0.14) (`Theme.uiNightPaper`);
   * `w = clamp(4 f, 0, 1)`; `(a′, b′) = w · 1.1 · gain · gate · (a, b) + (1 − w) · (ap, bp)`
     (the night paper's slight cool tint on paper, the feature's hue on ink);
   * back to sRGB; out-of-gamut colours lose chroma (same L and hue, bisection on the chroma
     scale) rather than being clipped per channel;
   * pixels in a photo's edge bands (centre `(u, v)` with `u < left(v)`, `1 − u < right(v)`,
     `v < top(u)` or `1 − v < bottom(u)`) are the night paper.

   On the print (contrast on #212124): paper becomes the night paper (#212124) exactly, black
   ink light grey #DEDEDE (L 0.90, 11.9:1), contours calm tan (#836545, 3.0:1, as on paper),
   blue water pastel (#98BDFF) and dashed streams mid blue (#488EDB, 4.7:1), the yellow road
   #C5C500 and the orange road #FFA63A at L′ 0.8 (8.7:1, 8.2:1), the red route salmon #FF9F90
   (8.1:1) while the boundary's pink band goes deep red #CB4C50 (3.6:1, ΔE 0.21 from the
   route), the cyan gate #00CCE3 (8.2:1), the vegetation hatch moss #749500 (L′ 0.62, 4.6:1,
   below the roads and the route), pale green and blue fills dark (L′ < 0.4, ~1.5:1). On the real
   photo the flare removal brings the ink to the print's tones: inside the page mean L 0.327
   (0.343 with the L stretch) and 5.6 % of the pixels above L 0.55 (8.5 %); contours tan rather
   than white, the yellow road and the route stronger, the table's slivers gone.

   The style precomputes the render as a 65³ lookup table over flat-fielded, flare-free sRGB
   (trilinear; 16-byte nodes, 4.4 MB, built in ~35 ms on 4 threads), plus the corner of the
   cube where every node is the night paper (x ≥ 63/64 for a print, lower for a photo), which
   skips the interpolation with the same bytes. A photo's flare removal runs before the table as
   a per-channel curve (1025 samples, linear in between): inside the table its kink at `φ` bent
   the interpolated colours (OKLab ΔE up to 0.042). Against the exact formula, on random pixels
   of the print the table is within 0.62/255 per channel for 99 % of pixels (mean 0.03/255) and
   within OKLab ΔE 0.0095 everywhere; on the real photo within 1.3/255 for 99 % (mean 0.09/255)
   and ΔE 0.031 (its chroma gain of 1.8 and the tint weight make the formula steeper there; a
   512 px tile's bytes are within 4 of the exact render for 99.9 %). The largest per-channel
   differences (up to ~22/255) are where the gamut mapping pins a channel at 0 (the B of a bright
   orange), where sRGB encoding is steepest and ΔE is ~0.003. A 512 px tile of the print takes
   ~6 ms on one 2.1 GHz Xeon core (a photo tile at the page's corner, with the flare curve and
   the edge bands, ~8 ms); rendering in place, the alpha untouched. Tiles render on
   CATiledLayer's threads and are cached as JPEG in `tiles-dark-v2/` (generated on first view,
   and in the background after import). The library card uses `thumb-dark.jpg`.

   Checked in `core/tests/dark.rs`: a 1024 px crop of the print, and the real photo's top-right
   corner across its edge bands, rendered as one tile, 2 × 2 or 3 × 3 tiles give identical
   bytes; the half-size crop rendered over the same rectangle is within 0.7/255 (mean) of the
   half-size render. After rendering, the paper is even: std of L over paper pixels 0.0594 →
   0.0095 on the real (folded) photo, 0.0869 → 0.0072 on the synthetic photo with its one-sided
   shadow (night paper L 0.249). `print_inks_keep_their_hierarchy` prints and checks the
   print's inks above; the real photo has no bright orange left at its edges and stays calm
   (mean L ≤ 0.335). The print profiled at 1/2, 1/3, 1/4 and 1/6 scale gets the same flat white
   paper, `ink` 0.12, `gain` 1, no edge bands; a pale or a mid-blue sea printed over 40 % of it
   renders its bare pixels at one L′ (0.41, 0.51); a sparse print (0.5 % ink) keeps its orange
   track orange. Degenerate overviews (blank, black, one colour, 8 × 8, 8 × 2048, noise) profile
   without panics (smaller than 8 px a side: `"status":"failed"`); random valid profiles render
   random tiles, rectangles far outside the map and strides with padding without panics.
   `mm_dark_style_free` ignores NULL, a style freed already and any pointer that is not a live
   style (the core keeps the set of live styles). `dark_dump` (ignored) writes before/after
   images of the print, the real photo and a synthetic photo.

   Known limits, accepted: pale point symbols (the police station's pale cyan, the place of
   worship's light grey, the legend's pale vegetation disc) keep their paper contrast (1.4–1.9:1)
   and read as faint marks; lifting them would lift the pale fills too. On the phone photo the
   rivers' chroma (~0.014 after the flat field) is at the level of the residual colour cast on
   greys, so the grey gate keeps most river pixels grey; a laxer gate gave casts on text and no
   clear gain. In direct sun a dark map is less legible than paper (glare adds the same light to
   both colours, which hurts the darker contours and text most): the app's Paper appearance
   stays one setting away. A *photo* of a map with a solid fill wider than a cell or two (a sea)
   still takes the fill for paper (its shading cannot be told from the fill's tint by the 90th
   percentile); a photo whose paper is clipped to pure white over a quarter of the page passes
   the digital test and is not flattened; a photo with paper as clean as a print's
   (`dead < 0.035`, e.g. a document camera scan) is treated as a print (no flare removal, no
   chroma gain), which suits a scan whose paper the camera already whitened.

   Tried and not adopted: trading lightness for chroma in the gamut mapping (pure red keeps
   C 0.21 at L 0.68) turns the map's reds into the app's own trail and position-puck red;
   `L′` exponent 0.70 (brightens contours that should recede); a per-map exponent from the ink
   tones (the flare removal already brings the photo to the print's tones); a finer paper grid
   for photos (`long side / 64`: crease band std of L 0.025 → 0.018, invisible at 1:1, for a
   3× larger profile, twice the profiling time, and solid tints wider than 3 cells taken for
   paper); a laxer grey gate on prints (little visible gain, risks colour fringes from JPEG
   noise around text).

## Repository layout

```
core/                 Rust crate mapme-core (lib + staticlib + C header)
  src/geo.rs          Web Mercator, haversine, bearing, UTM, Affine/Homography fits
  src/image.rs        GrayF32 / Mask buffers, area resize, Gaussian blur, Sobel, bilinear,
                      perspective warp, unsharp mask
  src/fft.rs          2-D complex FFT helpers on rustfft
  src/dem.rs          Terrarium decode, tile math, DemStore, DemMosaic sampling
  src/contour.rs      contour colour model + mask extraction, photo illumination
                      normalisation and hysteresis masks
  src/skeleton.rs     thinning, junction removal, segment extraction, sampling
  src/search.rs       coarse orientation-field FFT search
  src/refine.rs       contour-constancy least-squares refinement (similarity, affine,
                      projective for photos) + scoring
  src/warp.rs         photo warp: offset lattice for paper that is not flat, fitted to
                      traced grid lines and contours, held-out checks, inverse
  src/page.rs         page (paper sheet) finder for photos: mm_detect_page
  src/dark.rs         dark map: paper grid + flat field, OKLab tone map, 65³ LUT, tile render
  src/labels.rs       coordinate / scale / POI label parsing
  src/hints.rs        search hints: label hints from coordinate labels, hint_queries
  src/wide.rs         wide search planning: coarse zoom, chunks, mm_search_estimate
  src/grid.rs         grid-line detection, label↔line association, GCP fit
  src/control_points.rs manual calibration fits
  src/georef.rs       Session + orchestrator (request → response): hint areas, near area
  src/georef/wide_search.rs  the wide search: chunks, zoom-11 re-checks, acceptance
  src/ffi.rs          C ABI
  include/mapme_core.h
  tests/              integration tests on fixtures/grootkloof (photo.rs: synthetic
                      photos of the sheet, flattened from imperfect corners;
                      realphoto.rs: the real phone photo of the folded sheet;
                      warp.rs: the warp contract and its accuracy on that photo;
                      dark.rs: dark map rendering; wide.rs: hints, hint_queries,
                      the chunked wide search on synthetic terrain;
                      wide_measure.rs: ignored network measurements)
fixtures/grootkloof/  the real map (PDF + extracted JPEG), DEM tiles, OCR fixture, truth
ios/                  XcodeGen project (project.yml), app, unit tests, UI tests
scripts/              build-core-ios.sh (XCFramework), package-ipa.sh, ...
.github/workflows/    core.yml (Linux), ios.yml (macOS build, test, IPA)
```

## iOS app structure (ios/MapMe)

* `App/` has `MapMeApp` (entry, `onOpenURL` import) and `AppModel` (root
  `@Observable` state: library, settings, location, track recorder).
* `Core/` has `MapMeCore.swift`, a thin Swift wrapper over the C ABI
  (`GeorefSession`, `fitControlPoints`, `detectPage`, `darkProfile` and the
  thread-safe `DarkStyle`), plus `CoreTypes.swift` with Codable
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
* `Services/` also has the photo path: `PhotoQuadDetector` (Vision document
  segmentation / rectangles and the core's `mm_detect_page` on the same
  downscaled photo; the core's page is used when Vision returns the whole frame
  or nothing and it is confident, Vision's when both agree, otherwise the one
  with the clearer border; then sub-pixel edge snapping), `MapPhotoRectifier`
  (CIPerspectiveCorrection with aspect-ratio recovery) and, in Debug builds,
  `SyntheticPhoto` for tests.
* `Views/` holds the Library, the import flow (corner editor for photos, then
  progress), the Map screen (`MapCanvasUIView`: CATiledLayer pyramid,
  pan/pinch/rotate gestures, camera with tracking modes none / follow / follow
  with heading and a compass; overlays for the position puck, heading beam,
  accuracy, dotted trail, POI markers and waypoints), the stats card, Nearby
  sheet, Calibration (MapKit) and Settings.
* Photos of paper maps keep the original as `source-photo.*` and the flattened
  page as `source.jpg` (the base raster); `MapDocument.photoSource` stores the
  page corners so they can be adjusted later, and georeferencing runs with
  `"photo": true`.

Map files live in `Application Support/Maps/<uuid>/` as `meta.json`, the original
source, `tiles/{level}/{col}_{row}.jpg` (512 px tiles, level 0 = full
resolution) and `thumb.jpg`. The dark map adds `dark-profile.json` (the core's
profile of the map, from the largest pyramid level ≤ 2048 px stitched into one
raster), `tiles-dark-v2/` (each light tile drawn by `DarkStyle` with its
normalised rectangle `u0 = col·512·2^level / width`, `du = tile width·2^level /
width`, likewise v; made on first view and by `DarkPrerender` at utility
priority after an import or a new flattening) and `thumb-dark.jpg` for the
library card.

## Performance budget

* Import + render + tile pyramid for an A3 300-dpi PDF: < 3 s on an A15.
* Grid georeference (OCR + core): < 3 s. Contour georeference: < 4 s plus DEM
  download (the default 50 km near disc: ~2 s coarse search over 64 zoom-11
  tiles, 0.8 s with a scale label; the inner 20 km first, so a user near
  the sheet waits as before). Wide search on the reference box: 6–26 s of
  core time to find the sheet from up to 650 km, 26 s for a whole 1 000 km
  search that fails (see *Wide search*), at most one chunk mosaic (≤ 8 × 8
  tiles) in memory. Photo mode adds the normalisation (~0.1 s for 9 MP) and costlier
  line and mask extraction: ~0.9 s grid and ~2 s contour path on the CI-class
  Linux box (4 cores), < 6 s even when three candidates must be refined. The
  warp adds ~0.5 s of fit, plus the contour products in the grid path (one
  more `need_dem` round) and the line detection in the contour path: ~1.3–1.5 s
  grid and ~2.4–2.7 s contour path on the real photo.
* Map interaction at 60/120 fps, because tiles are drawn off the main thread
  by CATiledLayer and overlays are cheap CAShapeLayers.
* Release core: `opt-level=3`, LTO, `codegen-units=1`, `panic=unwind` (needed
  to catch panics at the FFI boundary).
