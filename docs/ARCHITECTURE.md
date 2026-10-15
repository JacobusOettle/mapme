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
  (≈19 m) for refinement and verification. Small-scale maps (over 12 m/px)
  are searched on zooms 8–10 instead (*Scale bands*) and refined on zoom
  12–10 (a refinement takes at most 36 tiles; a finer zoom does not help a
  generalised small-scale map: on zoom-11 tiles a 1:250 000 sheet at 50 m/px
  fits worse than on zoom 10, its lines crossing detail it leaves out).
  Mercator cells shrink on the
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
   Garbled DMS reads (`25°48/00\S`, `2548100s`: marks read as other symbols,
   as digits or lost) are repaired (`labels::coordinate_readings`); the fit
   keeps the reading that agrees with the other labels. An axis with a single
   label is fitted on the other axis' scale (`lone_label`): nothing outvotes
   a misread there, so with contours requested the terrain under the fit
   confirms it (the answer) or rules it out (dropped); with nothing to judge
   by, the contour search runs and the fit is the fallback, flagged for
   checking (confidence < 0.5). Such a fit, like a labels-only one, must also
   have the printed scale when the request gives one (dpi and an OCR'd 1:n).
   If labels exist but no lines are found, label centres are used directly as
   a coarse fit, which is then refined with contours when a DEM is available.
   See *Grid labels as a phone reads them* for the rules.
2. **Contour matching against a DEM** (`contour_lines.rs`, `contour.rs`,
   `skeleton.rs`, `search.rs`, `refine.rs`). This needs somewhere to look: the GPS position
   (the map is assumed to lie within `search_radius_m` of you, 50 km by
   default, up to 2 000 km) and/or hints (places named on the map that the app
   geocoded, the map's own coordinate labels). Hint areas are searched first,
   then the near disc around the fix, then the wide area beyond 50 km chunk by
   chunk (see *Wide search*):
   * *Contour mask*: the thin lines of the contour ink, found at the raster's
     effective resolution (a smoothly or blockily upsampled PDF image is
     analysed at its native size): pixels darker than the background on both
     sides, classified by their ink (colour divided by the background, so
     hillshade, tints and yellowed paper drop out) with the warm or, on maps
     without coloured lines, the grey/black ink model whose lines hang together
     best; the user can override by picking a contour colour. See *Contour
     lines of any map style*.
   * *Coarse search*: build a doubled-angle orientation field from the mask
     (structure tensor of the blurred mask; contour lines run perpendicular to
     the terrain gradient) and the same field from DEM gradients. Then do an
     FFT-based normalised complex cross-correlation over translation, for every
     candidate scale (geometric steps of about 4%) and rotation (0° by default,
     or a sweep if `allow_rotation`). This finds where the map's contour pattern
     matches the real terrain. The prototype found the Grootkloof map
     unambiguously in a 70×80 km window: peak 0.77 vs 0.28 for the next distinct
     location, about 40 m error. Only the real part of a correlation is
     wanted, so two templates (neighbouring scales) share one inverse
     transform (the Hermitian parts of both spectra, one as the real and one
     as the imaginary part: `search::correlate_set`); spectra are kept column
     by column (one transpose per transform instead of two), a template's is
     made once per FFT size of the window's tiles from its non-zero rows, and
     each surface's peaks are picked block by block in one pass
     (`find_peaks`). The same NCC surfaces as before (`tests`:
     `correlation_matches_direct_ncc`, `peaks_match_full_scans`), 2–2.8× faster:
     the 50 km disc at zoom 11 for a 2 678 × 3 396 px sheet over 0.4–12 m/px
     7.8 → 2.8 s (11.7 → 5.7 s of CPU).
   * *Refinement* (the "3D match"): skeletonise the mask, split it into contour
     segments at junctions, and sample DEM elevations along every segment. If
     the georeference is right, each contour segment has constant elevation.
     Gauss-Newton / Levenberg–Marquardt with a robust loss minimises the
     within-segment elevation variance over similarity (4 parameters), then
     affine (6), with DEM gradients giving the Jacobian. The prototype
     converged to about 14 m max error over the whole sheet (the SRTM noise
     floor) from starts up to 470 m off, in under a second. A refinement costs
     0.3–0.6 s, so an area's candidates are refined only when its best one
     stands out at all (peak ratio ≥ 1.15, where `refine::confidence` starts
     to give it search evidence: `georef::REFINE_MIN_RATIO`), and its
     runners-up only while their peak ratio against it is ≥ 0.5
     (`RUNNER_UP_MIN_RATIO`). Of the 727 candidates the contour benchmark
     refined 90–110 km from its maps (every one fitting at 0) the best of
     their area peaked under 1.15 in 225 of 247 areas; every map located was
     its area's best candidate, at 1.35 or more.
   * *Confidence*: coarse peak ratio (best vs best distinct location) +
     refined robust cost + inlier fraction. If the map is not in the search
     area, the result is a low score and `failed` status, never a confident
     wrong answer. See *Any map: scales, priors, units* for what keeps this
     true beyond 1:25 000 sheets.
3. **Manual control points** (`control_points.rs`): the user taps matching
   points on the map and on an Apple satellite map (or "I'm here" at their
   GPS position). 2 points give a similarity, 3 an affine, 4+ an affine
   (homography for photos).

### Contours on any map: what makes it work

The contour path was built on 1:25 000 MCSA sheets (brown 5 m contours on white paper,
300 dpi, 1.9 m/px). It has to locate any map with contour lines: Gaia GPS and OpenTopoMap
exports, USGS, Swiss, OS, Kompass and N50 sheets, photocopies, faded prints, bare grey
contours, 1:250 000 maps over hypsometric tints, small maps on large pages. Five things
make that work:

1. **The map, not the page** (`content.rs`). A map smaller than its page (an export placed
   unscaled on an A4 sheet, a sheet reduced onto a page) is found as the block of ink on the
   paper and everything the contour path sizes by the image is sized by it. See *A map
   smaller than its page*.
2. **Effective resolution** (`contour_lines::estimate_level`). A PDF whose 100 ppi raster
   the app renders at 300 dpi spreads every 1 px line over 3–4 soft pixels (or 3 × 3
   blocks). The mask is made on the raster box-averaged back to about its native size
   (level 2–4) and its samples mapped back to page pixels exactly; sharp scans and the
   MCSA sheets stay at level 1.
3. **Lines first, then ink** (`ink_lines`, `model_bits`, `select_model`). A contour pixel
   is darker than the background on both sides 1–3 px away (tints, hillshade and area
   edges never are). Its ink is the colour divided by the background (printed inks) or
   white-balanced by the paper (digital maps); of five warm ink models, faded to strong,
   and a grey/black one (only on maps without coloured lines), the one whose lines hang
   together best wins. See *Contour lines of any map style*.
4. **Scale range and priors** (`wide::scale_bands`, `scalebar.rs`). 0.4–50 m/px in bands
   (zoom-11 tiles up to 12 m/px, a coarser zoom beyond); a printed ratio with the dpi is a
   constraint (a scale bar that disagrees with it is searched after it), a scale bar ("1
   mi", "0 1 2 km") a ±15 % prior searched first, never the only range searched. Maps of
   more than 40 km get their projection's bend (UTM, national grids). See *Any map:
   scales, priors, units* and *Large sheets: the projection's bend*.
5. **Units and names** (`labels::height_unit`, `hints::hint_queries`). Heights are in feet
   when the map says so or the terrain agrees; attribution and projection notes are no
   places; the names on the map are what the app geocodes into hints when the user is
   far from it ("Search wider").

What keeps it honest is the refinement's fit (explained variance, within-segment spread
scaled to the DEM cell and the map's pixel, contour-interval SNR) and the gradient
alignment (contours cross the slope; a wrong fit lies along it): of the 727 wrong
candidates the far runs below refine (97 maps), none fits above 0 (alignment 0.45 at the
least; right fits 0.11–0.36).

Contour benchmark (`fixtures/tools/bench_contours.py`, contour path only): 88 synthetic
maps of 14 styles (the 76-map corpus and fixtures/styles) and 9 real ones (the three MCSA
sheets; the user's two Gaia exports as the app renders the page, rendered blockily, and
as their native rasters), with the GPS at the map centre, 10–30 km off and 90–110 km off.
The core before this work (c66236b), after it (9913526) and now:

| | before | then | now |
|---|---|---|---|
| synthetic, GPS at the centre / 10–30 km off | 52 / 51 of 88 | 78 / 77 of 88 | 80 / 79 of 88 |
| real, GPS at the centre and 10–30 km off | 6 of 9 | 9 of 9 | 9 of 9 |
| confidently wrong (any run) | 3 | 0 | 0 |
| GPS 90–110 km off: located / confident | 0 / 0 | 0 / 0 | 0 / 0 |

("now": with the map found on its page and large sheets' projection bend, 9913526 →
this change; the two maps gained are 1:250 000 UTM sheets that were 112 and 172 m off at
their edges, and every other 1:100 000–1:250 000 UTM sheet is closer: see *Large
sheets: the projection's bend*. The real maps' errors are unchanged to the decimetre.)

By style (centre, before → now): photocopies 0 → 5 of 5, grey contours 0 → 5 of 6,
Gaia-like pages 1 → 8 of 10, 1:250 000 hypsometric 0 → 5 of 9, Kompass 3 → 5 of 5, the
others (dense, faded, MCSA-like, N50, OpenTopoMap, OS, Swiss, USGS) 48 → 50 of 53. The
Gaia pages as the app renders them: 11/16 m and 14/17 m (median/max) with the GPS at the
centre or 15 km off, and from the Magaliesberg (450 km) through a geocoded name on the
map (`tests/gaia_contours.rs`); the MCSA sheets 2–7 m as before. What still fails
(no answer, or a right one outside the benchmark's tolerance; none confidently wrong):

* flat terrain (Kansas, the Free State: 6 maps, tens of metres of relief): the
  orientation field holds no pattern to find, or the right fit has too few contour
  levels for the interval check (20 m contours over 36 m of relief): no answer;
* a 1:250 000 sheet in UTM rendered blockily at 17 m/px: its fit stays too loose to be
  accepted (the other sheets of 40–110 km in UTM now get their projection's bend and
  lie within 2 px at their edges: *Large sheets: the projection's bend*);
* faint 1 px lines on a native 18 m/px Gaia raster, 11 % of them sampled: 36/82 m;
* a raster upsampled 3× after its dense contours had merged into solid areas: located
  from the centre only.

### Any map: scales, priors, units

The pipeline was first tuned on 1:25 000 sheets at 300 dpi (1.9 m/px, 5 m
contours, refined on zoom 13). Maps from elsewhere (Gaia GPS exports,
1:250 000 sheets, screenshots, the raster of a PDF imported as an image)
differ in scale, contour interval, units and furniture:

* **Scale bands** (`wide::scale_bands`, `wide::default_scale_range`). Without
  a scale prior the search covers 0.4–50 m/px, up to the scale at which the
  image's long side would show 150 km (`MAX_MAP_EXTENT_M`: 30 m/px for a
  4 960 px A3 page). Scales up to 12 m/px (`FINE_MAX_SCALE`) are searched as
  before on zoom-11 tiles, and first; the rest is a band of its own on the
  coarsest zoom (at most three coarser) on which the band's smallest scale
  keeps a template of 64 cells (`band_zoom`: zoom 10 for a 1 118 × 791 px
  export, 9 or 8 for a 300-dpi page): on zoom 11 those templates would be
  searched on a 4–16× downsampled mosaic anyway, and the mosaic holding such
  a map would be hundreds of tiles. The coarse band's DEM box may hold as many
  cells as zoom 11's (`ScaleBand::max_box_half_m`: 240 km on zoom 9). Each
  hint area searches the fine band, then the coarse band; the near disc the
  fine band in the inner 20 km, then the coarse band over the whole disc
  (0.1–0.4 s), then the fine band over the whole disc (the costliest search
  of all: 1–3 s over 50 km), cheapest first, so that a 23 m/px export
  without a scale bar is found in 1.2 s instead of 5–6 s; a coarse band
  refines only candidates whose peak ratio reaches 1.15 (true small-scale
  matches peak at 3–8: refining three wrong ones costs ~2 s in every search
  the map is not in). A coarse candidate is re-checked one zoom finer than its band
  (`wide::recheck_zoom`). The wide search plans each band (`wide_band_reach`:
  the fine band on zoom 9/10 as before, the coarse one up to two zooms
  coarser) out to the radius its own scales allow (a small image is too
  small for zoom 9 at up to 12 m/px but not at 50 m/px: its coarse band goes
  past the fine band's 150 km, `wide_limit` is the farthest band's reach and
  each chunk's placements stop at its band's), leaves out a band inside
  another on the same zoom (a scale bar's inside the default range's), and
  merges their chunks into one sweep, nearest first
  (`wide::merge_plans`: tiles are released after their last use in the merged
  order, so bands sharing a zoom never ask for a released tile).
  `diagnostics.search_areas[].band` is `{zoom, min, max}`,
  `diagnostics.wide.zooms` the bands' wide zooms. At the fine end a map's
  template must keep 24 cells on zoom 11: 0.64 m/px for a 300-dpi page
  (1:7 600), 1.4 m/px for a 1 170 px phone screenshot; finer scales of the
  default range are skipped there. A scale prior wholly finer than that (a
  1:5 000 plan at 300 dpi) is searched on the zoom that reaches it (12 or 13),
  in a box of as many cells (±30 km, ±15 km).
* **Distinct places** (`search::distinct_m`): candidates closer than
  max(2 km, 8 % of the map's short side) are one place. A 1:25 000 sheet
  keeps 2 km; a large-area export's correlation peaks are wider than that,
  and a peak's own shoulder must not count as the runner-up.
* **Refining small-scale maps**: the translation grid on the coarsest DEM
  level spans ±max(450 m, 15 map pixels). `refine::seg_std_floor_m` is the
  within-segment spread a right fit stays under: 4 m on zoom 13 as always,
  × √(cell / 19.1 m) on coarser tiles, or 0.35 × the map's pixel, whichever
  is largest; the fit-quality factor runs from it (1) to three times it
  (0). A 1:250 000 sheet at 25 m/px fits its true place at 11–25 m on zoom
  10 and wrong places at 40–80 m; with the fixed 4–12 m it was never
  accepted. Zoom 10 it stays: on zoom-11 tiles a generalised 1:250 000
  sheet at 50 m/px fits worse (its lines cross detail it leaves out:
  gradient alignment 0.42 instead of 0.36, fit 0.31 instead of 0.82).
* **Regional projections**: a map in UTM or a national grid is no affine
  image of Web Mercator once it spans tens of km (Mercator's scale grows by
  tan(lat) / R per metre north: 1 % over 75 km at 40°). The best affine fit
  of the 1:250 000 Rockies sheet (2 000 × 1 500 px at 50 m/px, UTM) to its
  truth is 104 m off (median; 200 m at the edges), a homography 53 m. A
  map whose samples span more than 40 km (ground diagonal) therefore gets a
  projective stage after the affine one (`refine::REGIONAL_SPAN_M`), kept
  if it lowers the cost by 1 % with perspective terms ≤ 0.02 over the
  samples' extent and a near-conformal local part, and then its
  projection's bend (`ConformalStage`), which takes out the rest: see
  *Large sheets: the projection's bend*. Web Mercator exports (OpenTopoMap
  at 20–40 m/px) keep their affine fit.
* **Gradient alignment** (`RefineResult::gradient_alignment`, Σ|g·t̂| / Σ|g|
  over the samples, g the DEM gradient, t̂ the contour's direction mapped to
  the ground): contour lines run across the slope, so right fits stay low
  (0.11–0.26 on every benchmark map and real sheet, 0.27–0.31 on flat ground,
  0.36 for a 1:250 000 sheet on zoom-10 tiles) while lines lying any which way
  give 2/π ≈ 0.64. A wrong fit that keeps its levels (few or short segments
  laid along gentle slopes: a sparse mask) still crosses the steep slopes:
  0.34 at the least with 15–21 segments, 0.46 with 900 or more.
  `fit_quality` multiplies by 1 − ramp(alignment, a, a + 0.07…0.08), a =
  0.26 up to 50 segments … 0.38 from 400 (`alignment_limits`). The far runs
  of the benchmark (76 maps, 90–110 km away) refined 21 wrong candidates to a
  fit of 0.3–1.0 without it (a Gaia page at 1.0 for a place 44 km off, at the
  wrong scale); with it, none of the 482 wrong candidates the far runs of
  the 76 maps and 6 Gaia cases refine (65 maps have contours to refine)
  fits above 0 (alignment 0.336 at the least).
* **Scale priors**. A printed ratio ("1:25 000") with the request's dpi, as
  before: ±25 %, a constraint (`scale_range.source` "scale_label"). A scale
  bar (`scalebar.rs`), with or without a dpi: a row of tick labels ("0",
  "1", "2 km": pixels per unit by least squares; or just "0" and "2 km" at
  the bar's two ends, within 1.5 text heights of one baseline), else a round
  distance with its unit ("1 mi", "2 km", "500 ft", "0.5 mi") and the bar
  line measured beside it in the image (the longest dark horizontal run
  within 2.5 text heights of the label, under it or reaching to within 1.5
  text heights of it, 3–60 text heights long and thin; a bar with end ticks
  is measured between the ticks' centres). Tick labels come first: beside
  "2 km" at the end of a bar of alternating blocks the line is one block
  (four synthetic sheets read 7–11× too coarse that way). It gives ground
  m/px ±15 % (`diagnostics.scale_bar`, source "scale_bar"), a prior, not a
  constraint: hint and near areas search its band first (the inner 20 km,
  then 50 km), then the default bands over the whole disc at once; the wide
  search the default bands (its band lies inside them: in one sweep nearest
  first it would add work, not reach). A bar read wrongly ("2 mi" for
  "2 km") so costs time but never hides the map: with the bar's range alone
  the wide search missed a sheet 120 km away that it finds without the bar
  (`tests/wide.rs`); the price is the wide search's compute of a map without
  a bar (Gaia page from 450 km, 2 000 km radius, no hints: 8.4 s with the
  bar's range alone, 14.4 s now, 13.0 s without OCR; tiles 360 / 424 / 424).
  A bar implying a scale outside 0.02–500 m/px ("2 m" for "2 mi") is a
  misread and ignored, and so is a label taller than 8 % of the image's
  short side (at least 48 px): five page-sized reads of "1 km" took the bar
  search 17 s on a 6 000 px page. With a printed ratio the ratio's range comes first
  and a bar that disagrees with it is searched after it: the dpi turns the
  ratio into pixels only for a page printed at that scale (a sheet reduced
  to fit the page is not), the bar is measured in the image. Of the 76 benchmark maps 57 have a bar
  read, all 57 within 15 % (the 19 others: no bar, or labels OCR misses).
  The Gaia pages: "1 mi" over 532 px → 3.025 m/px (truth 3.029 across ×
  3.077 down), "2 mi" over 421.5 px → 7.636 m/px (7.650 × 7.771). Photos
  and exported rasters with a bar get a prior too. North-up notes ("Web
  Mercator", "EPSG: 3857") change nothing: the search is north up unless
  `allow_rotation`.
* **Refinements reused**: a candidate whose start lies within 100 m
  (Mercator, at every image corner) or two map pixels of one refined before
  on the same DEM takes that result (the whole near disc finding the inner
  disc's peaks again, the default range after a scale bar's band).
* **Units** (`labels::height_unit`). Heights are in feet when the map says so
  ("Gaia Topo (feet)", "CONTOUR INTERVAL 40 FEET", "Elevations in feet"),
  when most of its heights have five digits (two or more: no metre height
  does), or, nothing said, when the terrain agrees: once the session holds
  the zoom-12/13 tiles under the located map, spot heights read as metres
  that fit it as feet (two or more, over twice as many as fit as metres) are
  converted. A height in feet has 3–5 digits (≤ 29 100 ft) and no leading
  zero; a five-digit one is no UTM easting (a grid value is round). POIs keep
  the printed text ("10876") with `elevation_m` in metres (3 315.0), and the
  spot-height check against the DEM compares metres. The refinement assumes
  no interval, it measures it (`contour_interval_m`: 24.0 m on the 80 ft Gaia
  map, 48.3 m on the 160 ft one, 92–100 m on 1:250 000 sheets).

* **Refinement samples**: the finest DEM level refines at most 60 000
  samples (`MAX_SAMPLES_FINE`), thinned per segment so that every segment
  keeps at least 8 (`thin_segments`); the line detector gives 300-dpi pages
  100–550 k, and three wrong candidates refined at 150 k cost 1–3 s more in
  every search that does not find the map. Photos keep 150 k (their
  projective stage needs them).

How the search and the extraction do together on the contour benchmark:
*Contours on any map: what makes it work*.

### Grid labels as a phone reads them

Phone OCR garbles the graticule labels printed over the map (Apple Vision and
the RapidOCR stand-in on the MCSA sheets: `25°48/00\S`, `2595000"s`,
`2548100s`, `25-4900s`, `27%3400cEV1Sept2017-ErasGous`), reads some twice and
misses others. The grid path copes, and rejects the fits misreads could make:

* **Readings and their costs** (`labels::coordinate_readings`). Every plausible
  reading of a label, cheapest first: the label as printed (cost 0), then
  `fuzzy_dms`, a template of degrees (1–3 digits), mark, minutes, mark,
  [seconds, mark], hemisphere. A mark read as the printed one is free; as any
  other symbol 1; as a letter OCR confuses with it 1 (o for °; l, I, i, j for ';
  c, u, l, I, i for "); as a digit 2 (9 or 0 for °, 1 for ', 1 or 4 for ");
  lost 1 (two characters may stand for one mark: `11` for "). A zero read as
  O, o, Q or D and a one read as l, I or | cost 1 each; a lowercase hemisphere
  letter is free, `5` for S costs 1 (only after a symbol for a mark); no
  hemisphere (a `Degrees` reading, either axis) costs 1 and needs the seconds
  and a symbol for a mark. At most 5 repairs. A reading never starts right
  after a °, ' or " (`°53'00"S` is the end of a cut label, not 53°S); a text
  holding two different coordinates gives none. `parse_coordinate` (label
  hints, bands) takes a garbled label only when all its cheapest readings
  agree.
* **Alternatives**. Every reading is a grid candidate: the fit keeps the one
  the other labels agree with (`25-50'00"S` is 50°S as printed, 25°50'S with a
  dash for the degree sign). A repaired reading of a label that also reads as
  printed (`340000E`: an easting, or 34°00'00"E with every mark lost) is an
  *alternative*: it may back up an axis fit but never be all of it, so six-digit
  UTM labels cannot make a graticule 18× the scale.
* **Units** (`grid::grid_unit`). Grid lines are drawn at a round interval and
  labelled with its multiples, so a line fit's step between adjacent lines may
  be at most 4.5× the finest standard unit its label values share
  (`MAX_LINE_STEP_UNITS`; units 10° … 1", with the quadrangle intervals 2°30',
  7'30" and 2'30" of USGS sheets and their ticks; UTM 100 km … 100 m):
  24°00'11" and 27°23' on adjacent meridians are no 3.4° grid.
* **Equal scale**. A fit must be conformal: equal Mercator metres per pixel
  along x and y (1/cos φ more along y on a plain lat/lon map) within 6 % for
  line fits, 15 % for labels-only fits (`LABELS_CONFORMAL_TOL`: a misread label
  is off by a whole interval) and 35 % for photos. A label in a corner of the
  map is a latitude when its text runs up or down. `n_lat`/`n_lon` count
  distinct values, not reads.
* **Labels-only fits** (label centres, no line found): confidence at most
  0.45, under the app's 0.5 "check this" threshold.
* **One label on an axis** (`GridResult::lone_label`,
  `diagnostics.grid.lone_label`; scans only). When no fit through two labels
  per axis exists, an axis whose edge-of-map labels all allow one value is
  fitted on the other axis' scale (slopes of opposite sign, the label's value
  on its line, the other lines on round values by counting), then refitted
  through the lines. It must earn its place: three lines on their values on
  each axis (`LONE_MIN_LINES`), every fit within one line or label of the best
  puts the map in the same place, within 250 km of the GPS fix when there is
  one, and the printed scale (below). With contours requested the terrain
  judges it (`georef::lone_check`, the zoom-13 tiles under the fit kept for the
  session's later calls): `refine::fit_confidence` ≥ 0.5 confirms it ("Located
  from 1 latitude and 2 longitude grid labels, confirmed by the terrain", full
  confidence); enough contours on the DEM (`refine::fit_evidence` ≥ 0.5, from
  the contour segments and the relief under them: full at 20 segments over
  30 m) that fit badly (`refine::fit_quality` < 0.5) rule it out (dropped;
  the contour search runs as usual); anything else (no
  terrain, offline, a map without contours, flat ground) answers it at
  confidence 0.45: "Only one latitude label could be read: check the dot
  against your surroundings."
* **Printed scale** (`georef::scale_agrees`). With the dpi and an OCR'd 1:n,
  a labels-only or one-label fit's ground scale must lie in 0.75·prior·cos φ …
  1.25·prior, the range the contour search would try.
* **Hatching** (`grid::drop_hatching`). Coloured fill patterns (vegetation,
  marsh) are long parallel coloured lines too: a run of three or more coloured
  lines of one family, each within 50 px (or 1.5 % of the image across them)
  of the next, is hatching and dropped, unless lines of the other family cross
  it at about the same spacing (±25 %: a dense coloured grid on a small
  screenshot; hatching runs one way). Grey and black lines are never dropped.

`tests/stress.rs` damages the printed labels of the three MCSA sheets the way
phone OCR does (seeded) and replays the app's request 60 km away without
terrain: with two whole labels per axis 99.5–100 % are located within 5 m,
and no answer is off by more than 50 m with confidence ≥ 0.5.

### Contour lines of any map style (`contour_lines.rs`)

The contour mask was a colour threshold on the raw pixels (`contour::auto_mask`, three
brown HSV boxes chosen by how thin their masks are). That works on clean scans with brown
contours on white paper and fails elsewhere: a contour pixel's colour is a blend of the ink
and whatever lies under it (hillshade, park and forest tints, yellowed paper), grey and black
contours have no hue, and a PDF whose 100 ppi raster the app renders at 300 dpi spreads every
line over 3–4 soft pixels, which the thinness test took for area fills (both Gaia GPS exports
gave "No contour lines found"). `contour_lines::contour_mask` (every non-photo request; photos
keep `photo_mask`) instead:

1. **Analysis level** (`estimate_level`, ~10–60 ms on 16 windows of 192 px). The integer box
   downsampling factor at which the map's lines are as thin as on a native raster:
   * *Pixel repetition* (`block_factor`): a raster upsampled by repeating pixels (a PDF image
     drawn without interpolation) has edges only between its blocks, so the gaps between
     consecutive edges (luminance steps > 24 levels) along rows and columns are multiples of
     the block size; on a native raster they take every length (the two edges of a 1-px
     line are 1 apart). ≥ 90 % of ≥ 1 000 gaps multiples of k (the largest k ≤ 4) → level k.
   * *Smooth upsampling*: per line pixel the smallest radius (1–6) whose valley depth (below)
     reaches 75 % of its deepest; the *thin share* is the share of line contrast resolved
     within 2 px. Native rasters: 0.12–0.80 (blurry scans and photocopies included); rasters
     upsampled 2–3× smoothly: 0.00–0.06. Level 1 while ≥ 0.10; else level 2 if the share
     there is ≥ 0.30 (2× upsampling: 0.33–0.55), else level 3 (4 if still < 0.15 there).
   The level image is the box average of f × f blocks (`downsample_box`): for a 3× upsampled
   raster, close to the raster itself. Sharp 300 dpi sheets stay at level 1 (thin share
   0.42–0.51 on the MCSA sheets).
2. **Line pixels** (`ink_lines`): a pixel is on a line when it is darker than the background
   on *both* sides, along x, y or a diagonal, 1–3 px away (1.4/2.8 diagonally): depth =
   min(side luminances) − pixel ≥ max(6 levels, 5 % of the side). The deepest offset is
   kept. Area edges, tints and hillshade gradients are darker on one side at most, so they
   never count; dense contours ~2.5 px apart stay separate (the side lands in the gap).
3. **Ink** (`model_bits`): a line over a coloured ground has one of two inks, depending on
   how the map was made. Printed inks overprint (multiply) what lies under them: the ink is
   the pixel divided by its background (per channel, the brighter of the two sides), the
   colour the line would have on white paper, so hillshade and tints divide out (a Gaia
   contour (197,185,159) on its green-grey ground has the ink of a tan line). Digital maps
   paint opaque or semi-transparent lines over their tints: the ink is the pixel itself,
   white-balanced by the map's paper (`paper_colour`, the 98th percentile per channel), so a
   yellowed print's paper turns white. Both are averaged over the line pixels within 2 px
   (`INK_RADIUS`, weighted by depth: a faded line's colour is a few levels from grey, about
   the JPEG noise), and a model takes the pixel when either matches; grey and black stay
   neutral either way.
4. **Ink model** (`ink_models`, `select_model`): five warm boxes (brown, tan, orange,
   red-brown, purple-brown: hue 330°–62°) that admit inks from faded (absolute chroma
   (max − min)/255 ≥ 0.015) to strong (≥ 0.10), and neutral (chroma < 0.05, value ≤ 0.93) for
   the pixels no warm box takes. Each is scored by its 8-connected components
   (`RunComponents`, by runs): pixels in pieces of at least the segment length minus pixels
   in shorter ones (text, specks, noise); the best warm model wins unless the neutral
   one scores higher, the warm lines are under 5 % of its long lines *and* its lines look
   like contours: ≥ 80 % of its pixels in long lines, covering ≥ 1.5 % of the image (black
   and grey ink is on every map: a sheet's roads, grid and text alone are ~60 % long and
   cover ~0.5 %; photocopied and grey contours ≥ 89 %, 2–40 %). Otherwise a map without
   coloured lines has no contours, as before. Components under 8 px are dropped.
   A picked contour colour (`contour_rgb`) keeps `model_from_rgb` on the raw colours of the
   line pixels.

The mask is at the analysis level: `skeleton::contour_samples` runs there with the segment
length and step divided by the level (`level_sampling`), and `scale_samples` maps sample
(i + 0.5) to base position (i + 0.5)·level, the centre of the block it averages (exact for the
box average); the orientation field uses cells of about `field_px_per_cell` base pixels
(`search::map_field_at`). `diagnostics.contours.extraction` reports the level, thin shares,
block size, chosen ink and every model's score.

Against the colour threshold, on the 88 synthetic maps of the contour benchmark (centre
runs, scored against their `contour_mask.png`): skeleton samples on true contours 0.83 →
0.97, true contour lines sampled 0.37 → 0.79 (photocopies and grey contours 0 → 0.95,
Gaia-like PDF pages 0.01 → 0.50, Kompass 0.13 → 0.72, MCSA-like 0.13 → 0.81). The located
counts of the whole pipeline are in *Contours on any map: what makes it work*.

### A map smaller than its page (`content.rs`)

A PDF can hold a map smaller than its page: a 1 118 × 791 px Gaia export placed unscaled
on an A4 sheet the app renders at 300 dpi (2 480 × 3 508 px), a 1:25 000 sheet reduced to
a third of an A3 page, a printout with a wide margin, a title and a legend. Everything the
contour path sizes by the image is sized by the map instead: the shortest contour segment
and the sampling step (`skeleton::default_sampling`: 54 px for the A4 page, 30 px for the
map), the analysis level's windows, the ink model's cover test, the orientation field's
cells, the coarse search's templates, its scale range and bands, the distinct-place
distance and the coarse and fine DEM boxes (the page's footprint pushed the refinement of
a 9 m/px map onto zoom-11 tiles: 31/35 m instead of 4/8 m).

`content::map_content` finds the map as the block of ink on the paper: cells of 1/160 of the
long side holding ≥ 2 % ink pixels (a channel 24 levels darker than the paper), closed over
3 cells (sparse contours, lakes) and opened over 4 (a frame round the page, lines of text
and the scale bar are thinner than a map), the bounding box of the largest group and of
those at least a fifth its size, plus a cell (5–90 ms on 1–17 MP). The contour path works
on that rectangle (`diagnostics.contours.map_area`) when it covers at most 75 % of the
page, is ≥ 300 px a side and at most 8 % of the cells outside it hold ink (a full-bleed map
whose flat part has contours too far apart to close keeps the whole raster: 19 % on the
grey Kansas map); otherwise, and for photos, the whole raster as before. The mask, skeleton
and field are made on the rectangle (`ContourProducts::compute_in`; samples in page pixels),
the coarse search places the rectangle (`Run::to_page` turns its candidates into page
transforms), the scale bands, DEM boxes, fine plans and refinement reuse use its size and
corners, and the label hints measure offsets to its centre. Grid fits, photos and the
response (corners, warp lattice) stay in page pixels.

Maps on pages (`fixtures/tools/page_compose.py`: the real maps resized onto a white A4/A3
sheet at 300 dpi under a title, with legend lines under them; `synth_topo.py suite pages`:
synthetic rasters placed unscaled on A4/A3 PDF pages with their title, legend and scale
bar), GPS at the map centre, median/max error and confidence, before → after (the runs
10–30 km off are the same; from 90–110 km away none is located in either build):

| page | map | before | after |
|---|---|---|---|
| Gaia raster, Langalibalele, on A4 (real) | 1 118 × 791, 9.1 m/px, 10 % | 31/35 m | 4/8 m |
| Gaia raster, Giant's Castle, on A4 landscape (real) | 1 118 × 791, 23 m/px, 10 % | 67/122 m | 11/13 m |
| Grootkloof × 0.6 on A3 (real) | 3.2 m/px, 36 % | 15/19 m | 6/8 m |
| Castle Gorge × 0.45 on A4 (real) | 4.2 m/px, 41 % | 13/14 m | 6/8 m |
| Mhlabatini × 0.7 on A3 (real) | 2.7 m/px, 49 % | 21/24 m | 5/7 m |
| Gaia-like raster on A4, two places | 9 m/px, 10 % | 8/14, 6/10 m | 7/15, 6/13 m |
| Gaia-like raster on A4 landscape | 12 m/px, 10 % | 33/49 m | 12/26 m |
| Gaia-like raster on A4 landscape | 18 m/px, 20 % | no contours found | 50/89 m, 0.57 |
| OpenTopoMap-like on A3 | 15 m/px, 9 % | 17/26 m, 0.85 | 14/22 m, 0.59 |
| Swiss-like on A4 | 10.6 m/px, 15 % | 28/55 m | 19/29 m |
| USGS-like on A3 | 6.35 m/px, 12 % | 12/25 m | 3/6 m |

Confidence 1.00 or 0.98 except where given. Located (benchmark tolerance) 8 → 11 of 12;
the 18 m/px raster's faint 1 px lines are its own limit (the raster alone: 36/82 m, 0.83),
and the OpenTopoMap page now refines on zoom 12 as the map alone does (13/20 m, 0.66;
the page's footprint had pushed it to zoom 10, whose looser spread floor gave 0.85). A
blank page, a page of text and a page with a photo-like picture still fail at once ("No
contour lines found", 0.3–0.7 s; `tests/map_on_page.rs`). Of the 97 benchmark maps, 11
work on a rectangle (the MCSA-like A3 pages, Kompass, Swiss, USGS and OS pages with their
legends, 45–71 % of the page; a Gaia-like page, 52 %): no verdict changes, one Kompass
page 20/32 → 10/24 m.

### Large sheets: the projection's bend

A sheet in UTM, a national transverse Mercator grid or a Lambert conic is a conformal
image of Web Mercator, so pixel → Mercator is an analytic function of the complex pixel
position w: an affine part plus c₂w² + c₃w³ + …. Its quadratic term is the Mercator
scale sec(lat) growing northward by tan(lat) / R per metre while the sheet's stays (plus
a little where its central meridian is off the map); over 40–110 km that bends the sheet
by 20–600 m against the best affine fit (the most at 70° N), of which a homography takes
out about half. A complex polynomial fitted to the contours could take
out the rest, but the data cannot place it: on the 1:250 000 benchmark sheets the fitted
c₂ lands tens of metres from the truth's (generalised contours on 76–306 m DEM cells), and
on one sheet in the wrong direction altogether (96 m off). The bend is therefore
predicted, not fitted (`refine::ConformalStage`, `tm_bend`): around the affine fit's
centroid the transverse Mercator → Web Mercator map is evaluated exactly
(`geo::latlon_to_tm`, `tm_to_latlon`, any central meridian) and its quadratic and cubic
coefficients taken by least squares over the map's extent, turned into pixel units
through the affine's complex scale; only the affine is then refitted under it. The
meridian is the map's UTM zone's, or the map's own when that fits better (a national
grid's is near it: OS GB −2°, the Lo system's odd degrees). For a Lambert conic, an
oblique Mercator or a polar stereographic sheet the scale against Web Mercator changes
about as fast (n / cos lat against tan lat for a conic at its standard parallel; sec lat
for the stereographic), so the same bend is the first-order answer. Web Mercator maps
have none.

It runs after the affine and projective stages for a map whose samples span more than
40 km, when they cover ≥ 60 % of the map's rectangle along both axes (2nd–98th
percentile) and the map does not print "Web Mercator"/"EPSG:3857" (Gaia GPS pages do);
it is kept when its coefficients are within 1.5× what a conformal projection can do at
that latitude and size (+2 m), its cost is at most 3 % above the affine's (Web Mercator
exports: 17–23 % above), the fit explains anything at all (a wrong place is not
cross-validated), and in spatial 4-fold cross validation (segments by the block of their
centroid in a 4 × 4 grid, folds of four blocks spread over the map; each model refitted
without them and costed on them, ≤ 20 000 samples, 10 LM iterations) its held-out cost,
taken 1 % lower as the prior for a physical bend, is the lowest of the affine, the kept
projective fit and itself. Kept, `RefineResult::conformal` holds it, `transform` stays its
affine part at the centroid, and the response carries it as `pixel_to_mercator` (the best
homography over the image) plus a `warp` (`Warp::from_model`, fold-free by construction;
`diagnostics.refined.conformal`: |c₂|, |c₃|, meridian). It adds 0.3–0.9 s to the
refinement of such a map (the cross validation); smaller maps never run it.

Large sheets of the benchmark (`synth_topo.py suite regional`: UTM, a TM national grid,
Kompass and USGS 1:100 000 at 31–70°, and Web Mercator exports of the same size), GPS at
the centre, median/max error at the truth points and confidence, before → after (the
10–30 km offset runs are the same, the 90–110 km ones fail honestly in both):

| sheet | m/px, lat, extent | best affine / homography of the truth | before | after |
|---|---|---|---|---|
| 1:250 000 Rockies (UTM) | 50, 40°, 112 km | 103/199, 53/99 | 65/170 m, 0.97 | 25/61 m, 0.98 |
| 1:250 000 High Atlas | 40, 31°, 90 km | 48/93, 25/47 | 53/172 m, 0.96 | 29/65 m, 0.96 |
| 1:250 000 Garhwal | 30, 31°, 81 km | 38/73, 19/38 | 37/110 m, 0.83 | 22/43 m, 0.82 |
| 1:250 000 Aurlandsfjord | 20, 61°, 54 km | 51/99, 26/49 | 26/112 m, 0.68 | 15/33 m, 0.78 |
| 1:250 000 Cradle Mountain | 25, −42°, 56 km | 28/53, 14/27 | 33/87 m, 0.96 | 27/34 m, 0.96 |
| 1:250 000 Drakensberg | 25, −29°, 68 km | 25/49, 13/25 | 17/51 m, 0.94 | 11/33 m, 0.95 |
| 1:250 000 Lyngen Alps | 40, 70°, 108 km | 307/596, 159/293 | no answer | 44/65 m, 0.90 |
| 1:250 000 Bernese Oberland | 35, 47°, 94 km | 93/179, 48/89 | 46/145 m, 0.85 | 10/21 m, 0.91 |
| 1:250 000 Glen Coe (TM, meridian −2°) | 30, 57°, 81 km | 98/189, 49/96 | 52/175 m, 0.97 | 12/18 m, 0.99 |
| 1:250 000 Fitz Roy | 45, −49°, 101 km | 117/226, 60/114 | 61/218 m, 0.89 | 23/39 m, 0.89 |
| Kompass 1:200 000 Norway | 25, 61°, 68 km | 83/160, 43/80 | 61/93 m, 0.58 | 16/43 m, 0.93 |
| USGS 1:100 000 Rockies | 10, 40°, 36 km | 11/19, 6/11 | 19/37 m, 1.00 | 16/24 m, 1.00 |
| USGS 1:100 000 High Atlas | 20, 31°, 67 km | 27/51, 14/26 | 29/69 m, 1.00 | 28/44 m, 1.00 |
| USGS 1:100 000 Blue Ridge | 12, 39°, 46 km | 16/30, 9/17 | 19/35 m, 1.00 | the same (projective kept) |
| OpenTopoMap, Bernese Oberland | 40, 47°, 90 km | Web Mercator | 11/19 m, 0.97 | the same (no bend) |
| OpenTopoMap, Lyngen Alps | 30, 70°, 67 km | Web Mercator | 43/49 m, 0.63 | the same (no bend) |
| Gaia page, High Atlas (EPSG:3857 printed) | 12, 31°, 44 km | Web Mercator | 11/23 m, 0.82 | the same (no bend) |

Every UTM/TM sheet located now lies within 2 px or 60 m at its worst truth point (the
bend itself is right to the decimetre there: what remains is an affine error, the
translation and scale noise of a generalised sheet on coarse DEM cells); none of the 22
large maps is confidently wrong or off at its edges in any run. Still without an answer:
the flat Free State sheet (its contours hold no pattern) and a 1:250 000 sheet rendered
blockily at 17 m/px whose fit stays too loose (0.49); a 35 m/px Gaia raster whose 1 px
lines the extraction misses.

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
  map worth geocoding into `hints`, best first, at most 12: the titles, then
  the names of `pois` (see *Names on the map*). Grootkloof (Apple Vision):
  `["HAMERKOP", "GROOTKLOOF", "MAGALIESBERG", "EASY GULLY", "WOODY GULLY", …]`.
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
  data for the search area couldn't be downloaded …"; with more tiles
  missing than searched: "Terrain for much of the search area couldn't be
  downloaded (90 of 154 tiles), and the rest holds no match. …".
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
* `pois` are map labels classified as place names, peaks, water or spot
  heights, in base-image pixels, without the labels the grid used (see *Names
  on the map*).
* `warp` (absent or `null` unless one of these) is a smooth correction: in
  photo mode for paper that does not lie flat (fold creases, curl), and for a
  sheet of more than 40 km in a regional projection (UTM, a national grid)
  the bend of that projection against Web Mercator (see *Large sheets: the
  projection's bend*; then `pixel_to_mercator` is the homography that fits
  the bent model best over a 9 × 9 grid of the image, `Warp::from_model`).
  No single homography fits a folded sheet: on `fixtures/grootkloof/photo` (a
  phone photo of the printed sheet, folded in four) the best one is off by
  ~37 m median and ~180 m at the corners. With a warp the full model is

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
  ignores `warp` gets the flat-model answer. `diagnostics.warp` says how a
  photo's warp was fitted (`source`: `"grid"` or `"contours"`, constraint
  counts, RMS before and after, largest offset), `diagnostics.refined.conformal`
  a projection's bend (|c₂|, |c₃| in Mercator metres, the central meridian).

### Names on the map (`pois`, `hint_queries`)

`labels::classify_pois` turns the OCR text into the Nearby list; the app shows
it as read, so it must hold what is printed on the sheet, once, and nothing
else. Apple Vision's own reads of the three MCSA sheets
(`fixtures/*/vision-ocr.json`, iOS 26.2 simulator) are the reference:
`tests/sheets.rs::vision_pois_are_the_names_printed_on_the_sheets` pins the
exact lists, checked against the images.

1. *Reads*. Identical text close together (several OCR passes) is one read,
   the most confident.
2. *Furniture* (never a POI): scale-bar numbers (three or more on a baseline
   in arithmetic progression), legend columns (three or more short labels
   sharing a left edge at line spacing), lettering over 1.6× the median text
   height (the title), coordinates, a scale, angle marks, ©, imprints (a month
   next to a year, or a version tag with a date: "ISV1 Sept 2017", "GIS V1
   Sept 2017 - Eras Gous"), labels with a furniture word (`META_WORDS`:
   "sheet", "declination", "contours", …) and labels of legend words only
   (`LEGEND_WORDS`, also cut short or run together with their capitals:
   "Non-Peren", "PerennialRiver"; a one-word name such as "Lakeview" or
   "PARKVIEW" is a name). Legend words with a number are a numbered place
   ("CAMP 2", "HUT 4"; "2 km" is not). Credits, attribution and projection
   notes of map apps and tile services (`labels::credit_text`: "Gaia Topo
   (feet): © Gaia GPS, OpenStreetMap", the "GAIAGPS" logo, "Web Mercator |
   EPSG: 3857", "© Mapbox", "OpenStreetN" cut short) are furniture too, and
   never a hint query.
3. *Spot heights*: three or four digits, not a multiple of 50 (contour
   labels), −400…8 850 m; on a map in feet (`labels::height_unit`, see *Any
   map*) three to five digits, up to 29 100 ft, converted to metres at the
   end (the printed text stays the name); also with the triangle read beside it ("1501 A",
   "▲ 1720"; an "A" in front is a road's: "A 470"), with the summit's mark read
   in front or a stroke behind ("$10807", "=10876", "'10348", "10305L"; a minus
   only where no land lies that low: "-10876" ft, not "-348" m), or with one
   digit read as the letter it looks like, not in front ("108Q7" → 10807: O,
   Q, D for 0, I, l for 1); the box is then that of the digits, to centre the
   height under its name. A height with fewer digits than nearly all others and under
   half their median is a misread ("112" for a "1412" a grid line cuts); so is
   a three-digit one well under a sheet of four-digit heights in one thousand
   when the thousand added falls among them ("478" for 1478), unless other
   three-digit heights lie just under them (a fjord, an alpine valley); and,
   once the session holds the zoom-12/13 tiles under the located map, one
   more than 150 m outside the terrain around it
   (`diagnostics.spot_heights_off_dem`; a peak keeps its name). A peak takes
   the nearest spot height within six text heights.
4. *Names*. A trailing height becomes the elevation ("Sugarbush Hill 1523m"),
   a trailing route distance goes ("STILE 1.7Km", "2,3 km"), symbols at the
   ends go. "Main" and a feature that is no line is a name, not legend text
   ("Main Cave", "Main Camp"; "Main road" is). Not a name: under three letters or 60 % letters, letters and
   digits run together in most words ("GISV1$eet20x"), three letters or fewer
   with no vowel or with vowels only (Y aside: "EYE" is a name), or read up or
   down the map ("ERN", a piece of a turned label; four letters are a name as
   often: "ISAR", "TEES" along their rivers), a lone abbreviation of four
   letters or fewer ("KUMB.", cut by the frame).
5. *Mending* (`labels::mend_name`), with the sheet's own words as the
   dictionary (`labels::Vocabulary`): a *known* word is one of the lexicon
   (`LEGEND_WORDS`, `NOTE_WORDS`, `PLACE_WORDS`: feature words and their
   qualifiers in English and Afrikaans) or one of four letters or more read
   in two reads or more, accents aside.
   * A lexicon word read with an accent loses it ("GÁTE", "AČCOMMODATION";
     "AASVOËL" keeps its diaeresis, French "HÔTEL" and "RÉSERVE" their
     accents).
   * A stray glyph in front goes: a digit glued to a word ("1KWAMI LODGE"), a
     lone consonant before the words ("D LONG POOL", the pool's symbol; not
     initials; a lone vowel or Y only before a known word, else it is an
     article: "A CORUÑA", "O CEBREIRO", "Y GARN"), or a letter glued to a known
     word when the two cannot start a word ("WCASTLE", the waterfall's W;
     `plausible_onset` knows English, Afrikaans, Nguni, Sotho-Tswana, Welsh,
     Norse, Slavic and other European onsets: "FFOREST", "MTHATHA",
     "KGALAGADI", "KJERAG") or another read of the label at the same place is
     without it ("PLONG POOL" beside "LONG POOL"). A stroke (I, l) glued behind
     a known word likewise ("KLOOFI"; another letter makes another word:
     "RIVIERA", "MAINE", "HOOGE", "ROCKY").
   * A last word cut short is completed when exactly one lexicon word on the
     sheet starts with it, one to three letters longer ("EASY GUL" → "EASY
     GULLY" beside "WOODY GULLY", "CASCADE KL" → "CASCADE KLOOF", "PIHRANA
     POO" → "PIHRANA POOL"; a name read twice completes nothing: no "LIGHTNING
     RAND"), unless the word before it takes a name ("RIVER CAM", "RÍO PAS",
     "LOCH EIL", "ST LO").
   * A name the OCR was unsure of (confidence under 0.5; Vision gives 0.3 to
     its guesses) stays only when all its words are known ("PHANL GULLI"
     goes).
6. *Out of place* (`drop_out_of_place`): names that do not fit the map's
   lettering. A read with a letter no map holds but by the dozen (the stacked
   accents of Vietnamese) where few reads hold one (under 15 % of ten or
   more): "nDệ Dickı" on Gaia map A. A dotless ı of Turkish there is an i that
   lost its dot (Vision on the italic face): "Ntubenı Pass" is Ntubeni Pass,
   and "guide pickı", Vision's read of the small italic "(guide pickup)", goes
   by the next rule. On a map whose names start with a capital, a name whose
   first word is in lower case, with no capital ("uKhahlamba") and no
   particles before a capitalised word ("de Hoop", "van Reenen's Pass", "van
   der Stel Pass", "de la Rey", "an der Isar", "los Glaciares"): pieces of
   italic lettering ("ance", "ince" of "entrance", "picku", "lain Cave" of
   "Main Cave" half under the north arrow). On a map in mixed case, three
   capitals or fewer: amenity icons read as letters ("LAA").
7. *Pieces and misreads* (`drop_fragments`). A name is a piece of a read next
   to it when that read holds its text (exactly, or within one edit from five
   characters when the boxes overlap: the next line of a wrapped label is
   another word, "Ts'oana" under "Ntloana-") or their boxes all but coincide
   and they share most of their text; the longer read stays ("KUDUKONA" over
   "UDU", "1283" over "283"), and a piece of legend text goes with it. Two
   reads of one label (boxes that all but coincide, lengths within one
   letter, neither a piece of the other, at most a quarter of the letters
   apart: "MIDDLE RIDGE KLOOF" / "MIODLE RIDGEKIOOF", "HAMERKOP KLOOF" /
   "UAMERKOD KI OOF") keep the better read: the clearly more confident one (by
   0.25), else the one with more letters in known words ("KEEP RIGHT" over
   "KEED RIGHT", "LONG POOL" over "KONG POOL"), read alike by more reads
   ("10348" read twice over "10548"), in more words, the likelier printed of
   two spellings a confusion apart (see 10: "Oribi" over "Oribl", "Ntloana"
   over "Ntloano"), more confident, longer. An unreadable read ("21728")
   absorbs nothing; names numbered apart are two places ("RAND WATER BOX 1"
   one line over "RAND WATER BOX 2").
8. *Garbled turned labels* (`drop_garbled_turned`). Labels printed up, down
   or slanting along a stream are read worst: such a read with no known word
   whose box (widened by a quarter of the text height) meets another such
   read with no known word, that it shares no three characters with and that
   runs on from it (the two boxes together about as long, corner to corner,
   as both) is a garbled piece ("ROOIKRI", "ANTZ" of ROOIKRANTZ GULLY); so is
   one such read that is a lone feature word ("GULLY" of UPPER CASTLE GULLY).
   A turned name on its own stays ("GAUTENG", "CANDELABRA"), so does one read
   apart from its feature word ("HLOBANE" of HLOBANE KLOOF) and two names
   printed side by side along a border, whose boxes overlap
   ("KWAZULU-NATAL", "LESOTHO").
9. *Wrapped labels* (`join_stacked`). Screen maps wrap names over lines,
   centred ("Giant's" / "Castle", "Thaba-" / "Putsoa", "Bannerman" / "Hut"
   on Gaia GPS): lines across the map of about the same letter height (within
   0.7×), centred on each other within 0.6 letter heights, the lower one
   starting at most 0.6 letter heights under the upper one, are one name, when
   one of them is a single word (or the upper ends with a hyphen, which joins
   without a space) and they share no word ("NORTH PEAK" over "SOUTH PEAK",
   "RAND WATER BOX 1" over "… 2" stay two). The lower line may be legend
   words ("Hut", "Cave"). A height centred under the name gives it its
   elevation and makes it a peak. Words a read ran together are split at a
   capital when one is a feature word ("MountDurnford" → "Mount Durnford";
   not "KaMasihlenga", "uKhahlamba").
10. *Italic misreads* (`mend_feature_words`, `unify_spellings`). Gaia Topo
   letters its names in an italic sans whose one-storey "a" Vision reads as
   "o", its "l" as "i" or "f", its "rn" as "m". `labels::CONFUSIONS` lists
   such confusions as (read, printed, cost), by direction: 1 for a common one
   ("o" for "a", "m" for "rn", "l" for "i"), 1.25 for a plausible one ("i" for
   "l"), 2 for a rare one ("a" for "o"); never a free edit distance ("Boss" is
   no misread of "Pass"). In a name of two words or more, a word of four
   letters or more that is no lexicon word and one confusion (≤ 1.25) from
   exactly one feature word (`MENDABLE_FEATURES`: pass, castle, cave, peak,
   mount, bush, ridge, kloof, river, falls, lake, valley, gorge, hill, point,
   camp, …) becomes it, where a feature word stands: after the name, or first
   for a feature that leads names (mount, lake, river, castle: "Costle
   Gorge"): "Langalibalele Poss", "Giant's POss", "Giant's Costle", "Castie",
   "Castfe" → "… Pass", "… Castle"; "Poss" alone, "Cove" and "Coves"
   (lexicon words), "Red Paint", "Gregory Peck", "Howgill Fells", "Castell y
   Bere", "Hilt Farm", "Poot Kloof", "Volley Point", "Fake Lake" stay ("Loke
   Point" would become "Lake Point"). A feature noun with a "y" behind a
   proper name lost the "y" to a mark beside it ("Grysbok Bushy" → "Grysbok
   Bush"; "BIG ROCKY", "Rocky Point", "Cape Rocky" stay). Two spellings a
   confusion apart of one word in two names become the likelier printed one
   where the OCR made that confusion here twice already ("Songebethu" beside
   "Sangebethu" on a map with "Poss" twice) or ran letters together
   ("Dumford" beside "Durnford"): villages a letter apart are two places on a
   map read upright (Marton and Morton, Walton and Watton, Colne and Calne).
   Words are compared only when one confusion could explain them (at most two
   glyphs differ, or one place where one word is a glyph longer), so the
   names of a sheet of 2 000 reads take 0.2 s (`tests/names.rs`). The confusions
   seen mending and dropping misreads tell which glyphs Vision misreads on
   this map; one misread more than once (the italic "a" of "Poss" twice and
   "Costle") makes a name with that glyph in a word outside the lexicon
   *doubtful* (`PoiRead::doubtful`: "Mount Dumford", "Songebethu",
   "KaMasihlengo Pass" on map B): it may be spelled wrong, and
   `hint_queries` asks it after the sure names of its group. Proper names
   are never mended: there is no gazetteer to mend them by.

On the RapidOCR stand-ins of the Gaia pages the POIs are "Mount Durnford"
(peak, 3 294 m, printed 10807), "Langalibalele Pass", "Grysbok Bush",
"Bannerman Hut", "Ntubeni Pass", "Giant's Castle" (read "Giant's Castie",
mended; peak, 3 315 m) …, the heights in feet as metres; the hint queries
start "Mount Durnford", "Langalibalele Pass", "Grysbok Bush" (map A) and
"Mount Durnford", "Giant's Castle", "Ntubeni Pass", "KaMasihlenga Pass" (map
B); before, "Gaia GPS", "Gaia Topo Gaia GPS", "OpenStreetMap" and "Web
Mercator" led both, "Web Mercator|EPSG:3857" was a place and the heights
(10807 → 10 807 m) were dropped or kept in feet.

Apple Vision's own reads of the Gaia pages (`fixtures/gaia-*/vision-ocr.json`,
the app's OCRService on the page as MapImporter renders it, iOS 26.2
simulator; `tests/names.rs` pins them): map B (Giant's Castle) gives "Giant's
Castle" (peak, 3 315 m, its height read "=10876" and "-10876"),
"Langalibalele Pass", "Giant's Pass", "Bannerman's Pass", "Bannerman Hut",
"Ntubeni Pass", "Mount Dumford" (3 294 m, "108Q7"), "Ntloana-Ts'oana"
(3 262 m), "Thaba-Putsoa", "Legheku", "Songebethu", "KaMasihlengo Pass",
"Grysbok Bush", "Oribi Ridge Rte"; map A (Langalibalele) "Langalibalele
Pass", "Mount Durnford" (3 294 m, "$10807"), "Main Cave", "Grysbok Bush",
"Kambule" (3 154 m: "10348" read twice over "10548"), "Potterill" (3 141 m,
"10305L"), "Erskine", "Bond", "Katana", "Lango-le-Balele", "Umtshezi". Before,
map B read "Bannerman's Poss", "Langalibalele Poss", "Giant's POss", "Giant's
Costle" (no height), "Ntloano" (its second line taken for a misread of it)
and "LAA"; map A "Grysbok Bushy", "nDệ Dickı", "guide pickı", "ance", lost
"Main Cave" as legend text and gave Mount Durnford, Potterill and Kambule no
or the wrong height. Still misread, with no second read to tell: "Songebethu"
(Sangebethu), "Mount Dumford" (map A reads it right), "KaMasihlengo Pass"
(KaMasihlenga), "Lango-le-Balele" (Langa-le-Balele), "Legheku" (Leqheku).

On Vision's reads this leaves 54 POIs on Grootkloof (56 before), 53 on Castle
Gorge (57) and the same 33 on Mhlabatini; on the RapidOCR stand-ins no name
is lost. What stays wrong has no second read to correct it: "HAMERKOR GATE"
(HAMERKOP), "MARETEWA RIVER" (MARETLWANE), "KUDU KONAÍ", the height "2015"
(a 1700 contour label read across), and "ZIMBABWE" and "HAMERKOP", the first
words of the turned ZIMBABWE KLOOF and HAMERKOP GULLY. Not read at all:
MIDDLE RIDGE (taken for a piece of MIDDLE RIDGE KLOOF), CAVE (a legend word),
and turned labels lettered nearly title size (CAVE KLOOF, FERN KLOOF).

`hints::hint_queries` picks the names worth geocoding for a wide search
(`hints`), best first, at most 12; the app looks up the first 8 for hints, and
the "Search wider" sheet the first 4 to say how far away they are and pick a
radius, so the first four decide whether a far map is found:

1. *Titles*: lettering at least 1.6× the median font size across the map, and
   furniture lines ("MCSA MAGALIESBERG SHEET 7 OF 10" → "MAGALIESBERG"; their
   furniture words, numbers and short acronyms dropped), split at "&", "/",
   " - " and the like, mended (`mend_name`: "FERN KLOOFI" → "FERN KLOOF").
   Those another read on the sheet confirms come first (the title "CASTLE
   GORGE" and "CASTLE GORGE WATERFALL" on the map), larger first; then the
   others, less a misread of a confirmed one (one that shares 60 % of its
   letters in a run and whose other words are not known words: "DOTKLOOF"
   beside "GROOTKLOOF"; "KLEIN WINTERHOEK" beside "GROOT WINTERHOEK" and
   "DRAKENSBERG SOUTH" beside "DRAKENSBERG NORTH" are other places).
2. *Names*: the place, peak and water names of `classify_pois` (so none of
   its pieces, misreads or furniture), in groups: names of several words with
   a feature word a gazetteer files places by (`FEATURE_WORDS`: pass, peak,
   mount, hut, cave, lodge, hotel, school, falls, kloof, gorge, gully; pike,
   fell, gill, hause, beinn, llyn, piz, val, rifugio, …) where it names the
   place: after the name, or first for one that leads names (mount, lake,
   cerro, laguna, piz, beinn, …) or before "of" and the like ("Pike of
   Stickle"); first in a label otherwise it says what a thing is ("Hut
   Circles", "Mountain Rescue Post" in an OS sheet's small print, asked after
   "Scafell Pike"). Or of a summit with its height ("Giant's Castle" over
   "10876"); other names
   of several words; single words; routes ("Oribi Ridge Rte": rte, route,
   trail, track, path); lexicon words with a qualifier, a place name as often
   as a description ("LITTLE ROCK", "GREAT FALLS", "NEW FOREST", "GROOT
   RIVIER"; "LONG POOL"), last. In each group the names that may be
   misspelled (`doubtful`, item 10 above) come after the others, then
   summits first, then by letter size (a line across the map: its box's
   height, unless its letters are narrower than 0.4 of it; a wrapped label:
   the mean of its lines') and confidence; turned labels count 0.6×. Route
   notes ("KEEP LEFT", "TURN RIGHT AT SIGN"), names of lexicon words only
   ("KRAAL", "FARM GATE": they find no place; "Main Cave" is a name),
   credits, coordinates and heights are left out. An article in front stays
   ("DE AAR", "LA PAZ"); connectives at the ends go.
3. The same letters read twice are one query, spaced as some read spaces
   them ("CASTLEGORGE" → "CASTLE GORGE"); a query inside a longer one in text
   at least as large, or inside a word of one ranked as high ("ABATINI" of
   "MHLABATINI"), is a partial read; one with an acronym or a symbol glued to
   another query is a glued read ("MCSAMAGALIESBERG").

On Vision's reads: Grootkloof `["HAMERKOP", "GROOTKLOOF", "MAGALIESBERG",
"EASY GULLY", "GROOTKLOOF PEDESTRIAN GATE", "HAMERKOR GATE", "SUGARBUSH HILL
COUNTRY COTTAGE", "WOODY GULLY", "RIETFONTEIN VALLEY", "KWAMI LODGE",
"MILORHO LODGE", "SPARKLING WATERS HOTEL"]`, Castle Gorge `["CASTLE GORGE",
"MAGALIESBERG", "SHOOP MOUNTAIN LODGE", "GRASSY POOL", "MAGALIESBERG MOUNTAIN
LODGE", "CASTLE GORGE WATERFALL", "MARETLWANE BUSH SCHOOL", …]`, Mhlabatini
`["MAGALIESBERG", "MHLABATINI", "FERN KLOOF", "OLIFANTSHOEK PRIMARY SCHOOL",
"AASVOËL GULLY", "AASVOELKRANS RESORT", "MOUNT SERENE", …]`; Gaia map B
`["Giant's Castle", "Langalibalele Pass", "Ntubeni Pass", "Bannerman's Pass",
"Giant's Pass", "Bannerman Hut", "Mount Dumford", "KaMasihlengo Pass", …]`,
map A `["Mount Durnford", "Langalibalele Pass", "Main Cave", "Grysbok Bush",
"Kambule", …]`. Before the feature-word groups, lodges, hotels and resorts
came after "PARAMOUNT CONFERENCE", "BARTLETT FARM PARKING", "RAND WATER BOX"
and "BUSSHBREAK ACCOMMODATION", and the Gaia lists started "Grysbok Bush",
"Bannerman's Poss" (map B) and "Mount Durnford", "nDệ Dickı", "guide pickı"
(map A). Before the clean-up of Vision's misreads, "DOTKLOOF",
"KDO8PROPERTY", "ual Change per year West", "HTA FAHIN", "ARIG BEACON" and "B
KONG POOL" were among them, "DOTKLOOF" first.

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
   latitude and one longitude label (their medians, read by the grid's
   parser, `labels::parse_coordinate`, which takes a garbled DMS label when
   its cheapest readings agree), or a corner label
   holding both; the map centre lies along the offset from that labelled
   point to the image centre at the unknown scale, so the hint is the disc
   around that segment (scale range from an OCR'd "1:25 000" and the dpi, or
   0.4–12 m/px: a map with coordinate labels is no small-scale overview),
   plus 15 % of the offset for a map that is not north up and 2 km.
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
   map usually is, a fifth to a third of the cost (the 50 km disc's coarse search takes
   ~1–3 s, the 20 km one ~0.3–0.6 s), so a user near the sheet waits no
   longer than with the old 20 km default; without a confident match there,
   the whole disc follows, a scale prior's band first, then the coarser bands
   before the near zoom's (*Any map: scales, priors, units*). In a hint or
   near area the fit gap among its refined candidates counts as search
   evidence (`refine::select_refined`); a
   match that only this gap makes acceptable must also stand out locally
   like a wide match (its zoom-11 re-check below peaks at ≥ 2.2; see
   *Adversarial checks*: without it the phone photo was placed 137 and 145 km
   off at confidence 0.82 and 0.999).
   *Lines unlike contour lines* (`georef::line_organisation`): contour lines
   nest, the next line runs alongside, while the lines the detector finds in
   a photo, hillshade, tints, drainage or hachures meet their neighbours at
   any angle. Of up to 5 000 samples the nearest sample of another segment
   within 1/40 of the image's short side is looked up; the share running
   within 25° of the sample's line is 0.45–0.98 on every map the contour
   benchmark locates, 0.17–0.74 on the reviewer's 18 non-maps that have
   lines (10 under 0.40), 0.25–0.51 on flat-terrain maps that cannot be
   located. With no scale printed or drawn, nothing found in the inner 20 km
   and fewer than 0.40 parallel (`LINES_MIN_PARALLEL`), the search stops
   there ("No match within 20 km of you (…). The lines on this image don't
   run like contour lines, so it wasn't searched farther.",
   `search.searched_radius_m` 20 km, `diagnostics.search_stopped`), wide
   search included: those non-maps fail in 0.3–0.7 s instead of 2.7–4.1 s
   (GPS at the reviewer's place and 100 km off, alternating with the build
   before). Photos (their own mask) are not judged.
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
(`diagnostics.wide.min_scale_m_per_px`). These rules are the near zoom's
scale band's (up to 12 m/px); a small-scale band (*Any map*) is planned the
same way from its own scales and map extent, up to two zooms coarser
(`wide::wide_band_reach`: zoom 7 for a 300-dpi page at 12–30 m/px), and its
chunks are searched in the same sweep, nearest first (`wide::merge_plans`).
The measurements below are of the near zoom's band alone.

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
sheet. (These times are of the build that introduced the wide search; the
search has since become ~2× cheaper: the same 1 000 km from Windhoek fails
in 14.0 s of core time with the scale label, 30.6 s without; see
*Estimate*.)

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
* **Far north, antimeridian, small maps, offline**: a synthetic sheet 120 km
  from Longyearbyen (78°N) is found with the near disc at zoom 9 and the
  wide search at zoom 7; a fix at 179.3°E finds a sheet at 179.75°W and plans
  as many tiles as anywhere else; a 1.5 km map over 1 000 km searches 81
  tiles (was 18 147 planned) and says it was searched to 50 km; with every
  wide tile missing the answer says the terrain couldn't be downloaded
  instead of "no match within 300 km". The author's runs above (Polokwane,
  Durban with and without the scale, Windhoek, the photo from Durban)
  reproduce tile for tile.

**Estimate** (`mm_search_estimate`, `wide::estimate`): the plan the search
would make for the map, in the search's order — hint areas (at most 8 after
ordering); the near disc (the inner 20 km of the fine band, the scale
prior's bands, the coarse bands, the fine band's whole disc); each band's
wide chunks out to its reach (`wide_band_reaches`, as `wide_setup`), zooms
for latitude — plus measured allowances for candidate checks (10 near-zoom
tiles per chunk for its re-checks, 14 zoom-13 tiles per refinement, 1.5
refinements a search: only a candidate that stands out is refined, 0–3 in
the runs below). Bytes from the tile weights above. Compute from each coarse
search's planned work (`search::search_work`: per DEM level of its scale
plan, templates + 2 FFTs of its tiles' cells, in millions): per hint or near
area 4.9 ms per million cells + 33 ms + 3.5 ms per megapixel of the map; per
wide chunk 6.2 ms per million cells + 0.20 s + 4.9 ms per megapixel (its
re-checks included); 30 ms per megapixel of contour extraction that the
search waits for; 0.35 s per refinement; times 1.5 for an iPhone. Download
at 2 MB/s plus 0.5 s per round.

The request takes `radius_m`, `gps`, `hints` and `max_tiles_per_round` as
the search does, and the map: `image_size` `[w, h]` in pixels (default a
300-dpi A3 sheet, 3 507 × 4 960), and its scale prior, if any: a printed
scale's ground m/px range `scale_m_per_px` `[lo, hi]` (its bands alone), a
scale bar's `scale_bar_m_per_px` (its bands first, then the default range),
or `"scale_known": true` (the typical sheet's 1:25 000 at 300 dpi, ±25 %).
Both ranges are what a search reports in `diagnostics.scale_range`. The
answer also gives the planned work (`search_work`, of it
`search_work_wide`, millions of FFT cells).

Measured against complete searches (`wide_measure::estimate_check`; the
fix at Windhoek, or Nairobi for 2 000 km, so that every map is out of reach
and every search runs to its end; tiles from a local cache; the reference
box at low load; core time; estimate = iPhone compute / 1.5):

| map | px | scale prior | R km | tiles meas / est | MB meas / est | rounds meas / est | core s meas / est |
|---|---|---|---|---|---|---|---|
| Grootkloof scan | 3 507 × 4 960 | none | 50 | 75 / 91 | 3.4 / 2.8 | 5 / 5 | 2.0 / 2.1 |
| | | | 250 | 190 / 216 | 9.1 / 10.7 | 11 / 11 | 6.7 / 6.8 |
| | | | 1 000 | 1 212 / 1 154 | 64.9 / 78.5 | 39 / 35 | 30.6 / 29.9 |
| | | | 2 000 | 3 368 / 3 337 | 208.9 / 239.9 | 90 / 87 | 81.3 / 80.3 |
| Grootkloof scan | 3 507 × 4 960 | label | 50 | 73 / 70 | 2.9 / 1.9 | 4 / 4 | 2.0 / 1.4 |
| | | | 250 | 167 / 166 | 7.3 / 7.9 | 9 / 9 | 3.8 / 3.3 |
| | | | 1 000 | 993 / 995 | 53.9 / 68.5 | 30 / 30 | 14.0 / 13.0 |
| Gaia page | 3 508 × 2 480 | bar | 50 | 124 / 105 | 5.1 / 4.1 | 6 / 6 | 1.9 / 1.7 |
| | | | 250 | 240 / 207 | 10.3 / 9.9 | 12 / 12 | 6.5 / 6.1 |
| | | | 1 000 | 1 245 / 1 131 | 64.4 / 76.6 | 36 / 36 | 29.3 / 27.2 |
| | | | 2 000 | 3 589 / 3 314 | 218.3 / 237.9 | 89 / 87 | 73.0 / 72.5 |
| Gaia raster | 1 118 × 791 | none | 50 | 74 / 95 | 3.4 / 3.1 | 4 / 5 | 0.9 / 1.3 |
| | | | 250 | 227 / 239 | 9.8 / 10.9 | 13 / 14 | 5.4 / 5.9 |
| | | | 1 000 | 1 391 / 1 300 | 72.3 / 79.1 | 55 / 56 | 32.8 / 29.7 |
| | | | 2 000 | 4 017 / 3 809 | 228.0 / 243.5 | 152 / 151 | 83.7 / 83.7 |
| sparse Rockies | 2 118 × 1 714 | bar | 50 | 86 / 107 | 4.2 / 4.2 | 4 / 5 | 0.9 / 1.3 |
| | | | 250 | 215 / 239 | 10.3 / 11.9 | 12 / 12 | 5.0 / 5.6 |
| | | | 1 000 | 1 494 / 1 415 | 88.7 / 94.2 | 47 / 44 | 31.0 / 29.0 |

Over all 28 runs (these and 120 and 500 km): from 120 km on (23 runs)
core time −14 … +16 % (mean |error| 7 %), tiles −14 … +24 %, megabytes
−7 … +31 % (a tile's bytes depend on its terrain: the sea compresses to
almost nothing, so the estimate's average weights overstate a coast), rounds
−11 … +10 %. A 50 km search takes 1–2 s, and its refinements (0–3 at
~0.35 s; 1.5 counted) swing that by ±0.5 s: −31 … +49 % there (within
0.6 s), tiles −15 … +28 %, megabytes −35 … +1 %.

From Pretoria, a typical sheet, scale unknown (in brackets: with a scale
label, `"scale_known": true`):

| R km | zoom | tiles | MB | rounds | chunks | ~s on an iPhone (compute + download) |
|---|---|---|---|---|---|---|
| 20 | 11 | 60 (37) | 1.8 (1.0) | 4 (3) | 0 | 5 (2.1 + 2.9) (4) |
| 50 | 11 | 89 (70) | 2.6 (1.9) | 5 (4) | 0 | 7 (3.1 + 3.8) (5) |
| 100 | 10 | 191 (134) | 7.1 (4.5) | 9 (7) | 3 | 15 (7.1 + 8.0) (10) |
| 250 | 9 | 220 (168) | 11.0 (8.1) | 11 (9) | 5 | 21 (10.2 + 11.0) (13) |
| 500 | 9 | 428 (361) | 26.2 (22.1) | 16 (14) | 10 | 39 (17.6 + 21.1) (26) |
| 1 000 | 9 | 1 195 (1 027) | 82.2 (71.3) | 35 (30) | 29 | 103 (44.6 + 58.6) (70) |
| 2 000 | 9 | 4 094 (3 702) | 295.9 (269.7) | 103 (93) | 97 | 343 (143.8 + 199.4) (245) |

(A whole search is planned; one that finds the map stops early, so these
are upper bounds. The previous model, from mosaic cells of the fine band and
two refinements per area and band, gave 15 s for 50 km and 131 s for
1 000 km: about twice the time a search now takes.)

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
smaller tiles); the estimate takes the map's size and scale prior when the
request gives them (else a typical sheet, no scale), but not photo mode
(slower refinements) nor the terrain's bytes.

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
  src/content.rs      the map's content area on its page (a small map on a large sheet)
  src/contour_lines.rs  the contour mask of any map style: analysis level (upsampling,
                      pixel repetition), two-sided line detector, ink colour, ink model
                      choice, run-based components
  src/skeleton.rs     thinning, junction removal, segment extraction, sampling
  src/search.rs       coarse orientation-field FFT search
  src/refine.rs       contour-constancy least-squares refinement (similarity, affine,
                      projective for photos and large maps, a large sheet's projection
                      bend) + scoring
  src/warp.rs         photo warp: offset lattice for paper that is not flat, fitted to
                      traced grid lines and contours, held-out checks, inverse
  src/page.rs         page (paper sheet) finder for photos: mm_detect_page
  src/dark.rs         dark map: paper grid + flat field, OKLab tone map, 65³ LUT, tile render
  src/labels.rs       coordinate / scale / POI label parsing, height units
  src/scalebar.rs     scale bars: tick labels or a distance label and its measured bar
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
                      wide_measure.rs: ignored network measurements;
                      sheets.rs: the Castle Gorge and Mhlabatini sheets; stress.rs:
                      the grid under simulated phone-OCR damage; gaia_contours.rs: the
                      user's Gaia GPS exports, smooth and blocky page renders and the
                      native raster, the app's requests near and from 450 km; gaia.rs:
                      their scale bars, names, feet, the 23 m/px raster; styles.rs:
                      the synthetic maps of many styles in fixtures/styles;
                      map_on_page.rs: a Gaia raster unscaled on an A4 page, blank
                      and text pages)
fixtures/grootkloof/  the real map (PDF + extracted JPEG), DEM tiles, OCR fixture, truth
fixtures/castle-gorge/, fixtures/mhlabatini/  two more sheets of the series: PDF, JPEG,
                      DEM tiles, truth, RapidOCR (ocr.json) and Apple Vision OCR
                      (vision-ocr.json); fixtures/tools/ builds them
fixtures/gaia-langalibalele/, fixtures/gaia-giants-castle/  the user's Gaia GPS PDF
                      exports: page renders, native rasters, OCR, truth, DEM tiles
fixtures/styles/      synthetic maps of 12 styles with exact truth (case.json in the
                      common benchmark format); fixtures/tools/synth_topo.py renders
                      them, the 76-map corpus and the regional and pages suites,
                      page_compose.py puts a case's map on a larger page,
                      bench_contours.py runs the benchmark
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
* Finding a map farther away ("Search wider", see *Wide search*). The first
  search covers `AppSettings.searchRadiusKm` (50 km by default, 10–100 km in
  Settings; a stored 20 km, the old default never picked on purpose, becomes
  50 km). When it finds no match the import result and the Library's
  "Search wider…" open `SearchWiderSheet`: 100, 250, 500 or 1 000 km (60–600 mi)
  or a custom 10–2 000 km, the smallest preset at least 2.5× the radius
  already searched selected; what it will download from `mm_search_estimate`
  ("Wi-Fi recommended" from 25 MB), and on an expensive path (mobile data,
  hotspot) Start asks first above 50 MB; "Use names on the map" on by
  default. As the sheet opens it looks up the first 4 names on the map
  (`HintGeocoder.namedPlaces`: each name's place nearest you, anywhere within
  2 000 km) and lists them with their distance ("Giant's Castle, 452 km
  away"), since a search checks only the places within the distance picked.
  Unless a distance has been picked by then, it selects the smallest that
  reaches the largest group of them lying within 150 km of each other, plus
  the 15 km circle checked around each (`WideSearch.suggestedRadiusKm`: a
  preset, else a custom distance rounded up to 10 km or 10 mi, at most
  2 000 km; nothing when they lie within the radius already searched), and
  says under the list whether the distance picked reaches them. After the
  first search the result screen names the first of them ("Search wider to
  look for “Giant's Castle” and other places named on the map"); names in
  capitals are put in title case, mixed case is kept as printed.
  `WideSearch` holds those limits and the outcome kinds
  (`GeorefFailure`: no terrain match, stopped at the tile limit, no
  position, no contours, terrain unavailable, …) the result screen words
  its message and buttons from. The search itself (`GeoreferenceService.run`):
  * names first: unless the names are known from the first search, one
    grid-only core call lists them (`hint_queries`); a grid answer of
    confidence 0.5 or more ends the search there (`probeMinConfidence`: a
    flagged one-label fit, which the first search's terrain may have ruled
    out, does not). `HintGeocoder` looks up the first 8 with Apple Maps
    (`MKLocalSearch` biased to the search circle, points of interest,
    addresses and on iOS 18 physical features; 3 at a time, 4 s each, a
    lookup that ignores cancellation is abandoned after a second's grace),
    keeps each name's two nearest places within the radius, round-robin
    over the names, skips places within 5 km of an earlier one, at most 12
    hints of 15 km. Offline (`NetworkPathObserver`, an `NWPathMonitor`:
    online, expensive, constrained) the lookups are skipped;
  * then `TerrainPullLoop` answers the core's `need_dem` rounds with
    `DEMService` for as long as the core makes progress (asks for tiles it
    has not had, or its `search` status reaches farther, fetches more, tries
    more hints or changes stage): no fixed number of rounds. Downloads stop
    at the tile cap (600 for the first search, 2 500 or 1.5× the estimate
    for a wide one) or when the first two rounds bring nothing (offline);
    the tiles asked for are then marked missing so the core can finish with
    what it has (at most 40 more calls). Asking only for tiles it already had
    three times in a row without progress ends the loop (stalled), and 2 000
    calls is the absolute bound;
  * `DEMService` downloads at most 6 tiles at once, retries a transient
    failure (timeout, dropped connection, 5xx, 429) twice after 0.4 and 1.2 s
    (offline and 4xx fail at once), and keeps its disk cache
    (`Caches/dem/z/x/y.png`) least-recently-used under 400 MB: a hit
    refreshes the file's date, and every 8 MB written a background trim
    brings it back to 80 % of the cap.

  The import screen shows the stage and the core's `search` progress
  ("Searched 320 of 1 000 km"), and a failed wide search says what was
  searched.
* Safe areas: a full-screen cover that SwiftUI presents while another
  presentation is still opening or closing can be laid out with a zero safe
  area (on a device the corner editor's Cancel / Done were drawn under the
  status bar, out of reach). `RootView` therefore presents the
  import cover only once `PresentationGate.settled()` finds no view
  controller in the app's windows in a presentation or dismissal transition
  (each waited for to its end, at most 2 s), and the chrome of full-screen
  surfaces does not rely on SwiftUI's safe area alone
  (`Views/Common/WindowSafeArea.swift`): `windowSafeAreaPadding()` (corner
  editor, progress screen, calibration, the Search wider sheet's Start bar)
  and `SafeAreaReader` (map screen) measure the view's frame in its window
  with a UIKit probe and add whatever part of the window's unsafe area
  (`UIWindow.safeAreaInsets`) SwiftUI's safe area left uncovered, re-measured
  on resizing and rotation and once the presentation it came in with has
  finished. The corner editor's header buttons are large controls (at least
  44 pt tall). Debug builds launched with `-importSamplePhotoAfterSheet`
  open a stand-in for the photo picker whose pick starts the sample photo's
  import as the sheet closes (`PhotoImportUITests`); `-mockPlaces
  "Name=lat,lon;…"` replaces Apple Maps' place lookups with a fixed table
  (`SearchWiderUITests`).
* PDFs are rendered at 300 dpi with `interpolationQuality = .high`, set before
  the page is drawn: rasters embedded in the page (a GPS app's export is one
  ~100 ppi image without /Interpolate, scaled about 3×) are resampled
  smoothly rather than in nearest-neighbour blocks. `PDFRasterRenderTests`
  renders a hand-made PDF with a 1 px line in a 100 ppi image, checks the
  upscaling is smooth and unbiased and attaches the measured profile next to
  nearest, bilinear, bicubic and Lanczos predictions (poppler's `pdftoppm`,
  which the corpus tools use, is bilinear below 4× with a half-pixel shift).
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
  the sheet waits as before). The contour mask (`contour_lines`: level
  estimate, line detector, ink, model choice) takes ~0.25–0.45 s on a 17 MP
  300 dpi sheet at level 1 (the colour threshold it replaced: ~0.2–0.3 s;
  ~0.7–0.9 s against ~0.55 s on one core), while the thinner mask makes the
  skeleton ~10–20 % cheaper; an A4 page analysed at level 3 takes ~0.15 s for
  the mask and a fifth of the full-resolution skeleton and field time. On the
  style corpus a located map costs 0.9–3 s of core time with the GPS at its
  centre (back to back with the previous mask: −20 % to +45 %, the extra from
  the mask and from refining the contour samples the old mask missed, up to
  several times as many). The whole contour path against c66236b, the two
  builds alternating one process at a time on 25 maps (19 synthetic ones of 14 styles,
  the MCSA sheets, the Gaia pages and a Gaia raster): maps located 1.05 →
  0.76 s median, 4.4 → 3.6 s p95, 3.9 s at most (each within the 4 s); a
  50 km search that cannot find the map (the fix 90–110 km away) 4.9 →
  4.3 s median, 9.0 → 7.8 s p95 on the 19 maps both builds search (the
  fine-level sample cap pays for the coarse scale band and the scale bar's
  second pass), while maps the old mask found no contours on now take 3–6 s
  to fail instead of 0.3 s. Then the search's own cost (two templates per
  inverse transform, block-wise peak picking: `search.rs`; refinement only of
  candidates that stand out; the coarse band before the near zoom's 50 km
  disc; a stop at 20 km for lines unlike contour lines: `georef.rs`), the
  whole contour benchmark (97 maps × GPS at the centre, 10–30 km off and
  90–110 km off) against 725018e, the two builds alternating one process at
  a time (load 2–5 from other jobs), same verdicts run for run, none
  confidently wrong:

  | runs | old p50 / p95 / max | new p50 / p95 / max |
  |---|---|---|
  | located (173) | 0.90 / 2.68 / 6.2 s | 0.79 / 1.54 / 3.4 s |
  | failed (112) | 5.80 / 9.02 / 10.8 s | 2.24 / 3.71 / 6.7 s |
  | GPS 90–110 km off (97) | 6.01 / 9.22 / 10.8 s | 2.31 / 3.71 / 6.7 s |

  Every located run now within 4 s (169 of 173 before), 171 of 173 within
  4 s on an iPhone (×1.5: 161 before), 288 of 291 runs of any kind within 4 s
  (193 before); CPU time of a failing run 10.7 → 4.9 s median. The
  black-and-white photocopy (2 678 × 3 396 px, 5 m/px) 6.2 → 3.4 s with the
  GPS 22 km off and 7.0 → 3.2 s 98 km off (11–18 s under load before); the
  23 m/px native Gaia raster 5.2–5.4 → 0.9–1.1 s. The reviewer's non-maps
  (41 images: photos, noise, blank pages, hillshade, tints, drainage,
  hachures, roads; GPS at the place and 100 km off; alternating the same
  way, load ~3): 2.18 → 0.29 s median, 3.90 → 1.74 s p95, 4.5 → 2.7 s at
  most, none located (the old build placed a photo of a coffee cup at
  confidence 0.52); those whose lines do not nest 2.7–4.1 → 0.3–0.7 s, the
  others searched 0.6–4.5 → 0.6–2.7 s (photos of objects whose edges
  happen to run in parallel: a brick wall, a cup; a 48 MP page whose
  terrain is not there; noise images unchanged at 0.6–2.3 s, their
  skeleton's time). Wide search on the reference box: see the tables
  under *Wide search*
  (a whole 1 000 km search that fails: 14 s of core time with a printed
  scale, 29–33 s without, ~20–50 s on an iPhone plus the download; the
  estimate's compute within ±16 % of it from 120 km), at most one
  chunk mosaic (≤ 8 × 8 tiles) in memory. Photo mode adds the normalisation (~0.1 s for 9 MP) and costlier
  line and mask extraction: ~0.9 s grid and ~2 s contour path on the CI-class
  Linux box (4 cores), < 6 s even when three candidates must be refined. The
  warp adds ~0.5 s of fit, plus the contour products in the grid path (one
  more `need_dem` round) and the line detection in the contour path: ~1.3–1.5 s
  grid and ~2.4–2.7 s contour path on the real photo. Finding the map on its
  page (`content.rs`) takes 5–90 ms (one pass over the pixels, 1–17 MP), once
  per session; a small map on a page then costs what the map alone costs
  (a Gaia raster on an A4 page: 1.1 s instead of 1.6 s, the page's footprint
  no longer refined). A large sheet's projection bend adds 0.3–0.9 s to its
  refinements (mostly the cross validation, which a wrong place skips).
* Map interaction at 60/120 fps, because tiles are drawn off the main thread
  by CATiledLayer and overlays are cheap CAShapeLayers.
* Release core: `opt-level=3`, LTO, `codegen-units=1`, `panic=unwind` (needed
  to catch panics at the FFI boundary).
