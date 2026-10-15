"""Synthetic topographic maps with exact ground truth, for the contour benchmark.

Renders a map of a real place from Terrarium DEM tiles in one of many cartographic
styles and writes a benchmark case directory (the common case format read by
fixtures/tools/bench_contours.py and core/tests/styles.rs):

    case.json          name, image, gps (map centre), truth_points (pixel -> lat/lon,
                       25 points over the map content), dem_dir, ocr, dpi, m_per_px,
                       contour_interval_m, style, tags, notes, contour_mask, ...
    map.jpg            the map as the app would get it (page render, scan or screenshot)
    contour_mask.png   the true contour-line pixels of map.jpg (ink of the contour layer)
    ocr.json           the text printed on the map (exact boxes, a perfect-OCR stand-in;
                       coordinate labels of the grid are left out: this is a contour benchmark)

Usage:
    python synth_topo.py suite bench  OUT_ROOT [--dem-cache DIR] [--only SUBSTR] [-j N]
    python synth_topo.py suite regression OUT_ROOT [--dem-cache DIR]
    python synth_topo.py case OUT_DIR --region R --style S --mpp M --size W H
                         [--upsample K --upsample-mode bilinear|nearest] [--skew DEG] [--seed N]
    python synth_topo.py list

Requirements: numpy, scipy, opencv-python, pillow, contourpy, pyproj, scikit-image.

The realism gap. The core matches contours against Terrarium zoom 11 (coarse search) and
zoom 13 (refinement). A map traced from those same tiles would be trivially easy, so the
contours here come from another zoom (14, or 12 for small-scale maps) resampled to the
map grid, generalised (Gaussian smoothing of the terrain and of the traced lines), offset
by a smooth random elevation field (a different survey: a few metres over ~1.5 km), and
displaced by a smooth elastic warp of 1-2.5 px (drafting); tiny closed loops are dropped,
lines break under labels, every 4th/5th line is a bolder index contour, and the maps carry
the usual clutter (hillshade, forest, water, streams, trails, roads, grid, names, spot
heights, contour labels, legend and scale bar in the margin), then the page effects
(page upsampling of a low-ppi raster, scan blur and noise, photocopy, fading, skew, JPEG).
The truth is exact: pixel -> lat/lon through the map projection (Web Mercator, UTM or a
national transverse Mercator) and the page geometry; the warp and the DEM offsets are
the map's drafting error, not part of the truth.
"""
import argparse
import hashlib
import heapq
import json
import math
import os
import sys
import time
import urllib.request
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor

import contourpy
import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageFont
from pyproj import CRS, Geod, Transformer
from scipy import ndimage
from skimage.graph import route_through_array

TERRARIUM = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"
R_EARTH = 6378137.0
ORIGIN = math.pi * R_EARTH
FT = 0.3048
GEOD = Geod(ellps="WGS84")

# ---------------------------------------------------------------------------
# Regions
# ---------------------------------------------------------------------------

REGIONS = {
    "drakensberg": dict(lat=-29.27, lon=29.47, lang="za", treeline=1900, forest=0.06, snowline=None, tm=29,
                        notes="uKhahlamba-Drakensberg escarpment, 1400-3400 m"),
    "magaliesberg": dict(lat=-25.80, lon=27.45, lang="za", treeline=9999, forest=0.15, snowline=None, tm=27,
                         notes="Magaliesberg, low quartzite ridges, 1100-1800 m"),
    "alps": dict(lat=46.56, lon=7.93, lang="de", treeline=2100, forest=0.5, snowline=2900, tm=None,
                 notes="Bernese Oberland, 600-4100 m, glaciers"),
    "rockies": dict(lat=39.63, lon=-105.78, lang="us", treeline=3450, forest=0.6, snowline=None, tm=None,
                    notes="Colorado Front Range, 2700-4300 m"),
    "scottish_highlands": dict(lat=56.66, lon=-5.02, lang="gd", treeline=400, forest=0.10, snowline=None, tm=-2,
                               notes="Glen Coe, sea level to 1150 m"),
    "lake_district": dict(lat=54.47, lon=-3.08, lang="gb", treeline=450, forest=0.25, snowline=None, tm=-2,
                          notes="Lake District fells, 50-950 m, lakes"),
    "appalachians": dict(lat=38.55, lon=-78.40, lang="us", treeline=9999, forest=0.85, snowline=None, tm=None,
                         notes="Blue Ridge / Shenandoah, 300-1200 m, forested, low relief"),
    "norway_fjords": dict(lat=60.88, lon=7.15, lang="no", treeline=900, forest=0.4, snowline=1650, tm=None,
                          notes="Aurlandsfjord, sea level to 1750 m"),
    "patagonia": dict(lat=-49.30, lon=-72.98, lang="es", treeline=1000, forest=0.35, snowline=1500, tm=None,
                      notes="Fitz Roy massif, 300-3400 m, glaciers and lakes"),
    "himalaya_foothills": dict(lat=30.42, lon=78.12, lang="hi", treeline=3300, forest=0.55, snowline=None, tm=None,
                               notes="Garhwal foothills near Mussoorie, 500-2300 m"),
    "tasmania": dict(lat=-41.66, lon=145.95, lang="au", treeline=1100, forest=0.5, snowline=None, tm=None,
                     notes="Cradle Mountain, 700-1550 m, lakes"),
    "atlas": dict(lat=31.06, lon=-7.92, lang="ber", treeline=9999, forest=0.04, snowline=None, tm=None,
                  notes="High Atlas (Toubkal), 1200-4150 m, arid"),
    "flat_freestate": dict(lat=-27.62, lon=27.25, lang="za", treeline=9999, forest=0.0, snowline=None, tm=27,
                           flat=True, notes="Free State highveld plains, a few tens of metres of relief"),
    "flat_kansas": dict(lat=38.45, lon=-98.25, lang="us", treeline=9999, forest=0.02, snowline=None, tm=None,
                        flat=True, notes="Great Plains, near-flat"),
    "lyngen": dict(lat=69.60, lon=20.25, lang="no", treeline=500, forest=0.2, snowline=1100, tm=None,
                   notes="Lyngen Alps, sea level to 1830 m, fjords and glaciers"),
    "svalbard": dict(lat=78.15, lon=16.0, lang="no", treeline=0, forest=0.0, snowline=600, tm=None,
                     notes="Svalbard, Adventdalen / Longyearbyen, sea level to 1100 m, glaciers"),
}

NAME_PARTS = {
    "za": (["Groot", "Klein", "Rooi", "Blou", "Wit", "Leeu", "Bobbejaan", "Isi", "Nta", "Mhla", "Ezi", "Kwa"],
           ["kloof", "kop", "berg", "nek", "fontein", "baba", "thini", "ndlovu", "rant", "poort", "hoek", "vlei"]),
    "de": (["Schwarz", "Wetter", "Gross", "Klein", "Roth", "Lauter", "Grindel", "Wengen", "Mürren", "Ober", "Hinter"],
           ["horn", "alp", "stock", "grat", "egg", "fluh", "berg", "tal", "gletscher", "joch", "matte"]),
    "us": (["Bear", "Elk", "Grizzly", "Pine", "Cabin", "Silver", "Gold", "Buffalo", "Deer", "Laurel", "Cedar"],
           [" Peak", " Creek", " Ridge", " Gulch", " Hollow", " Knob", " Mountain", " Run", " Gap", " Lake"]),
    "gd": (["Beinn", "Sgurr", "Stob", "Meall", "Coire", "Glen", "Allt", "Bidean", "Aonach", "Buachaille"],
           [" Dearg", " Mor", " Beag", " nam Bian", " Eagach", " Etive", " Liath", " Ruadh", " Dubh"]),
    "gb": (["High", "Great", "Low", "Scafell", "Grisedale", "Helvellyn", "Skiddaw", "Wast", "Black", "Red"],
           [" Pike", " Fell", " Crag", " Tarn", " Beck", " Gill", " Howe", " Edge", " Knott", " Water"]),
    "no": (["Aurlands", "Flåm", "Stor", "Vest", "Kvit", "Svart", "Holm", "Berge", "Fjær", "Rauda"],
           ["fjellet", "tinden", "dalen", "vatnet", "breen", "nosi", "hovden", "stølen", "elva", "skaret"]),
    "es": (["Cerro", "Laguna", "Arroyo", "Monte", "Aguja", "Río", "Lago", "Paso", "Glaciar", "Loma"],
           [" Blanco", " Torre", " Eléctrico", " Fitz Roy", " del Desierto", " Madre", " Piedras", " Huemul"]),
    "hi": (["Bhadraj", "Kempty", "Jabarkhet", "Dhanaulti", "Surkanda", "Lal", "Bhatta", "Kolu", "Rikholi"],
           [" Danda", " Khala", " Gaon", " Lekh", " Tibba", " Dhar", " Gad", " Chakrata"]),
    "au": (["Cradle", "Barn", "Dove", "Marions", "Hansons", "Lake", "Little", "Crater", "Waldheim", "Fury"],
           [" Bluff", " Tier", " Peak", " Rivulet", " Plains", " Lookout", " Lake", " Gorge"]),
    "ber": (["Jbel", "Tizi n'", "Assif", "Ait", "Imlil", "Aremd", "Tamsoult", "Ouanoukrim", "Imouzzer"],
            [" Toubkal", " Ouanoums", " Mizane", " Likemt", " Tazaghart", " Oussaka", " Afella", " Timesguida"]),
}

# ---------------------------------------------------------------------------
# Styles
# ---------------------------------------------------------------------------

BASE_STYLE = dict(
    proj="utm", paper=(250, 249, 245), contour=(180, 125, 75), contour_w=0.9, index_every=5, index_w=1.7,
    index_rgb=None, contour_alpha=1.0, units="m", interval=None, hillshade=0.0, shade_tint=None, forest=None, forest_edge=None,
    forest_symbols=False, park=None, water=(170, 210, 235), water_edge=(80, 140, 200), glacier=None,
    glacier_contour=None, rock=False, hypso=False, farmland=None, streams=(70, 140, 205), stream_w=0.9,
    stream_area_km2=0.5, trails=(), roads=(), grid=None, text=(30, 30, 30), water_text=(40, 100, 170),
    font="sans", name_pt=7.5, halo=None, spot="tri", spot_rgb=(30, 30, 30), contour_labels=True,
    contour_label_rgb=None, loop_min_px=18, gen=1.0, warp_px=1.5, ppi=254, effect=None, page="scan",
    ratio=None, legend=True, attribution="", title_font="serif", page_margins=(0.35, 0.55, 0.35, 0.9),
    frame_page=False,
)

NICE_RATIOS = [10000, 12500, 15000, 20000, 24000, 25000, 30000, 40000, 50000, 62500, 75000, 100000, 125000,
               150000, 200000, 250000, 300000, 500000]

# Paper sizes (inches, portrait) for PDF pages: the smallest that holds the map and its margins.
PAPERS = [("A4", 8.27, 11.69), ("Letter", 8.5, 11.0), ("A3", 11.69, 16.54), ("A2", 16.54, 23.39),
          ("A1", 23.39, 33.11)]

STYLES = {
    # Gaia GPS "Gaia Topo (feet)" PDF export: thin light-brown contours in feet over hillshade,
    # green park tint, dense blue streams, grey dashed trails, Web Mercator, ~100 ppi raster.
    "gaia": dict(proj="merc", paper=(231, 234, 221), contour=(182, 168, 132), contour_w=1.0, index_w=1.4,
                 index_rgb=(166, 148, 110), contour_alpha=0.8, page_margins=(0.12, 0.12, 0.12, 0.2), frame_page=True,
                 pdf_aniso=(0.008, 0.02), units="ft", hillshade=0.4, park=(206, 226, 196),
                 streams=(118, 184, 206), stream_w=0.9, stream_area_km2=0.05, trails=[dict(rgb=(110, 110, 115), w=1.3, dash=(4, 3), n=3, kind="trail")],
                 roads=[dict(rgb=(250, 250, 250), w=2.0, casing=(170, 170, 170), casing_w=3.2, n=1)],
                 text=(70, 70, 70), halo=(250, 250, 248), font="sans_italic", name_pt=7.0, spot="peak",
                 spot_rgb=(80, 70, 60), contour_labels=True, ppi=100, page="pdf", legend=False,
                 attribution="Gaia Topo (feet): © Gaia GPS, OpenStreetMap", loop_min_px=10, gen=0.8,
                 margin_lines=["Web Mercator | EPSG:3857", "Declination: {decl}°"], scalebar="mi"),
    # OpenTopoMap: brown contours, strong hillshade, forest green, yellow/orange roads.
    "opentopo": dict(proj="merc", paper=(242, 239, 232), contour=(165, 115, 75), contour_w=0.9, index_w=1.6,
                     hillshade=0.6, forest=(176, 211, 152), farmland=(236, 236, 214), streams=(90, 150, 220),
                     stream_area_km2=0.3,
                     roads=[dict(rgb=(250, 205, 120), w=2.4, casing=(150, 120, 80), casing_w=3.6, n=2),
                            dict(rgb=(255, 255, 255), w=1.6, casing=(160, 160, 160), casing_w=2.6, n=2)],
                     trails=[dict(rgb=(170, 60, 60), w=1.0, dash=(3, 2), n=2, kind="trail")],
                     halo=(248, 248, 244), name_pt=7.5, ppi=110, page="screen", legend=False,
                     attribution="© OpenStreetMap contributors, SRTM | map style: © OpenTopoMap (CC-BY-SA)",
                     loop_min_px=12),
    # USGS 7.5' quadrangle (scanned): brown contours in feet with bold index, green woodland,
    # blue water, red/black roads, UTM ticks, 1:24 000.
    "usgs": dict(proj="utm", paper=(252, 251, 246), contour=(196, 133, 82), contour_w=1.0, index_w=2.1,
                 units="ft", forest=(205, 232, 192), streams=(60, 130, 210), stream_w=1.1, stream_area_km2=0.4,
                 roads=[dict(rgb=(215, 45, 45), w=2.0, casing=(40, 40, 40), casing_w=3.4, n=1),
                        dict(rgb=(250, 250, 250), w=1.6, casing=(40, 40, 40), casing_w=3.0, n=2)],
                 trails=[dict(rgb=(40, 40, 40), w=1.0, dash=(5, 3), n=2, kind="trail")],
                 grid=dict(kind="utm_ticks", rgb=(40, 40, 40), w=1.0), font="sans", name_pt=7.5, ppi=250,
                 effect="scan", ratio=24000, attribution="Produced by the United States Geological Survey (synthetic)"),
    # USGS 1:100 000 metric-or-feet series: 100 ft interval, pale woodland, purple-ish roads.
    "usgs100k": dict(proj="utm", paper=(252, 251, 246), contour=(185, 130, 85), contour_w=0.9, index_w=1.8,
                     units="ft", interval=100 * FT, forest=(212, 235, 200), streams=(60, 130, 210),
                     stream_area_km2=1.5,
                     roads=[dict(rgb=(200, 60, 60), w=2.2, casing=(40, 40, 40), casing_w=3.4, n=2)],
                     grid=dict(kind="utm", rgb=(60, 60, 160), w=0.8), name_pt=7.0, ppi=250, effect="scan",
                     ratio=100000, attribution="Topography compiled 1980 (synthetic)"),
    # Swiss national map style: brown contours in soil, rock drawn black (no contours),
    # glaciers with blue contours, strong grey-violet relief shading, black km grid.
    "swiss": dict(proj="utm", paper=(250, 250, 244), contour=(174, 112, 60), contour_w=0.85, index_w=1.5,
                  hillshade=0.75, shade_tint=((120, 122, 155), (255, 251, 228)), forest=(192, 222, 178),
                  forest_edge=(90, 140, 80), glacier=(232, 242, 252), glacier_contour=(70, 140, 215), rock=True,
                  streams=(50, 120, 210), stream_area_km2=0.4,
                  roads=[dict(rgb=(250, 250, 250), w=1.8, casing=(30, 30, 30), casing_w=3.0, n=2)],
                  trails=[dict(rgb=(200, 30, 30), w=1.0, dash=(4, 2), n=3, kind="trail")],
                  grid=dict(kind="utm", rgb=(30, 30, 30), w=0.7), font="sans", name_pt=7.5, ppi=254,
                  effect="scan", ratio=50000, attribution="Landeskarte-like (synthetic)"),
    # OS Landranger 1:50 000: orange contours (10 m), blue km grid, pink/orange/yellow roads,
    # woodland with tree symbols.
    "os": dict(proj="tm", paper=(252, 252, 250), contour=(222, 148, 82), contour_w=0.85, index_w=1.35,
               interval=10.0, forest=(214, 236, 200), forest_symbols=True, streams=(40, 130, 210),
               stream_area_km2=0.3,
               roads=[dict(rgb=(232, 100, 110), w=2.6, casing=(60, 60, 60), casing_w=3.6, n=1),
                      dict(rgb=(245, 172, 78), w=2.2, casing=(60, 60, 60), casing_w=3.2, n=1),
                      dict(rgb=(252, 242, 120), w=1.8, casing=(60, 60, 60), casing_w=2.8, n=2)],
               trails=[dict(rgb=(200, 40, 40), w=1.0, dash=(2, 2), n=2, kind="trail")],
               grid=dict(kind="utm", rgb=(40, 120, 210), w=0.9), font="sans", name_pt=7.0, ppi=254,
               effect="scan", ratio=50000, attribution="Landranger-like (synthetic)"),
    # Kompass / tourist map: pale contours under bold red trails, pale green, light shading.
    "kompass": dict(proj="utm", paper=(252, 250, 242), contour=(205, 165, 125), contour_w=0.8, index_w=1.2,
                    hillshade=0.3, forest=(216, 236, 200), streams=(80, 150, 220), stream_area_km2=0.5,
                    trails=[dict(rgb=(225, 30, 40), w=2.6, dash=(6, 2), n=5, kind="trail"),
                            dict(rgb=(225, 30, 40), w=2.6, dash=None, n=2, kind="trail")],
                    roads=[dict(rgb=(250, 220, 90), w=2.4, casing=(70, 70, 70), casing_w=3.4, n=1)],
                    text=(20, 20, 20), name_pt=8.0, font="sans_bold", ppi=150, page="pdf", ratio=50000,
                    attribution="Wanderkarte-like (synthetic)"),
    # Black-and-white photocopy of a USGS-like sheet: grey contours, toner noise, skew.
    "bw_photocopy": dict(proj="utm", paper=(252, 251, 246), contour=(196, 133, 82), contour_w=1.0,
                         index_w=2.0, forest=(205, 232, 192), streams=(60, 130, 210), stream_area_km2=0.5,
                         roads=[dict(rgb=(215, 45, 45), w=2.0, casing=(40, 40, 40), casing_w=3.4, n=1)],
                         trails=[dict(rgb=(40, 40, 40), w=1.0, dash=(5, 3), n=2, kind="trail")],
                         grid=dict(kind="utm", rgb=(40, 40, 40), w=0.8), ppi=200, effect="photocopy",
                         ratio=25000, attribution="Copy - not for navigation (synthetic)"),
    # Sun-faded print: pale, low-contrast, yellowed.
    "faded": dict(proj="utm", paper=(248, 244, 232), contour=(175, 125, 85), contour_w=0.9, index_w=1.6,
                  hillshade=0.35, forest=(200, 225, 185), streams=(80, 140, 200), stream_area_km2=0.5,
                  roads=[dict(rgb=(210, 70, 60), w=2.0, casing=(50, 50, 50), casing_w=3.2, n=1)],
                  ppi=200, effect="faded", ratio=50000, attribution="Hiking map (synthetic), 1987 edition"),
    # Thin 1 px grey contours and nothing else (CAD / planning map).
    "thin_grey": dict(proj="utm", paper=(255, 255, 255), contour=(150, 150, 150), contour_w=1.0,
                      index_rgb=(105, 105, 105), index_w=1.0, streams=None, water=None, contour_labels=True,
                      contour_label_rgb=(110, 110, 110), loop_min_px=6, gen=0.6, ppi=150, page="screen",
                      legend=False, name_pt=7.0, text=(60, 60, 60), spot="dot", spot_rgb=(60, 60, 60),
                      attribution="Contours 20 m (synthetic)"),
    # Dense 1:25 000 sheet, 10 m interval.
    "dense": dict(proj="utm", paper=(253, 252, 248), contour=(182, 118, 66), contour_w=0.75, index_w=1.4,
                  interval=10.0, hillshade=0.25, forest=(210, 234, 198), streams=(60, 130, 210),
                  stream_area_km2=0.25, trails=[dict(rgb=(50, 50, 50), w=1.0, dash=(4, 2), n=3, kind="trail")],
                  roads=[dict(rgb=(250, 250, 250), w=1.8, casing=(50, 50, 50), casing_w=3.0, n=1)],
                  grid=dict(kind="utm", rgb=(60, 60, 60), w=0.7), ppi=254, effect="scan", ratio=25000,
                  loop_min_px=14, attribution="1:25 000 (synthetic)"),
    # Sparse small-scale map (1:100 000-1:250 000), 100 m interval over hypsometric tints.
    "sparse": dict(proj="utm", paper=(250, 248, 240), contour=(150, 100, 60), contour_w=0.9, index_w=1.5,
                   interval=100.0, hypso=True, hillshade=0.3, streams=(50, 110, 200), stream_w=1.0,
                   stream_area_km2=6.0,
                   roads=[dict(rgb=(210, 40, 40), w=2.0, casing=None, casing_w=0, n=3)],
                   grid=dict(kind="utm", rgb=(90, 90, 90), w=0.6), name_pt=7.0, ppi=200, effect="scan",
                   ratio=250000, loop_min_px=8, gen=1.4, attribution="Topographic 1:250 000 (synthetic)"),
    # MCSA Magaliesberg-like: muted purple-brown contours on white, grey graticule, grey roads,
    # green vegetation hatching, spot heights in the contour colour.
    "mcsa": dict(proj="merc", paper=(255, 255, 255), contour=(164, 144, 134), contour_w=1.3, index_w=2.6,
                 interval=20.0, streams=(40, 110, 220), stream_w=1.2, stream_area_km2=0.4,
                 roads=[dict(rgb=(255, 255, 255), w=2.4, casing=(150, 150, 150), casing_w=4.2, n=2)],
                 trails=[dict(rgb=(20, 20, 20), w=1.4, dash=(7, 5), n=2, kind="trail")],
                 grid=dict(kind="graticule", rgb=(40, 40, 40), w=0.9), hatch=(60, 170, 70), spot="tri_open",
                 spot_rgb=(30, 30, 30), contour_label_rgb=(164, 144, 134), font="sans_bold", name_pt=8.0,
                 ppi=300, page="pdf", ratio=25000, attribution="MCSA-like hiking sheet (synthetic) GIS V1"),
    # Norwegian N50-like: orange-brown contours, forest green, lots of water, blue UTM grid.
    "n50": dict(proj="utm", paper=(253, 252, 247), contour=(205, 128, 70), contour_w=0.85, index_w=1.5,
                interval=20.0, forest=(214, 236, 196), streams=(40, 120, 210), stream_area_km2=0.35,
                glacier=(236, 244, 252), glacier_contour=(60, 130, 210),
                roads=[dict(rgb=(240, 150, 60), w=2.2, casing=(60, 60, 60), casing_w=3.2, n=1)],
                trails=[dict(rgb=(200, 30, 30), w=1.0, dash=(3, 2), n=2, kind="trail")],
                grid=dict(kind="utm", rgb=(40, 110, 210), w=0.8), ppi=254, effect="scan", ratio=50000,
                attribution="Topografisk N50-like (synthetic)"),
}

STYLE_NOTES = {
    "gaia": "Gaia Topo (feet)-like PDF export: thin light-brown feet contours over hillshade, park tint, "
            "dense blue streams, grey dashed trails, Web Mercator ~100 ppi raster",
    "opentopo": "OpenTopoMap-like screenshot: brown contours, strong hillshade, forest green, roads",
    "usgs": "USGS 7.5-minute-like scan: brown feet contours (40 ft), bold index, green woodland, blue water",
    "usgs100k": "USGS 1:100 000-like scan: 100 ft contours, woodland, blue UTM grid",
    "swiss": "Swiss-like: brown contours, rock drawing without contours, blue glacier contours, relief shading",
    "os": "OS Landranger-like: orange 10 m contours, blue km grid, pink/orange/yellow roads, woodland symbols",
    "kompass": "Kompass-like tourist PDF: pale contours under bold red trails",
    "bw_photocopy": "black-and-white photocopy of a USGS-like sheet, toner noise, skew",
    "faded": "sun-faded, low-contrast, yellowed print",
    "thin_grey": "thin 1 px grey contours only",
    "dense": "dense 1:25 000 sheet, 10 m interval",
    "sparse": "small-scale 1:100 000-1:250 000 map, 100 m interval over hypsometric tints",
    "mcsa": "MCSA Magaliesberg-like: muted purple-brown contours on white, black graticule, grey roads",
    "n50": "Norwegian N50-like: orange-brown contours, forest, water, blue UTM grid",
}


def style_spec(name):
    s = dict(BASE_STYLE)
    s.update(STYLES[name])
    s["name"] = name
    return s


# ---------------------------------------------------------------------------
# Terrarium tiles
# ---------------------------------------------------------------------------


def merc(lat, lon):
    return R_EARTH * math.radians(lon), R_EARTH * math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))


def unmerc(x, y):
    return math.degrees(2 * math.atan(math.exp(y / R_EARTH)) - math.pi / 2), math.degrees(x / R_EARTH)


def merc_np(lat, lon):
    return R_EARTH * np.radians(lon), R_EARTH * np.log(np.tan(np.pi / 4 + np.radians(lat) / 2))


def tile_range(z, x0, y0, x1, y1):
    """Tiles of zoom z covering the Mercator box (x0, y0)-(x1, y1)."""
    n = 2 ** z
    tx0 = int(math.floor((min(x0, x1) + ORIGIN) / (2 * ORIGIN) * n))
    tx1 = int(math.floor((max(x0, x1) + ORIGIN) / (2 * ORIGIN) * n))
    ty0 = int(math.floor((ORIGIN - max(y0, y1)) / (2 * ORIGIN) * n))
    ty1 = int(math.floor((ORIGIN - min(y0, y1)) / (2 * ORIGIN) * n))
    clamp = lambda v: min(max(v, 0), n - 1)
    return clamp(tx0), clamp(ty0), clamp(tx1), clamp(ty1)


def tiles_in_merc_box(z, x0, y0, x1, y1):
    tx0, ty0, tx1, ty1 = tile_range(z, x0, y0, x1, y1)
    return [(z, x, y) for x in range(tx0, tx1 + 1) for y in range(ty0, ty1 + 1)]


def ground_box(lat, lon, half_m):
    """Mercator box of +-half_m ground metres around (lat, lon)."""
    mx, my = merc(lat, lon)
    k = half_m / math.cos(math.radians(lat))
    return mx - k, my - k, mx + k, my + k


def fetch_tiles(cache, tiles, quiet=False):
    os.makedirs(cache, exist_ok=True)
    todo = [t for t in sorted(set(tiles)) if not os.path.exists(os.path.join(cache, "%d_%d_%d.png" % t))]

    def get(t):
        p = os.path.join(cache, "%d_%d_%d.png" % t)
        for attempt in range(4):
            try:
                with urllib.request.urlopen(TERRARIUM.format(z=t[0], x=t[1], y=t[2]), timeout=60) as r:
                    data = r.read()
                tmp = p + ".part%d" % os.getpid()
                with open(tmp, "wb") as f:
                    f.write(data)
                os.replace(tmp, p)
                return 1
            except Exception as e:  # noqa: BLE001 - retry any network error
                if attempt == 3:
                    print("  tile %s: %s" % (t, e), file=sys.stderr)
                time.sleep(1 + attempt)
        return 0

    if todo:
        with ThreadPoolExecutor(8) as ex:
            n = sum(ex.map(get, todo))
        if not quiet:
            print("  fetched %d/%d tiles into %s" % (n, len(todo), cache), file=sys.stderr)


def decode_tile(path):
    a = np.asarray(Image.open(path).convert("RGB"), dtype=np.float32)
    return a[..., 0] * 256.0 + a[..., 1] + a[..., 2] / 256.0 - 32768.0


class Mosaic:
    def __init__(self, cache, z, box):
        self.z = z
        tx0, ty0, tx1, ty1 = tile_range(z, *box)
        fetch_tiles(cache, [(z, x, y) for x in range(tx0, tx1 + 1) for y in range(ty0, ty1 + 1)])
        self.tx0, self.ty0 = tx0, ty0
        arr = np.zeros(((ty1 - ty0 + 1) * 256, (tx1 - tx0 + 1) * 256), np.float32)
        for x in range(tx0, tx1 + 1):
            for y in range(ty0, ty1 + 1):
                arr[(y - ty0) * 256:(y - ty0 + 1) * 256, (x - tx0) * 256:(x - tx0 + 1) * 256] = decode_tile(
                    os.path.join(cache, "%d_%d_%d.png" % (z, x, y)))
        # Terrarium's high zooms hold isolated garbage cells in places (down to -14 000 m in
        # Patagonia and Tasmania at z14): replace spikes by the local median.
        for size in (5, 9):
            med = ndimage.median_filter(arr, size)
            bad = (arr < -1000) | (np.abs(arr - med) > 120)
            if not bad.any():
                break
            arr[bad] = med[bad]
        self.arr = arr
        self.res = 2 * ORIGIN / (2 ** z * 256)

    def sample(self, mx, my, order=3):
        px = (mx + ORIGIN) / self.res - self.tx0 * 256 - 0.5
        py = (ORIGIN - my) / self.res - self.ty0 * 256 - 0.5
        return ndimage.map_coordinates(self.arr, [py.ravel(), px.ravel()], order=order, mode="nearest").reshape(
            np.shape(mx)).astype(np.float32)


# ---------------------------------------------------------------------------
# Map geometry: pixel <-> projection <-> lat/lon
# ---------------------------------------------------------------------------


class Geometry:
    """Native map raster of w x h px at `mpp` ground metres per px at the centre (lat0, lon0),
    drawn in `proj` (merc, utm, tm with central meridian `tm_lon0`) with grid north up."""

    def __init__(self, proj, lat0, lon0, w, h, mpp, tm_lon0=None):
        if proj == "merc":
            crs = CRS.from_epsg(3857)
        elif proj == "utm":
            zone = int((lon0 + 180) // 6) + 1
            crs = CRS.from_epsg((32600 if lat0 >= 0 else 32700) + zone)
        elif proj == "tm":
            crs = CRS.from_proj4("+proj=tmerc +lat_0=0 +lon_0=%g +k=0.9996 +x_0=500000 +y_0=%d +datum=WGS84 "
                                 "+units=m +no_defs" % (tm_lon0, 0 if lat0 >= 0 else 10000000))
        else:
            raise ValueError(proj)
        self.proj, self.crs = proj, crs
        self.fwd = Transformer.from_crs(CRS.from_epsg(4326), crs, always_xy=True)
        self.inv = Transformer.from_crs(crs, CRS.from_epsg(4326), always_xy=True)
        self.w, self.h, self.mpp = w, h, mpp
        cx, cy = self.fwd.transform(lon0, lat0)
        lon_e, lat_e, _ = GEOD.fwd(lon0, lat0, 90, 1000.0)
        lon_n, lat_n, _ = GEOD.fwd(lon0, lat0, 0, 1000.0)
        ex, ey = self.fwd.transform(lon_e, lat_e)
        nx, ny = self.fwd.transform(lon_n, lat_n)
        k = (math.hypot(ex - cx, ey - cy) + math.hypot(nx - cx, ny - cy)) / 2000.0
        self.sp = mpp * k  # planar metres per pixel
        self.c = (cx, cy)
        # Clockwise angle of map-up (grid north) from true north.
        self.rotation_deg = -math.degrees(math.atan2(nx - cx, ny - cy))

    def px_to_planar(self, x, y):
        return self.c[0] + (np.asarray(x) - self.w / 2) * self.sp, self.c[1] + (self.h / 2 - np.asarray(y)) * self.sp

    def planar_to_px(self, e, n):
        return (np.asarray(e) - self.c[0]) / self.sp + self.w / 2, self.h / 2 - (np.asarray(n) - self.c[1]) / self.sp

    def px_to_ll(self, x, y):
        e, n = self.px_to_planar(x, y)
        lon, lat = self.inv.transform(e, n)
        return np.asarray(lat), np.asarray(lon)

    def ll_to_px(self, lat, lon):
        e, n = self.fwd.transform(lon, lat)
        return self.planar_to_px(e, n)

    def merc_grid(self, gw, gh, f):
        """Mercator coordinates of the centres of a gw x gh grid of f x f native px cells."""
        step = 16
        lx = np.arange(0, gw + step, step, dtype=np.float64)
        ly = np.arange(0, gh + step, step, dtype=np.float64)
        X, Y = np.meshgrid((lx + 0.5) * f, (ly + 0.5) * f)
        lat, lon = self.px_to_ll(X, Y)
        mx, my = merc_np(lat, lon)
        gy, gx = np.mgrid[0:gh, 0:gw].astype(np.float32) / step
        out = []
        for a in (mx, my):
            out.append(ndimage.map_coordinates(a, [gy.ravel(), gx.ravel()], order=1, mode="nearest").reshape(gh, gw))
        return out

    def merc_bbox(self, margin_px=0):
        xs = np.array([-margin_px, self.w + margin_px, self.w + margin_px, -margin_px, self.w / 2, self.w / 2, -margin_px,
                       self.w + margin_px], dtype=np.float64)
        ys = np.array([-margin_px, -margin_px, self.h + margin_px, self.h + margin_px, -margin_px, self.h + margin_px,
                       self.h / 2, self.h / 2], dtype=np.float64)
        lat, lon = self.px_to_ll(xs, ys)
        mx, my = merc_np(lat, lon)
        return float(mx.min()), float(my.min()), float(mx.max()), float(my.max())


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------


def smooth_noise(rng, shape, corr_px, amp=1.0):
    """Zero-mean smooth random field with unit std times amp (correlation length corr_px)."""
    h, w = shape
    s = max(1.0, corr_px / 4.0)
    sh, sw = max(4, int(h / s) + 4), max(4, int(w / s) + 4)
    n = rng.standard_normal((sh, sw)).astype(np.float32)
    n = ndimage.gaussian_filter(n, 1.5, mode="wrap")
    n = cv2.resize(n, (w, h), interpolation=cv2.INTER_CUBIC)
    n -= n.mean()
    sd = n.std()
    return n * (amp / sd if sd > 0 else 0)


def up_to(a, w, h, interp=cv2.INTER_LINEAR):
    """Resize a grid of cells to the native raster (cell centres aligned)."""
    if a.shape == (h, w):
        return a
    return cv2.resize(a.astype(np.float32), (w, h), interpolation=interp)


def resample_line(p, step=1.0):
    if len(p) < 2:
        return p
    d = np.hypot(np.diff(p[:, 0]), np.diff(p[:, 1]))
    s = np.concatenate([[0], np.cumsum(d)])
    if s[-1] < step:
        return p
    t = np.arange(0, s[-1], step)
    return np.stack([np.interp(t, s, p[:, 0]), np.interp(t, s, p[:, 1])], 1)


def smooth_line(p, sigma, closed):
    if len(p) < 5 or sigma <= 0:
        return p
    mode = "wrap" if closed else "nearest"
    q = np.stack([ndimage.gaussian_filter1d(p[:, 0], sigma, mode=mode),
                  ndimage.gaussian_filter1d(p[:, 1], sigma, mode=mode)], 1)
    if not closed:
        q[0], q[-1] = p[0], p[-1]
    else:
        q = np.vstack([q, q[:1]])
    return q


def line_length(p):
    return float(np.hypot(np.diff(p[:, 0]), np.diff(p[:, 1])).sum()) if len(p) > 1 else 0.0


def dash_line(p, on, off, phase=0.0):
    d = np.hypot(np.diff(p[:, 0]), np.diff(p[:, 1]))
    s = np.concatenate([[0], np.cumsum(d)])
    out = []
    period = on + off
    t = -phase
    while t < s[-1]:
        a, b = max(t, 0.0), min(t + on, s[-1])
        if b > a:
            tt = np.concatenate([[a], s[(s > a) & (s < b)], [b]])
            out.append(np.stack([np.interp(tt, s, p[:, 0]), np.interp(tt, s, p[:, 1])], 1))
        t += period
    return out


def split_by_zone(p, zone_img, keep):
    """Split polyline p into runs whose zone (sampled at the vertices) is in keep; returns [(zone, run)]."""
    h, w = zone_img.shape
    xi = np.clip(p[:, 0].astype(int), 0, w - 1)
    yi = np.clip(p[:, 1].astype(int), 0, h - 1)
    z = zone_img[yi, xi]
    out = []
    start = 0
    for i in range(1, len(p) + 1):
        if i == len(p) or z[i] != z[start]:
            if z[start] in keep and i - start >= 2:
                out.append((int(z[start]), p[start:i]))
            start = i
    return out


def draw_lines(shape, lines, width, ss):
    """Anti-aliased polylines of `width` native px; returns coverage in [0, 1] at native size."""
    h, w = shape
    canvas = np.zeros((h * ss, w * ss), np.uint8)
    th = max(1, int(round(width * ss)))
    pts = [np.round((l * ss - 0.5) * 16).astype(np.int32) for l in lines if len(l) >= 2]
    for i in range(0, len(pts), 2000):
        cv2.polylines(canvas, pts[i:i + 2000], False, 255, thickness=th, lineType=cv2.LINE_AA, shift=4)
    a = cv2.resize(canvas, (w, h), interpolation=cv2.INTER_AREA).astype(np.float32) / 255.0
    if width * ss < 1:
        a *= width * ss
    return a


def blend(img, alpha, rgb):
    a = alpha[..., None]
    img *= 1 - a
    img += a * (np.asarray(rgb, np.float32) / 255.0)


FONT_FILES = {
    "sans": ["DejaVuSans.ttf", "LiberationSans-Regular.ttf", "FreeSans.ttf"],
    "sans_bold": ["DejaVuSans-Bold.ttf", "LiberationSans-Bold.ttf", "FreeSansBold.ttf"],
    "sans_italic": ["DejaVuSans-Oblique.ttf", "LiberationSans-Italic.ttf", "FreeSansOblique.ttf"],
    "serif": ["DejaVuSerif.ttf", "LiberationSerif-Regular.ttf", "FreeSerif.ttf"],
    "serif_italic": ["DejaVuSerif-Italic.ttf", "LiberationSerif-Italic.ttf", "FreeSerifItalic.ttf"],
}
FONT_DIRS = ["/usr/share/fonts/truetype/dejavu", "/usr/share/fonts/truetype/liberation",
             "/usr/share/fonts/truetype/freefont", "/usr/share/fonts/TTF", "/Library/Fonts",
             "/System/Library/Fonts/Supplemental"]
_font_cache = {}


def font(kind, px):
    px = max(6, int(round(px)))
    key = (kind, px)
    if key not in _font_cache:
        f = None
        for name in FONT_FILES.get(kind, FONT_FILES["sans"]):
            for d in FONT_DIRS:
                p = os.path.join(d, name)
                if os.path.exists(p):
                    f = ImageFont.truetype(p, px)
                    break
            if f:
                break
        _font_cache[key] = f or ImageFont.load_default(px)
    return _font_cache[key]


def text_patch(text, fnt, angle=0.0):
    l, t, r, b = fnt.getbbox(text)
    im = Image.new("L", (r - l + 6, b - t + 6), 0)
    ImageDraw.Draw(im).text((3 - l, 3 - t), text, font=fnt, fill=255)
    if abs(angle) > 0.5:
        im = im.rotate(angle, resample=Image.BICUBIC, expand=True)
    return np.asarray(im, np.float32) / 255.0


class Labels:
    """Greedy label placement without overlaps; patches are composited at the end."""

    def __init__(self, w, h, cell=4):
        self.w, self.h, self.cell = w, h, cell
        self.occ = np.zeros((h // cell + 2, w // cell + 2), bool)
        self.items = []

    def free(self, x0, y0, pw, ph, pad=2):
        c = self.cell
        a0, b0 = max(0, (y0 - pad) // c), max(0, (x0 - pad) // c)
        a1, b1 = (y0 + ph + pad) // c + 1, (x0 + pw + pad) // c + 1
        if x0 < 2 or y0 < 2 or x0 + pw > self.w - 2 or y0 + ph > self.h - 2:
            return False
        return not self.occ[a0:a1, b0:b1].any()

    def place(self, patch, cx, cy, rgb, halo=None, halo_px=1.5, text=None, ocr=True, knock=True, force=False):
        ph, pw = patch.shape
        x0, y0 = int(round(cx - pw / 2)), int(round(cy - ph / 2))
        if not force and not self.free(x0, y0, pw, ph):
            return False
        if x0 < 0 or y0 < 0 or x0 + pw > self.w or y0 + ph > self.h:
            return False
        c = self.cell
        self.occ[y0 // c:(y0 + ph) // c + 1, x0 // c:(x0 + pw) // c + 1] = True
        self.items.append(dict(patch=patch, x0=x0, y0=y0, rgb=rgb, halo=halo, halo_px=halo_px, text=text,
                               ocr=ocr, knock=knock))
        return True

    def knockout(self):
        m = np.zeros((self.h, self.w), np.float32)
        for it in self.items:
            if not it["knock"]:
                continue
            p = it["patch"]
            r = int(math.ceil(it["halo_px"] if it["halo"] else 1.0)) + 1
            k = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (2 * r + 1, 2 * r + 1))
            d = cv2.dilate((p > 0.15).astype(np.uint8), k)
            ph, pw = p.shape
            sub = m[it["y0"]:it["y0"] + ph, it["x0"]:it["x0"] + pw]
            np.maximum(sub, d.astype(np.float32), out=sub)
        return m

    def render(self, img):
        for it in self.items:
            p = it["patch"]
            ph, pw = p.shape
            sub = img[it["y0"]:it["y0"] + ph, it["x0"]:it["x0"] + pw]
            if it["halo"] is not None:
                r = max(1, int(round(it["halo_px"])))
                k = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (2 * r + 1, 2 * r + 1))
                halo = cv2.dilate(p, k)
                blend(sub, halo * 0.9, it["halo"])
            blend(sub, p, it["rgb"])

    def ocr_items(self):
        out = []
        for it in self.items:
            if not it["ocr"] or not it["text"]:
                continue
            ys, xs = np.nonzero(it["patch"] > 0.2)
            if len(xs) == 0:
                continue
            out.append(dict(text=it["text"], x=it["x0"] + xs.min(), y=it["y0"] + ys.min(),
                            w=xs.max() - xs.min() + 1, h=ys.max() - ys.min() + 1))
        return out


# ---------------------------------------------------------------------------
# Hydrology (streams from flow accumulation on a coarse grid)
# ---------------------------------------------------------------------------

NB8 = [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)]


def flow_accumulation(E, outlet):
    """Priority-flood filling, D8 receivers and accumulation (cells). `outlet`: cells that drain out."""
    h, w = E.shape
    filled = E.astype(np.float64).copy()
    done = np.zeros((h, w), bool)
    heap = []
    border = np.zeros((h, w), bool)
    border[0, :] = border[-1, :] = border[:, 0] = border[:, -1] = True
    for y, x in zip(*np.nonzero(border | outlet)):
        heap.append((filled[y, x], int(y), int(x)))
        done[y, x] = True
    heapq.heapify(heap)
    fl = filled.ravel()
    dn = done.ravel()
    while heap:
        e, y, x = heapq.heappop(heap)
        for dy, dx in NB8:
            yy, xx = y + dy, x + dx
            if 0 <= yy < h and 0 <= xx < w:
                j = yy * w + xx
                if not dn[j]:
                    dn[j] = True
                    if fl[j] <= e:
                        fl[j] = e + 1e-3
                    heapq.heappush(heap, (fl[j], yy, xx))
    filled = fl.reshape(h, w)
    pad = np.pad(filled, 1, constant_values=np.inf)
    best = np.zeros((h, w))
    rec = -np.ones((h, w), np.int64)
    idx = np.arange(h * w).reshape(h, w)
    for dy, dx in NB8:
        nb = pad[1 + dy:1 + dy + h, 1 + dx:1 + dx + w]
        drop = (filled - nb) / math.hypot(dy, dx)
        yy = np.clip(np.arange(h)[:, None] + dy, 0, h - 1)
        xx = np.clip(np.arange(w)[None, :] + dx, 0, w - 1)
        better = drop > best
        best = np.where(better, drop, best)
        rec = np.where(better, idx[yy, xx], rec)
    rec[outlet] = -1
    order = np.argsort(-filled, axis=None)
    acc = np.ones(h * w)
    recf = rec.ravel()
    for i in order.tolist():
        r = recf[i]
        if r >= 0:
            acc[r] += acc[i]
    return acc.reshape(h, w), rec


def stream_lines(acc, rec, stream):
    """Polylines (cell coordinates, x/y) of the stream network, with their accumulation at the outlet."""
    h, w = acc.shape
    recf = rec.ravel()
    sf = stream.ravel()
    ups = np.zeros(h * w, np.int32)
    for i in np.nonzero(sf)[0]:
        r = recf[i]
        if r >= 0 and sf[r]:
            ups[r] += 1
    starts = [i for i in np.nonzero(sf)[0] if ups[i] != 1]
    lines = []
    for s in starts:
        path = [s]
        i = s
        while True:
            r = recf[i]
            if r < 0 or not sf[r]:
                break
            path.append(r)
            if ups[r] != 1:
                break
            i = r
        if len(path) >= 2:
            ys, xs = np.divmod(np.array(path), w)
            lines.append((np.stack([xs + 0.5, ys + 0.5], 1).astype(np.float64), float(acc.ravel()[path[-1]])))
    return lines


# ---------------------------------------------------------------------------
# Rendering one case
# ---------------------------------------------------------------------------


def default_interval(style, mpp, relief, slope_med):
    """The map series' interval for the scale: finer on low relief (a sheet of the plains still
    shows ~8 lines), coarser where lines would crowd closer than ~3 px on the median slope."""
    if style["interval"]:
        return style["interval"]
    if style["units"] == "ft":
        steps = [(5, 40), (12, 80), (25, 200), (1e9, 400)]
        iv = next(v for m, v in steps if mpp <= m) * FT
        ladder = [v * FT for v in (10, 20, 40, 80, 100, 200, 400, 1000)]
    else:
        steps = [(3.5, 10), (7, 20), (14, 50), (30, 100), (1e9, 200)]
        iv = next(v for m, v in steps if mpp <= m)
        ladder = [5, 10, 20, 25, 50, 100, 200, 500]
    for v in sorted(ladder, reverse=True):
        if v <= iv + 1e-9 and (relief / v >= 8 or v == min(ladder)):
            iv = v
            break
    tan = math.tan(math.radians(max(slope_med, 0.5)))
    while iv / tan / mpp < 3.0 and iv < max(ladder):
        iv = min(v for v in ladder if v > iv + 1e-9)
    return iv


def make_name(rng, lang):
    pre, suf = NAME_PARTS.get(lang, NAME_PARTS["us"])
    return pre[rng.integers(len(pre))] + suf[rng.integers(len(suf))]


def hypso_colour(e):
    stops = np.array([-50, 0, 300, 800, 1400, 2000, 2700, 3500, 4500], np.float32)
    cols = np.array([(200, 225, 240), (178, 214, 160), (196, 224, 170), (232, 232, 176), (236, 214, 160),
                     (222, 188, 140), (204, 168, 132), (225, 215, 205), (248, 248, 248)], np.float32) / 255.0
    out = np.empty(e.shape + (3,), np.float32)
    for c in range(3):
        out[..., c] = np.interp(e, stops, cols[:, c])
    return out


def render_case(spec, out_dir, dem_cache, dem_dir_rel, prefetch=True, verbose=True):
    t0 = time.time()
    style = style_spec(spec["style"])
    region = REGIONS[spec["region"]]
    seed = int(hashlib.sha1(spec["name"].encode()).hexdigest()[:8], 16) ^ spec.get("seed", 0)
    rng = np.random.default_rng(seed)
    k = int(spec.get("upsample", 1))
    up_mode = spec.get("upsample_mode", "bilinear")
    mpp = float(spec["mpp"]) * k  # native raster ground m/px
    W, H = int(spec["size"][0]) // k, int(spec["size"][1]) // k  # native raster size
    lat0 = region["lat"] + spec.get("dlat", 0.0)
    lon0 = region["lon"] + spec.get("dlon", 0.0)
    proj = spec.get("proj", style["proj"])
    tm_lon0 = region["tm"] if region["tm"] is not None else 6 * (int((lon0 + 180) // 6)) - 180 + 3
    page_kind = spec.get("page", style["page"])
    # Printed scale: Web Mercator maps print the nominal scale in Mercator metres.
    kfac = 1.0 / math.cos(math.radians(lat0)) if proj == "merc" else 1.0
    ratio = style["ratio"]
    if page_kind == "pdf":
        # The app renders PDFs at 300 dpi: the embedded raster has 300/k ppi, and a printed
        # 1:n must agree with it (the core may use it with the dpi as a scale prior).
        ppi = 300.0 / k
        if ratio:
            denom = mpp / k * kfac * 300 / 0.0254
            ratio = min(NICE_RATIOS, key=lambda n: abs(math.log(n / denom)))
            mpp = ratio * 0.0254 / 300 / kfac * k
    elif ratio:
        # A scan of a printed sheet: its dpi follows from the sheet's scale.
        ppi = ratio * 0.0254 / (mpp * kfac)
    else:
        ppi = style["ppi"]
    geo = Geometry(proj, lat0, lon0, W, H, mpp, tm_lon0=tm_lon0)
    pt = ppi / 72.0  # native px per typographic point

    # --- terrain on the contour grid --------------------------------------------------
    f = 1 if W * H <= 6_000_000 else 2
    gw, gh = (W + f - 1) // f, (H + f - 1) // f
    gm = mpp * f  # ground m per grid cell
    extent_km2 = W * H * mpp * mpp / 1e6
    z14_tile_km = 2 * ORIGIN / 2 ** 14 / 1000 * math.cos(math.radians(lat0))
    zsrc = 14 if (mpp < 12 and extent_km2 / z14_tile_km ** 2 < 300) else 12
    mbox = geo.merc_bbox(margin_px=0.08 * max(W, H))
    mos = Mosaic(dem_cache, zsrc, mbox)
    mx, my = geo.merc_grid(gw, gh, f)
    e_raw = mos.sample(mx, my, order=3)
    sea = e_raw <= 0.0
    relief = float(np.percentile(e_raw[~sea], 99.5) - np.percentile(e_raw[~sea], 0.5)) if (~sea).any() else 0.0
    # Cartographic generalisation: smooth the terrain, three times more on gentle slopes (where
    # the source DEM's noise would otherwise become squiggles and tiny loops).
    gen_sigma_m = style["gen"] * max(1.5 * mpp, 12.0) * (1.0 + 0.5 * (mpp > 15))
    e_gen = ndimage.gaussian_filter(e_raw, gen_sigma_m / gm)
    e_soft = ndimage.gaussian_filter(e_raw, 3 * gen_sigma_m / gm)
    gy, gx = np.gradient(e_soft, gm)
    wflat = np.clip((8.0 - np.degrees(np.arctan(np.hypot(gx, gy)))) / 6.0, 0, 1)
    e_gen = e_gen * (1 - wflat) + e_soft * wflat
    del e_soft
    gy, gx = np.gradient(e_gen, gm)
    slope = np.degrees(np.arctan(np.hypot(gx, gy)))
    interval = default_interval(style, mpp, relief, float(np.median(slope[~sea])) if (~sea).any() else 0.0)
    survey = smooth_noise(rng, e_raw.shape, 1500.0 / gm, amp=float(np.clip(0.15 * interval, 1.5, 8.0)))
    e_map = e_gen + survey

    # Lakes: perfectly flat areas of the source DEM (water bodies are flattened in it).
    win = max(3, int(round(90.0 / gm)) | 1)
    rng_local = ndimage.maximum_filter(e_raw, win) - ndimage.minimum_filter(e_raw, win)
    flat = (rng_local < 0.3) & ~sea
    lab, n = ndimage.label(flat)
    lakes = np.zeros_like(flat)
    if n:
        sizes = ndimage.sum(flat, lab, range(1, n + 1)) * gm * gm / 1e6
        good = np.nonzero(sizes > 0.15)[0] + 1
        lakes = np.isin(lab, good)
        lakes = ndimage.binary_dilation(lakes, iterations=max(1, int(30 / gm)))
        lakes &= rng_local < 2.0
    water = sea | lakes
    if style.get("water") is None:
        water = np.zeros_like(water)

    glacier = np.zeros_like(sea)
    if style["glacier"] is not None and region["snowline"] is not None:
        g = (e_gen > region["snowline"]) & (slope < 32)
        g = ndimage.binary_opening(g, iterations=2)
        glacier = ndimage.binary_closing(g, iterations=3) & ~water
    rock = np.zeros_like(sea)
    if style["rock"]:
        rock = (slope > 38) & (e_gen > min(region["treeline"], 2600)) & ~glacier & ~water
        rock = ndimage.binary_opening(ndimage.binary_closing(rock, iterations=2), iterations=1)

    # Zones (native raster): 0 none, 1 normal contour, 2 glacier contour, 3 rock (no contours).
    zone_g = np.ones(e_raw.shape, np.uint8)
    zone_g[glacier] = 2
    zone_g[rock] = 3
    zone_g[water] = 0
    zone = cv2.resize(zone_g, (W, H), interpolation=cv2.INTER_NEAREST)

    # --- drafting warp (applied to drawn linework) -------------------------------------
    warp_amp = style["warp_px"] * (0.7 + 0.6 * rng.random())
    wdx = smooth_noise(rng, (gh, gw), 160.0 / f, amp=warp_amp)
    wdy = smooth_noise(rng, (gh, gw), 160.0 / f, amp=warp_amp)

    def warp(p):
        xi = np.clip(p[:, 0] / f - 0.5, 0, gw - 1)
        yi = np.clip(p[:, 1] / f - 0.5, 0, gh - 1)
        dx = ndimage.map_coordinates(wdx, [yi, xi], order=1)
        dy = ndimage.map_coordinates(wdy, [yi, xi], order=1)
        return p + np.stack([dx, dy], 1)

    # --- contour lines ----------------------------------------------------------------
    lo, hi = np.nanmin(e_map[~sea]) if (~sea).any() else 0, np.nanmax(e_map)
    levels = np.arange(math.ceil(lo / interval) * interval, hi, interval)
    zmask = np.ma.masked_array(e_map, mask=sea)
    cg = contourpy.contour_generator(z=zmask, line_type="Separate", quad_as_tri=False)
    unit_scale = 1 / FT if style["units"] == "ft" else 1.0
    contours = []  # (level_value_in_units, is_index, polyline native px)
    # Closed loops smaller than ~120 m around (or the style's minimum) are left out.
    loop_min = max(style["loop_min_px"] * (1 + 0.3 * (k > 1)), 120.0 / mpp)
    for lev in levels:
        idx_no = int(round(lev / interval))
        is_index = idx_no % style["index_every"] == 0
        for seg in cg.lines(lev):
            if len(seg) < 3:
                continue
            p = (np.asarray(seg, np.float64) + 0.5) * f
            closed = np.allclose(p[0], p[-1])
            p = resample_line(p, 1.0)
            if closed and len(p) < loop_min:
                continue
            if not closed and len(p) < 6:
                continue
            p = smooth_line(p, 1.6 + 0.6 * style["gen"], closed)
            p = warp(p)
            contours.append((round(lev * unit_scale), is_index, p))

    # --- streams -----------------------------------------------------------------------
    streams = []
    if style["streams"] is not None:
        fs = max(1, int(math.ceil(max(gw, gh) / 420)))
        hs, ws = gh // fs, gw // fs
        e_s = cv2.resize(e_gen, (ws, hs), interpolation=cv2.INTER_AREA)
        out_s = cv2.resize(water.astype(np.uint8), (ws, hs), interpolation=cv2.INTER_NEAREST).astype(bool)
        acc, rec = flow_accumulation(e_s, out_s)
        cell_km2 = (gm * fs) ** 2 / 1e6
        thr = style["stream_area_km2"] * (0.6 + 0.8 * rng.random())
        st = (acc * cell_km2 >= thr) & ~out_s
        for line, a in stream_lines(acc, rec, st):
            p = resample_line(line * fs * f, 1.0)
            if len(p) < 4:
                continue
            p = warp(smooth_line(p, 3.0, False))
            p += np.stack([smooth_noise(rng, (len(p), 1), 30, 0.8)[:, 0], smooth_noise(rng, (len(p), 1), 30, 0.8)[:, 0]], 1)
            streams.append((p, a * cell_km2))

    # --- routes (trails, roads) by least-cost paths on the coarse grid --------------------
    fr = max(1, int(math.ceil(max(gw, gh) / 300)))
    hr, wr = gh // fr, gw // fr
    slope_r = cv2.resize(slope.astype(np.float32), (wr, hr), interpolation=cv2.INTER_AREA)
    water_r = cv2.resize(water.astype(np.uint8), (wr, hr), interpolation=cv2.INTER_NEAREST).astype(bool)

    def route(kind):
        pen = 8.0 if kind == "road" else 14.0
        cost = 1.0 + (slope_r / pen) ** 2 + water_r * 200.0
        cost *= 1.0 + 0.3 * rng.random(cost.shape)
        for _ in range(10):
            side = rng.integers(4)
            if side == 0:
                a = (0, int(rng.integers(wr)))
            elif side == 1:
                a = (hr - 1, int(rng.integers(wr)))
            elif side == 2:
                a = (int(rng.integers(hr)), 0)
            else:
                a = (int(rng.integers(hr)), wr - 1)
            b = (int(rng.integers(hr // 5, 4 * hr // 5 + 1)), int(rng.integers(wr // 5, 4 * wr // 5 + 1)))
            if water_r[a] or water_r[b]:
                continue
            path, _ = route_through_array(cost, a, b, fully_connected=True, geometric=True)
            p = (np.array(path, np.float64)[:, ::-1] + 0.5) * fr * f
            if len(p) < 4:
                continue
            return warp(smooth_line(resample_line(p, 1.0), 4.0, False))
        return None

    routes = []
    for kind_key, kind in (("roads", "road"), ("trails", "trail")):
        for r in style[kind_key]:
            for _ in range(r["n"]):
                p = route(kind)
                if p is not None:
                    routes.append((r, p))

    # --- labels ------------------------------------------------------------------------
    labels = Labels(W, H)
    halo = style["halo"]
    name_px = style["name_pt"] * pt
    # Spot heights at summits.
    win_peak = max(5, int(round(1500.0 / gm)))
    peaks = (e_raw == ndimage.maximum_filter(e_raw, win_peak)) & ~sea
    prom = e_raw - ndimage.minimum_filter(e_raw, win_peak)
    cand = [(e_raw[y, x], y, x) for y, x in zip(*np.nonzero(peaks & (prom > 2 * interval)))]
    cand.sort(reverse=True)
    n_spot = max(2, int(W * H / (450 * 450)))
    spot_font = font(style["font"] if style["font"] != "sans_bold" else "sans", 0.9 * name_px)
    spot_rgb = style["spot_rgb"]
    for e, y, x in cand[:n_spot * 2]:
        if len([1 for it in labels.items if it.get("text") and it["text"].isdigit()]) >= n_spot:
            break
        cx, cy = (x + 0.5) * f, (y + 0.5) * f
        val = int(round(e / FT)) if style["units"] == "ft" else int(round(e))
        s = max(3, int(round(2.2 * pt)))
        mk = Image.new("L", (2 * s + 3, 2 * s + 3), 0)
        d = ImageDraw.Draw(mk)
        if style["spot"] in ("tri", "peak"):
            d.polygon([(s + 1, 1), (2 * s + 1, 2 * s), (1, 2 * s)], fill=255)
        elif style["spot"] == "tri_open":
            d.polygon([(s + 1, 1), (2 * s + 1, 2 * s), (1, 2 * s)], outline=255, width=max(1, s // 4))
        else:
            d.ellipse([s - 1, s - 1, s + 3, s + 3], fill=255)
        mk = np.asarray(mk, np.float32) / 255.0
        txt = text_patch(str(val), spot_font)
        if not labels.free(int(cx - mk.shape[1] / 2), int(cy - mk.shape[0] / 2), mk.shape[1], mk.shape[0]):
            continue
        ty = cy + mk.shape[0] / 2 + txt.shape[0] / 2
        if style["spot"] == "peak":
            name = make_name(rng, region["lang"])
            nt = text_patch(name, font(style["font"], name_px))
            if labels.place(mk, cx, cy, spot_rgb, halo, text=None, ocr=False):
                labels.place(nt, cx, ty, style["text"], halo, text=name)
                labels.place(txt, cx, ty + nt.shape[0] * 0.9, style["text"], halo, text=str(val))
        else:
            if labels.place(mk, cx, cy, spot_rgb, halo, text=None, ocr=False):
                labels.place(txt, cx + mk.shape[1] / 2 + txt.shape[1] / 2 + 1, cy, style.get(
                    "contour_label_rgb") or style["text"], halo, text=str(val))
    # Place names.
    n_names = max(3, int(W * H / (300 * 300)))
    for _ in range(n_names * 4):
        if sum(1 for it in labels.items if it.get("kind") == "name") >= n_names:
            break
        name = make_name(rng, region["lang"])
        x, y = rng.uniform(0.05, 0.95) * W, rng.uniform(0.05, 0.95) * H
        on_water = water[min(gh - 1, int(y / f)), min(gw - 1, int(x / f))]
        big = rng.random() < 0.25
        fk = "serif_italic" if on_water else (style["font"] if not big else "sans_bold")
        p = text_patch(name.upper() if big and not on_water else name, font(fk, name_px * (1.35 if big else 1.0)))
        rgb = style["water_text"] if on_water else style["text"]
        if labels.place(p, x, y, rgb, halo, text=name.upper() if big and not on_water else name):
            labels.items[-1]["kind"] = "name"
    # Contour labels along index contours.
    if style["contour_labels"]:
        cl_font = font("sans", 0.85 * name_px)
        cl_rgb = style.get("contour_label_rgb") or style.get("index_rgb") or style["contour"]
        spacing = 380 + 200 * rng.random()
        for val, is_index, p in contours:
            if not is_index or len(p) < 150:
                continue
            for s0 in np.arange(spacing * rng.random(), len(p) - 40, spacing):
                i = int(s0)
                a, b = p[max(0, i - 12)], p[min(len(p) - 1, i + 12)]
                ang = -math.degrees(math.atan2(b[1] - a[1], b[0] - a[0]))
                if ang > 90:
                    ang -= 180
                if ang < -90:
                    ang += 180
                patch = text_patch(str(val), cl_font, ang)
                labels.place(patch, p[i, 0], p[i, 1], cl_rgb, halo, text=str(val), ocr=abs(ang) < 20)

    knock = labels.knockout()
    keep = 1.0 - knock

    # --- composition (native raster, float RGB 0..1) ------------------------------------
    ss = 4 if W * H <= 3_000_000 else (3 if W * H <= 8_000_000 else 2)
    img = np.empty((H, W, 3), np.float32)
    img[:] = np.asarray(style["paper"], np.float32) / 255.0
    eN = up_to(e_gen, W, H)
    waterN = up_to(water.astype(np.float32), W, H) > 0.5
    glacN = up_to(glacier.astype(np.float32), W, H) > 0.5
    rockN = up_to(rock.astype(np.float32), W, H) > 0.5

    if style["hypso"]:
        hc = hypso_colour(np.floor(eN / interval) * interval)
        img[:] = img * 0.25 + hc * 0.75
    if style["farmland"] is not None:
        fm = smooth_noise(rng, (H, W), 300.0, 1.0) > 0.6
        blend(img, fm.astype(np.float32) * 0.6, style["farmland"])
    if style["park"] is not None:
        ang = rng.uniform(0, 2 * math.pi)
        yy, xx = np.mgrid[0:H, 0:W].astype(np.float32)
        d = (xx - W * rng.uniform(0.3, 0.7)) * math.cos(ang) + (yy - H * rng.uniform(0.3, 0.7)) * math.sin(ang)
        d += smooth_noise(rng, (H, W), 200.0, 25.0)
        park = (d > 0).astype(np.float32)
        park = cv2.GaussianBlur(park, (0, 0), 0.8)
        blend(img, park * 0.85, style["park"])
        edge = np.clip(1.0 - np.abs(d) / 1.2, 0, 1)
        blend(img, edge * 0.8, (120, 170, 120))
        del yy, xx, d
    forestN = np.zeros((H, W), np.float32)
    if style["forest"] is not None and region["forest"] > 0:
        nz = smooth_noise(rng, (H, W), 700.0 / mpp, 1.0)
        allowed = (eN < region["treeline"]) & ~waterN & ~glacN & ~rockN
        if allowed.any():
            thr = np.quantile(nz[allowed], 1 - region["forest"])
            forestN = ((nz > thr) & allowed).astype(np.float32)
            forestN = cv2.GaussianBlur(forestN, (0, 0), 0.7)
            blend(img, forestN, style["forest"])
            if style["forest_edge"] is not None:
                ed = np.clip(np.abs(cv2.Laplacian(forestN, cv2.CV_32F)) * 2.0, 0, 1)
                blend(img, ed * 0.6, style["forest_edge"])
    if style.get("hatch"):
        hz = smooth_noise(rng, (H, W), 500.0 / mpp, 1.0) > 1.1
        period = max(4, int(round(2.0 * pt)))
        lines = ((np.arange(W) % period) < max(1, period // 4)).astype(np.float32)[None, :]
        blend(img, hz.astype(np.float32) * lines * 0.8, style["hatch"])
    if style["glacier"] is not None and glacN.any():
        blend(img, glacN.astype(np.float32), style["glacier"])
    if style["water"] is not None and waterN.any():
        wf = cv2.GaussianBlur(waterN.astype(np.float32), (0, 0), 0.6)
        blend(img, wf, style["water"])
        ed = np.clip(np.abs(cv2.Laplacian(wf, cv2.CV_32F)) * 2.5, 0, 1)
        blend(img, ed * 0.9, style["water_edge"])
    if style["hillshade"] > 0:
        z_ex = 1.0 if relief > 600 else (1.6 if relief > 200 else 2.5)
        gyN, gxN = np.gradient(eN * z_ex, mpp)
        alt = math.radians(45)
        nrm = np.sqrt(1 + gxN ** 2 + gyN ** 2)
        # Lambertian shading, light from the north-west (image up-left) at 45 deg. With x right
        # and y down, the surface normal is (-gx, -gy, 1) and the light (-1, -1, sqrt2)/2 * cos(alt).
        lvec = np.array([-math.cos(alt) / math.sqrt(2), -math.cos(alt) / math.sqrt(2), math.sin(alt)])
        shade = (-gxN * lvec[0] - gyN * lvec[1] + lvec[2]) / nrm
        shade = np.clip(shade / math.sin(alt), 0, 1.3)
        s = style["hillshade"]
        if style["shade_tint"] is not None:
            dark, light = (np.asarray(c, np.float32) / 255.0 for c in style["shade_tint"])
            t = np.clip(shade, 0, 1.3)[..., None]
            mult = np.where(t < 1, dark + (1 - dark) * t, 1 + (light - 1) * (t - 1) / 0.3)
            img *= 1 - s + s * mult
        else:
            img *= (1 - s + s * np.clip(shade, 0, 1.15))[..., None]
        del gyN, gxN, nrm, shade
    if style["rock"] and rockN.any():
        ys, xs = np.nonzero(rockN)
        nsel = min(len(xs), int(len(xs) / 18))
        sel = rng.choice(len(xs), nsel, replace=False)
        gyN, gxN = np.gradient(eN, mpp)
        segs = []
        for i in sel:
            x, y = xs[i] + 0.5, ys[i] + 0.5
            gxx, gyy = gxN[ys[i], xs[i]], gyN[ys[i], xs[i]]
            nn = math.hypot(gxx, gyy) + 1e-6
            L = 2.0 + 3.0 * rng.random()
            segs.append(np.array([[x, y], [x + gxx / nn * L, y + gyy / nn * L]]))
        blend(img, draw_lines((H, W), segs, 0.6, ss) * 0.85, (40, 40, 40))
        del gyN, gxN

    # Contours.
    cmask = np.zeros((H, W), np.float32)
    zone_cols = {1: style["contour"], 2: style["glacier_contour"] or style["contour"]}
    for zid in (1, 2):
        for idx_flag in (False, True):
            lines = []
            for val, is_index, p in contours:
                if is_index != idx_flag:
                    continue
                for z, run in split_by_zone(p, zone, (zid,)):
                    lines.append(run)
            if not lines:
                continue
            wdt = style["index_w"] if idx_flag else style["contour_w"]
            a = draw_lines((H, W), lines, wdt, ss) * keep
            np.maximum(cmask, a, out=cmask)
            a *= style["contour_alpha"]
            rgb = zone_cols[zid]
            if idx_flag and style["index_rgb"] is not None and zid == 1:
                rgb = style["index_rgb"]
            blend(img, a, rgb)

    # Streams.
    if streams:
        for wmul, sel in ((1.0, [p for p, a in streams if a < 5]), (1.6, [p for p, a in streams if a >= 5])):
            if sel:
                a = draw_lines((H, W), sel, style["stream_w"] * wmul, ss) * keep
                blend(img, a, style["streams"])
    # Roads and trails.
    for r, p in routes:
        if r.get("casing") is not None:
            blend(img, draw_lines((H, W), [p], r["casing_w"], ss), r["casing"])
        parts = dash_line(p, r["dash"][0] * pt / 1.4, r["dash"][1] * pt / 1.4) if r.get("dash") else [p]
        blend(img, draw_lines((H, W), parts, r["w"], ss), r["rgb"])
    # Grid.
    grid_info = None
    if style["grid"] is not None:
        gs = style["grid"]
        lines = []
        if gs["kind"] == "graticule" and proj == "merc":
            lat_a, lon_a = geo.px_to_ll(np.array([0, W]), np.array([H, 0]))
            span = max(lon_a[1] - lon_a[0], 1e-6) * 60
            step = next(s for s in (1, 2, 5, 10, 15, 30, 60) if span / s <= 8) / 60.0
            for lon in np.arange(math.ceil(lon_a[0] / step) * step, lon_a[1], step):
                x, _ = geo.ll_to_px(lat0, lon)
                lines.append(np.array([[x, 0], [x, H]], np.float64))
            for lat in np.arange(math.ceil(lat_a[0] / step) * step, lat_a[1], step):
                _, y = geo.ll_to_px(lat, lon0)
                lines.append(np.array([[0, y], [W, y]], np.float64))
            grid_info = dict(kind="graticule", step_min=step * 60)
        else:
            step = next(s for s in (1000, 2000, 5000, 10000, 20000) if W * geo.sp / s <= 14)
            e0, n0 = geo.px_to_planar(0, H)
            e1, n1 = geo.px_to_planar(W, 0)
            for e in np.arange(math.ceil(e0 / step) * step, e1, step):
                x, _ = geo.planar_to_px(e, n0)
                if gs["kind"] == "utm_ticks":
                    lines += [np.array([[x, 0], [x, 4 * pt]]), np.array([[x, H - 4 * pt], [x, H]])]
                else:
                    lines.append(np.array([[x, 0], [x, H]], np.float64))
            for n_ in np.arange(math.ceil(n0 / step) * step, n1, step):
                _, y = geo.planar_to_px(e0, n_)
                if gs["kind"] == "utm_ticks":
                    lines += [np.array([[0, y], [4 * pt, y]]), np.array([[W - 4 * pt, y], [W, y]])]
                else:
                    lines.append(np.array([[0, y], [W, y]], np.float64))
            grid_info = dict(kind=gs["kind"], step_m=step)
        if lines:
            blend(img, draw_lines((H, W), lines, gs["w"], ss) * 0.9, gs["rgb"])
    # Forest symbols (OS).
    if style["forest_symbols"] and forestN.any():
        ys, xs = np.nonzero(forestN > 0.5)
        nsel = min(len(xs), int(len(xs) / (14 * pt) ** 2))
        sel = rng.choice(len(xs), nsel, replace=False) if nsel else []
        r = max(1.0, 0.9 * pt)
        circ = [np.array([[xs[i] + r * math.cos(t), ys[i] + r * math.sin(t)] for t in np.linspace(0, 2 * math.pi, 9)])
                for i in sel]
        blend(img, draw_lines((H, W), circ, 0.7, ss), (60, 140, 60))
    labels.render(img)
    np.clip(img, 0, 1, out=img)

    # --- page: upsampling, margins ---------------------------------------------------------
    content = (img * 255 + 0.5).astype(np.uint8)
    cm8 = (cmask >= 0.35).astype(np.uint8)
    # Some PDF exporters place the raster in a box of another aspect (the user's Gaia GPS
    # exports: 99.8 x 101.4 ppi, 1.6 % anisotropic): the page stretches one axis.
    pdf_aniso = 0.0
    if page_kind == "pdf" and style.get("pdf_aniso"):
        lo_a, hi_a = style["pdf_aniso"]
        pdf_aniso = float(spec.get("pdf_aniso", (1 if rng.random() < 0.5 else -1) * rng.uniform(lo_a, hi_a)))
    kx = k * (1 + pdf_aniso)  # final px per native px along x
    CW, CH = int(round(W * kx)), H * k
    if (CW, CH) != (W, H):
        interp = cv2.INTER_LINEAR if up_mode == "bilinear" else cv2.INTER_NEAREST
        content = cv2.resize(content, (CW, CH), interpolation=interp)
        cm8 = cv2.resize(cm8, (CW, CH), interpolation=cv2.INTER_NEAREST)
    kx = CW / W
    fppi = ppi * k  # final px per inch
    fpt = fppi / 72.0
    if page_kind == "screen":
        ml = mr = mt = mb = 0
    elif page_kind == "pdf":
        l_in, t_in, r_in, b_in = style["page_margins"]
        land = spec.get("landscape", CW > CH)
        PWp, PHp = CW + (l_in + r_in) * fppi, CH + (t_in + b_in) * fppi
        # A forced paper (spec "paper": "A4", "A3", ...): the map placed unscaled on a sheet larger
        # than it, below the top margin, the rest of the sheet blank.
        papers = [p for p in PAPERS if p[0] == spec["paper"]] if spec.get("paper") else PAPERS
        for _, pw_in, ph_in in papers:
            if land:
                pw_in, ph_in = ph_in, pw_in
            if PWp <= pw_in * fppi + 0.5 and PHp <= ph_in * fppi + 0.5:
                PWp, PHp = int(round(pw_in * fppi)), int(round(ph_in * fppi))
                break
        PWp, PHp = int(math.ceil(PWp)), int(math.ceil(PHp))
        ml = (PWp - CW) // 2
        mr = PWp - CW - ml
        mt = int(round(t_in * fppi))
        mb = PHp - CH - mt
    else:
        ml = mr = int(0.02 * CW + 0.15 * fppi)
        mt = int(0.02 * CH + 0.3 * fppi)
        mb = int(0.03 * CH + 0.8 * fppi)
    PW, PH = CW + ml + mr, CH + mt + mb
    page_rgb = (255, 255, 255) if page_kind == "pdf" else style["paper"]
    page = Image.new("RGB", (PW, PH), page_rgb)
    page.paste(Image.fromarray(content), (ml, mt))
    drw = ImageDraw.Draw(page)
    ocr = []

    def put(text, x, y, kind="sans", size_pt=8.0, rgb=(30, 30, 30), anchor="la", record=True):
        fnt = font(kind, size_pt * fpt)
        drw.text((x, y), text, font=fnt, fill=rgb, anchor=anchor)
        if record:
            l, t, r_, b = drw.textbbox((x, y), text, font=fnt, anchor=anchor)
            ocr.append(dict(text=text, bbox=[float(l), float(t), float(r_ - l), float(b - t)], confidence=0.95))

    units_txt = "feet" if style["units"] == "ft" else "metres"
    iv_units = interval / FT if style["units"] == "ft" else interval
    if page_kind == "screen":
        put(style["attribution"], CW - 6 * fpt, CH - 3 * fpt, "sans", 5.5, (60, 60, 60), anchor="rd")
        # On-map scale bar.
        bar_m = nice_length(0.18 * CW * mpp / k)
        bar_px = bar_m / (mpp / k)
        x0, y0 = 10 * fpt, CH - 12 * fpt
        drw.rectangle([x0 - 3, y0 - 9 * fpt, x0 + bar_px + 3 * fpt + 40, y0 + 3], fill=(255, 255, 255))
        drw.line([x0, y0, x0 + bar_px, y0], fill=(40, 40, 40), width=max(1, int(fpt)))
        put(fmt_len(bar_m, style["units"]), x0 + bar_px / 2, y0 - 2, "sans", 6, (40, 40, 40), anchor="mb")
    elif style["frame_page"]:
        # Gaia-like export: a frame around the page, the map inside it, text in thin strips and
        # over the map corners.
        inset = int(0.08 * fppi)
        drw.rectangle([inset, inset, PW - inset, PH - inset], outline=(20, 20, 20), width=max(2, int(1.2 * fpt)))
        put("GAIA GPS" if style["name"] == "gaia" else make_name(rng, region["lang"]).upper(), ml + 0.3 * fppi,
            mt + 0.15 * fppi, "sans_bold", 9, (40, 40, 40))
        if style.get("margin_lines"):
            decl = int(round(rng.uniform(-28, 12)))
            put(style["margin_lines"][1].format(decl=decl), ml + CW - 0.1 * fppi, mt + 0.45 * fppi, "sans_bold", 6,
                (20, 20, 20), anchor="ra")
            put(style["margin_lines"][0], PW - mr - 0.1 * fppi, PH - mb + 0.05 * fppi + 12 * fpt, "sans", 5.5,
                (60, 60, 60), anchor="ra")
        put(style["attribution"], ml + 0.05 * fppi, mt + CH + 0.04 * fppi, "sans", 6, (40, 40, 40))
        unit = style.get("scalebar", "km")
        bar_m = nice_length(0.15 * CW * mpp / k, miles=(unit == "mi"))
        bar_px = bar_m / (mpp / k)
        bx, by = PW - mr - 0.1 * fppi - bar_px, mt + CH + 0.06 * fppi
        drw.line([bx, by + 3 * fpt, bx + bar_px, by + 3 * fpt], fill=(30, 30, 30), width=max(1, int(0.6 * fpt)))
        for xx in (bx, bx + bar_px):
            drw.line([xx, by, xx, by + 3 * fpt], fill=(30, 30, 30), width=max(1, int(0.6 * fpt)))
        put(fmt_len(bar_m, "ft" if unit == "mi" else "m"), bx + bar_px / 2, by, "sans", 5.5, anchor="md")
    else:
        drw.rectangle([ml - 1, mt - 1, ml + CW, mt + CH], outline=(20, 20, 20), width=max(1, int(0.8 * fpt)))
        title = make_name(rng, region["lang"]).upper()
        put(title, ml, mt - 0.12 * fppi, style["title_font"], 14, (20, 20, 20), anchor="ld")
        if style.get("margin_lines"):
            decl = int(round(rng.uniform(-28, 12)))
            for i, t in enumerate(style["margin_lines"]):
                put(t.format(decl=decl), PW - mr, mt - 0.12 * fppi - i * 11 * fpt, "sans", 7, (60, 60, 60), anchor="rd")
        yb = mt + CH + 0.18 * fppi
        put(style["attribution"], ml, yb, "sans", 6.5, (60, 60, 60))
        if ratio:
            put("Scale 1:%s" % format(ratio, ",").replace(",", " "), PW / 2, yb, "sans_bold", 9, anchor="ma")
        put("Contour interval %g %s" % (round(iv_units, 1), units_txt), PW / 2, yb + 14 * fpt, "sans", 7, anchor="ma")
        # Scale bar.
        unit = style.get("scalebar", "km")
        bar_m = nice_length(0.2 * CW * mpp / k, miles=(unit == "mi"))
        bar_px = bar_m / (mpp / k)
        bx, by = PW - mr - bar_px, yb + 10 * fpt
        for i in range(4):
            seg = [bx + i * bar_px / 4, by, bx + (i + 1) * bar_px / 4, by + 4 * fpt]
            drw.rectangle(seg, fill=(20, 20, 20) if i % 2 == 0 else (255, 255, 255), outline=(20, 20, 20))
        put("0", bx, by - 2, "sans", 6.5, anchor="mb")
        put(fmt_len(bar_m, "ft" if unit == "mi" else "m"), bx + bar_px, by - 2, "sans", 6.5, anchor="mb")
        if style["legend"]:
            lx, ly = ml, yb + 14 * fpt
            items = [("Contour (%g %s)" % (round(iv_units, 1), units_txt), style["contour"], style["contour_w"]),
                     ("Index contour", style["index_rgb"] or style["contour"], style["index_w"])]
            if style["streams"]:
                items.append(("Stream", style["streams"], style["stream_w"]))
            for r in list(style["roads"])[:2] + list(style["trails"])[:1]:
                items.append(("Trail" if r.get("kind") == "trail" else "Road", r["rgb"], r["w"]))
            for i, (t, rgb, wdt) in enumerate(items):
                yy = ly + i * 10 * fpt
                drw.line([lx, yy + 4 * fpt, lx + 22 * fpt, yy + 4 * fpt], fill=tuple(rgb), width=max(1, int(round(wdt * k))))
                put(t, lx + 26 * fpt, yy, "sans", 6.5)
    for it in labels.ocr_items():
        ocr.append(dict(text=it["text"], bbox=[float(ml + kx * it["x"]), float(mt + k * it["y"]), float(kx * it["w"]),
                                               float(k * it["h"])], confidence=0.9))

    # --- page effects ------------------------------------------------------------------------
    arr = np.asarray(page, np.float32) / 255.0
    eff = spec.get("effect", style["effect"])
    if eff == "scan":
        arr = cv2.GaussianBlur(arr, (0, 0), 0.55)
        arr += rng.normal(0, 0.012, arr.shape).astype(np.float32)
    elif eff == "faded":
        hsv = cv2.cvtColor(np.clip(arr, 0, 1), cv2.COLOR_RGB2HSV)
        hsv[..., 1] *= 0.42
        hsv[..., 2] = 0.5 + 0.5 * hsv[..., 2]
        arr = cv2.cvtColor(hsv, cv2.COLOR_HSV2RGB)
        arr = arr * 0.82 + np.array([0.97, 0.92, 0.80], np.float32) * 0.18
        arr = cv2.GaussianBlur(arr, (0, 0), 0.6)
        arr += rng.normal(0, 0.01, arr.shape).astype(np.float32)
    elif eff == "photocopy":
        g = arr[..., 0] * 0.299 + arr[..., 1] * 0.587 + arr[..., 2] * 0.114
        g = np.clip((g - 0.18) / 0.72, 0, 1) ** 1.7
        ramp = np.linspace(0.92, 1.05, PW, dtype=np.float32)[None, :] * np.linspace(1.03, 0.94, PH, dtype=np.float32)[:, None]
        g = np.clip(g * ramp, 0, 1)
        speck = rng.random(g.shape) < 0.0015
        g[speck] *= 0.2
        g = cv2.GaussianBlur(g, (0, 0), 0.7) + rng.normal(0, 0.02, g.shape).astype(np.float32)
        arr = np.repeat(g[..., None], 3, axis=2) * np.array([1.0, 0.995, 0.985], np.float32)
    arr = np.clip(arr, 0, 1)
    final = (arr * 255 + 0.5).astype(np.uint8)
    mask_page = np.zeros((PH, PW), np.uint8)
    mask_page[mt:mt + CH, ml:ml + CW] = cm8

    # Truth (before skew): native px -> final px.
    fx = np.linspace(0.05, 0.95, 5)
    pts_native = [(W * a, H * b) for b in fx for a in fx]
    lat_t, lon_t = geo.px_to_ll(np.array([p[0] for p in pts_native]), np.array([p[1] for p in pts_native]))
    truth = [[ml + kx * x, mt + k * y, float(la), float(lo)] for (x, y), la, lo in zip(pts_native, lat_t, lon_t)]
    lat_c, lon_c = geo.px_to_ll(np.array([W / 2]), np.array([H / 2]))
    rotation = geo.rotation_deg
    frame = [ml, mt, ml + CW, mt + CH]

    # Scans and prints are not exactly conformal: paper shrinkage and the scanner stretch one
    # axis by a few tenths of a percent.
    aniso = float(spec.get("aniso", rng.uniform(-0.006, 0.006) if page_kind == "scan" else 0.0))
    aniso_total = (1 + aniso) * (1 + pdf_aniso) - 1
    if abs(aniso) > 1e-6:
        NW = int(round(PW * (1 + aniso)))
        sx = NW / PW
        final = cv2.resize(final, (NW, PH), interpolation=cv2.INTER_LINEAR)
        mask_page = cv2.resize(mask_page, (NW, PH), interpolation=cv2.INTER_NEAREST)
        for t in truth:
            t[0] *= sx
        for o in ocr:
            o["bbox"][0] *= sx
            o["bbox"][2] *= sx
        frame = [frame[0] * sx, frame[1], frame[2] * sx, frame[3]]
        PW = NW

    skew = float(spec.get("skew", 0.0))
    if abs(skew) > 1e-6:
        # Rotate the whole page (a skewed scan): positive = clockwise.
        M = cv2.getRotationMatrix2D((PW / 2, PH / 2), -skew, 1.0)
        cos, sin = abs(M[0, 0]), abs(M[0, 1])
        NW, NH = int(PH * sin + PW * cos), int(PH * cos + PW * sin)
        M[0, 2] += NW / 2 - PW / 2
        M[1, 2] += NH / 2 - PH / 2
        bg = tuple(int(v) for v in final[2, 2])
        final = cv2.warpAffine(final, M, (NW, NH), flags=cv2.INTER_LINEAR, borderValue=bg)
        mask_page = cv2.warpAffine(mask_page, M, (NW, NH), flags=cv2.INTER_NEAREST, borderValue=0)
        for t in truth:
            x, y = t[0], t[1]
            t[0], t[1] = M[0, 0] * x + M[0, 1] * y + M[0, 2], M[1, 0] * x + M[1, 1] * y + M[1, 2]
        for o in ocr:
            x, y, w_, h_ = o["bbox"]
            cs = [(x, y), (x + w_, y), (x, y + h_), (x + w_, y + h_)]
            tx = [M[0, 0] * a + M[0, 1] * b + M[0, 2] for a, b in cs]
            ty = [M[1, 0] * a + M[1, 1] * b + M[1, 2] for a, b in cs]
            o["bbox"] = [min(tx), min(ty), max(tx) - min(tx), max(ty) - min(ty)]
        rotation += skew
        PW, PH = NW, NH

    # --- write the case -------------------------------------------------------------------
    os.makedirs(out_dir, exist_ok=True)
    q = int(spec.get("jpeg_q", 88))
    Image.fromarray(final).save(os.path.join(out_dir, "map.jpg"), quality=q, subsampling=2 if q < 90 else 0)
    Image.fromarray(mask_page * 255).convert("1").save(os.path.join(out_dir, "contour_mask.png"), optimize=True)
    with open(os.path.join(out_dir, "ocr.json"), "w") as fo:
        json.dump(dict(description="Synthetic map text with exact boxes (perfect-OCR stand-in, synth_topo.py); "
                                   "grid coordinate labels left out", observations=ocr), fo, indent=0)
    # Measured contour colour (what a user would pick): median of the final image on the line cores.
    core = cv2.erode(mask_page, np.ones((2, 2), np.uint8)) if k > 1 else mask_page
    sel = final[core > 0]
    picked = [int(v) for v in np.median(sel, axis=0)] if len(sel) > 50 else None
    dpi = float(fppi) if page_kind == "pdf" else None
    tags = [style["name"], spec["region"], "proj-" + proj, "units-" + style["units"], mpp_band(mpp / k),
            "upsample-x%d-%s" % (k, up_mode) if k > 1 else "upsample-none", "page-" + page_kind]
    if abs(rotation) > 0.3:
        tags.append("rotated")
    if region.get("flat"):
        tags.append("flat")
    if eff:
        tags.append("effect-" + eff)
    case = dict(
        name=spec["name"], image="map.jpg", gps=[float(lat_c[0]), float(lon_c[0])],
        truth_points=[dict(x=round(t[0], 2), y=round(t[1], 2), lat=round(t[2], 7), lon=round(t[3], 7)) for t in truth],
        dem_dir=dem_dir_rel, ocr="ocr.json", dpi=dpi, m_per_px=round(mpp / k / math.sqrt(1 + aniso_total), 4),
        m_per_px_xy=[round(mpp / k / (1 + aniso_total), 4), round(mpp / k, 4)],
        contour_interval_m=round(interval, 3), style=style["name"], tags=tags,
        notes="Synthetic (fixtures/tools/synth_topo.py): %s; region %s (%s). Contours from Terrarium z%d, "
              "generalised, survey offsets and drafting warp %.1f px; %s." % (
                  STYLE_NOTES[style["name"]], spec["region"], region["notes"], zsrc, warp_amp,
                  "page render x%d %s of a %g ppi raster" % (k, up_mode, ppi) if k > 1 else page_kind),
        contour_mask="contour_mask.png", contour_rgb=picked, region=spec["region"],
        rotation_deg=round(rotation % 360, 3), projection=proj, image_size=[int(PW), int(PH)],
        map_frame_px=[round(v, 1) for v in frame] if abs(skew) < 1e-6 else None, anisotropy=round(aniso_total, 5),
        relief_m=round(relief, 1), dem_source_zoom=zsrc, scale_denominator=ratio, contour_units=style["units"],
        n_contour_lines=len(contours), grid=grid_info,
        generator=dict(spec=spec, seed=seed, native_size=[W, H], native_m_per_px=round(mpp, 4), ppi=round(ppi, 1)),
    )
    case["gps_variants"] = gps_variants(spec["name"], case["gps"])
    with open(os.path.join(out_dir, "case.json"), "w") as fo:
        json.dump(case, fo, indent=1, ensure_ascii=False)
    if prefetch:
        prefetch_core_tiles(case, geo, mpp, k, dem_cache)
    if verbose:
        print("%s: %dx%d, %.2f m/px, interval %.1f m, %d contour lines, %.1f s" % (
            spec["name"], PW, PH, mpp / k, interval, len(contours), time.time() - t0), file=sys.stderr)
    return case


def mpp_band(m):
    for lo, hi in ((0, 3), (3, 6), (6, 12), (12, 25), (25, 1e9)):
        if lo <= m < hi:
            return "mpp-%g-%s" % (lo, "inf" if hi > 1e8 else "%g" % hi)
    return "mpp-?"


def nice_length(m, miles=False):
    if miles:
        mi = m / 1609.344
        for v in (0.25, 0.5, 1, 2, 5, 10, 20, 50):
            if v >= mi * 0.6:
                return v * 1609.344
        return 50 * 1609.344
    for v in (100, 200, 250, 500, 1000, 2000, 2500, 5000, 10000, 20000, 25000, 50000):
        if v >= m * 0.6:
            return v
    return 50000


def fmt_len(m, units):
    if units == "ft":
        mi = m / 1609.344
        return "%g mi" % round(mi, 2)
    return "%g km" % (m / 1000) if m >= 1000 else "%d m" % m


def offset_point(lat, lon, dist_m, bearing):
    lon2, lat2, _ = GEOD.fwd(lon, lat, bearing, dist_m)
    return [float(lat2), float(lon2)]


def gps_variants(name, centre):
    """GPS fixes of the benchmark: the map centre, 10-30 km off it, 90-110 km away (the map is
    outside the 50 km default search radius then: the core must fail, never answer confidently)."""
    h = int(hashlib.sha1(("gps" + name).encode()).hexdigest()[:8], 16)
    b1 = h % 360
    d1 = 10_000 + (h >> 9) % 20_000
    b2 = (h >> 3) % 360
    d2 = 90_000 + (h >> 15) % 20_000
    return dict(centre=centre, offset=offset_point(centre[0], centre[1], d1, b1),
                far=offset_point(centre[0], centre[1], d2, b2), offset_km=round(d1 / 1000, 1),
                far_km=round(d2 / 1000, 1))


def prefetch_core_tiles(case, geo, mpp, k, cache):
    """Tiles the core will ask for: zoom 11 over the near search of each GPS variant (50 km plus the
    map), the fine zoom (13, or coarser per the core's 36-tile cap) under the map +-3 km."""
    lat, lon = case["gps"]
    tiles = []
    half_map = 0.5 * math.hypot(geo.w, geo.h) * mpp
    v = case["gps_variants"]
    for key, extra in (("centre", 50_000 + half_map), ("offset", 50_000 + 6_000), ("far", 50_000 + 6_000)):
        la, lo = v[key]
        tiles += tiles_in_merc_box(11, *ground_box(la, lo, extra))
    x0, y0, x1, y1 = geo.merc_bbox()
    pad = 3000 / math.cos(math.radians(lat))
    z = 13
    while z > 6 and len(tiles_in_merc_box(z, x0 - pad, y0 - pad, x1 + pad, y1 + pad)) > 36:
        z -= 1
    tiles += tiles_in_merc_box(z, x0 - pad, y0 - pad, x1 + pad, y1 + pad)
    fetch_tiles(cache, tiles, quiet=True)


def image_footprint(case):
    """Mercator corners of the whole image (page margins included) from an affine fit of the truth."""
    pts = case["truth_points"]
    A = np.array([[p["x"], p["y"], 1.0] for p in pts])
    M = np.array([merc(p["lat"], p["lon"]) for p in pts])
    coef, *_ = np.linalg.lstsq(A, M, rcond=None)
    w, h = case["image_size"]
    corners = np.array([[0, 0, 1.0], [w, 0, 1.0], [w, h, 1.0], [0, h, 1.0]]) @ coef
    return corners[:, 0].min(), corners[:, 1].min(), corners[:, 0].max(), corners[:, 1].max()


def regression_tiles(case, cache, out_dem, coarse_margin_m=4000.0):
    """Minimal tiles for a committed regression case with the GPS at its centre: zoom 11 under the
    image footprint + coarse_margin_m (the coarse search at the true place), and the fine zoom the
    core picks for the refinement (13, or coarser past 36 tiles) under the footprint + 20 % (at
    least 1.5 km), as refine::refine_dem_box. Copied from the cache into out_dem."""
    import shutil
    x0, y0, x1, y1 = image_footprint(case)
    c = 1 / math.cos(math.radians(case["gps"][0]))
    m = coarse_margin_m * c
    tiles = tiles_in_merc_box(11, x0 - m, y0 - m, x1 + m, y1 + m)
    pad = max(0.2 * max(x1 - x0, y1 - y0), 1500.0 * c)
    z = 13
    while z > 6 and len(tiles_in_merc_box(z, x0 - pad, y0 - pad, x1 + pad, y1 + pad)) > 36:
        z -= 1
    tiles += tiles_in_merc_box(z, x0 - pad, y0 - pad, x1 + pad, y1 + pad)
    fetch_tiles(cache, tiles, quiet=True)
    os.makedirs(out_dem, exist_ok=True)
    for t in tiles:
        name = "%d_%d_%d.png" % t
        if not os.path.exists(os.path.join(out_dem, name)):
            shutil.copy(os.path.join(cache, name), os.path.join(out_dem, name))
    return tiles


# ---------------------------------------------------------------------------
# Suites
# ---------------------------------------------------------------------------


def C(name, style, region, mpp, size, up=1, mode="bilinear", skew=0.0, **kw):
    d = dict(name=name, style=style, region=region, mpp=mpp, size=size, upsample=up, upsample_mode=mode, skew=skew)
    d.update(kw)
    return d


def bench_suite():
    """>= 70 cases: every style on several regions, scales 3-50 m/px, widths 1000-5000 px,
    page upsampling x2/x3 (bilinear and nearest), skew, JPEG, two flat regions."""
    return [
        # Gaia-like PDF pages (300 dpi render of a ~100 ppi raster) and native rasters.
        C("gaia-drakensberg-a", "gaia", "drakensberg", 3.0, (3354, 2373), 3, "bilinear", dlat=-0.04),
        C("gaia-drakensberg-b", "gaia", "drakensberg", 6.0, (3354, 2373), 3, "bilinear", dlat=0.02, dlon=0.03),
        C("gaia-rockies-native", "gaia", "rockies", 9.0, (1118, 791), page="screen"),
        C("gaia-patagonia-native", "gaia", "patagonia", 18.0, (1600, 1100), page="screen"),
        C("gaia-tasmania-x3n", "gaia", "tasmania", 4.0, (3000, 2100), 3, "nearest"),
        C("gaia-appalachians-x2", "gaia", "appalachians", 4.5, (2400, 1700), 2, "bilinear"),
        C("gaia-atlas-x3", "gaia", "atlas", 12.0, (3354, 2373), 3, "bilinear"),
        C("gaia-himalaya-x3n", "gaia", "himalaya_foothills", 6.0, (3354, 2373), 3, "nearest"),
        C("gaia-kansas-x3", "gaia", "flat_kansas", 3.0, (3354, 2373), 3, "bilinear"),
        # OpenTopoMap-like screenshots.
        C("otm-alps", "opentopo", "alps", 10.0, (1600, 1200)),
        C("otm-atlas", "opentopo", "atlas", 20.0, (1500, 1100)),
        C("otm-himalaya-x2n", "opentopo", "himalaya_foothills", 7.5, (2400, 1800), 2, "nearest"),
        C("otm-norway", "opentopo", "norway_fjords", 12.0, (2000, 1400)),
        C("otm-lakedistrict-x2", "opentopo", "lake_district", 3.0, (2400, 1800), 2, "bilinear"),
        C("otm-magaliesberg", "opentopo", "magaliesberg", 10.0, (1800, 1200)),
        C("otm-kansas", "opentopo", "flat_kansas", 10.0, (1600, 1200)),
        C("otm-freestate", "opentopo", "flat_freestate", 12.0, (1600, 1200)),
        C("otm-patagonia-x2", "opentopo", "patagonia", 6.0, (2400, 1800), 2, "bilinear"),
        # USGS-like scans (feet).
        C("usgs-rockies", "usgs", "rockies", 3.5, (3000, 3600), skew=0.7),
        C("usgs-appalachians", "usgs", "appalachians", 4.0, (3000, 3600)),
        C("usgs-kansas", "usgs", "flat_kansas", 4.0, (2800, 3400)),
        C("usgs-drakensberg", "usgs", "drakensberg", 5.0, (2600, 3200), skew=-1.2),
        C("usgs-tasmania", "usgs", "tasmania", 6.0, (2400, 3000)),
        C("usgs-rockies-x2n", "usgs", "rockies", 3.0, (3000, 3600), 2, "nearest", page="pdf"),
        C("usgs100k-rockies", "usgs100k", "rockies", 10.0, (3500, 2000)),
        C("usgs100k-appalachians", "usgs100k", "appalachians", 12.0, (3600, 2200)),
        # Swiss-like.
        C("swiss-alps-25k", "swiss", "alps", 4.0, (3000, 2200)),
        C("swiss-alps-50k", "swiss", "alps", 8.0, (2500, 1800), dlat=0.03),
        C("swiss-alps-x2", "swiss", "alps", 5.0, (2400, 1800), 2, "bilinear", page="pdf"),
        C("swiss-patagonia", "swiss", "patagonia", 10.0, (2400, 1800)),
        C("swiss-norway", "swiss", "norway_fjords", 8.0, (2400, 1800)),
        C("swiss-himalaya", "swiss", "himalaya_foothills", 12.0, (2400, 1800)),
        # OS Landranger-like.
        C("os-scottish", "os", "scottish_highlands", 5.0, (2500, 2500)),
        C("os-lakedistrict", "os", "lake_district", 5.0, (2500, 2500)),
        C("os-drakensberg", "os", "drakensberg", 6.0, (2400, 2400)),
        C("os-tasmania", "os", "tasmania", 6.0, (2400, 2400)),
        C("os-scottish-x3", "os", "scottish_highlands", 5.0, (2700, 2400), 3, "bilinear", page="pdf"),
        # Kompass-like tourist PDFs.
        C("kompass-alps-x2", "kompass", "alps", 3.0, (2800, 2000), 2, "bilinear"),
        C("kompass-atlas", "kompass", "atlas", 8.0, (2400, 1700)),
        C("kompass-tasmania-x2n", "kompass", "tasmania", 5.0, (2400, 1700), 2, "nearest"),
        C("kompass-norway", "kompass", "norway_fjords", 8.0, (2400, 1700)),
        # Black-and-white photocopies.
        C("bw-magaliesberg", "bw_photocopy", "magaliesberg", 4.0, (2400, 3000), skew=1.5),
        C("bw-scottish", "bw_photocopy", "scottish_highlands", 5.0, (2400, 3000), skew=2.5),
        C("bw-rockies", "bw_photocopy", "rockies", 6.0, (2400, 3000), skew=0.8),
        C("bw-alps", "bw_photocopy", "alps", 10.0, (2400, 1800), skew=-4.0),
        # Faded prints.
        C("faded-drakensberg", "faded", "drakensberg", 5.0, (2600, 2000)),
        C("faded-alps", "faded", "alps", 8.0, (2600, 2000)),
        C("faded-tasmania", "faded", "tasmania", 6.0, (2400, 2000)),
        C("faded-appalachians", "faded", "appalachians", 4.0, (2600, 2000)),
        C("faded-rockies", "faded", "rockies", 10.0, (2400, 1800)),
        # Thin grey contours only.
        C("grey-patagonia", "thin_grey", "patagonia", 8.0, (2000, 1500)),
        C("grey-magaliesberg", "thin_grey", "magaliesberg", 5.0, (2000, 1500)),
        C("grey-himalaya", "thin_grey", "himalaya_foothills", 10.0, (2000, 1500)),
        C("grey-kansas", "thin_grey", "flat_kansas", 6.0, (2000, 1500)),
        C("grey-alps", "thin_grey", "alps", 4.0, (2400, 1800)),
        # Dense 1:25 000, 10 m.
        C("dense-lakedistrict", "dense", "lake_district", 3.0, (3000, 3000)),
        C("dense-magaliesberg", "dense", "magaliesberg", 3.0, (3000, 3000)),
        C("dense-appalachians", "dense", "appalachians", 3.5, (3000, 3000)),
        C("dense-alps-5000", "dense", "alps", 3.0, (5000, 3500)),
        C("dense-scottish", "dense", "scottish_highlands", 3.0, (3000, 3000)),
        C("dense-tasmania", "dense", "tasmania", 3.0, (3000, 3000)),
        # Sparse 1:100 000-1:250 000, 100 m.
        C("sparse-drakensberg", "sparse", "drakensberg", 25.0, (2400, 1800)),
        C("sparse-himalaya", "sparse", "himalaya_foothills", 30.0, (2400, 1800)),
        C("sparse-atlas", "sparse", "atlas", 40.0, (2000, 1500)),
        C("sparse-rockies", "sparse", "rockies", 50.0, (2000, 1500)),
        C("sparse-norway", "sparse", "norway_fjords", 20.0, (2400, 1800)),
        C("sparse-patagonia-x2n", "sparse", "patagonia", 17.5, (2400, 1800), 2, "nearest", page="pdf"),
        C("sparse-freestate", "sparse", "flat_freestate", 30.0, (2000, 1500)),
        C("sparse-tasmania", "sparse", "tasmania", 25.0, (2000, 1500)),
        # MCSA-like.
        C("mcsa-magaliesberg", "mcsa", "magaliesberg", 3.8, (2500, 3500), page="scan"),
        C("mcsa-drakensberg", "mcsa", "drakensberg", 3.7, (2500, 3500)),
        C("mcsa-freestate", "mcsa", "flat_freestate", 3.8, (2500, 3500)),
        C("mcsa-atlas", "mcsa", "atlas", 3.6, (2500, 3500)),
        # Norwegian N50-like.
        C("n50-norway", "n50", "norway_fjords", 5.0, (2500, 2500)),
        C("n50-patagonia", "n50", "patagonia", 6.0, (2500, 2500)),
        C("n50-scottish", "n50", "scottish_highlands", 5.0, (2500, 2500)),
    ]


def regional_suite():
    """Large-area sheets (40-100 km across) in UTM or a national transverse Mercator, at several
    latitudes (31-70 degrees), and Web Mercator exports of the same size: the projection's bend
    (refine.rs, the conformal stage) must be taken out of the first and kept out of the second."""
    return [
        C("regional-lyngen-utm", "sparse", "lyngen", 40.0, (2400, 1800)),
        C("regional-norway-kompass", "kompass", "norway_fjords", 25.0, (2400, 1800)),
        C("regional-alps-utm", "sparse", "alps", 35.0, (2400, 1800)),
        C("regional-scottish-tm", "sparse", "scottish_highlands", 30.0, (2400, 1800), proj="tm"),
        C("regional-patagonia-utm", "sparse", "patagonia", 45.0, (2000, 1500)),
        C("regional-atlas-usgs100k", "usgs100k", "atlas", 20.0, (3000, 2200)),
        C("regional-alps-otm", "opentopo", "alps", 40.0, (2000, 1500)),
        C("regional-lyngen-otm", "opentopo", "lyngen", 30.0, (2000, 1500)),
        C("regional-norway-gaia", "gaia", "norway_fjords", 35.0, (1800, 1300), page="screen"),
        # 78 N: Terrarium's zoom 9-10 terrain there is too coarse for a 30 m/px map, and zoom 11
        # would take ~400 tiles: no answer (it must not be a confident wrong one).
        C("regional-svalbard-otm", "opentopo", "svalbard", 30.0, (2000, 1500)),
    ]


def pages_suite():
    """Small maps on large pages: native rasters placed unscaled on A4/A3 sheets at 300 dpi with
    title, legend and scale bar around them and the rest of the sheet blank (the app's render of
    a PDF that holds a small map)."""
    return [
        C("page-gaia-a4", "gaia", "rockies", 9.0, (1118, 791), 1, page="pdf", paper="A4", landscape=False),
        C("page-gaia-a4-land", "gaia", "drakensberg", 12.0, (1118, 791), 1, page="pdf", paper="A4"),
        C("page-otm-a3", "opentopo", "alps", 15.0, (1500, 1100), 1, page="pdf", paper="A3", landscape=False),
        C("page-swiss-a4", "swiss", "patagonia", 10.0, (1300, 1000), 1, page="pdf", paper="A4", landscape=False),
        C("page-usgs-a3", "usgs", "tasmania", 6.0, (1600, 1300), 1, page="pdf", paper="A3", landscape=False),
    ]


def regression_suite():
    """Small cases committed under fixtures/styles: 4 regions x 3 styles, one DEM directory per
    region (regions whose Terrarium source is SRTM: small tiles)."""
    return [
        C("gaia-drakensberg", "gaia", "drakensberg", 3.5, (3300, 2340), 3, "bilinear", jpeg_q=80),
        C("sparse-drakensberg", "sparse", "drakensberg", 30.0, (1000, 750), jpeg_q=82),
        C("mcsa-drakensberg", "mcsa", "drakensberg", 4.0, (1200, 1500), page="scan", jpeg_q=80),
        C("swiss-patagonia", "swiss", "patagonia", 8.0, (1400, 1000), jpeg_q=82),
        C("otm-patagonia", "opentopo", "patagonia", 10.0, (1200, 900), jpeg_q=82),
        C("os-patagonia", "os", "patagonia", 5.0, (1300, 1300), jpeg_q=80),
        C("usgs-tasmania", "usgs", "tasmania", 4.0, (1400, 1700), skew=0.8, jpeg_q=80),
        C("bw-tasmania", "bw_photocopy", "tasmania", 6.0, (1300, 1300), skew=2.0, jpeg_q=80),
        C("grey-tasmania", "thin_grey", "tasmania", 6.0, (1200, 900), jpeg_q=85),
        C("kompass-himalaya", "kompass", "himalaya_foothills", 4.0, (2800, 2000), 2, "nearest", jpeg_q=80),
        C("faded-himalaya", "faded", "himalaya_foothills", 6.0, (1300, 1000), jpeg_q=82),
        C("dense-himalaya", "dense", "himalaya_foothills", 3.0, (1500, 1500), jpeg_q=80),
    ]


def _run_one(args):
    spec, out_root, cache, dem_rel, prefetch = args
    try:
        render_case(spec, os.path.join(out_root, spec["name"]), cache, dem_rel, prefetch=prefetch)
        return spec["name"], None
    except Exception as e:  # noqa: BLE001 - report and continue with the other cases
        import traceback
        traceback.print_exc()
        return spec["name"], repr(e)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("suite")
    s.add_argument("which", choices=["bench", "regression", "regional", "pages"])
    s.add_argument("out")
    s.add_argument("--dem-cache", default=None, help="Terrarium tile cache (default OUT/../dem)")
    s.add_argument("--only", default=None, help="only cases whose name contains this")
    s.add_argument("-j", type=int, default=2)
    c = sub.add_parser("case")
    c.add_argument("out")
    c.add_argument("--region", required=True, choices=sorted(REGIONS))
    c.add_argument("--style", required=True, choices=sorted(STYLES))
    c.add_argument("--mpp", type=float, required=True, help="final image ground metres per pixel")
    c.add_argument("--size", type=int, nargs=2, required=True, metavar=("W", "H"), help="map content size (final px)")
    c.add_argument("--upsample", type=int, default=1)
    c.add_argument("--upsample-mode", default="bilinear", choices=["bilinear", "nearest"])
    c.add_argument("--skew", type=float, default=0.0)
    c.add_argument("--page", default=None, choices=["pdf", "scan", "screen"])
    c.add_argument("--seed", type=int, default=0)
    c.add_argument("--dem-cache", required=True)
    c.add_argument("--dem-dir", default=None, help="dem_dir written into case.json (default: the cache, relative)")
    sub.add_parser("list")
    a = ap.parse_args()
    if a.cmd == "list":
        print("styles:", ", ".join(sorted(STYLES)))
        print("regions:", ", ".join(sorted(REGIONS)))
        for sp in bench_suite():
            print("bench:", sp["name"])
        return
    if a.cmd == "case":
        name = os.path.basename(os.path.normpath(a.out))
        spec = C(name, a.style, a.region, a.mpp, tuple(a.size), a.upsample, a.upsample_mode, a.skew, seed=a.seed)
        if a.page:
            spec["page"] = a.page
        rel = a.dem_dir or os.path.relpath(a.dem_cache, a.out)
        render_case(spec, a.out, a.dem_cache, rel)
        return
    out = os.path.abspath(a.out)
    os.makedirs(out, exist_ok=True)
    if a.which in ("bench", "regional", "pages"):
        cache = a.dem_cache or os.path.join(os.path.dirname(out), "dem")
        suite = {"bench": bench_suite, "regional": regional_suite, "pages": pages_suite}[a.which]
        specs = [sp for sp in suite() if not a.only or a.only in sp["name"]]
        jobs = [(sp, out, cache, os.path.relpath(cache, os.path.join(out, sp["name"])), True) for sp in specs]
        with ProcessPoolExecutor(max(1, a.j)) as ex:
            for name, err in ex.map(_run_one, jobs):
                if err:
                    print("FAILED %s: %s" % (name, err), file=sys.stderr)
    else:
        cache = a.dem_cache or os.path.join(os.path.dirname(out), "dem-cache")
        specs = [sp for sp in regression_suite() if not a.only or a.only in sp["name"]]
        for sp in specs:
            d = os.path.join(out, sp["name"])
            dem_rel = "../dem/" + sp["region"]
            case = render_case(sp, d, cache, dem_rel, prefetch=False)
            regression_tiles(case, cache, os.path.join(out, "dem", sp["region"]))


if __name__ == "__main__":
    main()
