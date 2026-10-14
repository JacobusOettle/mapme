# Gaia GPS export – Giant's Castle / Mount Durnford / Thaba-Putsoa (Gaia Topo, feet, 2 mi bar)

The second map of the user's report (with `fixtures/gaia-langalibalele`, whose README has the
details shared by both): a Gaia GPS export of the uKhahlamba-Drakensberg escarpment from
Bannerman's Pass and Langalibalele Pass south past Giant's Castle to KaMasihlenga Pass, with the
Lesotho plateau (Thaba-Putsoa, Legheku, Ntloana-Ts'oana) to the west. Map data © Gaia GPS,
© OpenStreetMap contributors. **User-supplied, private test fixture**: keep it in this
repository's tests, do not redistribute.

Same style as Langalibalele at 2.524× the scale: north-up Web Mercator, thin faint light-brown
contours every **160 ft** (48.768 m) over hillshading, the park's green tint east of the
escarpment (where the contours are fainter still), dense thin blue streams, grey dashed trails,
spot heights in feet (Giant's Castle 10876, Legheku 10925, Sangebethu 10830, Mount Durnford
10807, Ntloana-Ts'oana 10702). **22.99 m/px natively** (7.65 × 7.77 m/px on the 300 dpi page),
26 × 18 km.

## The PDF

`gaia-giants-castle.pdf` (as supplied): the same jsPDF 4.2.1 A4 landscape layout as
Langalibalele, no georeferencing metadata, scale bar "2 mi" = 101.172 pt. One RGB raster
1118 × 791 (`/Interpolate` false) placed with `806.4 0 0 561.6 18. 15.68 cm`: on the page one
raster pixel is 3.0054 × 2.9583 px (**1.59 % anisotropic**), the legend band hides the bottom
35.5 raster rows.

## Files

As in `fixtures/gaia-langalibalele`: `map.jpg` (the app's 300 dpi render, raster upsampled
smoothly – what I believe CoreGraphics draws, see that README), `map_nearest.jpg` (nearest
neighbour), `native.png` (the raster), `truth.json`, `case.json` / `case_nearest.json` /
`case_native.json`, `ocr.json` / `ocr_native.json` (RapidOCR stand-ins for Apple Vision,
`../tools/ocr_like_app.py`), `dem/`. The images were made with

```sh
python ../gaia-langalibalele/render_page.py gaia-giants-castle.pdf native.png map.jpg map_nearest.jpg
```

### truth.json (measured independently of the core)

Native px (pixel *i* covers [*i*, *i*+1)) → Web Mercator, isotropic and axis-aligned; the page
affine follows from the placement. Consensus (mean scale, mean centre) of three fits: the map's
hillshade against a Terrarium z13 hillshade (NCC over translation, scale and light: from 315°,
66° up), the same against Copernicus GLO-30 (independent DEM) on a z14 Mercator grid, and the
contour lines alone (phase coherence of the Terrarium elevation at the contour pixels modulo
the interval, which peaks at 48.65–48.75 m = 160 ft).

| check | result (native px; 1 px = 23 m) |
|---|---|
| estimates vs. truth at the 25 truth points (max / rms) | hillshade Terrarium 0.12 / 0.10, Copernicus 0.24 / 0.22, contours 0.35 / 0.27; anisotropic hillshade fits 0.23 / 0.14 and 0.23 / 0.22 (square to 0.04 %); scale spread 0.015 % |
| 12 windows registered alone against the DEM hillshade | Terrarium rms 0.32, max 0.54; Copernicus rms 0.30, max 0.45 |
| summit icons vs. the highest Copernicus cell within 210 m | Legheku (−0.5, −1.3) px, Thaba-Putsoa (−0.5, −1.0), Sangebethu (0.1, −1.2), DEM within 3–11 m of the spot heights; Mount Durnford's icon sits 7 px north of its summit (3 px on map A: Gaia offsets icons by zoom) |
| printed scale bar (2 mi = 101.172 pt) | implies 22.947 m/px; truth 22.990 (the bar is 0.19 % short) |

Good to about **±0.4 native px (±9 m)**. This map's fit is the better determined of the two
(26 km wide, strong relief everywhere), and the Langalibalele truth is partly derived from it.

### dem/

Terrarium tiles, 4.2 MB:

```sh
python ../tools/fetch_dem.py dem --box 11 -29.6816 28.9817 -28.9792 29.8648 \
  --box 12 -29.4750 29.2187 -29.1859 29.6278 --box 10 -29.6816 28.9817 -28.9792 29.8648 \
  --box 9 -29.6816 28.9817 -28.9792 29.8648
```

z11 within 30 km of the map (30 tiles), **z12** under it ± 7 km (30 tiles: the map's footprint
plus the refinement margin needs more than 36 zoom-13 tiles, so `fine_plan` refines at zoom 12),
z10 (9) and z9 (4) within 30 km. No zoom-13 tiles (they would add ~1.3 MB). Tiles beyond these
are marked missing.

## Baseline: HEAD 5c5702e, `core/examples/georef_cli`

As for Langalibalele. GPS: centre (−29.326791, 29.423245); 10 km NE (−29.263271, 29.496102);
60 km E (−29.326791, 30.041462); Magaliesberg (−25.80, 27.45) with a 2 000 km radius and a hint at
Giant's Castle (−29.343878, 29.483057).

| image | GPS | outcome | error | diagnostics |
|---|---|---|---|---|
| map.jpg | centre, 10 km NE, 60 km E, 60 km E at 200 km, Magaliesberg + hint | **failed**: "No contour lines found on this map. Pick the contour colour, or calibrate manually." | – | colour model *saturated*, mask **0.0019**, 0 segments |
| map_nearest.jpg | centre | **ok**, 0.982, affine, refined at z12 | 16.0 / 19.5 m | colour model *default*, mask 0.072, 699 segments, 7.709 m/px |
| native.png | centre | **failed**: "No match within 50 km of you (searched 30 terrain tiles; 19 couldn't be downloaded)" | – | *default* model, mask 0.086, 618 segments; the map is 22.99 m/px, outside the default scale range 0.4–12 m/px: candidates at 11.98 m/px, peak ratio 1.01 |
| native.png | 10 km NE / 60 km E | failed (same reason) | – | |
| native.png | Magaliesberg, 2 000 km, hint | failed | – | hint area searched, every fit confidence 0 (scale out of range) |

With the contour colour picked (`--contour-rgb 212,199,174`, `truth.json` `contour_rgb`)
`map.jpg` is located: 0.922, affine, 14.8 / 17.1 m (mask 0.13, 969 segments). The raster alone
upsampled without the stretch still has no contours under the automatic model (mask 0.002): on
this sheet the colour model is the blocker, the scale range on `native.png`.

Hint queries from `ocr.json`: "Gaia GPS", "Gaia Topo Gaia GPS", "OpenStreetMap", "OpenStreetN",
"Web Mercator", "Oribi Ridge Rte", "Ntubeni", "KaMasihlenga", "Thaba", "Grysbok", "Giant's", …
