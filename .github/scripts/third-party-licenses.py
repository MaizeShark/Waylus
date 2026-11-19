#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Writes the licences of the Rust crates linked into waylus for one target.

Usage: third-party-licenses.py <target triple> <output file>

Follows the normal (not build or dev) dependencies from the waylus package, skips proc
macros (they run at build time and are not part of the binary) and copies each crate's
licence files. Run from the repository root; cargo must be able to fetch the crates.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|copyright|unlicense)([-_.].*)?$", re.I)


def main() -> None:
    target, out = sys.argv[1], Path(sys.argv[2])
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
        check=True, capture_output=True, text=True).stdout)
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == "waylus")

    linked, todo = set(), [root]
    while todo:
        for dep in nodes[todo.pop()]["deps"]:
            pkg = packages[dep["pkg"]]
            if dep["pkg"] in linked or not any(k["kind"] is None for k in dep["dep_kinds"]):
                continue
            if any("proc-macro" in t["kind"] for t in pkg["targets"]):
                continue
            linked.add(dep["pkg"])
            todo.append(dep["pkg"])

    parts = [
        f"Third-party Rust crates in waylus ({target}), with their licences.\n"
        "The Rust standard library is also linked in: MIT OR Apache-2.0, "
        "https://github.com/rust-lang/rust\n"
    ]
    missing = []
    for pkg in sorted((packages[i] for i in linked), key=lambda p: (p["name"], p["version"])):
        crate_dir = Path(pkg["manifest_path"]).parent
        files = sorted(f for f in crate_dir.iterdir() if f.is_file() and LICENSE_FILE.match(f.name))
        if pkg.get("license_file"):
            extra = crate_dir / pkg["license_file"]
            if extra.is_file() and extra not in files:
                files.append(extra)
        header = f"{pkg['name']} {pkg['version']}: {pkg.get('license') or 'see licence file'}"
        if pkg.get("repository"):
            header += f"\n{pkg['repository']}"
        parts.append("=" * 78 + "\n" + header + "\n")
        if not files:
            missing.append(pkg["name"])
            parts.append("(The crate ships no licence file; its licence is the SPDX expression above.)\n")
        for f in files:
            parts.append(f"--- {f.name} ---\n{f.read_text(encoding='utf-8', errors='replace').rstrip()}\n")
    out.write_text("\n".join(parts), encoding="utf-8")
    print(f"{out}: {len(linked)} crates" + (f"; without licence files: {', '.join(missing)}" if missing else ""))


if __name__ == "__main__":
    main()
