# Synthetic maps of many styles (contour benchmark, regression subset)

MapMe must locate *any* map with contour lines, not only the MCSA Magaliesberg sheets it was
built on. These cases are maps of real places rendered by `fixtures/tools/synth_topo.py` in
the styles people actually import (Gaia GPS PDF exports, OpenTopoMap screenshots, USGS
quadrangles, Swiss, OS Landranger, Kompass, Norwegian N50, MCSA-like sheets, small-scale
hypsometric maps, photocopies, faded prints, bare grey contours), with exact truth. This
directory is the small committed subset that `core/tests/styles.rs` runs; the full benchmark
(76 cases, 14 styles, 14 regions) is generated on demand, see below.

| case | style | region | m/px | page | at HEAD 5c5702e (centre GPS) |
|---|---|---|---|---|---|
| gaia-drakensberg | Gaia Topo (feet) PDF, 100 ppi raster x3 bilinear, stretched 1-2 % as jsPDF places it, A4 at 300 dpi | Drakensberg | 3.5 | pdf | fails: upsampled lines count as solid, saturated model wins, no contours |
| sparse-drakensberg | 1:250 000, 100 m over hypsometric tints | Drakensberg | 30 | scan | fails: outside the 0.4-12 m/px scale range; tints fill the colour models |
| mcsa-drakensberg | MCSA-like, muted purple-brown on white, graticule | Drakensberg | 4 | scan | located, ~2 m |
| swiss-patagonia | Swiss-like: rock drawing, blue glacier contours, relief shading | Patagonia | 8 | scan | located, ~12 m |
| otm-patagonia | OpenTopoMap-like screenshot | Patagonia | 10 | screen | located, ~12 m |
| os-patagonia | OS Landranger-like: orange 10 m, blue grid, coloured roads | Patagonia | 5 | scan | located, ~2 m |
| usgs-tasmania | USGS 7.5'-like, 40 ft, bold index, woodland, skewed scan | Tasmania | 4 | scan | located, ~3 m |
| bw-tasmania | black-and-white photocopy, 2° skew | Tasmania | 6 | scan | fails: grey contours, every colour mask empty |
| grey-tasmania | thin 1 px grey contours only | Tasmania | 6 | screen | fails: grey contours, every colour mask empty |
| kompass-himalaya | Kompass-like PDF: pale contours under bold red trails, x2 nearest | Himalayan foothills | 4.23 | pdf | located, ~10 m |
| faded-himalaya | sun-faded, yellowed print | Himalayan foothills | 6 | scan | fails: yellowed paper fills the default model, saturated wins empty |
| dense-himalaya | dense 1:25 000, 10 m | Himalayan foothills | 3 | scan | located, ~1 m |

Since the contour mask became a line detector on the contour ink at the raster's effective
resolution (`core/src/contour_lines.rs`, docs/ARCHITECTURE.md *Contour lines of any map style*)
and the search covers 0.4-50 m/px with scale bars as priors (*Any map: scales, priors,
units*), every case is located from the centre: gaia-drakensberg ~7 m (analysed at level 3),
bw-tasmania ~9 m and grey-tasmania ~4 m (grey/black ink), faded-himalaya ~1 m,
sparse-drakensberg 40/104 m (median/max at 30 m/px, searched on zoom-9 tiles).

## Case format (common benchmark format)

Each `<case>/case.json` holds `name`, `image` (`map.jpg`, what the app analyses: a 300 dpi page
render of a PDF, a scan or a screenshot), `gps` (map centre), `truth_points` (25 points
`{x, y, lat, lon}` over the map content in image pixels, exact), `dem_dir` (Terrarium
`{z}_{x}_{y}.png` tiles, relative), `ocr` (`ocr.json`: the text printed on the map with exact
boxes, a perfect-OCR stand-in; grid coordinate labels are left out, this measures contours),
`dpi` (300 for PDF pages, else null), `m_per_px` (ground, at the centre), `contour_interval_m`,
`style`, `tags`, `notes`, `contour_mask` (`contour_mask.png`, the true contour pixels),
`contour_rgb` (median colour of the drawn contours: what a user would pick), `gps_variants`
(`centre`, `offset` 10-30 km off, `far` 90-110 km away), `scale_denominator` (printed 1:n),
`rotation_deg` (map-up from true north: UTM convergence and scan skew), `anisotropy` (scan
stretch), `projection`, `image_size`, `map_frame_px`, `relief_m`, `dem_source_zoom` and the
generator's parameters. `fixtures/tools/bench_contours.py` reads the same format, including the
real-map cases (fixtures/gaia-*).

## How the maps are made (and why they are not easy)

The core matches against Terrarium zoom 11 (coarse) and 13 (refinement). The maps are traced
from zoom 14 (12 for small scales) resampled to the map grid, cleaned of Terrarium's garbage
cells, generalised (Gaussian smoothing, three times stronger on gentle slopes), offset by a
smooth random "survey" field (0.15 x interval, 1.5-8 m over ~1.5 km) and drawn with a smooth
elastic drafting warp of 1-2.5 px; closed loops under ~120 m are dropped, lines break under
labels, every 5th is an index contour, intervals follow the map series (metres or feet, finer
on low relief, coarser where lines would crowd). Clutter: hillshade (neutral or Swiss
violet/yellow), forest and park tints, hypsometric tints, lakes and sea, glaciers, rock
hatching, D8 streams, least-cost trails and roads in contour-like colours, UTM/national grids or
graticules, names, spot heights (metres or feet), contour labels, legend with contour samples,
scale bar and 1:n in the margin. Pages: PDFs at 300 dpi on A4/Letter/A3 with the raster
upsampled x2/x3 (bilinear or nearest; Gaia-like exports also stretched 0.8-2 % along one axis), scans (blur, noise, 0-0.6 % anisotropy, skew),
screenshots. Effects: photocopy, fading, JPEG. Projections: Web Mercator, UTM, national TM.

## DEM tiles

`dem/<region>/`: zoom 11 under each case's image footprint + 4 km, and the fine zoom the core
picks for the refinement (13, or coarser past 36 tiles) under the footprint + 20 %
(`synth_topo.regression_tiles`); for sparse-drakensberg (30 m/px, searched in the coarse
scale band) also the zoom-9/10 tiles of its search and the zoom-13 tiles its refinement
asks for (20 tiles). Nothing else: the core marks the rest missing, as offline. So
the "far" tests here only check that little terrain does not produce a confident answer; the
full benchmark has the terrain around the far GPS too.

## Regenerate / run the full benchmark

```sh
# this subset (deterministic; tiles from AWS Terrarium into a cache)
python fixtures/tools/synth_topo.py suite regression fixtures/styles --dem-cache /tmp/dem
# the full corpus (76 cases, ~15 min, ~1 GB of tiles shared under OUT/../dem)
python fixtures/tools/synth_topo.py suite bench /tmp/bench/cases -j 2
cd core && cargo build --release --example georef_cli && cd ..
python fixtures/tools/bench_contours.py /tmp/bench/cases fixtures/gaia-* --fetch -j 2 \
    --cli core/target/release/examples/georef_cli --out /tmp/bench/out
```

`bench_contours.py` runs each case with the GPS at the centre, 10-30 km off and 90-110 km away,
scores the errors at the truth points, flags CONFIDENT WRONG answers (ok, confidence >= 0.5,
median error > 100 m), measures the contour extraction against `contour_mask.png` (the CLI's
`--dump-mask`) and writes `results.json` and `summary.md` (per style / scale / upsampling /
region, failure modes with diagnostics). Requirements: numpy, scipy, opencv-python, pillow,
contourpy, pyproj, scikit-image.
