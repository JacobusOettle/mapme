# MapMe

[![Core](https://github.com/JacobusOettle/mapme/actions/workflows/core.yml/badge.svg)](https://github.com/JacobusOettle/mapme/actions/workflows/core.yml)
[![iOS](https://github.com/JacobusOettle/mapme/actions/workflows/ios.yml/badge.svg)](https://github.com/JacobusOettle/mapme/actions/workflows/ios.yml)

**Your live position as a red dot on any map you give it.** Import a PDF, an
image or a screenshot of a topographic map (the trailhead board, a club map like
the MCSA Magaliesberg *Grootkloof* sheet) and MapMe works out where on Earth it
is, then shows where you are on it while you hike. iPhone first; the core is
ready for Android.

* **Locates maps by itself**: from the coordinate grid printed on the map or,
  when there is none, by matching the map's **contour lines** against real
  terrain elevation data within 50 km of you. Map from farther away? **Search
  wider** looks up to 2 000 km out (with an honest download and time estimate
  first), checking the places named on the map first. Manual calibration as a
  fallback.
* **Apple Maps-style position puck** with a heading beam and accuracy halo;
  follow, follow-with-heading (map turns as you turn) and a compass.
* **Photos of paper maps**: scan with the document camera or pick a photo; the
  sheet is found on the table, flattened from its corners and located in photo
  mode (lighting normalised, full perspective fit, plus a smooth correction for
  folded or curled paper).
* **Dark map by default**: the paper map itself is redrawn for the screen by
  the core, not colour-inverted: lighting, shadows and paper tint are
  flattened, lightness is inverted in OKLab and every feature keeps its colour
  (tan contours, blue streams, red routes, light grey print on #212124 paper).
  Settings → Map appearance → Paper shows it as printed.
* **Dotted trail** of where you walked (GPX export), **nearby points of
  interest** from the map's own labels plus OpenStreetMap, with distance and
  direction.
* Dark, fast and light: native SwiftUI/UIKit with a tiled map canvas, and a
  Rust core with no runtime or garbage collector.

## Screenshots

The Grootkloof PDF in the iPhone simulator with a simulated walk, drawn dark
(the default): located from its grid labels (±1 m), your position puck with
its heading beam, the dotted trail you walked, Apple Maps-style controls and the
stats card. The second row ends with a real phone *photo* of the printed sheet,
folded in four and lying on a wooden table, going through the corner editor and
photo mode; the last row has the paper appearance, the library and the "Search
wider" sheet offered when a map is not found near you. Every push refreshes
these on the
[`ci-screenshots`](https://github.com/JacobusOettle/mapme/tree/ci-screenshots)
branch (with the IPA of that build).

| Following | Heading up | Stats card |
|---|---|---|
| <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/ScreenshotTests.testMapScreenshots__01-map_0.png" width="240"> | <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/ScreenshotTests.testMapScreenshots__02-heading_0.png" width="240"> | <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/ScreenshotTests.testMapScreenshots__03-panel_0.png" width="240"> |

| Nearby | Photo: adjust corners | Photo: located |
|---|---|---|
| <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/ScreenshotTests.testMapScreenshots__04-nearby_0.png" width="240"> | <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/PhotoImportUITests.testPhotoOfPaperMap__05-photo-corners_0.png" width="240"> | <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/PhotoImportUITests.testPhotoOfPaperMap__06-photo-map_0.png" width="240"> |

| Paper appearance | Library | Search wider |
|---|---|---|
| <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/ScreenshotTests.testMapScreenshotsPaper__01b-map-paper_0.png" width="240"> | <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/ScreenshotTests.testLibraryAppears__00-library_0.png" width="240"> | <img src="https://github.com/JacobusOettle/mapme/raw/ci-screenshots/SearchWiderUITests.testSearchWiderSheet__07-search-wider_0.png" width="240"> |

## Install on your iPhone

Download `MapMe-unsigned.ipa` from the
[latest-build release](https://github.com/JacobusOettle/mapme/releases/tag/latest-build)
and sideload it with Sideloadly or AltStore using a free Apple ID; TestFlight
and building in Xcode are also covered. Step by step:
**[docs/INSTALL.md](docs/INSTALL.md)**, which also explains how to open the
Grootkloof PDF in MapMe.

## How locating works

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

The app renders the map once at import, runs Apple Vision OCR on it and hands
the raster, the text and your GPS fix to the Rust core, which tries in order:

1. **Grid labels.** Coordinate labels (`25°49'00"S`, decimal degrees, UTM) are
   paired with the long straight grid lines they annotate, fitted robustly
   (RANSAC, equal spacing) and turned into ground control points. Accuracy is a
   few metres; this is how the Grootkloof sheet is located, offline.
2. **Contour matching (the "3D match").** Contour pixels are picked out by
   colour. Their orientation field is cross-correlated (FFT, over scale and
   optionally rotation) with the orientation field of real terrain from
   Terrarium elevation tiles within the search radius of your GPS position,
   which finds where the map's contour pattern fits the land. Then a robust
   least-squares refinement moves the georeference until every traced contour
   line has a constant elevation in the DEM. On the Grootkloof sheet this finds
   the map unambiguously in a 70 × 80 km window and converges to about 14 m,
   the noise floor of the elevation data.
3. **Photos of paper maps.** The core finds the sheet on the table
   (`mm_detect_page`), the app flattens it, and both paths above run in photo
   mode: lighting is normalised, the fit may be fully projective, and a smooth
   offset lattice ("warp") corrects paper that does not lie flat, fitted to the
   traced grid lines and to the contours' elevations. On the reference photo
   (`fixtures/grootkloof/photo`: the printed sheet folded in four, where no
   single perspective fit is better than 37 m) the position error is ~12 m
   median from the grid labels and ~15 m from contours alone (~9 m inside the
   graticule; the legend strip at the bottom, with no grid or contours, is the
   weakest part).
4. **Manual calibration.** Tap the same point on the map and on a satellite
   view (or "I'm here"); 2 points give a similarity, 3 or more an affine fit.

Coordinate conventions, the JSON API and the performance budget are in
**[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**. Why Rust and Swift rather
than Go: native UI is the snappiest on iPhone, and Rust compiles to a small
static library with no GC runtime that also builds for Android.

## Repository

```
core/        Rust crate mapme-core: georeferencing (lib + staticlib + C header), tests on the real map
ios/         XcodeGen spec (project.yml), SwiftUI/UIKit app, unit tests, UI tests
fixtures/    grootkloof/: the real PDF, its 300 dpi raster, DEM tiles, OCR fixture, ground truth
scripts/     build-core-ios.sh, check_swift_syntax.py, pick-simulator.py, package-ipa.sh, export-screenshots.sh
docs/        ARCHITECTURE.md, INSTALL.md
.github/     workflows/core.yml (Linux), workflows/ios.yml (macOS build, tests, IPA, release)
```

## Development

```sh
# Rust core (any OS)
cd core
cargo test --release                       # unit + integration tests on the Grootkloof fixture
cargo run --release --example georef_cli -- ../fixtures/grootkloof/map.jpg \
    --gps -25.75,27.45 --dem-dir ../fixtures/grootkloof/dem --dpi 300 [--ocr ../fixtures/grootkloof/ocr.json]

# Swift syntax check without Xcode (tree-sitter in a venv)
python3 -m venv .venv && .venv/bin/pip install tree-sitter==0.26.0 tree-sitter-swift==0.7.4
.venv/bin/python scripts/check_swift_syntax.py ios

# iOS app (macOS + Xcode 16+, brew install xcodegen rustup)
scripts/build-core-ios.sh                  # -> build/MapMeCore.xcframework
cd ios && xcodegen generate && open MapMe.xcodeproj
```

`make help` wraps these (`make gate` runs the same cheap checks as CI's gate
job; `make ios-test` and `make ipa` run the macOS steps locally).

## CI

| Workflow | Runs on | What it does |
|---|---|---|
| **Core** (`core.yml`) | Linux | `cargo fmt --check`, `clippy -D warnings`, `cargo test --release` on the real fixture, `cargo check` for `aarch64-apple-ios`, the dev CLI on the Grootkloof map (with and without OCR) checked against the ground truth. Artifact `georef-cli`. |
| **iOS** → *gate* | Linux | Swift syntax (tree-sitter), XcodeGen spec, shell/Python scripts, `rustfmt`, `clippy`. Must pass before anything runs on macOS. |
| **iOS** → *build-test* | macOS 15 | Rust XCFramework (cached by core source hash), XcodeGen, build for testing, unsigned Release device build → `MapMe-unsigned.ipa`, unit + UI tests on an iPhone simulator. Artifacts `MapMe-unsigned-ipa`, `screenshots`, `test-logs` (and `xcresult` on failure). |
| **iOS** → *release* | Linux | On `main`, `v*` tags or a manual run with *publish*: replaces the `latest-build` prerelease with the new IPA (tags also get their own release). |
| **iOS** → *testflight* | macOS 15 | Only when the signing secrets exist (see [INSTALL.md](docs/INSTALL.md#b-testflight-paid-apple-developer-account)): signed archive → TestFlight. |

**macOS minutes are expensive on a private repository**: each macOS minute
counts as 10 of the plan's included minutes (the free plan's 2,000 minutes are
about 200 macOS minutes, and one iOS run takes roughly 12 to 20). The workflows
are built to spend as few as possible:

* path filters (docs-only changes run nothing), and one run per branch: a push
  and its pull request share a concurrency group and newer pushes cancel older
  runs;
* the macOS job starts only after the Linux gate passes, and the Rust
  XCFramework is restored from cache unless `core/` changed;
* steps after a cancellation are skipped; tests have per-test time limits.

Put `[skip ci]` in a commit message to skip CI for that push, and batch work
into fewer pushes when iterating on Swift. Usage: GitHub → Settings → Billing.
Optional repository variables: `XCODE_VERSION` (e.g. `16.4`) and `APP_BUNDLE_ID`.
