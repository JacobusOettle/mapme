#!/usr/bin/env python3
"""Fast Swift *syntax* check with tree-sitter (no Xcode needed).

Catches unbalanced braces, malformed declarations and similar parse errors
before spending macOS CI minutes. It does not type-check.

    python3 -m venv .venv && .venv/bin/pip install tree-sitter tree-sitter-swift
    .venv/bin/python scripts/check_swift_syntax.py ios
"""
import pathlib
import sys

import tree_sitter_swift
from tree_sitter import Language, Parser


def errors(node, out):
    if node.type == "ERROR" or node.is_missing:
        out.append(node)
        return
    for child in node.children:
        errors(child, out)


def main(roots):
    parser = Parser(Language(tree_sitter_swift.language()))
    files = sorted(p for r in roots for p in pathlib.Path(r).rglob("*.swift"))
    bad = 0
    for f in files:
        src = f.read_bytes()
        tree = parser.parse(src)
        errs = []
        errors(tree.root_node, errs)
        for e in errs[:5]:
            line = e.start_point[0] + 1
            col = e.start_point[1] + 1
            snippet = src.splitlines()[line - 1].decode("utf-8", "replace").strip() if line - 1 < len(src.splitlines()) else ""
            kind = f"missing {e.type}" if e.is_missing else "syntax error"
            print(f"{f}:{line}:{col}: {kind}: {snippet[:120]}")
        bad += bool(errs)
    print(f"checked {len(files)} Swift files, {bad} with syntax errors")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:] or ["ios"]))
