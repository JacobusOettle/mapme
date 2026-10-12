"""Download Terrarium DEM tiles (AWS open data, elevation-tiles-prod) for a fixture.

usage: python fetch_dem.py OUT_DIR --box Z SOUTH WEST NORTH EAST [--box ...]

Every tile of zoom Z that intersects the lat/lon box is saved as OUT_DIR/{z}_{x}_{y}.png
(the layout the tests and examples/georef_cli.rs read). Existing files are kept.
The boxes used for each fixture are listed in its README.md.
"""
import argparse, math, os, urllib.request
from concurrent.futures import ThreadPoolExecutor

URL = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"


def tile(lat, lon, z):
    n = 2 ** z
    x = int((lon + 180.0) / 360.0 * n)
    r = math.radians(lat)
    y = int((1.0 - math.log(math.tan(r) + 1.0 / math.cos(r)) / math.pi) / 2.0 * n)
    return min(max(x, 0), n - 1), min(max(y, 0), n - 1)


def tiles_in_box(z, south, west, north, east):
    x0, y0 = tile(north, west, z)
    x1, y1 = tile(south, east, z)
    return [(z, x, y) for x in range(x0, x1 + 1) for y in range(y0, y1 + 1)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--box", nargs=5, action="append", required=True, metavar=("Z", "SOUTH", "WEST", "NORTH", "EAST"))
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    jobs = sorted({t for b in a.box for t in tiles_in_box(int(b[0]), *map(float, b[1:]))})

    def get(t):
        z, x, y = t
        p = os.path.join(a.out, f"{z}_{x}_{y}.png")
        if os.path.exists(p):
            return 0
        urllib.request.urlretrieve(URL.format(z=z, x=x, y=y), p)
        return 1

    with ThreadPoolExecutor(8) as ex:
        n = sum(ex.map(get, jobs))
    print(f"{a.out}: {len(jobs)} tiles, {n} downloaded")


if __name__ == "__main__":
    main()
