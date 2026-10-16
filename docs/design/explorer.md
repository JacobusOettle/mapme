# Explorer features: design

MapMe so far puts you on a paper map. This round makes it the map you hike with: plan and follow
routes, see the climb ahead and the daylight left, carry notes and photos, never fall off the edge
of a sheet, see your maps in 3D and on a world view, save battery, and do all of it offline.

Six builders work in parallel from the scaffolding commit that adds this document. This document
is the contract: user flows, where every control lives, data and persistence, services and
threads, how things are drawn, the rules each feature follows, what each builder owns and how the
pieces meet. When the code and this document disagree, fix one of them in the same change.

| # | Feature | Builder |
|---|---|---|
| 1 | Routes and waypoints: plan by tapping, distance/climb/descent/walking time, follow with off-route warnings, GPX import/export | routes |
| 3 | Elevation profile of the recorded track and planned routes, daylight banner with ETA | routes |
| – | Map auto-switching, world view of all maps, walking off a map (terrain continues) | maps |
| – | Notes and photos on the map | notes+battery |
| – | Battery saver | notes+battery |
| – | 3D view of the map draped on the terrain | 3d |
| – | Offline regions, trailhead caching, matching offline | offline |
| – | Terrain engine in the core: elevations, profiles, grids, contour rendering, tile plans | core |

## 1. Principles

* **Geo items are global.** Waypoints, routes and notes are stored by latitude/longitude in one
  store, owned by no map. Every map that covers them shows them (on the sheet and in the terrain
  beyond its edge), and so do the world view and the 3D view. A route drawn on one sheet continues
  on the next.
* **Offline first.** Hikers are out of signal. Everything except geocoding a place name works
  from what is on the device: pinned regions, the terrain kept around located maps, the terrain
  cache. When something needs the network the UI says so, and never spins forever.
* **The core does the numbers.** Elevation sampling, smoothing, contour tracing and tile plans are
  Rust (`core/src/terrain.rs`, C ABI `mm_terrain_*`, `mm_tile_plan`), shared with Android later.
  Swift does UI, I/O and scheduling.
* **Honest.** Estimates say what they are ("≈ 340 MB, measured from 16 sample tiles"); walking
  times are "at your pace" and say how the pace was found; the battery saver says what it can
  and cannot change.
* **Apple-like and dark.** Glass controls (`floatingChrome`, `MapRoundButtonStyle`), SF Symbols,
  `Theme` tokens only, haptics on state changes, every control 44 pt, accessibility labels and
  identifiers on everything a UI test touches.
* **Main thread stays free.** Core calls run off the main thread; CATiledLayer draws on its own
  threads; overlays reproject once per map and only transform per frame.

## 2. Where everything lives

### Map screen

```
┌──────────────────────────────────────────────┐
│ (‹)  [ Grootkloof · Located grid ±2 m ]  ┌──┐ │  Layers / Tracking  (existing stack)
│      ┌─────────────────────────────┐     │  │ │
│      │ ⚠ You've walked off …       │     └──┘ │
│      │ 2 h 10 min of daylight left │     (N)  │  compass (when rotated)
│      │ Switched to … · Undo        │     (▦)  │  Maps here      maps     mapsHereButton
│      └─────────────────────────────┘     (⬡)  │  3D             3d       terrain3DButton
│        banners: MapTopBanners            (〰) │  Routes         routes   routeToolsButton
│                                          (✎)  │  Note / photo   notes    addNoteButton
│                                          (🍃) │  Battery saver  battery  batterySaverBadge
│                                               │  (only while on)
│                 the map canvas                │
│                                               │
│  ┌─────────────────────────────────────────┐  │
│  │ selection card: dropped pin, waypoint,   │  │  SelectedGeoItemCard / DroppedPinCard
│  │ route (with profile), note, POI          │  │
│  ├─────────────────────────────────────────┤  │
│  │ stats panel  — or the active tool's bar  │  │  TrailStatsPanel / MapTool.makeBar()
│  └─────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
```

* The **side column** (`Views/Map/MapSideColumn.swift`) sits under the existing layers/tracking
  stack and the compass: separate round glass buttons, 10 pt apart, top to bottom Maps here, 3D,
  Routes, Note, Battery. A feature's button is `EmptyView` until the feature fills it in; a button
  that cannot act (3D on a map that is not located) is shown dimmed at 55 % opacity like the
  tracking button, with a one-line explanation in its menu or a toast.
* **Banners** (`MapTopBanners`) stack under the title capsule, left of the controls, at most
  360 pt wide: walked-off-the-map (maps), route guidance and daylight (routes), then the screen's
  toast (`MapScreenContext.toast`, 5 s, optional action such as Undo).
* **Long press** on the map drops a pin (`DroppedPinCard`: distance, coordinates, then four
  actions: Waypoint, Note, Photo, Route). This replaces the old "long press saves a waypoint at
  once"; the waypoint is one tap more and the other three are new.
* **Bottom**: the selection card above the stats panel; while a tool is active (route editing)
  its bar replaces the stats panel.

### Elsewhere

* **Library** toolbar: the globe button (`worldViewButton`) opens the world view; the + menu has
  "GPX route or waypoints" (the same Files picker, which now accepts `.gpx`).
* **Settings** sections, in order: Display, Maps (maps), Routes (routes), Battery (battery),
  Locating, Offline terrain (offline), Tracks, Storage, About.
* **Open in / AirDrop / share sheet**: `.gpx` files open MapMe and go to the GPX import
  (`AppModel.handleIncoming` → `GPXService.importFile`).

## 3. Architecture

```
            SwiftUI screens                    UIKit canvas (main thread)
   MapScreen ── MapScreenContext ──────────── MapCanvasUIView
      │  side column, banners, cards             │  sheet, tiles (CATiledLayer threads)
      │  sheets/covers (MapPresentation)         │  MapOverlayView (puck, trail, POIs)
      │                                          │  feature containers: routes · markers · top
      ▼                                          ▼
   AppModel ─ services (@MainActor @Observable)  CanvasOverlaySet: one object per feature
   ├ geoItems: GeoItemStore  (JSON, atomic)        overlays (screen space) + map-space layers
   ├ navigator: RouteNavigator   gpx: GPXService
   ├ mapSwitcher: MapSwitcher    notes: NoteService   battery: BatterySaver
   ├ offline: OfflineRegionService ── PinnedTerrainStore (Application Support/Terrain)
   └ terrain: TerrainProvider (thread-safe) ── TerrainStore (core MMTerrain, in memory)
                     │ loads need_tiles via TerrainTileSource
                     ▼
               DEMService: pinned → Caches/dem (LRU 400 MB) → network (s3 Terrarium)
```

* `FeatureHooks` (`App/FeatureHooks.swift`) calls each feature's hooks: `attach` at the end of
  `AppModel.init` (wire services, no I/O or prompts: the app hosts the unit tests),
  `appDidLaunch` (once the UI is up; Debug launch arguments), `mapDidLocate` (import, locate
  again, search wider, calibration), `mapDidDelete`.
* Services are `@MainActor @Observable final class`es made in `AppModel.init` with no arguments;
  each gets the model in `attach(_:)` (weakly) and takes what it needs from it.

## 4. Data model and persistence

All in `Model/GeoItems.swift`, `Model/GeoItemStore.swift`, `Model/OfflineRegion.swift`.

| Type | What | Key fields |
|---|---|---|
| `Waypoint` | a named point (dropped, or GPX `<wpt>`) | `position`, `elevationM?`, `symbol` (`WaypointSymbol`), `comment?`, `origin` |
| `Route` | planned (`.planned`), GPX `<rte>` (`.gpxRoute`) or `<trk>` (`.gpxTrack`) | `points: [RoutePoint]` (`isAnchor`: a handle the user placed or an rtept; trace points between), `color?`, `stats?` (`RouteStats`, valid while `geometryHash` matches) |
| `MapNote` | a note, optional photo | `position`, `text`, `photo?: NotePhoto` (`fileName`, size, `takenAt?`, `headingDegrees?`, `placement`: exif / myLocation / manual) |
| `OfflineRegion` | a downloaded area | `bounds: GeoBounds`, `detail` (`.standard` z8–12, `.high` z8–13), status, progress counters, `estimatedBytes` |
| `TerrainOwner` | who keeps pinned tiles | `.region(id)`, `.map(id)` |
| `GeoItemRef` | `(kind, id)` of a waypoint/route/note | selection, hits |

Files, in Application Support (geo items are the user's data and are backed up; pinned terrain
can be downloaded again and is excluded from backups):

```
GeoItems/waypoints.json        {"version":1,"waypoints":[…]}
GeoItems/notes.json            {"version":1,"notes":[…]}
GeoItems/routes/<uuid>.json    {"version":1,"route":{…}}      one file per route (tracks are long)
GeoItems/photos/<uuid>.jpg     ≤ 2048 px, JPEG 0.8, no metadata;  <uuid>-thumb.jpg 160 px square
Terrain/tiles/{z}/{x}/{y}.png  pinned terrain (isExcludedFromBackup: it can be downloaded again)
Terrain/owners/{region|map}-<uuid>.json   {"version":1,"ranges":[TileRange],…}
OfflineRegions/regions.json    the regions and their progress (offline builder)
```

Rules:

* `GeoItemStore` publishes changes at once (`revision` bumps on every change) and writes atomically
  on one serial background queue, newest snapshot last; `flush()` waits (tests, before
  suspension). Dates are Foundation's seconds since 2001, so items compare equal after a reload.
* A file that does not decode is moved aside (`<name>.unreadable-<date>.json`), never
  overwritten; that kind starts empty. User data is never silently dropped.
* Any field added later to a persisted type is Optional (or gets a hand-written decoder).
* Waypoints dropped before this round live in maps' `meta.json` as `MapPOI(kind: .waypoint,
  source: .user)`. `GeoItemMigration.migrateMapWaypoints` (routes) moves them into the store once
  (`origin: .migrated(mapID:)`) and removes them from the maps; it is idempotent.
* Map deletion (`MapLibrary.didDelete` → `FeatureHooks.mapDidDelete`) releases the map's pinned
  terrain. Geo items are not deleted with maps.

## 5. Terrain

### The core's terrain engine

One `TerrainStore` (C `MMTerrain`, Swift `Core/TerrainStore.swift`) for the whole app, owned by
`TerrainProvider` (`AppModel.terrain`). It holds decoded Terrarium tiles in memory (64 MiB by
default, least recently used dropped) and answers queries; it never downloads. A query lacking
tiles answers `need_dem` with `need_tiles` and a partial result; `TerrainProvider.resolve` loads
them (`DEMService`: pinned, then cache, then network; missing ones marked missing) and asks
again, at most 6 rounds. Pinning is the app's (`PinnedTerrainStore`); the core store is only a
cache. The JSON fields are in docs/ARCHITECTURE.md, "C ABI", Terrain; summary:

| Function | Used for |
|---|---|
| `mm_terrain_elevations` | waypoint/note elevations, the puck's terrain height in 3D |
| `mm_terrain_profile` | route stats (distance, ascent, descent with 3 m hysteresis after smoothing), the profile chart, progress mapping (`vertex_distance_m`) |
| `mm_terrain_grid` | the 3D mesh (heights over a Mercator rectangle into a float buffer) |
| `mm_terrain_render` | contours + hillshade: in a map's pixel space (terrain beyond the sheet, the 3D texture) or in Mercator (world view overlay) |
| `mm_tile_plan` | offline regions (tiles per zoom + estimated bytes) and the trailhead rule |

Contour interval: the smallest of 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000 m that is at
least 4 × the ground metres per point (`ContourInterval.automatic` mirrors the core). A tiled
view passes one `scale_m_per_pt` per zoom level so all its tiles agree; index lines every 5.
Styles: dark (background `Theme.uiOffMapNightPaper` beyond a sheet, contours the dark map's tan
#836545, index lines lighter and 1.6× wider), paper (background `Theme.uiOffMapPaper`, brown
contours). Hillshade 0.25 in dark, 0.35 on paper (light from the north-west, 45°).

### Which terrain each feature needs

| Feature | Zooms | Where |
|---|---|---|
| Locating a map (existing) | 11 near disc (coarser past 60°), 9–10 wide, 8–10 coarse bands; 13 refinement (12/11 accepted when 13 is missing) | search area |
| Route stats, profiles | 13 (falls back to 12, 11) | along the line |
| Terrain beyond a sheet | 12 (13 when zoomed past 2 m/pt; battery saver: ≤ 12) | sheet + continuation margin |
| World view overlay | the overlay tile's zoom, 8–12 | on screen |
| 3D | grid 12 (13 under the sheet when held); texture rendered in map space | sheet + continuation margin |
| Trailhead cache | 9–10 within 50 km, 11 within 30 km, 12 over sheet + margin, 13 over sheet + 3 km | around each located map |

### Continuation margin

How far a map continues beyond its sheet with terrain: `M = clamp(½ × the sheet's long side
(ground), 3 km, 15 km)` (`terrain::continuation_margin_m`), in the map's pixels at its centre
(`MapContinuation.marginPixels`), reduced until the georeference stays sane over the enlarged
rectangle: for a homography with perspective (photos), the projective denominator `w` at the
enlarged corners within ±25 % of its value at the centre. The same margin bounds the canvas
camera, the off-map terrain layer, the 3D view's surroundings and the trailhead's zoom-12 area.

## 6. Map screen extension points

All in `Views/Map/`; owned by the lead (builders call them, and only edit their own slot types).

* `MapScreenContext` (one per screen): `mapID` (the map shown; `switchMap(to:announce:)` changes
  it in place with a camera handoff and a toast with Undo), `tileStore` (replaced together with
  the map), `selectedItem`, `selectedForeignPOI`, `droppedPin`, `tool`, `tracking` (mirrors the
  screen), `sheet` / `cover` (`MapPresentation`: a feature presents its own sheet or full-screen
  cover; the screen adds the app model's environment), `toast`, `controller`, `overlays`.
* `CanvasOverlay` (screen space; `level`: `.routes` under the trail, `.markers` above the map's
  POIs and under the puck, `.top`): `mapDidChange(CanvasMapContext)` reprojects (projector with
  warp, `drawableRect` = sheet + continuation, `requestFrame`), `render(CanvasFrame)` positions per
  frame (no allocations on the common path; implicit animations are off), `hitTest` (within
  ~22 pt; geo items win over the map's own POIs), `advance` for animations.
  Observe your data with `ObservationLoop` and call `requestFrame()` after it changes.
* `CanvasMapSpaceLayer` (a view in map pixels, transformed with the raster; its bounds may extend
  beyond the sheet with a negative origin, and the canvas puts the bounds' centre where that map
  pixel is on screen): the off-map terrain.
* `MapTool` (route editing, freehand tracing): gets taps, long presses and one-finger drags first
  (`beginDrag` returning true keeps the map still); `makeBar()` replaces the stats panel.
* `CanvasCameraHandoff`: metres per point, north's screen direction and the centre coordinate
  carried to the next map; the canvas applies it when that map appears.
* `MapCanvasUIView.Inputs` gained `featureOverlays`, `mapSpaceLayers`, `tool`,
  `continuationMargin` (the camera's centre may go that far beyond the sheet; following works
  there; zooming out fits sheet + margin), `cameraHandoff`, `maxFrameRate`, `animationsEnabled`.

## 7. Features

### 7.1 Routes and waypoints (routes)

**Plan.** Route button → menu: New Route, Routes…, Import GPX…, and Follow / Stop Following when a
route is selected. New Route activates `RouteEditTool`:

* tap adds an anchor at the end (a tap within 22 pt of a segment inserts there); the first point
  defaults to your position when you are on the map and tap "Start here";
* drag an anchor to move it (the hit area is 44 pt; the map does not pan);
* tap an anchor: a small menu (Delete, Name…, Make End);
* the pencil toggles **freehand**: one-finger drags trace points along a trail (sampled every
  6 pt, simplified with Douglas–Peucker at 1 pt), two fingers pan and zoom;
* the bar shows live stats — distance, ↑ ascent, ↓ descent, walking time — recomputed 0.3 s after
  the last change (`RoutePlanner.profile` off the main thread), with Undo, the pencil, Cancel and
  Done (Done names it "Route <date>"; rename later).
* Stats come from the core profile (zoom 13, falling back); without terrain the bar shows the
  distance and "Elevation needs terrain (offline)".

**Draw** (`RouteOverlay`, level `.routes`): 5 pt line with a 1.5 pt casing (black 35 % on the dark
map, white 90 % on paper) in the route's colour (planned violet `Theme.uiRoute`, imported teal);
anchors as 10 pt white-ringed dots only while editing; start a green dot, end a chequered flag;
the followed route 6 pt with chevrons every 80 pt and its walked part at 45 % opacity. Paths are
built in screen space per zoom bucket (√2 steps) from points simplified to 0.5 pt, and only
transformed in between, like the trail; long tracks (> 20 k points) are simplified off the main
thread once per map. A route is drawn wherever it crosses the drawable rect, on every map.

**Waypoints** (`WaypointOverlay`, level `.markers`): `OverlayArt`-style 26 pt markers with the
waypoint's symbol in red, the name as a halo label when zoomed in (≤ 7 m/pt), greedy placement
among themselves. Tap → `WaypointCard`: name, distance and direction, elevation (terrain), Rename,
Symbol, Route Here, Share (GPX), Delete. Waypoints and notes also appear in Nearby (`NearbySheet`
and the stats panel's preview list them with the map's POIs).

**Follow** (`RouteNavigator`): Follow on a route card. It keeps location updates in the
background (`location.setBackgroundUpdates(true, for: "routeFollow")`), asks for notification
permission the first time, and computes on every fix:

* progress: the fix projected onto the route (local metres), searching ±500 m of route distance
  around the last progress first (loops, out-and-back), the whole route when that is farther than
  the threshold; distance done and remaining, remaining ascent/descent from the profile, ETA;
* off route: distance to the route d > T = max(40 m, 2 × horizontal accuracy) for ≥ 20 s and two
  fixes in a row → `isOffRoute`; back on at d < 0.6 T. On entering: warning haptic
  (`UINotificationFeedbackGenerator.warning`), the red banner "Off route · 120 m · back ↗ NE";
  in the background a local notification ("You're 120 m off Bannerman Pass"), once per episode
  plus a reminder after 5 min;
* arrival within 30 m of the end: success haptic, "Arrived at Bannerman Hut", offer Stop.

**GPX** (`GPXService`, `XMLParser` off the main thread): GPX 1.0 and 1.1; `wpt` → waypoints
(`name`, `ele`, `desc` → comment, `sym` mapped to `WaypointSymbol` where it fits), each `rte` → a
`.gpxRoute` (rtepts are anchors with names), each `trk` → one `.gpxTrack` (segments joined; points
are not anchors), `ele` kept. Security-scoped URLs, the Inbox copy removed after reading
(`LSSupportsOpeningDocumentsInPlace` stays false: files are copied in, as maps are; the type is
declared in project.yml as `com.topografix.gpx`, `UTType.gpx` in `App/AppDestination.swift`). A file
imported before (same name and content hash) asks Replace / Keep Both. The summary sheet (via
`context.sheet` or the library) says "2 routes and 12 waypoints from Bannerman Pass.gpx" with
"Show on Map": the most detailed located map covering most of the items, else the world view.
Malformed files: "This GPX file couldn't be read: …"; empty: "has no waypoints, routes or tracks".
Export: a share sheet (`ShareLink` with a temporary `.gpx`) for a route, a waypoint, all waypoints,
or everything near a map; recorded tracks keep their export (`Track.gpx()`).

### 7.2 Elevation profile and daylight (routes)

**Profile** (`ElevationProfileView`, Swift Charts): `AreaMark` + `LineMark` of the profile samples
(distance → elevation), y from min − 5 % to max + 5 %, the axes in the user's units, a marker at
your progress while following, waypoints near the route as point marks, drag to scrub (the
canvas shows the scrubbed point via the route overlay). It appears in the route card (compact,
96 pt), full-screen from it, in the stats panel's expanded details for the recorded trail, and in
a saved track's detail page (`TracksView`, which already charts GPS altitude: prefer the DEM
profile when available, GPS altitude otherwise, and say which).

**Sun** (`SunTimes.compute`, NOAA solar calculator, on the device): sunrise and sunset at −0.833°,
civil dawn/dusk at −6°, for the calendar day of the device's time zone at your position; polar
day/night handled ("The sun doesn't set today").

**Walking time** (`PaceModel`): Tobler's hiking function v = 6 · e^(−3.5 · |S + 0.05|) km/h on each
profile segment (S = Δh/Δd of the smoothed profile), summed, × your pace factor. The factor is
learned from your recorded tracks (median over the last 10 with ≥ 1 km of moving, moving =
0.2–3 m/s, stops over 2 min left out, clamped 0.6–2.0; 1.0 without tracks), or set in Settings
→ Routes ("Learned: 1.12× Tobler" / Custom slider). While following, the last 45 min of your
moving pace on this walk is blended in (up to half the weight). Moving time only: the banner says
"at your pace", breaks are not predicted.

**Daylight banner** (`RouteGuidanceBanner`): shown while following a route, or when less than 2 h
of daylight remain and you have a fix. "2 h 10 min of daylight left · at your pace you reach
Bannerman Hut at 17:40" (the route's last named anchor, else "the end of <route>"); without a
route "1 h 40 min of daylight left · sunset 18:02". Amber when the ETA is within 15 min of sunset,
red after civil dusk ("you reach … 40 min after dark"). Tap → a sheet with sunrise, sunset, civil
dusk, daylight left, pace and the ETA's parts. `-mockNow <ISO8601>` fixes the clock in Debug.

### 7.3 Maps: auto-switching, world view, walking off a map (maps)

**Coverage** (`MapSwitcher.coverage`): a map covers a position when the position, through its
georeference with the warp (`Georeference.Projector`), falls inside the sheet's pixel rectangle;
`insideMarginM` is the distance to the nearest edge in ground metres (negative outside) and
`metersPerPixel` the ground scale there.

**Best map**: of the located maps covering you with `insideMarginM ≥ max(100 m, 2 × accuracy)`,
the finest `metersPerPixel`; within 10 % of each other, the current map, then the most recently
opened.

**Switch automatically** (Settings → Maps, on by default) when all of: following (`.follow` or
`.followWithHeading`), no tool active and nothing presented, a fix ≤ 10 s old with accuracy
≤ 50 m, no switch in the last 60 s, and either

* you are off the current sheet by ≥ max(30 m, accuracy) for 5 s and another map covers you, or
* another map is ≥ 1.5× finer at your position and has covered you (with the margin) for 20 s.

`context.switchMap(to:announce:)` keeps the metres per point, north's direction on screen, the
centre and the tracking mode; toast "Switched to Mhlabatini · more detailed here" with Undo, a
selection haptic. Undo switches back and excludes that map from auto-switching until you leave
its sheet (or 15 min). Opening a map from the library never auto-switches away from it in the
first 30 s.

**Maps here** (`MapsHereButton`): `square.stack.3d.up` with a badge counting the maps covering you
(or the view's centre when you are not near); its menu lists them (name, scale as "1:25 000-ish"
from m/px at 300 dpi or "4 m/px", "You're here"/distance, the current one checked), then "World
View". `mapSwitcher.mapsHere` updates as you move.

**Walking off a map.** Decision: **both**, layered.

1. *Near the sheet: terrain in the same canvas, in the map's pixel space*, out to the continuation
   margin (§5). `OffMapTerrainLayer` is a `CATiledLayer` view below the sheet whose bounds are the
   sheet grown by the margin on every side; its tiles are drawn by `mm_terrain_render` (space
   `map`, the map's homography and warp, `scale_m_per_pt` per level, style from the appearance)
   on CATiledLayer's threads; missing terrain → `TerrainProvider.prefetch` → `setNeedsDisplay(in:)`
   of that tile when it arrives. Why: walking off the edge changes nothing you are doing — same
   camera, zoom, heading mode, puck, trail, routes, waypoints, notes and POIs (other maps' too),
   no mode switch, no jump. In the map's own projection the terrain lines up with the sheet's
   contours at the edge.
2. *Beyond that, or where no map covers you: the world view* in Web Mercator (below). Why not
   one unbounded pixel space: a photo's homography diverges away from the sheet and a warp is only
   defined over it; tile counts grow with the square of the distance; and a sheet's pixel space
   means nothing 50 km away.

The sheet keeps its shadow; where terrain shows beyond it, `MapEdgeOverlay` draws a 1 pt hairline
along the edge (`Theme.uiSheetEdge`). Off the sheet, `WalkedOffMapBanner` says "You've walked off
Grootkloof · showing terrain" with the maps covering you (switch) or "World View"; beyond the
margin it says "You're 18 km from Grootkloof", following stops at the margin and the off-screen
arrow points to you as before.

**Other maps' places** (`OtherMapsPOIOverlay`, setting on by default): POIs (map labels and OSM)
of your other located maps that fall in this map's drawable rect, deduplicated against this map's
POIs and each other (same name within 150 m), drawn like POIs at 80 % opacity under this map's
own; tap → the POI card ("on Mhlabatini").

**World view** (`WorldView`, pushed from the library's globe and from Maps here): an
`MKMapView` (UIViewRepresentable; SwiftUI `Map` cannot take a tile overlay), dark muted standard
configuration with realistic elevation; overlays: a terrain `MKTileOverlay` whose `loadTile`
renders contours + hillshade in Mercator with `mm_terrain_render` (transparent background, zooms
8–16 rendered from DEM ≤ 12), so it works offline from pinned and cached terrain where Apple's
base map is blank; each located map's footprint (its four corners) with its dark thumbnail drawn
in (an affine from three corners is enough at this scale) and an accent outline; routes, tracks,
waypoints and notes (clustered annotations); offline regions as dashed rectangles (toggle); you.
Tap a footprint → callout (name, scale, distance) → Open. It is the terrain view when you are far
from every map (the walked-off banner's "World View" opens it following you).

### 7.4 Notes and photos (notes)

* **Add**: the note button's menu — Note Here (at you), Take Photo (camera,
  `UIImagePickerController` in `context.cover`; position = your fix at capture), Choose Photo
  (`PhotosPicker`; position from the photo's EXIF GPS, else ask: At My Location / At the Map's
  Centre / Cancel); the dropped pin's Note / Photo place it at the pin (a photo's own GPS wins
  only if the user keeps it: "This photo was taken 2.1 km away. Use its location?").
* **Editor** (`context.sheet`): multi-line text, the photo, where and when, elevation from the
  terrain; Save / Cancel; later Edit and Delete from the card.
* **Storage and privacy**: the photo is decoded and re-encoded (≤ 2048 px, JPEG 0.8) without any
  metadata into `GeoItems/photos/`, plus a 160 px square thumbnail; the original is not kept and
  no PHAsset is referenced (no photo library permission needed with PhotosPicker). Nothing is
  uploaded; sharing a photo shares the re-encoded JPEG without location; GPX export writes notes
  as `<wpt>` with their text, never photos.
* **Draw** (`NoteOverlay`, `.markers`): photo notes as 34 pt round thumbnails (2 pt white ring,
  shadow), text notes as 26 pt amber (`Theme.uiNote`) markers with `note.text`; within 40 pt of
  each other they collapse into a stack with a count badge; zoomed out past 15 m/pt they become
  10 pt dots. Tap → `NoteCard` (photo, first line as the title, the text, date, distance and
  direction) → tap the photo for a full-screen viewer with Share.

### 7.5 Battery saver (battery)

Modes (Settings → Battery): Off, With Low Power Mode (default: on while
`ProcessInfo.isLowPowerModeEnabled`, following `NSProcessInfoPowerStateDidChange`), Always.
Exactly what it changes — all of it within what iOS allows an app:

| | Normal | Saver |
|---|---|---|
| `desiredAccuracy` | Best | NearestTenMeters (still GPS: anything coarser turns GPS off and fails where there is no Wi-Fi or cell) |
| `distanceFilter` | none | 5 m on screen; 10 m in the background (recording, following) |
| `headingFilter` | 1° | 5°; heading updates stopped in the background and when not in heading mode with the map off screen |
| `pausesLocationUpdatesAutomatically` | false | false (iOS pausing would cut tracks; never changed) |
| map redraw | up to 120 Hz | ≤ 30 Hz (`CADisplayLink.preferredFrameRateRange`) |
| idle animations | breathing puck | off (they keep the render server drawing when nothing moves) |
| keep screen awake | the setting | suspended: auto-lock applies |
| network | OSM places, terrain prefetch, trailhead caching | on demand only; trailhead caching waits |
| terrain detail | zoom 13 | zoom ≤ 12 for off-map, world and 3D |
| 3D | 60 fps, 4× MSAA | 30 fps, no MSAA |

What iOS does not let an app do, and the saver does not pretend to: duty-cycle the GPS (deferred
updates are gone since iOS 13), dim or turn off the screen (`UIScreen.brightness` only lasts while
the app is in front and is reset; it is not touched), or change the system's refresh rate
(Low Power Mode already caps ProMotion at 60 Hz).

Honest estimate, in the Settings footer and the badge's popover: with the screen on, the display
and the GPS take most of the power; the saver's lower redraw rate, no idle animations and fewer
location callbacks typically give 10–25 % more time with MapMe open, and the screen locking by
itself is the biggest saving; with the screen off while recording, the GPS dominates and the
saver changes little (≤ 10 %). The popover shows the drain measured this session
(`UIDevice.batteryLevel` sampled every 5 min while unplugged, shown after ≥ 15 min and ≥ 2 %:
"−6 % per hour"). `BatterySaverBadge`: a `leaf.fill` glass button in the side column while the
saver is on.

Wiring: `BatterySaver` applies `LocationPowerProfile` through `LocationService.apply(_:)`, and
exposes `maxFrameRate`, `animationsEnabled`, `allowsKeepScreenAwake`, `maxTerrainZoom`,
`allowsPrefetch`, which the map screen, the canvas, the 3D view and the terrain features read.

### 7.6 3D view (3d)

The 3D button (`cube`, side column; dimmed when the map is not located) presents
`Terrain3DScreen` in `context.cover`. SceneKit (`SCNView` via UIViewRepresentable; deprecated on
iOS 26, warnings accepted, works on iOS 17–26; RealityKit's non-AR camera control on iOS 17 is
too thin):

* **Area**: the sheet plus the continuation margin (§5), as a Mercator rectangle around the
  enlarged corners. **Mesh**: `mm_terrain_grid` at zoom 12 (13 when the sheet is small enough
  that the grid would be finer than zoom 12's cells), cols × rows = clamp(extent / 2 cells,
  96, 384) per side; local ENU metres (x east = Δmerc_x · cos φ₀, z south, y up =
  (h − h_min) × exaggeration, 1.5× by default, 1–3× in a menu); smooth normals; skirts down to
  h_min − 50 m on the four sides so the block reads as a diorama; NaN heights filled from
  neighbours.
* **Texture**: one image in the map's pixel space covering the enlarged rectangle (≤ 4096 px on
  the long side): `mm_terrain_render` (space `map`, the appearance's style, hillshade) for all of
  it, then the sheet's own raster (the stitched pyramid level ≤ 4096 px, dark or paper as set)
  drawn into the sheet's rectangle. Per-vertex UVs = the vertex's map pixel (georeference inverse
  with the warp, `Georeference.Projector`) ÷ the enlarged size. The map is therefore draped
  exactly, warp included, and continues into DEM contours outside it.
* **On top**: POIs (the map's, other maps', waypoints, notes) as billboards (`SCNBillboardConstraint`,
  `OverlayArt.marker` images, labels beyond 600 m hidden), fading with distance; the recorded
  track (pink) and routes (their colours) as ribbons 2.5 m above the terrain sampled along the
  line every grid cell, unlit, depth-biased; you as a red sphere on a pole with a heading cone.
* **Camera**: `allowsCameraControl` with `.orbitTurntable`, inertia; starts looking north at 45°
  over the sheet (over you when you are on it); Reset (`scope`), a compass that turns with the
  camera (tap: face north), Close. Building takes ~1 s off the main thread with a progress ring;
  missing terrain → "Terrain for this area isn't on your iPhone yet" + Retry (offline) and the
  flat map in 3D.

### 7.7 Offline regions and trailhead caching (offline)

**Pick a region** (Settings → Offline terrain → Add Region…, full screen):

* an `MKMapView` with a search field: `MKLocalSearch` (result types address + point of interest,
  `.physicalFeature` on iOS 18+, a national-park POI filter) for a country, state, province, park
  or place; a result's `boundingRegion` becomes the rectangle;
* presets: "Around me" (50 km), "Around my maps" (all located maps + 30 km);
* the rectangle is an overlay with four corner handles (44 pt) to adjust; it pans and zooms with
  the map;
* the bottom panel: name (from the search, editable), Standard / High detail with one-line
  summaries, the estimate ("≈ 342 MB · 2 475 tiles · measured", "Wi-Fi recommended" from 25 MB),
  free space, Download. Offline, search is disabled and the presets and the rectangle still work.

**Detail levels and caps.** Standard = zooms 8–12: locating maps offline (the coarse search on
8–11 and a refinement on zoom 12, ~38 m cells: typically ±10–20 m instead of ±5–10 m; the ±
the app shows stays honest), profiles, terrain beyond sheets, 3D. High = 8–13: as online. Caps:
60 000 tiles (≈ 15 min on good Wi-Fi) and 2.5 GB of estimate; free space must exceed the
estimate × 1.25 + 500 MB. Too large → "Too large for High detail: pick a smaller area or
Standard".

**Sizes, measured** (HEAD requests on s3 elevation-tiles-prod, October 2026, 9–25 tiles per zoom
in nine regions: the appendix). Terrarium tiles weigh 2–4× more where the source is
high-resolution (US 3DEP, European DEMs) than where it is SRTM:

| mean KB per tile | z8 | z9 | z10 | z11 | z12 | z13 |
|---|---|---|---|---|---|---|
| SRTM terrain (Drakensberg, Magaliesberg, Khumbu, Tasmania, Patagonia) | 82 | 74 | 61 | 53 | 44 | 35 |
| high-resolution sources (Swiss Alps, Colorado, Scotland, Kansas) | 82 | 102 | 94 | 127 | 117 | 103 |

| region (bounding box) | Standard z8–12: tiles / MB | High z8–13: tiles / MB |
|---|---|---|
| uKhahlamba-Drakensberg Park | 232 / 13 | 862 / 36 |
| Yosemite NP | 146 / 19 | 503 / 60 |
| Lesotho | 1 150 / 61 | 4 399 / 184 |
| KwaZulu-Natal | 3 732 / 198 | 14 354 / 600 |
| Switzerland | 2 475 / 342 | 9 443 / 1 211 |
| Nepal | 6 978 / 450 | 27 361 / 1 474 |
| Colorado | 6 596 / 854 | 25 594 / 3 037 (over the cap) |
| Scotland | 7 878 / 958 | 30 963 / 3 179 (over the cap) |
| South Africa | 41 851 / 1 440 | 165 645 / 4 722 (over the cap) |

Because of that spread, the estimate is **measured** when online: `mm_tile_plan` gives the tile
ranges and a prior (`bytes` for SRTM terrain, `bytes_high` for high-resolution sources); the app
then HEADs 16 tiles spread over the plan's two finest zooms and scales each zoom by the sampled
mean (≈ 1 s). Offline the prior's `bytes_high` is shown, marked "estimated".

**Download engine** (`OfflineRegionService` + a downloader):

* one region at a time, others queued; 8 concurrent requests (`URLSession` data tasks, 15 s
  timeouts); each tile written straight into `Terrain/tiles` (a tile already in `Caches/dem` is
  moved, not downloaded again); the owner's ranges recorded first, so a crash leaves nothing
  unowned;
* progress = tiles on disk / planned (resume = skip tiles present); retries: 3 attempts per tile
  (0.5, 2, 8 s), 404 = no data (counted as done); failures listed at the end ("12 tiles couldn't
  be downloaded · Retry");
* pause / resume / cancel (cancel deletes the region's tiles nobody else owns); offline →
  paused "Waiting for network"; an expensive path asks first unless Settings allows mobile data;
  Low Data Mode pauses "Paused in Low Data Mode"; storage full → paused with the reason;
* **in the background**: on iOS 26 the download continues as a `BGContinuedProcessingTask`
  (identifier `com.jacobusoettle.mapme.offline-download`, declared in project.yml) with the
  system's progress UI; on iOS 17–18 it continues for the ~30 s `beginBackgroundTask` grants
  (or as long as a recording or followed route keeps MapMe running) and then pauses, resuming by
  itself when MapMe is back in front. The UI says which.
* the list: name, detail, size on disk, progress or date, swipe to delete.

**Pinned store** (`PinnedTerrainStore`): `Terrain/tiles` + owners' range manifests; a tile stays
while any owner covers it (an in-memory index of ranges per zoom answers "is it pinned" in
microseconds). `DEMService.tile` looks there first, then `Caches/dem`, then the network; the LRU
trim only ever sees `Caches/dem`, so pinned tiles are never evicted. Settings → Storage shows
"Offline terrain" separately.

**Trailhead caching**: after every successful locate (and calibration) while online and
`cacheTerrainAroundMaps` is on (default): `mm_tile_plan` with the map's corners as `trailhead`
(zooms 9–10 within 50 km, 11 within 30 km, 12 over the sheet + continuation margin, 13 over the
sheet + 3 km; a fine zoom over 400 tiles is dropped: 3–14 MB for a 1:25 000 sheet), downloaded at
utility priority and pinned for owner `.map(id)`; deleted with the map. Located offline (from a
region) → remembered and done when the network is back (`NetworkPathObserver`). The 3D view and
the off-map layer thereby work offline around every located map.

**Matching offline**: with the tiles pinned, `TerrainPullLoop` gets them from `DEMService`
instantly. Two changes (offline builder): after a cutoff (tile cap, offline) the loop still asks
`DEMService` for the tiles **on the device** (a cache-only fetch) and marks only the others
missing, so a search that starts offline uses the region; and `GeoreferenceService` skips the
place-name lookups offline as before. The "Search wider" sheet says "Offline: searching the
terrain on your iPhone (Lesotho, terrain around 3 maps)".

## 8. Threading and performance

* Core terrain calls: `Task.detached(priority: .userInitiated)` for interactive work (route stats
  while editing, 3D build), `.utility` for prefetch and trailhead caching; on CATiledLayer's
  threads for tiles. `TerrainStore` is safe on all of them at once.
* Budgets (A15): a profile of 1 000 samples < 20 ms with tiles held; a 384² grid < 30 ms; a 512 px
  render tile < 15 ms (dark map tiles are ~6 ms); a tile plan < 5 ms for a country; the
  `need_tiles` round trip dominated by the download.
* Overlays: reproject in `mapDidChange` (off the main thread for > 20 k points, then hop back);
  per frame only transforms and positions; collision placement only on zoom/rotation changes (as
  `MapOverlayView` does).
* `GeoItemStore` writes on its own queue; JSON of 10 k route points encodes in ~5 ms.

## 9. Errors and edge cases

* Map not located: no overlays, no 3D (dimmed button with a reason), no auto-switch; geo items
  appear once it is located.
* Photos of maps with a warp: always project through `Georeference.Projector` (homography + warp
  inverse); never use the homography alone.
* Antimeridian: `GeoBounds` with west > east; plans split ranges; Mercator rectangles never cross
  it (the world view clips).
* Latitudes past ±85°: clamped by Web Mercator; no terrain.
* No terrain (offline, nothing pinned): stats show distance only, profiles "Elevation needs
  terrain", 3D and off-map show their offline messages; nothing spins.
* Ocean: Terrarium has bathymetry; contours below 0 m are not drawn, 0 m is drawn as an index line
  (the coast).
* Flat terrain: few contours; the interval does not shrink below the rule.
* Huge GPX (100 k points): parsed off the main thread, drawn simplified, profiled with
  `max_samples`.
* Duplicate imports: Replace / Keep Both.
* Polar day/night: daylight banner says so; no ETA warning.
* Low storage: atomic writes fail → the change is kept in memory and a toast says "Couldn't save:
  storage full".
* Permission denied (camera, notifications, location Always): the feature explains and offers
  Settings; nothing breaks.
* Time zone: the device's (what the hiker's watch says).

## 10. Tests and CI screenshots

Every new screen gets a UI test that attaches a screenshot (`uiAttachScreenshot`: JPEG, medium
quality) so the lead can check it on CI. Shared helpers in `MapMeUITests/ExplorerUITestSupport.swift`
(`uiOpenSampleMap`, `uiAllowSystemAlerts`, `uiSampleLocation`, `uiOffMapLocation`). Debug launch
arguments (handled in each feature's `appDidLaunch`):

| Argument | Effect | Owner |
|---|---|---|
| `-seedExplorerItems` | a planned route, 3 waypoints and a photo note on the Grootkloof sheet | routes, notes |
| `-importSampleGPX` | imports a bundled GPX (a route + a track + waypoints near Grootkloof) through the real path | routes |
| `-followSampleRoute` | follows the seeded route | routes |
| `-mockNow 2026-06-21T15:30:00+02:00` | the clock for daylight and ETA | routes |
| `-seedSecondSampleMap` | clones the located sample as "Grootkloof overview" at ¼ resolution (pyramid level 2, `H·diag(4,4,1)`), for switching tests | maps |
| `-openWorldView` | opens the world view at launch | maps |
| `-mockLowPowerMode` | the battery saver sees Low Power Mode on | battery |
| `-openOfflineRegions` | opens Settings → Offline terrain | offline |
| `-mockRegionPlace "Name=s,w,n,e"` | replaces MKLocalSearch in the region picker | offline |
| `-open3D` | opens the 3D view once the sample map shows | 3d |

Screenshots (name prefixes keep CI's gallery ordered): 20 world view, 21 maps here, 22 off-map
terrain (`uiOffMapLocation`), 23 auto-switch toast; 30 route planning bar, 31 route card with
profile, 32 following with the daylight banner, 33 off route, 34 GPX import summary, 35 track
profile; 40 note editor, 41 notes on the map, 42 photo note card; 45 battery saver settings and
badge; 50 offline terrain settings, 51 region picker with estimate, 52 downloading; 60 3D view,
61 3D with a route.

Unit tests (`MapMeTests/<Feature>Tests.swift`): GPX parse/export round trips (Gaia- and
AllTrails-style files inline), sun times against NOAA reference values (Pretoria, Tromsø in
June/December, Quito), Tobler/pace learning, route progress and off-route dwell on synthetic
fixes, coverage and switching rules (pure functions over two synthetic georeferences), the
continuation margin, tile plans and the pinned store's owner arithmetic, battery profile mapping,
3D UV computation through a warp, EXIF GPS reading from a generated JPEG. Rust:
`core/tests/terrain.rs` (store bound and eviction under concurrency, elevations and profiles on
synthetic tiles — a cone's ascent equals its height within the smoothing, grid NaN handling,
renders identical across tilings, the interval rule, `need_tiles` order, plan counts against
`tiles_covering`, trailhead caps, antimeridian), timing assertions with
`common::time_budget_ms`. `core/tests/terrain_contract.rs` already holds the ABI's contract
(arguments, NULLs, JSON, threads) and must keep passing.

## 11. File ownership

Builders create files only under their own folders and edit only the files listed for them. The
lead owns everything else, including every file marked "lead"; a builder that needs a change
there describes it in its report instead of making it.

| Builder | Owns (create / edit freely) |
|---|---|
| core | `core/src/terrain.rs` (may split into `core/src/terrain/*.rs`), `core/src/ffi/terrain.rs`, `core/tests/terrain*.rs`, `core/src/dem.rs` (add helpers; georef behaviour unchanged), docs/ARCHITECTURE.md "C ABI" → Terrain and performance numbers |
| routes | `ios/MapMe/Services/Routes/*`, `ios/MapMe/Views/Routes/*`, `Views/Map/NearbySheet.swift`, `Views/Map/MapStatsPanel.swift`, `Views/Settings/TracksView.swift`, `Model/Track.swift` (GPX helpers), `MapMeTests/Routes*Tests.swift`, `MapMeTests/GPX*Tests.swift`, `MapMeTests/Daylight*Tests.swift`, `MapMeUITests/Routes*UITests.swift` |
| maps | `ios/MapMe/Services/Maps/*`, `ios/MapMe/Views/Maps/*`, `MapMeTests/Maps*Tests.swift`, `MapMeUITests/Maps*UITests.swift`, `MapMeUITests/WorldView*UITests.swift` |
| notes+battery | `ios/MapMe/Services/Notes/*`, `ios/MapMe/Views/Notes/*`, `ios/MapMe/Services/Battery/*`, `ios/MapMe/Views/Battery/*`, `MapMeTests/Notes*Tests.swift`, `MapMeTests/Battery*Tests.swift`, `MapMeUITests/Notes*UITests.swift`, `MapMeUITests/Battery*UITests.swift` |
| offline | `ios/MapMe/Services/Offline/*`, `ios/MapMe/Views/Offline/*`, `ios/MapMe/Services/Terrain/TerrainProvider.swift`, `Services/DEMService.swift`, `Services/TerrainPullLoop.swift`, `Views/Import/SearchWiderSheet.swift` (offline note only), `Model/OfflineRegion.swift` (add Optional fields), `MapMeTests/Offline*Tests.swift`, `MapMeUITests/Offline*UITests.swift` |
| 3d | `ios/MapMe/Views/Terrain3D/*`, `MapMeTests/Terrain3D*Tests.swift`, `MapMeUITests/Terrain3D*UITests.swift` |

Integration points (shared files a builder may touch, and how):

* **core**: `core/include/mapme_core.h` — signatures frozen; doc wording only. `core/src/lib.rs` —
  `mod` lines only.
* **routes**: `LocationService.setBackgroundUpdates(_:for: "routeFollow")` — call only.
  `GeoItemStore` — call only; extra queries in an `extension GeoItemStore` in a routes file.
  `MapScreenContext.tool / sheet / cover / toast / selectedItem` — set only.
* **maps**: `Views/Map/MapCanvasUIView.swift` — only `clampCenter`, `minZoom`, `followTarget`,
  `makeHandoff`, `applyHandoff` (continuation and switching behaviour). `MapScreenContext.switchMap`
  — call only.
* **notes+battery**: `Services/LocationService.swift` — the body of `apply(_:)` and the manager's
  configuration lines it needs. `MapOverlayView.setAnimationsEnabled` — call only.
* **offline**: `Services/TerrainPullLoop.swift`, `Services/DEMService.swift` (pinned lookup,
  cache-only fetch) — owned. `AppModel.terrain` — the provider's internals only.
* **3d**: `MapScreenContext.cover` — set only. `TerrainStore.grid` / `.render` — call only.
* **everyone**: `App/FeatureHooks.swift`, `Views/Map/CanvasOverlaySet.swift`,
  `Views/Map/MapSideColumn.swift`, `Views/Map/MapSelectionCards.swift`, `AppSettings.swift`,
  `ios/project.yml` are lead files: their lines already call every builder's types. A builder
  needing a new setting, Info.plist key or hook asks the lead.

## 12. Rules for builders

1. Fill in the stubs with the signatures as declared. Add types and files freely in your folders;
   do not rename or remove a declared type, method or accessibility identifier another file uses.
2. Swift 5 language mode, iOS 17.0 target: guard iOS 18/26 APIs with `#available`, keep
   `#if compiler(>=6.2)` around Liquid Glass; system frameworks only (SwiftUI, UIKit, MapKit,
   SceneKit, Charts, PhotosUI, CoreLocation, CoreMotion, BackgroundTasks, UserNotifications).
3. Services are `@MainActor @Observable final class`; anything a background thread touches is a
   value type or an `@unchecked Sendable` class with its own lock (as `TerrainStore`, `TileStore`,
   `TerrainProvider`). Core calls never on the main thread.
4. Persisted types: new fields Optional; files written atomically off the main thread; unreadable
   files moved aside, never overwritten.
5. Look: `Theme` tokens (`route`, `importedRoute`, `note`, `uiOffMap*`, `uiSheetEdge` are new),
   `floatingChrome`, `MapRoundButtonStyle`, SF Symbols, haptics via `.sensoryFeedback`; every new
   control has an accessibility label and an identifier; dark first, paper checked.
6. Comments: as dense as the existing code — say why, cite the design section, give numbers.
7. Tests: unit tests for every rule with numbers in this document; a UI test with a screenshot for
   every new screen; `scripts/check_swift_syntax.py ios` clean; re-read your Swift as the compiler
   (labels, optionals, isolation, exhaustive switches, Codable); Rust: `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test --release` with a private
   `CARGO_TARGET_DIR`.
8. Commits end with the session's attribution lines; never push; never mention model names.
9. Report: what you built, the screenshots' names, measurements, and any change you need in a lead
   file.

## Appendix: tile size measurements

HEAD requests (`Content-Length`) on `https://s3.amazonaws.com/elevation-tiles-prod/terrarium/`,
October 2026: a 3×3 (z8–9), 4×4 (z10–12) or 5×5 (z13) block of tiles around each point.

| region (centre) | z8 | z9 | z10 | z11 | z12 | z13 KB |
|---|---|---|---|---|---|---|
| Swiss Alps (46.55, 8.0) | 86 | 152 | 145 | 138 | 133 | 122 |
| Colorado Rockies (39.6, −106.0) | 84 | 79 | 68 | 138 | 129 | 112 |
| Drakensberg (−29.3, 29.4) | 83 | 82 | 69 | 60 | 48 | 37 |
| Magaliesberg (−25.85, 27.4) | 60 | 52 | 39 | 35 | 32 | 26 |
| Kansas (38.5, −98.5) | 51 | 43 | 30 | 105 | 92 | 83 |
| Scottish Highlands (57.0, −5.2) | 91 | 137 | 133 | 129 | 115 | 94 |
| Khumbu (27.95, 86.85) | 82 | 93 | 83 | 71 | 59 | 49 |
| Tasmania (−42.0, 146.4) | 102 | 74 | 59 | 50 | 39 | 27 |
| Patagonia (−49.3, −73.0) | 82 | 70 | 56 | 49 | 43 | 38 |

The jump at zoom 11 in Colorado and Kansas is where the source changes to 3DEP. The largest tile
seen was 156 KB (z9, Swiss Alps). `wide::tile_bytes`, measured over southern African searches,
stays as it is for `mm_search_estimate`; `terrain::tile_bytes_typical` / `tile_bytes_high` are
these two rows.
