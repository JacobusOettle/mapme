"""Contour-georeferencing benchmark: runs the core CLI on benchmark cases and scores it.

usage: python bench_contours.py CASE_DIR_OR_GLOB... [--cli PATH] [--out DIR] [-j 2] [--timeout 600]
                                [--variants centre,offset,far] [--methods contours] [--fetch]
                                [--picked-colour] [--no-dump] [--label NAME] [--extra "--rotate"]

A case is a directory holding case.json in the common case format (fixtures/tools/synth_topo.py
writes them; so do the real-map fixtures): name, image, gps (map centre), truth_points
[{x, y, lat, lon}...] in image pixels, dem_dir (Terrarium {z}_{x}_{y}.png tiles, relative to the
case), ocr (file or null), dpi (or null), m_per_px, contour_interval_m, style, tags, notes, and
optionally contour_mask (PNG of the true contour pixels), gps_variants ({centre, offset, far}
[lat, lon]), photo / allow_rotation (request flags). A directory without case.json is searched
for */case.json.

Each case runs once per GPS variant, the way the app would ask (methods grid,contours are what
the app sends; the default here is contours only, as this measures the contour path):
  centre  GPS at the map centre                     -> should be located
  offset  GPS 10-30 km off the centre               -> should be located (50 km search)
  far     GPS 90-110 km away (outside 50 km)        -> must fail, or answer with confidence < 0.5
Build the CLI first: cd core && cargo build --release --example georef_cli.

Per run: status, method, median / p90 / max error at the truth points (ground metres), confidence,
found vs true m/px, time, the CLI's diagnostics (mask fraction, colour model, segments, scale range,
coarse candidates, refinement) and, for the centre run of cases with a contour_mask, the
extraction's precision / recall (the CLI's --dump-mask: skeleton samples and colour mask against
the true contour pixels). A run is "located" when the median error is within max(30 m, 2 px) and
the max within max(75 m, 4 px). CONFIDENT WRONG = status ok, confidence >= 0.5 and median error
> 100 m: the app would put the user's dot in the wrong place without warning.

Writes OUT/results.json and OUT/summary.md (per style / scale / upsampling / region tables,
failure modes with diagnostics, the per-case table). --fetch downloads the tiles the core asked
for that are missing from a case's dem_dir (AWS Terrarium) and reruns that case, as the app would.
At most 3 runs in parallel.
"""
import argparse
import glob
import hashlib
import json
import math
import os
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import numpy as np

R_EARTH = 6378137.0
TERRARIUM = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"
MODELS = {
    "default": (10.0, 45.0, 0.08, 0.75, 0.35, 0.92),
    "muted": (10.0, 45.0, 0.08, 0.55, 0.35, 0.92),
    "saturated": (12.0, 45.0, 0.25, 0.92, 0.40, 0.97),
}
VARIANTS = ("centre", "offset", "far")


# ---------------------------------------------------------------------------
# Geometry
# ---------------------------------------------------------------------------


def unmerc(x, y):
    return math.degrees(2 * math.atan(math.exp(y / R_EARTH)) - math.pi / 2), math.degrees(x / R_EARTH)


def haversine(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * 6371008.8 * math.asin(min(1.0, math.sqrt(a)))


def destination(lat, lon, dist_m, bearing_deg):
    d = dist_m / 6371008.8
    b = math.radians(bearing_deg)
    p1, l1 = math.radians(lat), math.radians(lon)
    p2 = math.asin(math.sin(p1) * math.cos(d) + math.cos(p1) * math.sin(d) * math.cos(b))
    l2 = l1 + math.atan2(math.sin(b) * math.sin(d) * math.cos(p1), math.cos(d) - math.sin(p1) * math.sin(p2))
    return [math.degrees(p2), (math.degrees(l2) + 540) % 360 - 180]


def gps_for(case, variant):
    v = case.get("gps_variants") or {}
    if variant in v:
        return list(v[variant])
    lat, lon = case["gps"]
    if variant == "centre":
        return [lat, lon]
    h = int(hashlib.sha1(("gps" + case["name"]).encode()).hexdigest()[:8], 16)
    if variant == "offset":
        return destination(lat, lon, 10_000 + (h >> 9) % 20_000, h % 360)
    return destination(lat, lon, 90_000 + (h >> 15) % 20_000, (h >> 3) % 360)


def apply_h(m, x, y):
    X = m[0] * x + m[1] * y + m[2]
    Y = m[3] * x + m[4] * y + m[5]
    W = m[6] * x + m[7] * y + m[8]
    return X / W, Y / W


def warp_offset(wp, x, y):
    """The response's warp B(p) (Mercator metres; docs/ARCHITECTURE.md, Georeference response)."""
    if not wp:
        return 0.0, 0.0
    cols, rows, o = wp["cols"], wp["rows"], wp["offsets_m"]
    u = min(max((x - wp["origin"][0]) / wp["spacing"][0], 0.0), cols - 1)
    v = min(max((y - wp["origin"][1]) / wp["spacing"][1], 0.0), rows - 1)
    i, j = min(int(u), cols - 2), min(int(v), rows - 2)
    fu, fv = u - i, v - j
    out = [0.0, 0.0]
    for (ii, jj, wgt) in ((i, j, (1 - fu) * (1 - fv)), (i + 1, j, fu * (1 - fv)), (i, j + 1, (1 - fu) * fv),
                          (i + 1, j + 1, fu * fv)):
        k = 2 * (jj * cols + ii)
        out[0] += wgt * o[k]
        out[1] += wgt * o[k + 1]
    return out[0], out[1]


def apply_model(resp, x, y):
    """pixel -> Mercator through the response's full model: homography plus warp."""
    mx, my = apply_h(resp["pixel_to_mercator"], x, y)
    dx, dy = warp_offset(resp.get("warp"), x, y)
    return mx + dx, my + dy


def model_name(m):
    if not m:
        return None
    t = (m.get("hue_min"), m.get("hue_max"), m.get("sat_min"), m.get("sat_max"), m.get("val_min"), m.get("val_max"))
    for name, ref in MODELS.items():
        if all(v is not None and abs(v - r) < 1e-3 for v, r in zip(t, ref)):
            return name
    return "custom(h%.0f-%.0f s%.2f-%.2f v%.2f-%.2f)" % t if None not in t else "custom"


# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------


def find_cases(patterns):
    dirs = []
    for pat in patterns:
        for p in sorted(glob.glob(pat)) or [pat]:
            if os.path.isfile(os.path.join(p, "case.json")):
                dirs.append(os.path.abspath(p))
            elif os.path.isdir(p):
                for q in sorted(glob.glob(os.path.join(p, "**", "case.json"), recursive=True)):
                    dirs.append(os.path.abspath(os.path.dirname(q)))
    seen, out = set(), []
    for d in dirs:
        if d not in seen:
            seen.add(d)
            out.append(d)
    return out


def load_case(d):
    with open(os.path.join(d, "case.json")) as f:
        c = json.load(f)
    c["_dir"] = d
    c.setdefault("name", os.path.basename(d))
    c.setdefault("tags", [])
    return c


def fetch_missing(dem_dir, missing_file):
    if not os.path.exists(missing_file):
        return 0
    tiles = set()
    with open(missing_file) as f:
        for line in f:
            p = line.split()
            if len(p) == 3:
                tiles.add(tuple(int(v) for v in p))
    todo = [t for t in tiles if not os.path.exists(os.path.join(dem_dir, "%d_%d_%d.png" % t))]

    def get(t):
        path = os.path.join(dem_dir, "%d_%d_%d.png" % t)
        for attempt in range(3):
            try:
                with urllib.request.urlopen(TERRARIUM.format(z=t[0], x=t[1], y=t[2]), timeout=60) as r:
                    data = r.read()
                tmp = path + ".part%d" % os.getpid()
                with open(tmp, "wb") as fo:
                    fo.write(data)
                os.replace(tmp, path)
                return 1
            except Exception:  # noqa: BLE001 - retried, then reported as missing
                time.sleep(1 + attempt)
        return 0

    if not todo:
        return 0
    with ThreadPoolExecutor(8) as ex:
        return sum(ex.map(get, todo))


# ---------------------------------------------------------------------------
# One run
# ---------------------------------------------------------------------------


def run_cli(args, timeout):
    t = time.time()
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None, "", time.time() - t, "timeout"
    wall = time.time() - t
    out = p.stdout.strip()
    resp = None
    if out:
        i = out.find("{")
        try:
            resp = json.loads(out[i:]) if i >= 0 else None
        except json.JSONDecodeError:
            resp = None
    err = None if resp is not None else "crash (exit %d): %s" % (p.returncode, p.stderr.strip()[-300:])
    return resp, p.stderr, wall, err


def extraction_metrics(dump_png, truth_png, upsample):
    """Precision / recall of the CLI's contour extraction against the true contour pixels."""
    from PIL import Image
    from scipy import ndimage
    d = np.asarray(Image.open(dump_png).convert("L"))
    t = np.asarray(Image.open(truth_png).convert("L")) > 0
    if d.shape != t.shape or not t.any():
        return None
    samples = d == 255
    mask = d >= 128
    dist_t = ndimage.distance_transform_edt(~t)
    tol = 2.0 + upsample
    out = dict(true_fraction=round(float(t.mean()), 5), mask_fraction=round(float(mask.mean()), 5))
    if mask.any():
        out["mask_precision"] = round(float((dist_t[mask] <= 1.5 + 0.5 * upsample).mean()), 3)
        dist_m = ndimage.distance_transform_edt(~mask)
        out["mask_recall"] = round(float((dist_m[t] <= 1.0).mean()), 3)
    if samples.any():
        out["sample_precision"] = round(float((dist_t[samples] <= tol).mean()), 3)
        # Recall along the true lines: thin the truth, then distance to the nearest sample.
        from skimage.morphology import skeletonize
        sk = skeletonize(t)
        dist_s = ndimage.distance_transform_edt(~samples)
        diag = math.hypot(*t.shape)
        step = min(6, max(1, round(diag / 2000)))
        out["sample_recall"] = round(float((dist_s[sk] <= tol + step).mean()), 3)
        out["n_samples"] = int(samples.sum())
    else:
        out["sample_precision"] = None
        out["sample_recall"] = 0.0
        out["n_samples"] = 0
    return out


def upsample_of(case):
    for t in case.get("tags", []):
        if t.startswith("upsample-x"):
            try:
                return int(t.split("-")[1][1:])
            except ValueError:
                pass
    return 1


def run_case(case, variant, opts):
    d = case["_dir"]
    image = os.path.join(d, case["image"])
    dem_dir = os.path.normpath(os.path.join(d, case.get("dem_dir") or "dem"))
    gps = gps_for(case, variant)
    rec = dict(case=case["name"], variant=variant, style=case.get("style"), region=case.get("region"),
               tags=case.get("tags", []), m_per_px=case.get("m_per_px"), gps=gps, dir=d)
    tmp = tempfile.mkdtemp(prefix="bench_")
    args = [opts.cli, image, "--gps", "%.7f,%.7f" % tuple(gps), "--dem-dir", dem_dir, "--methods", opts.methods]
    if case.get("ocr"):
        args += ["--ocr", os.path.join(d, case["ocr"])]
    if case.get("dpi"):
        args += ["--dpi", str(case["dpi"])]
    if case.get("photo"):
        args += ["--photo"]
    if case.get("allow_rotation"):
        args += ["--rotate"]
    if opts.picked_colour and case.get("contour_rgb"):
        args += ["--contour-rgb", ",".join(str(int(v)) for v in case["contour_rgb"])]
    dump = None
    if not opts.no_dump and variant == "centre" and case.get("contour_mask"):
        dump = os.path.join(tmp, "dump.png")
        args += ["--dump-mask", dump]
    missing = os.path.join(tmp, "missing.txt")
    args += ["--missing-out", missing]
    args += opts.extra
    fetched = 0
    for attempt in range(4):
        if os.path.exists(missing):
            os.remove(missing)
        resp, stderr, wall, err = run_cli(args, opts.timeout)
        if not opts.fetch or attempt == 3:
            break
        n = fetch_missing(dem_dir, missing)
        fetched += n
        if n == 0:
            break
    rec["tiles_fetched"] = fetched
    rec["wall_s"] = round(wall, 2)
    core_ms = None
    for line in stderr.splitlines():
        if line.startswith("core time"):
            try:
                core_ms = float(line.split()[2])
            except (IndexError, ValueError):
                pass
    rec["core_s"] = round(core_ms / 1000, 2) if core_ms is not None else None
    n_missing = 0
    if os.path.exists(missing):
        with open(missing) as f:
            n_missing = sum(1 for _ in f)
    rec["tiles_missing"] = n_missing
    if err:
        rec.update(status="error", error=err)
        return finish(rec, case, tmp)
    diag = resp.get("diagnostics") or {}
    rec.update(status=resp.get("status"), method=resp.get("method"), confidence=resp.get("confidence"),
               message=resp.get("message"), found_m_per_px=resp.get("scale_m_per_px"), rms_m=resp.get("rms_m"),
               rotation_deg=resp.get("rotation_deg"))
    con = diag.get("contours") or {}
    rec["mask_fraction"] = con.get("mask_fraction")
    rec["model"] = model_name(con.get("model")) if con.get("model") else None
    ext = con.get("extraction") or {}
    if ext.get("ink"):
        # The line extraction's ink model and analysis level (contour_lines.rs).
        rec["model"] = "%s L%s" % (ext["ink"], ext.get("level", 1))
        rec["level"] = ext.get("level")
    rec["n_segments"] = con.get("n_segments")
    rec["n_samples"] = con.get("n_samples")
    rec["scale_range"] = diag.get("scale_range")
    ref = diag.get("refined") or {}
    rec["refined"] = {k: ref.get(k) for k in ("fit_confidence", "cost_m", "inlier_fraction", "contour_interval_m",
                                              "dem_zoom", "affine", "projective", "conformal", "source")
                      if k in ref} or None
    if diag.get("search"):
        rec["search"] = diag.get("search")
    elif resp.get("search"):
        s = resp["search"]
        rec["search"] = {k: s.get(k) for k in ("stage", "searched_radius_m", "tiles_fetched", "coarse_zoom")}
    # Coarse candidates: is one of them at the true place and scale?
    tp = case.get("truth_points") or []
    if tp:
        tlat = statistics.fmean(p["lat"] for p in tp)
        tlon = statistics.fmean(p["lon"] for p in tp)
        span = max(haversine(a["lat"], a["lon"], b["lat"], b["lon"]) for a in tp for b in tp)
        cands = []
        for c in diag.get("candidates") or []:
            if not c.get("center"):
                continue
            dist = haversine(c["center"][0], c["center"][1], tlat, tlon)
            sr = (c.get("scale_m_per_px") or 0) / case["m_per_px"] if case.get("m_per_px") else None
            cands.append(dict(dist_km=round(dist / 1000, 2), scale_ratio=round(sr, 3) if sr else None,
                              peak_ratio=c.get("peak_ratio") and round(c["peak_ratio"], 2),
                              score=c.get("score") and round(c["score"], 3)))
        rec["candidates"] = cands[:5]
        rec["truth_in_candidates"] = any(
            c["dist_km"] * 1000 < max(1000.0, 0.15 * span) and c["scale_ratio"] and abs(c["scale_ratio"] - 1) < 0.15
            for c in cands)
    m = resp.get("pixel_to_mercator")
    rec["warp"] = bool(resp.get("warp"))
    if m and tp:
        errs = []
        for p in tp:
            lat, lon = unmerc(*apply_model(resp, p["x"], p["y"]))
            errs.append(haversine(lat, lon, p["lat"], p["lon"]))
        errs.sort()
        rec["err_median_m"] = round(statistics.median(errs), 1)
        rec["err_p90_m"] = round(errs[min(len(errs) - 1, int(0.9 * len(errs)))], 1)
        rec["err_max_m"] = round(errs[-1], 1)
    if dump and os.path.exists(dump) and case.get("contour_mask"):
        try:
            rec["extraction"] = extraction_metrics(dump, os.path.join(d, case["contour_mask"]), upsample_of(case))
        except Exception as e:  # noqa: BLE001 - diagnostics only
            rec["extraction"] = dict(error=repr(e))
    return finish(rec, case, tmp)


def finish(rec, case, tmp):
    import shutil
    shutil.rmtree(tmp, ignore_errors=True)
    return judge(rec)


def judge(rec):
    """Verdict and failure mode of a run record (from its errors, status and diagnostics)."""
    mpp = rec.get("m_per_px") or 1.0
    tol_med, tol_max = max(30.0, 2 * mpp), max(75.0, 4 * mpp)
    ok = rec.get("status") == "ok"
    conf = rec.get("confidence") or 0.0
    med = rec.get("err_median_m")
    rec["tol_m"] = [round(tol_med, 1), round(tol_max, 1)]
    if rec.get("status") == "error":
        rec["verdict"] = "timeout" if rec.get("error") == "timeout" else "crash"
    elif ok and med is not None and med <= tol_med and rec["err_max_m"] <= tol_max:
        rec["verdict"] = "located"
    elif ok and med is not None and med <= 100:
        rec["verdict"] = "imprecise"
    elif ok and med is not None:
        rec["verdict"] = "confident-wrong" if conf >= 0.5 else "wrong-low-confidence"
    elif ok:
        rec["verdict"] = "ok-unscored"
    else:
        rec["verdict"] = "failed"
    rec["confident_wrong"] = rec["verdict"] == "confident-wrong"
    # Right on the whole but well off somewhere on the map, without a warning.
    rec["confident_off"] = (ok and conf >= 0.5 and not rec["confident_wrong"] and rec.get("err_max_m") is not None
                            and rec["err_max_m"] > max(150.0, 6 * mpp))
    rec["mode"] = failure_mode(rec)
    return rec


def failure_mode(rec):
    v = rec["verdict"]
    if rec["variant"] == "far":
        if v in ("failed", "wrong-low-confidence"):
            return "honest-fail"
        if v == "confident-wrong":
            return "CONFIDENT-WRONG"
        return "found-anyway" if v in ("located", "imprecise") else v
    if v == "located":
        return "ok"
    if v in ("timeout", "crash"):
        return v
    if v == "confident-wrong":
        return "CONFIDENT-WRONG"
    mpp = rec.get("m_per_px")
    sr = rec.get("scale_range") or {}
    if mpp and sr.get("min") is not None and not (sr["min"] <= mpp <= sr["max"]):
        return "scale-out-of-range"
    msg = (rec.get("message") or "").lower()
    if (rec.get("n_samples") or 0) < 300 or "no contour" in msg:
        return "no-contours"
    ex = rec.get("extraction") or {}
    if ex.get("sample_precision") is not None and ex["sample_precision"] < 0.5:
        return "wrong-pixels"
    if ex.get("sample_recall") is not None and ex["sample_recall"] < 0.25:
        return "poor-recall"
    if v == "imprecise":
        return "imprecise"
    if v == "wrong-low-confidence":
        return "wrong-low-confidence"
    if rec.get("truth_in_candidates"):
        return "rejected-at-refine"
    return "coarse-miss"


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------


def med(vals):
    vals = [v for v in vals if v is not None]
    return statistics.median(vals) if vals else None


def fmt(v, nd=1):
    if v is None:
        return "-"
    if isinstance(v, float):
        return ("%." + str(nd) + "f") % v
    return str(v)


def group_table(runs, key_fn, title):
    groups = {}
    for r in runs:
        for k in key_fn(r):
            groups.setdefault(k, []).append(r)
    lines = ["### By %s" % title, "",
             "| %s | cases | centre located | offset located | far honest | CONF. WRONG | median err (located) | "
             "mask frac | sample P/R | median core s |" % title,
             "|---|---|---|---|---|---|---|---|---|---|"]
    for k in sorted(groups):
        rs = groups[k]
        c = [r for r in rs if r["variant"] == "centre"]
        o = [r for r in rs if r["variant"] == "offset"]
        f = [r for r in rs if r["variant"] == "far"]
        cw = sum(r["confident_wrong"] for r in rs)
        loc = [r["err_median_m"] for r in rs if r["verdict"] == "located"]
        ex = [r.get("extraction") or {} for r in c]
        p = med([e.get("sample_precision") for e in ex])
        rc = med([e.get("sample_recall") for e in ex])
        lines.append("| %s | %d | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            k, len({r["case"] for r in rs}),
            "%d/%d" % (sum(r["verdict"] == "located" for r in c), len(c)) if c else "-",
            "%d/%d" % (sum(r["verdict"] == "located" for r in o), len(o)) if o else "-",
            "%d/%d" % (sum(r["mode"] == "honest-fail" for r in f), len(f)) if f else "-",
            ("**%d**" % cw) if cw else "0", fmt(med(loc)), fmt(med([r.get("mask_fraction") for r in c]), 4),
            "%s / %s" % (fmt(p, 2), fmt(rc, 2)), fmt(med([r.get("core_s") for r in rs]))))
    return lines + [""]


def scale_band(m):
    if m is None:
        return "?"
    for lo, hi in ((0, 3), (3, 6), (6, 12), (12, 25), (25, 1e9)):
        if lo <= m < hi:
            return "%02d-%s m/px" % (lo, "inf" if hi > 1e8 else "%d" % hi)
    return "?"


def upsample_key(r):
    ups = [t for t in r["tags"] if t.startswith("upsample-")]
    return ups or ["upsample-none"]


def report(runs, meta):
    L = ["# Contour benchmark: %s" % meta["label"], "",
         "%s, %d cases, %d runs, CLI `%s`, methods `%s`, variants %s." % (
             meta["date"], len({r["case"] for r in runs}), len(runs), meta["cli"], meta["methods"],
             ",".join(meta["variants"])), ""]
    c = [r for r in runs if r["variant"] == "centre"]
    o = [r for r in runs if r["variant"] == "offset"]
    f = [r for r in runs if r["variant"] == "far"]
    cw = [r for r in runs if r["confident_wrong"]]
    L += ["| | located | imprecise (<100 m) | failed | wrong, low conf. | CONFIDENT WRONG | timeout/crash |",
          "|---|---|---|---|---|---|---|"]
    for name, rs in (("centre", c), ("offset", o), ("far", f)):
        if not rs:
            continue
        cnt = lambda *vs: sum(r["verdict"] in vs for r in rs)  # noqa: E731
        L.append("| %s | %d/%d | %d | %d | %d | %s | %d |" % (
            name, cnt("located"), len(rs), cnt("imprecise"), cnt("failed"), cnt("wrong-low-confidence"),
            ("**%d**" % cnt("confident-wrong")) if cnt("confident-wrong") else "0", cnt("timeout", "crash")))
    L.append("")
    if cw:
        L += ["## !!! CONFIDENT WRONG ANSWERS (%d): status ok, confidence >= 0.5, median error > 100 m !!!" % len(cw),
              "",
              "| case | variant | median err m | confidence | found m/px | true m/px | message |", "|---|---|---|---|---|---|---|"]
        for r in cw:
            L.append("| %s | %s | %s | %s | %s | %s | %s |" % (r["case"], r["variant"], fmt(r.get("err_median_m")),
                                                         fmt(r.get("confidence"), 3), fmt(r.get("found_m_per_px"), 2),
                                                         fmt(r.get("m_per_px"), 2), r.get("message")))
        L.append("")
    else:
        L += ["No confident wrong answers.", ""]
    off = [r for r in runs if r.get("confident_off")]
    if off:
        L += ["### Confident but off at the edges (%d): confidence >= 0.5, max error > max(150 m, 6 px)" % len(off), "",
              "| case | variant | median / max err m | confidence | found m/px | true m/px |", "|---|---|---|---|---|---|"]
        for r in off:
            L.append("| %s | %s | %s / %s | %s | %s | %s |" % (
                r["case"], r["variant"], fmt(r.get("err_median_m")), fmt(r.get("err_max_m")),
                fmt(r.get("confidence"), 3), fmt(r.get("found_m_per_px"), 3), fmt(r.get("m_per_px"), 3)))
        L.append("")
    # Failure modes (centre + offset).
    modes = {}
    for r in c + o:
        modes.setdefault(r["mode"], []).append(r)
    L += ["## Failure modes (centre and offset runs)", "", "| mode | runs | cases (variant) |", "|---|---|---|"]
    for mname, rs in sorted(modes.items(), key=lambda kv: -len(kv[1])):
        L.append("| %s | %d | %s |" % (mname, len(rs), ", ".join("%s (%s)" % (r["case"], r["variant"][0])
                                                                for r in rs[:12]) + (" ..." if len(rs) > 12 else "")))
    L.append("")
    L += group_table(runs, lambda r: [r.get("style") or "?"], "style")
    L += group_table(runs, lambda r: [scale_band(r.get("m_per_px"))], "scale")
    L += group_table(runs, upsample_key, "upsampling")
    L += group_table(runs, lambda r: [r.get("region") or "?"], "region")
    L += group_table(runs, lambda r: [t for t in r["tags"] if t.startswith("page-") or t.startswith("effect-")]
                     or ["-"], "page/effect")
    # Per case.
    L += ["## Per case", "",
          "| case | m/px | centre: verdict, med/max err m, conf, found m/px | offset | far | mode | mask frac | model | "
          "segs | P/R | scale range | core s |", "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    by_case = {}
    for r in runs:
        by_case.setdefault(r["case"], {})[r["variant"]] = r

    def cell(r):
        if not r:
            return "-"
        s = r["verdict"]
        if r.get("err_median_m") is not None:
            s += ", %s/%s" % (fmt(r["err_median_m"], 0), fmt(r["err_max_m"], 0))
        if r.get("confidence") is not None:
            s += ", %s" % fmt(r["confidence"], 2)
        if r.get("found_m_per_px") is not None:
            s += ", %s" % fmt(r["found_m_per_px"], 2)
        return s

    for name in sorted(by_case):
        v = by_case[name]
        r0 = v.get("centre") or next(iter(v.values()))
        ex = r0.get("extraction") or {}
        sr = r0.get("scale_range") or {}
        L.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s / %s | %s | %s |" % (
            name, fmt(r0.get("m_per_px"), 2), cell(v.get("centre")), cell(v.get("offset")), cell(v.get("far")),
            r0["mode"], fmt(r0.get("mask_fraction"), 4), r0.get("model") or "-", fmt(r0.get("n_segments")),
            fmt(ex.get("sample_precision"), 2), fmt(ex.get("sample_recall"), 2),
            "%s-%s %s" % (fmt(sr.get("min"), 1), fmt(sr.get("max"), 1), sr.get("source", "")) if sr else "-",
            fmt(r0.get("core_s"))))
    L.append("")
    L += ["Located: median error <= max(30 m, 2 px) and max <= max(75 m, 4 px) at the truth points. "
          "Modes: scale-out-of-range (true m/px outside the searched range), no-contours (< 300 samples), "
          "wrong-pixels (< 50 % of the skeleton samples on true contours), poor-recall (< 25 % of the true lines "
          "sampled), coarse-miss (no coarse candidate at the true place and scale), rejected-at-refine "
          "(a candidate was right but the answer failed), imprecise (ok, < 100 m off), wrong-low-confidence "
          "(ok, > 100 m off, confidence < 0.5). P/R: skeleton-sample precision / recall against the true "
          "contour pixels (centre run).", ""]
    return "\n".join(L)


def share_extraction(runs):
    """Every run of a case analyses the same image: give the others the centre run's extraction
    metrics and judge them again."""
    ex = {r["case"]: r["extraction"] for r in runs if r["variant"] == "centre" and r.get("extraction")}
    for r in runs:
        if not r.get("extraction") and r["case"] in ex:
            r["extraction"] = ex[r["case"]]
            judge(r)


def rerender(path, out):
    """Re-judge the runs of an earlier results.json and write its summary.md again."""
    with open(path) as f:
        d = json.load(f)
    for r in d["runs"]:
        judge(r)
    share_extraction(d["runs"])
    with open(os.path.join(out, "results.json"), "w") as f:
        json.dump(d, f, indent=1)
    md = report(d["runs"], d["meta"])
    with open(os.path.join(out, "summary.md"), "w") as f:
        f.write(md)
    print(md.split("## Failure modes")[0])


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("cases", nargs="*")
    ap.add_argument("--report", default=None, help="only re-render OUT/summary.md from this results.json")
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--cli", default=os.path.join(here, "..", "..", "core", "target", "release", "examples",
                                                  "georef_cli"))
    ap.add_argument("--out", default="bench-out")
    ap.add_argument("-j", type=int, default=2)
    ap.add_argument("--timeout", type=float, default=600)
    ap.add_argument("--variants", default="centre,offset,far")
    ap.add_argument("--methods", default="contours")
    ap.add_argument("--fetch", action="store_true", help="download missing tiles the core asks for, then rerun")
    ap.add_argument("--picked-colour", action="store_true", help="pass the case's contour_rgb as the user's pick")
    ap.add_argument("--no-dump", action="store_true", help="skip the extraction precision/recall")
    ap.add_argument("--label", default=None)
    ap.add_argument("--extra", default="", help="extra CLI arguments, e.g. '--rotate'")
    opts = ap.parse_args()
    if opts.report:
        os.makedirs(opts.out, exist_ok=True)
        rerender(opts.report, opts.out)
        return
    if not opts.cases:
        ap.error("no cases given")
    opts.cli = os.path.abspath(opts.cli)
    opts.extra = opts.extra.split()
    if not os.path.exists(opts.cli):
        sys.exit("no CLI at %s (cd core && cargo build --release --example georef_cli)" % opts.cli)
    if opts.j > 3:
        print("at most 3 runs in parallel", file=sys.stderr)
        opts.j = 3
    variants = [v for v in opts.variants.split(",") if v]
    for v in variants:
        if v not in VARIANTS:
            sys.exit("unknown variant %s" % v)
    cases = [load_case(d) for d in find_cases(opts.cases)]
    if not cases:
        sys.exit("no cases")
    jobs = [(c, v) for c in cases for v in variants]
    print("%d cases, %d runs" % (len(cases), len(jobs)), file=sys.stderr)
    os.makedirs(opts.out, exist_ok=True)
    runs = []
    t0 = time.time()

    def one(job):
        c, v = job
        try:
            r = run_case(c, v, opts)
        except Exception as e:  # noqa: BLE001 - one broken case must not stop the benchmark
            r = judge(dict(case=c["name"], variant=v, style=c.get("style"), region=c.get("region"),
                           tags=c.get("tags", []), m_per_px=c.get("m_per_px"), status="error",
                           error="bench: %r" % e))
        flag = "  <<< CONFIDENT WRONG" if r["confident_wrong"] else ""
        print("%-32s %-6s %-20s err %s m conf %s %ss%s" % (r["case"], v, r["verdict"], fmt(r.get("err_median_m")),
                                                          fmt(r.get("confidence"), 2), fmt(r.get("wall_s")), flag),
              file=sys.stderr)
        return r

    with ThreadPoolExecutor(opts.j) as ex:
        for r in ex.map(one, jobs):
            runs.append(r)
    share_extraction(runs)
    try:
        rev = subprocess.run(["git", "-C", here, "rev-parse", "--short", "HEAD"], capture_output=True,
                             text=True).stdout.strip()
    except OSError:
        rev = "?"
    meta = dict(label=opts.label or "HEAD %s" % rev, git=rev, date=time.strftime("%Y-%m-%d %H:%M"), cli=opts.cli,
                methods=opts.methods, variants=variants, wall_s=round(time.time() - t0, 1),
                picked_colour=opts.picked_colour, extra=opts.extra)
    with open(os.path.join(opts.out, "results.json"), "w") as f:
        json.dump(dict(meta=meta, runs=runs), f, indent=1)
    md = report(runs, meta)
    with open(os.path.join(opts.out, "summary.md"), "w") as f:
        f.write(md)
    print(md.split("## Failure modes")[0])
    n_cw = sum(r["confident_wrong"] for r in runs)
    if n_cw:
        print("!!! %d CONFIDENT WRONG answer(s) !!!" % n_cw, file=sys.stderr)
    print("wrote %s/results.json and %s/summary.md (%.0f s)" % (opts.out, opts.out, time.time() - t0),
          file=sys.stderr)


if __name__ == "__main__":
    main()
