#!/usr/bin/env bash
# Export test attachments (screenshots etc.) from an .xcresult bundle.
#
#   scripts/export-screenshots.sh build/TestResults.xcresult build/screenshots
#
# Xcode 16+: `xcresulttool export attachments` (+ manifest.json). Older Xcode, or
# if that fails: walk the legacy object graph (`xcresulttool get --legacy`).
# Files are moved to readable names (<Test>__<attachment>.<ext>) next to an
# index.html gallery; <out>/raw keeps manifest.json. Never fails the build: a
# missing bundle or an export problem only prints a warning.
set -uo pipefail

RESULT="${1:?usage: export-screenshots.sh <result.xcresult> <output-dir>}"
OUT="${2:?usage: export-screenshots.sh <result.xcresult> <output-dir>}"

if [[ ! -d "$RESULT" ]]; then
  echo "export-screenshots: no result bundle at $RESULT (tests did not run); nothing to export"
  exit 0
fi

mkdir -p "$OUT/raw"

mode=""
if xcrun xcresulttool export attachments --path "$RESULT" --output-path "$OUT/raw" >/dev/null 2>"$OUT/raw/.export.err"; then
  mode="manifest"
else
  echo "export-screenshots: 'xcresulttool export attachments' unavailable or failed; using the legacy graph walk"
  sed 's/^/  /' "$OUT/raw/.export.err" | head -5
  mode="legacy"
fi
rm -f "$OUT/raw/.export.err"

python3 - "$RESULT" "$OUT" "$mode" <<'PY'
import html
import json
import os
import re
import shutil
import subprocess
import sys

result, out, mode = sys.argv[1:4]
raw = os.path.join(out, "raw")
IMAGE_EXT = (".png", ".jpg", ".jpeg", ".heic", ".gif")


def clean(s, limit=80):
    s = re.sub(r"[^A-Za-z0-9._-]+", "_", s or "").strip("._")
    return s[:limit] or "attachment"


def unique(path):
    base, ext = os.path.splitext(path)
    n = 2
    while os.path.exists(path):
        path = "%s_%d%s" % (base, n, ext)
        n += 1
    return path


exported = []  # (test identifier, readable file name)


def add(src, test, name):
    if not os.path.isfile(src):
        return
    ext = os.path.splitext(name)[1] or os.path.splitext(src)[1]
    stem = os.path.splitext(name)[0]
    # Xcode suffixes suggested names with _<index>_<UUID>; drop the UUID.
    stem = re.sub(r"_[0-9A-Fa-f]{8}(-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}$", "", stem)
    test_part = clean(test.replace("()", "").replace("/", "."), 60)
    dst = unique(os.path.join(out, "%s__%s%s" % (test_part, clean(stem), ext.lower())))
    shutil.move(src, dst)
    exported.append((test, os.path.basename(dst)))


def from_manifest():
    path = os.path.join(raw, "manifest.json")
    if not os.path.isfile(path):
        return False
    with open(path) as f:
        manifest = json.load(f)

    def walk(node, test):
        if isinstance(node, dict):
            test = node.get("testIdentifier") or node.get("testIdentifierURL") or test
            if "exportedFileName" in node:
                name = node.get("suggestedHumanReadableName") or node["exportedFileName"]
                add(os.path.join(raw, node["exportedFileName"]), test, name)
            for v in node.values():
                walk(v, test)
        elif isinstance(node, list):
            for v in node:
                walk(v, test)

    walk(manifest, "test")
    return True


legacy_flag = None


def xcresult(args):
    """Run xcresulttool with --legacy if this Xcode needs it (16+), else without."""
    global legacy_flag
    flags = [legacy_flag] if legacy_flag is not None else [["--legacy"], []]
    last = None
    for flag in flags:
        cmd = ["xcrun", "xcresulttool"] + args[:1] + flag + args[1:]
        p = subprocess.run(cmd, capture_output=True, text=True)
        if p.returncode == 0:
            legacy_flag = flag
            return p.stdout
        last = p.stderr.strip()
    raise RuntimeError(last or "xcresulttool failed")


def get(ref_id=None):
    args = ["get", "--format", "json", "--path", result]
    if ref_id:
        args += ["--id", ref_id]
    return json.loads(xcresult(args))


def val(node, *keys):
    for k in keys:
        if not isinstance(node, dict):
            return None
        node = node.get(k)
    if isinstance(node, dict) and "_value" in node:
        return node["_value"]
    return node


def values(node, key):
    v = node.get(key) if isinstance(node, dict) else None
    return v.get("_values", []) if isinstance(v, dict) else []


def from_legacy():
    root = get()
    for action in values(root, "actions"):
        tests_ref = val(action, "actionResult", "testsRef", "id")
        if not tests_ref:
            continue
        plan = get(tests_ref)
        stack = []
        for summary in values(plan, "summaries"):
            for testable in values(summary, "testableSummaries"):
                stack += values(testable, "tests")
        while stack:
            node = stack.pop()
            stack += values(node, "subtests")
            summary_ref = val(node, "summaryRef", "id")
            if not summary_ref:
                continue
            test = val(node, "identifier") or val(node, "name") or "test"
            acts = values(get(summary_ref), "activitySummaries")
            while acts:
                act = acts.pop()
                acts += values(act, "subactivities")
                for att in values(act, "attachments"):
                    payload = val(att, "payloadRef", "id")
                    if not payload:
                        continue
                    fname = val(att, "filename") or val(att, "name") or payload
                    dst = os.path.join(raw, clean(payload, 120) + os.path.splitext(fname)[1])
                    xcresult(["export", "--type", "file", "--path", result, "--id", payload, "--output-path", dst])
                    add(dst, test, val(att, "name") or fname)


try:
    ok = mode == "manifest" and from_manifest()
    if not ok:
        from_legacy()
except Exception as e:  # diagnostics only; never fail the job
    print("export-screenshots: warning: %s" % e)

images = [(t, f) for t, f in exported if f.lower().endswith(IMAGE_EXT)]
with open(os.path.join(out, "index.html"), "w") as f:
    f.write("<!doctype html><meta charset=utf-8><title>MapMe test screenshots</title>"
            "<style>body{background:#111;color:#ddd;font:14px -apple-system,sans-serif;margin:16px}"
            "figure{display:inline-block;margin:8px;vertical-align:top}"
            "img{max-height:640px;max-width:100%;border:1px solid #333}</style>\n")
    for test, name in images:
        f.write("<figure><img src=\"%s\"><figcaption>%s</figcaption></figure>\n" % (html.escape(name), html.escape(name)))
    others = [n for _, n in exported if not n.lower().endswith(IMAGE_EXT)]
    if others:
        f.write("<h3>Other attachments</h3><ul>%s</ul>\n" % "".join(
            "<li><a href=\"%s\">%s</a></li>" % (html.escape(n), html.escape(n)) for n in others))

print("export-screenshots: %d attachments (%d images) -> %s" % (len(exported), len(images), out))
for _, name in exported:
    print("  " + name)

summary = os.environ.get("GITHUB_STEP_SUMMARY")
if summary:
    with open(summary, "a") as f:
        f.write("### Test attachments\n\n")
        if exported:
            f.write("%d files (%d screenshots) in the `screenshots` artifact:\n\n" % (len(exported), len(images)))
            f.write("".join("- `%s`\n" % n for _, n in exported))
        else:
            f.write("No attachments were recorded.\n")
        f.write("\n")
PY
exit 0
