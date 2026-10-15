"""A benchmark case's map placed on a larger page: a small map on a white A4/A3 sheet at 300 dpi.

usage: python page_compose.py SRC_CASE_JSON OUT_DIR [--scale S] [--paper A4|A3] [--landscape]
                              [--pos top|centre] [--dpi N | --no-dpi] [--title TEXT] [--name NAME]

Writes OUT_DIR/case.json (the common case format, see bench_contours.py), map.jpg and ocr.json: the
source map resized by S (1 = unscaled, as a PDF holding a native raster at 100 %), pasted on a
sheet of paper at 300 dpi below a title, with legend lines, a source line and a scale note under
it and the rest of the sheet blank. Truth points and OCR boxes are carried over (scaled and
offset); the source's DEM directory is referenced by absolute path. This is the page the app
renders when a PDF holds a map smaller than the page (core: content.rs finds the map on it).
"""
import argparse
import json
import os

from PIL import Image, ImageDraw, ImageFont

PAPERS = {"A4": (8.27, 11.69), "A3": (11.69, 16.54), "Letter": (8.5, 11.0)}
DPI = 300


def font(size_px, bold=False):
    names = ["DejaVuSans-Bold.ttf" if bold else "DejaVuSans.ttf"]
    for d in ("/usr/share/fonts/truetype/dejavu", "/usr/share/fonts/dejavu", "/usr/share/fonts"):
        for n in names:
            p = os.path.join(d, n)
            if os.path.exists(p):
                return ImageFont.truetype(p, size_px)
    return ImageFont.load_default()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("src")
    ap.add_argument("out")
    ap.add_argument("--scale", type=float, default=1.0)
    ap.add_argument("--paper", default="A4", choices=sorted(PAPERS))
    ap.add_argument("--landscape", action="store_true")
    ap.add_argument("--pos", default="top", choices=["top", "centre"])
    ap.add_argument("--dpi", type=float, default=None)
    ap.add_argument("--no-dpi", action="store_true")
    ap.add_argument("--title", default=None)
    ap.add_argument("--name", default=None)
    a = ap.parse_args()

    src_dir = os.path.dirname(os.path.abspath(a.src))
    with open(a.src) as f:
        c = json.load(f)
    img = Image.open(os.path.join(src_dir, c["image"])).convert("RGB")
    W, H = img.size
    s = a.scale
    if abs(s - 1.0) > 1e-9:
        img = img.resize((max(1, round(W * s)), max(1, round(H * s))), Image.LANCZOS)
    sx, sy = img.size[0] / W, img.size[1] / H
    pw, ph = PAPERS[a.paper]
    if a.landscape:
        pw, ph = ph, pw
    PW, PH = round(pw * DPI), round(ph * DPI)
    if img.size[0] > PW - 120 or img.size[1] > PH - 400:
        raise SystemExit("the map does not fit the page: %s on %dx%d" % (img.size, PW, PH))
    ox = (PW - img.size[0]) // 2
    oy = round(0.75 * DPI) if a.pos == "top" else (PH - img.size[1]) // 2
    page = Image.new("RGB", (PW, PH), (250, 250, 248))
    page.paste(img, (ox, oy))
    drw = ImageDraw.Draw(page)
    ocr = []

    def put(text, x, y, size, bold=False, anchor="la"):
        f = font(size, bold)
        drw.text((x, y), text, font=f, fill=(25, 25, 25), anchor=anchor)
        l, t, r, b = drw.textbbox((x, y), text, font=f, anchor=anchor)
        ocr.append(dict(text=text, bbox=[float(l), float(t), float(r - l), float(b - t)], confidence=0.95))

    title = a.title or c.get("name", "MAP").replace("-", " ").upper()
    put(title, ox, oy - 25, 60, bold=True, anchor="ld")
    yb = oy + img.size[1] + 40
    put("Printed from a map export; not to scale.", ox, yb, 30)
    for i, t in enumerate(["Legend", "Contour line", "Track", "Water", "Road"]):
        put(t, ox, yb + 60 + 42 * i, 28, bold=(i == 0))
    put("Page 1 of 1", PW - 150, PH - 120, 26, anchor="ra")

    src_ocr = []
    if c.get("ocr"):
        with open(os.path.join(src_dir, c["ocr"])) as f:
            o = json.load(f)
        src_ocr = o["observations"] if isinstance(o, dict) else o
    for o in src_ocr:
        x, y, w, h = o["bbox"]
        ocr.append(dict(o, bbox=[ox + x * sx, oy + y * sy, w * sx, h * sy]))

    os.makedirs(a.out, exist_ok=True)
    page.save(os.path.join(a.out, "map.jpg"), quality=90)
    with open(os.path.join(a.out, "ocr.json"), "w") as f:
        json.dump(dict(description="page_compose.py: %s on a %s sheet" % (c.get("name"), a.paper), observations=ocr),
                  f, indent=0)
    out = dict(c)
    out["name"] = a.name or os.path.basename(os.path.normpath(a.out))
    out["image"] = "map.jpg"
    out["ocr"] = "ocr.json"
    out["dpi"] = None if a.no_dpi else (a.dpi if a.dpi else (DPI if abs(s - 1.0) < 1e-9 else None))
    out["dem_dir"] = os.path.normpath(os.path.join(src_dir, c.get("dem_dir") or "dem"))
    out["m_per_px"] = c["m_per_px"] / ((sx + sy) / 2)
    out["truth_points"] = [dict(p, x=ox + p["x"] * sx, y=oy + p["y"] * sy) for p in c["truth_points"]]
    out["image_size"] = [PW, PH]
    out["map_frame_px"] = [ox, oy, ox + img.size[0], oy + img.size[1]]
    out["tags"] = [t for t in c.get("tags", []) if not t.startswith("page-")] + ["page-sheet", "map-on-page"]
    out.pop("contour_mask", None)
    out.pop("truth", None)
    out["notes"] = ("%s resized x%.3f on a %s%s sheet at %d dpi (map %.0f %% of the page; page_compose.py). %s"
                    % (c.get("name"), s, a.paper, " landscape" if a.landscape else "", DPI,
                       100.0 * img.size[0] * img.size[1] / (PW * PH), c.get("notes", "")))
    with open(os.path.join(a.out, "case.json"), "w") as f:
        json.dump(out, f, indent=1)
    print("%s: %dx%d page, map %dx%d at (%d, %d), %.2f m/px" % (out["name"], PW, PH, img.size[0], img.size[1], ox, oy,
                                                             out["m_per_px"]))


if __name__ == "__main__":
    main()
