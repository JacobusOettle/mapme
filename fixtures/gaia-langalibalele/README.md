# Gaia GPS export – Langalibalele Pass / Mount Durnford (Gaia Topo, feet, 1 mi bar)

A map the user exported from **Gaia GPS** and reported "doesn't work" in the app (together
with `fixtures/gaia-giants-castle`): the uKhahlamba-Drakensberg escarpment on the
KwaZulu-Natal / Lesotho border around Langalibalele Pass, Mount Durnford and Grysbok Bush,
relief 1 800–3 300 m. Map data © Gaia GPS, © OpenStreetMap contributors. **User-supplied,
private test fixture**: keep it in this repository's tests, do not redistribute.

It is the first fixture that is not an MCSA sheet: a consumer app's screen-style map, north-up
Web Mercator, with thin, faint light-brown contours every **80 ft** (24.384 m) over
hillshading, a green tint over the park (east of the escarpment), a dense network of thin blue
streams, grey dashed trails, summit triangles with spot heights in feet (Mount Durnford
10807, Erskine 10328, Bond 10354, Potterill 10305, Kambule 10348, Katana 10092, Thaba-Ntso
10807) and no graticule or grid. 9.10 m/px natively, 3.03 × 3.08 m/px on the 300 dpi page.

## The PDF

`gaia-langalibalele.pdf` (as supplied): jsPDF 4.2.1, one A4 landscape page (841.89 × 595.28 pt),
no GeoPDF / georeferencing metadata. Vector text: "Gaia Topo (feet): © Gaia GPS,
OpenStreetMap", "Web Mercator | EPSG: 3857", "Declination: -26°", a north arrow and the
scale bar ("1 mi" = 127.685 pt). The map itself is **one embedded RGB raster, 1118 × 791 px**
(`/I0`, uncompressed, no `/Interpolate`, i.e. false), drawn with

    q 806.4 0 0 561.6 18. 15.68 cm /I0 Do Q

i.e. at 99.8 ppi across and 101.4 ppi down: jsPDF stretches the raster into a box of another
aspect ratio, so **the page is 1.59 % anisotropic** (one raster pixel is 3.0054 page px wide
and 2.9583 px tall at 300 dpi) although the raster itself has square Web Mercator pixels. A
white legend band (page y 15.68–40.88 pt) is drawn over the bottom 35.5 raster rows, the Gaia
logo over its top-left corner and the north arrow over its top-right corner.

## Files

| file | what |
|---|---|
| `gaia-langalibalele.pdf` | the export as supplied |
| `map.jpg` | the page as the app renders it (300 dpi, 3508 × 2480), raster upsampled smoothly (bicubic) – **the app's input** |
| `map_nearest.jpg` | the same with the raster upsampled by pixel repetition (nearest neighbour) |
| `native.png` | the embedded raster, pixel for pixel (1118 × 791) |
| `native_q25.jpg`, `native_crop50.jpg` | `native.png` saved at JPEG quality 25 (then 88), and its central 50 % (559 × 395 px from (279, 197), quality 88): the adversarial review's partial-coverage repros (`core/tests/gaia_contours.rs`, `*_is_honest`); at quality 25 the contours survive only in the western 40 % |
| `truth.json` | pixel → Web Mercator for `native.png` and for the page, how it was measured, the checks |
| `case.json`, `case_nearest.json`, `case_native.json` | benchmark cases (common case format, `fixtures/tools/bench_contours.py`) for `map.jpg`, `map_nearest.jpg`, `native.png` |
| `ocr.json`, `ocr_native.json` | RapidOCR stand-ins for Apple Vision on `map.jpg` / `native.png` |
| `dem/` | Terrarium tiles |
| `render_page.py` | makes `map.jpg`, `map_nearest.jpg` and `native.png` from a Gaia PDF (also used for `fixtures/gaia-giants-castle`) |

### map.jpg: what the app renders

`MapImporter.renderPDF` draws page 1 into a 3508 × 2480 sRGB bitmap context (300/72 px per pt,
long side ≤ 6000 px; top page edge on the first row), white fill, `interpolationQuality = .high`,
then `drawPDFPage`. `render_page.py` reproduces that: MuPDF renders the vector content and the
logo (two layers with alpha: what is drawn before and after the raster), PIL resamples the raster
into its exact placement (page px 75–3435 × 75–2415, whole pixels at 300 dpi) and the layers are
composited in content-stream order. JPEG quality 92, 4:4:4:

    python render_page.py gaia-langalibalele.pdf native.png map.jpg map_nearest.jpg   # numpy pillow pikepdf pymupdf

(`pdftoppm -r 300` gives 3508 × 2481 and a bilinear raster; otherwise the same picture.)

**Smooth or blocky?** The raster has `/Interpolate false` and is upscaled 3×. Not verifiable
here (no CoreGraphics on Linux), so reasoned: the flag would reach CoreGraphics as the image's
`shouldInterpolate`; how a bitmap context set to `interpolationQuality = .high` (as in
`renderPDF`) treats such an image is not documented, but Preview / PDFKit, which draw pages
through the same `drawPDFPage`, are widely reported to show low-resolution images in PDFs smoothed
(blurred, not blocky) when zoomed, `/Interpolate false` or not (a common complaint about
pixel-art and `interpolation='none'` matplotlib figures viewed in Preview).
So **`map.jpg` (smooth, bicubic) is what I believe the app analyses**; `map_nearest.jpg` is the
alternative if CoreGraphics honoured the flag. Bicubic vs. CoreGraphics' own `.high` kernel
differ in detail, not in kind. A dump of the importer's render on the iOS simulator (the CI
E2E test already renders the MCSA PDFs) would settle it. It matters: the HEAD core locates
`map_nearest.jpg` and fails on `map.jpg` (baseline below).

### truth.json: how it was measured (independently of the core)

Native pixel (continuous coordinates, pixel *i* covers [*i*, *i*+1), as the core) → Web Mercator is
an isotropic axis-aligned affine `X = X0 + s·x, Y = Y0 − s·y`; the page affine follows from the
`cm` placement (`page.pixel_to_mercator`, anisotropic). The truth is the consensus (mean scale,
mean centre) of four estimates, each a full fit of its own:

1. **hillshade_terrarium** – the map's hillshade (luminance, 5 px median against the thin lines,
   band-passed) against a hillshade computed from Terrarium z13, normalised cross-correlation
   maximised over translation, scale and the light (best: from 320°, 66° up);
2. **hillshade_copernicus** – the same against **Copernicus GLO-30** (TanDEM-X based: an
   independent DEM), reprojected to a z14 Web Mercator grid;
3. **contour_layer_terrarium** – the contour lines alone: pixels of the brown line layer
   (colour line detector), the Terrarium elevation there, phase coherence modulo the interval,
   maximised over translation and scale. This also measured the interval: the coherence peaks at
   24.35–24.40 m = **80 ft** (40, 50, 100 ft and 10/20/25 m are flat);
4. **via_map_b** – this map registered directly onto `fixtures/gaia-giants-castle` (the same
   Gaia data at 2.524× the scale; NCC 0.85, 12 windows rms 0.54 px) and composed with that
   map's truth, whose scale is much better determined (it spans 26 km, its estimates agree to
   0.015 %).

Agreement and checks (native px; 1 px = 9.1 m):

| check | result |
|---|---|
| estimates vs. truth at the 25 truth points (max / rms) | hillshade Terrarium 1.12 / 0.68, Copernicus 0.87 / 0.67, contours 0.89 / 0.52, via map B 0.95 / 0.60; anisotropic hillshade fits 1.41 / 0.83 (Terrarium), 1.16 / 0.73 (Copernicus); scale spread 0.19 % |
| 12 windows, each registered alone against the DEM hillshade (translation only) | Terrarium rms 0.80, max 1.17; Copernicus rms 0.90, max 1.78 (the two DEMs differ by ~1 px ≈ 10 m in absolute position here) |
| summit icons vs. the highest Copernicus cell within 210 m | Thaba-Ntso (1.0, −2.0) px, DEM 3294.7 m vs. 10807 ft = 3294.0 m; Mount Durnford (0.5, −3.2), 3295.3 vs. 3294.0 m. Escarpment-edge icons (Erskine, Bond, Potterill, Katana) find a higher cell on the plateau behind them (4–7 px); Lango-le-Balele (no height) 18 px. Gaia places the same summit's icon 3 px north of it here and 7 px on map B: icons check identification and scale, not the last pixel |
| printed scale bar (1 mi = 127.685 pt) | implies 9.091 m/px, truth 9.103 (+0.13 %); map B's bar is 0.19 % off a far better fit: the bars are good to ~0.2 % |

The estimates span 0.19 % in scale (the map's own hillshade fits smallest, the transfer from
map B largest) and the two DEMs disagree by ~1 px in absolute position, so the truth is good to about **±1 native px
(±9 m; ±3 page px)**. The raster's pixels are square: the anisotropic fits differ by 0.2 %
(hillshade) but the direct registration onto map B by 0.04–0.08 %.

The core's own answer on `native.png` (GPS at the centre: 9.105 m/px, confidence 0.968) is
7.7 m (median) / 9.2 m (max) from this truth at the truth points, i.e. within it.

### ocr.json, ocr_native.json

Stand-ins for Apple Vision: `python ../tools/ocr_like_app.py map.jpg ocr.json` (and on
`native.png` for `ocr_native.json`): RapidOCR over the regions the app's `OCRService` reads
(1600 px tiles overlapping 15 %, plus rotated passes over the side strips), boxes in the image's
pixels, unedited (it garbles and splits labels, not the way Vision does). The page OCR reads the
attribution and projection lines too ("Gaia Topo (feet): © Gaia GPS, OpenStreetMap", "Web
Mercator | EPSG: 3857"), which the core turns into hint queries (baseline below).

### dem/

Terrarium tiles (`{z}_{x}_{y}.png`, AWS open data), 3.2 MB:

```sh
python ../tools/fetch_dem.py dem --box 11 -29.6565 29.0522 -28.9630 29.8780 \
  --box 13 -29.3691 29.3819 -29.2505 29.5483 --box 10 -29.6116 29.1037 -29.0079 29.8265 \
  --box 9 -29.6116 29.1037 -29.0079 29.8265
```

z11 within 35 km of the map (25 tiles), z13 under it ± 3 km (20 tiles, what the refinement asks
for), z10 (9) and z9 (4) within 30 km, the tiles a wider search pulls around the map. Tiles the
core asks for beyond these are marked missing, as when the phone is offline (the near search
from the centre asks for 30 zoom-11 tiles first, 25 of them here).

## Baseline: HEAD 5c5702e, `core/examples/georef_cli`

`georef_cli <image> --dem-dir dem --ocr <ocr> [--dpi 300] --gps …` (methods grid,contours, 50 km
radius unless noted). Truth: map centre (−29.308291, 29.465061); 10 km NE (−29.244772, 29.537905);
60 km E (−29.308291, 30.083166); Magaliesberg (−25.80, 27.45, ~430 km) with a 2 000 km radius
and a hint at Mount Durnford (−29.30612, 29.43885) standing in for a geocoded place name. Errors
at the 25 truth points, median / max.

| image | GPS | outcome | error | diagnostics |
|---|---|---|---|---|
| map.jpg | centre | **failed**: "Terrain for much of the search area couldn't be downloaded (39 of 64 tiles), and the rest holds no match" | – | colour model *saturated* (sat ≥ 0.25), mask 0.013, 116 segments. The coarse search **finds the map** (candidate 0: 3.045 m/px, score 0.53, peak ratio 3.5, 0.13 km off) but its refinement ends at fit confidence **0.0**; the best fit (0.44) is elsewhere |
| map.jpg | 10 km NE | failed (same) | – | same candidate 0 (10.06 km from the fix), refined to 0.0 |
| map.jpg | 60 km E | failed (49 of 64 tiles missing, no match) | – | expected (outside 50 km) |
| map.jpg | 60 km E, 200 km radius | failed (95 of 114 tiles missing, no match) | – | the zoom-9 wide search re-checks the right place (re-check ratio 7.6) but its refinement is again 0.0 |
| map.jpg | Magaliesberg, 2 000 km, hint | failed | – | hint area: the right candidate (3.05 m/px, peak ratio 3.8) refined to 0.0; wide search likewise |
| map_nearest.jpg | centre | **ok**, confidence 1.0, affine | 14.0 / 31.5 m | colour model *default*, mask 0.071, 462 segments, refined at z13 |
| native.png | centre | **ok**, 0.968, similarity | 7.7 / 9.2 m | *default* model, mask 0.089, 311 segments, 9.105 m/px |
| native.png | 10 km NE | ok, 0.968 | 7.7 / 9.2 m | |
| native.png | 60 km E | failed (41 of 56 tiles missing, no match) | – | expected |
| native.png | Magaliesberg, 2 000 km, hint | ok, 0.968 (in the hint area) | 7.7 / 9.2 m | |

Two things break `map.jpg`, and each alone is enough to fix this sheet (same CLI):

* **the page's 1.59 % anisotropy**: the raster alone, upsampled bicubically by 3.0054 both
  ways (no stretch) onto a white margin, is located with the same sparse *saturated* mask
  (0.014): confidence 0.998, similarity fit, 6.2 / 9.4 m. On the stretched page the right
  candidate's refinement ends at fit confidence 0; `map_nearest.jpg` (default model, 2× the
  samples) gets through it with the affine stage (affine: true, 14 / 31.5 m);
* **the colour model**: with `--contour-rgb 197,185,159` (the median colour of the contour
  pixels in `native.png`, `truth.json` `contour_rgb`) `map.jpg` is located: 0.974, affine,
  9.1 / 16.4 m (mask 0.17, 1 286 segments).

Hint queries the core proposes from `ocr.json`: "Gaia GPS", "Gaia Topo Gaia GPS", "OpenStreetN",
"OpenStreetMap", "Web Mercator", "Grysbok", "Kambule", "Langalibalele", "Katana", "MountDurnford",
… – the attribution and projection lines lead the list.
