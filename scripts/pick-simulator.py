#!/usr/bin/env python3
"""Pick an iPhone simulator for CI and print its UDID on stdout.

Rules: newest iOS runtime the selected Xcode can run (runtime version <= the
iphonesimulator SDK version), then the preferred device name on it
("iPhone 16 Pro", "iPhone 16", ...), then any iPhone. If no iPhone exists on
any usable runtime, one is created. Diagnostics go to stderr.

    scripts/pick-simulator.py                      # UDID on stdout
    scripts/pick-simulator.py --github-output      # also udid/name/os to $GITHUB_OUTPUT
    SIM_NAME="iPhone 15" SIM_OS=17.5 scripts/pick-simulator.py

Offline testing: --devices-json/--runtimes-json/--devicetypes-json take saved
`xcrun simctl list ... -j` output and --sdk-version replaces the xcrun query.
"""
import argparse
import json
import os
import re
import subprocess
import sys

PREFERRED = ["iPhone 17 Pro", "iPhone 17", "iPhone 16 Pro", "iPhone 16", "iPhone 15 Pro", "iPhone 15"]


def log(msg):
    print(msg, file=sys.stderr)


def run(cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def load_json(path, cmd):
    if path:
        with open(path) as f:
            return json.load(f)
    return json.loads(run(cmd))


def version_tuple(s):
    nums = [int(n) for n in re.findall(r"\d+", s or "")]
    return tuple((nums + [0, 0, 0])[:3])


def runtime_version_from_id(identifier):
    # com.apple.CoreSimulator.SimRuntime.iOS-18-2 -> (18, 2, 0)
    m = re.search(r"iOS-(\d+)-(\d+)(?:-(\d+))?$", identifier)
    if not m:
        return None
    return (int(m.group(1)), int(m.group(2)), int(m.group(3) or 0))


def ios_runtimes(runtimes_json, devices_json):
    """{runtime identifier: (version tuple, display name)} for available iOS runtimes."""
    out = {}
    for rt in runtimes_json.get("runtimes", []):
        ident = rt.get("identifier", "")
        platform = rt.get("platform") or ("iOS" if ".iOS-" in ident else "")
        if platform != "iOS" or rt.get("isAvailable") is False:
            continue
        out[ident] = (version_tuple(rt.get("version")), rt.get("name") or ident)
    # Runtimes that only show up in the device list (older simctl JSON).
    for ident in devices_json.get("devices", {}):
        if ident not in out:
            v = runtime_version_from_id(ident)
            if v:
                out[ident] = (v, "iOS %d.%d" % v[:2])
    return out


def device_score(name, preferred):
    """Lower is better."""
    if name in preferred:
        return (0, preferred.index(name), 0, name)
    nums = re.findall(r"\d+", name)
    model = int(nums[0]) if nums else 0
    if re.search(r"\b(SE|mini|Plus|Max|Air)\b|\d+e\b", name):
        penalty = 2  # odd screen sizes make poor screenshots
    elif "Pro" in name:
        penalty = 0
    else:
        penalty = 1
    return (1, -model, penalty, name)


def pick(devices_json, runtimes, sdk, sim_name, sim_os):
    preferred = ([sim_name] if sim_name else []) + PREFERRED
    usable = {k: v for k, v in runtimes.items() if sdk is None or v[0][:2] <= sdk[:2]}
    if not usable:
        log("warning: no runtime <= SDK %s; considering all iOS runtimes" % (sdk,))
        usable = runtimes
    if sim_os:
        wanted = version_tuple(sim_os)
        exact = {k: v for k, v in usable.items() if v[0][: len(sim_os.split("."))] == wanted[: len(sim_os.split("."))]}
        if exact:
            usable = exact
        else:
            log("warning: SIM_OS=%s not available; using the newest runtime" % sim_os)
    for ident, (ver, rname) in sorted(usable.items(), key=lambda kv: kv[1][0], reverse=True):
        phones = [
            d
            for d in devices_json.get("devices", {}).get(ident, [])
            if d.get("isAvailable", True) and d.get("name", "").startswith("iPhone")
        ]
        if phones:
            best = min(phones, key=lambda d: device_score(d["name"], preferred))
            return best, rname, ident
    return None, None, None


def create_device(runtimes, sdk, devicetypes_json, sim_name):
    """Create an iPhone on the newest usable runtime; returns (udid, name, runtime name)."""
    usable = sorted(
        ((k, v) for k, v in runtimes.items() if sdk is None or v[0][:2] <= sdk[:2]),
        key=lambda kv: kv[1][0],
        reverse=True,
    ) or sorted(runtimes.items(), key=lambda kv: kv[1][0], reverse=True)
    if not usable:
        return None
    ident, (_, rname) = usable[0]
    types = [
        t
        for t in devicetypes_json.get("devicetypes", [])
        if t.get("name", "").startswith("iPhone") and t.get("productFamily", "iPhone") == "iPhone"
    ]
    if not types:
        return None
    preferred = ([sim_name] if sim_name else []) + PREFERRED
    dtype = min(types, key=lambda t: device_score(t["name"], preferred))
    name = "MapMe CI %s" % dtype["name"]
    log("creating simulator %r (%s, %s)" % (name, dtype["identifier"], ident))
    udid = run(["xcrun", "simctl", "create", name, dtype["identifier"], ident]).strip()
    return udid, name, rname


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--devices-json")
    ap.add_argument("--runtimes-json")
    ap.add_argument("--devicetypes-json")
    ap.add_argument("--sdk-version", help="iphonesimulator SDK version (default: ask xcrun)")
    ap.add_argument("--no-create", action="store_true", help="fail instead of creating a simulator")
    ap.add_argument("--github-output", action="store_true", help="append udid/name/os to $GITHUB_OUTPUT")
    args = ap.parse_args()

    sim_name = os.environ.get("SIM_NAME", "").strip() or None
    sim_os = os.environ.get("SIM_OS", "").strip() or None

    sdk_str = args.sdk_version
    if sdk_str is None and not args.devices_json:
        try:
            sdk_str = run(["xcrun", "--sdk", "iphonesimulator", "--show-sdk-version"]).strip()
        except (OSError, subprocess.CalledProcessError) as e:
            log("warning: could not read the iphonesimulator SDK version: %s" % e)
    sdk = version_tuple(sdk_str) if sdk_str else None
    log("iphonesimulator SDK: %s" % (sdk_str or "unknown"))

    devices = load_json(args.devices_json, ["xcrun", "simctl", "list", "-j", "devices", "available"])
    try:
        runtimes_json = load_json(args.runtimes_json, ["xcrun", "simctl", "list", "-j", "runtimes", "available"])
    except (OSError, subprocess.CalledProcessError, ValueError) as e:
        log("warning: could not list runtimes: %s" % e)
        runtimes_json = {}
    runtimes = ios_runtimes(runtimes_json, devices)
    for ident, (ver, rname) in sorted(runtimes.items(), key=lambda kv: kv[1][0], reverse=True):
        n = sum(1 for d in devices.get("devices", {}).get(ident, []) if d.get("name", "").startswith("iPhone"))
        log("  runtime %-10s %-55s %d iPhones" % (rname, ident, n))

    device, rname, _ = pick(devices, runtimes, sdk, sim_name, sim_os)
    if device:
        udid, name = device["udid"], device["name"]
    else:
        if args.no_create:
            log("error: no available iPhone simulator")
            return 1
        try:
            dtypes = load_json(args.devicetypes_json, ["xcrun", "simctl", "list", "-j", "devicetypes"])
            created = create_device(runtimes, sdk, dtypes, sim_name)
        except (OSError, subprocess.CalledProcessError, ValueError) as e:
            log("error: could not create a simulator: %s" % e)
            created = None
        if not created:
            log("error: no iPhone simulator available and none could be created")
            return 1
        udid, name, rname = created

    log("picked: %s (%s) %s" % (name, rname, udid))
    if args.github_output and os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as f:
            f.write("udid=%s\nname=%s\nos=%s\n" % (udid, name, rname))
    print(udid)
    return 0


if __name__ == "__main__":
    sys.exit(main())
