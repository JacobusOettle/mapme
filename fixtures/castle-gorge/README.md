# MCSA Magaliesberg Sheet 07 – Castle Gorge (1:25 000)

Another sheet of the series of `fixtures/grootkloof` (Sheet 03), same style, frame, scale,
contour colour and Web Mercator layout ("GIS V1 Sept 2017 - Eras Gous"). Supplied by the
user as `07-MCSA-Magaliesberg-Sheet-07-_-Castle-Gorge-v1-1.pdf`. Map frame 25°46.5′–25°51.3′ S, 27°33.7′–27°37.4′ E;
latitude lines 25°47′ … 25°51′ S (y 607.09 … 4500.91), longitude lines 27°34′ … 27°37′ E (x 390.67 … 3019.62).

The layout is the same as Grootkloof's: the map runs to the double frame line, the 1′
graticule labels are printed just inside it over the map (latitudes vertically along the
left edge, longitudes along the bottom), with the club logo and the legend box in the
bottom-left corner and the "GIS V1 Sept 2017 - Eras Gous" credit along the bottom
(x 436–773). What differs:

- the westmost meridian, 27°34′, is at x 391, so its label runs into the credit and OCR
  reads them as one line (Grootkloof's first meridian is at x 878, clear of it);
- long parallel vertical green vegetation hatching in the south-east;
- `ocr.json` is unedited OCR output (Grootkloof's is hand-built and clean).

## How it was made

```sh
pdfimages -j castle-gorge.pdf x && mv x-000.jpg map.jpg   # the page's only image, extracted losslessly
python ../tools/measure_graticule.py map.jpg          # graticule line positions → truth.json
python ../tools/ocr_like_app.py map.jpg ocr.json      # RapidOCR with the app's OCR plan
python ../tools/fetch_dem.py dem --box 11 -26.1308 27.2433 -25.4998 28.4508 --box 13 -25.8829 27.5313 -25.7477 27.6538
```

- **castle-gorge.pdf**: the sheet as published (A3, one page, Qt 4.8.6). It is a single embedded
  3507×4960 JPEG at 300 ppi with no text layer, so all text has to come from OCR.
- **map.jpg**: that JPEG, extracted without re-encoding (`pdfimages -j`), the way
  `fixtures/grootkloof/map.jpg` was made (that file is byte-identical to the image in
  grootkloof.pdf).
  It is what the app gets from a 300 dpi render of the page, apart from a 0.06 % resampling
  (`pdftoppm -r 300` gives 3509×4963).
- **truth.json**: same schema as `fixtures/grootkloof/truth.json`, measured without the
  core. `fixtures/tools/measure_graticule.py` finds the thin dark 1′ graticule lines with
  row and column profiles and takes, for each line, the median of the sub-pixel centroids of
  300 px segments where the paper around the line is blank. Each line's value was read by eye
  from its printed label; every label sits centred on its line. The spacings (973.3–973.9 px
  per minute of latitude, 876.3 px per minute of longitude) match Grootkloof and a Web
  Mercator fit of each axis leaves residuals under 0.05 px. The lines were checked by drawing
  them over crops of every label. `contour_rgb` is the median of the brown contour pixels,
  `scale_denominator` and `contour_interval_m` are from the legend (1:25 000, 5 m).
- **ocr.json**: a *stand-in* for Apple Vision. `fixtures/tools/ocr_like_app.py` runs
  RapidOCR (rapidocr_onnxruntime) over the regions the app's `OCRService` reads (upright
  1600 px tiles overlapping 15 %, plus clockwise and counter-clockwise passes over the left
  and right 18 % margin strips) and maps the boxes back to map.jpg pixels. The output is not
  edited: it garbles, splits and merges labels the way OCR does, but not the way Vision does.
- **dem/**: Terrarium tiles (`{z}_{x}_{y}.png`, AWS open data `elevation-tiles-prod`).
  Zoom 11 from 35 km west to 86 km east and 35 km north to 35 km south of the map centre,
  which covers the coarse search for a GPS fix at the centre, 10 km off it and 60 km east
  of it (20 km radius plus the map); zoom 13 under the map frame plus 3 km, for the
  refinement. Nothing else: tiles the core asks for beyond these are marked missing, as when
  the phone is offline.

## Used by

`core/tests/sheets.rs`: grid with the fixture OCR and with phone-like OCR of the printed
labels (GPS 60 km away, no DEM), contours with the GPS at the centre (-25.8153, 27.5926), 10 km
off it (-25.7516, 27.6631) and 60 km east (-25.8153, 28.1913). Errors are measured at the
truth.json graticule intersections.
