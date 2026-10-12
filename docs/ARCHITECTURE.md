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
   control points, and an affine (≥3 non-collinear GCPs) is fit, or for
   photos a homography (see *Photo mode*). Typical accuracy is a few metres.
   If labels exist but no lines are found, label centres are used directly as
   a coarse fit, which is then refined with contours when a DEM is available.
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
  "search_radius_m": 20000,
  "allow_rotation": false,
  "methods": ["grid", "contours"],
  "verify": false,
  "photo": false
}
```

Every field is optional; defaults are as shown, except `gps`, `dpi` and
`contour_rgb`, which default to `null`, and `ocr`, which defaults to `[]`.
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
  src/grid.rs         grid-line detection, label↔line association, GCP fit
  src/control_points.rs manual calibration fits
  src/georef.rs       Session + orchestrator (request → response)
  src/ffi.rs          C ABI
  include/mapme_core.h
  tests/              integration tests on fixtures/grootkloof (photo.rs: synthetic
                      photos of the sheet, flattened from imperfect corners;
                      realphoto.rs: the real phone photo of the folded sheet;
                      warp.rs: the warp contract and its accuracy on that photo;
                      dark.rs: dark map rendering)
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
  download. Photo mode adds the normalisation (~0.1 s for 9 MP) and costlier
  line and mask extraction: ~0.9 s grid and ~2 s contour path on the CI-class
  Linux box (4 cores), < 6 s even when three candidates must be refined. The
  warp adds ~0.5 s of fit, plus the contour products in the grid path (one
  more `need_dem` round) and the line detection in the contour path: ~1.3–1.5 s
  grid and ~2.4–2.7 s contour path on the real photo.
* Map interaction at 60/120 fps, because tiles are drawn off the main thread
  by CATiledLayer and overlays are cheap CAShapeLayers.
* Release core: `opt-level=3`, LTO, `codegen-units=1`, `panic=unwind` (needed
  to catch panics at the FFI boundary).
