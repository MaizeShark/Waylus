#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Summarises WAYLUS_DAMAGE_LOG files (docs/damage-path.md, phase 1).

  analyze.py LOG.csv [LOG.csv ...] [--skip 1.5] [--markdown]

Per file: frames and rate, then p50 / p90 / p95 / p99 / max per frame of
  - compositor damage: rectangles, area, the 16-px tile patches as BGRA bytes and LZ4 bytes;
  - pixel diff (only with readback, WAYLUS_NO_ZEROCOPY=1): changed tiles as BGRA and LZ4
    bytes, and their bounding box.
--skip drops the first seconds (the first frame is always full).
"""
import argparse
import csv
import math


def pct(v, q):
    if not v:
        return math.nan
    v = sorted(v)
    return v[min(len(v) - 1, int(round((len(v) - 1) * q)))]


def kb(x):
    return "-" if x != x else (f"{x / 1024:.0f}" if x >= 10240 else f"{x / 1024:.1f}")


def load(path, skip):
    with open(path) as f:
        rows = [r for r in csv.DictReader(f)]
    rows = [r for r in rows if float(r["t_ms"]) >= skip * 1000]
    return rows


def num(r, k):
    v = r.get(k, "")
    return float(v) if v not in ("", None) else None


def series(rows, k):
    return [x for x in (num(r, k) for r in rows) if x is not None]


QS = (0.5, 0.9, 0.95, 0.99, 1.0)


def line(name, vals, fmt):
    return [name] + [fmt(pct(vals, q)) for q in QS]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("logs", nargs="+")
    ap.add_argument("--skip", type=float, default=1.5)
    ap.add_argument("--markdown", action="store_true")
    a = ap.parse_args()
    for p in a.logs:
        rows = load(p, a.skip)
        if not rows:
            print(f"{p}: no frames")
            continue
        t = series(rows, "t_ms")
        dur = (t[-1] - t[0]) / 1000 if len(t) > 1 else 0
        meta = sum(int(r["meta"]) for r in rows)
        full = sum(int(r["full"]) for r in rows)
        w, h = rows[0]["frame_w"], rows[0]["frame_h"]
        print(f"\n## {p}\n{len(rows)} frames in {dur:.1f} s ({len(rows) / max(dur, 1e-9):.1f} fps), {w}x{h}; "
              f"damage metadata on {meta}, full damage on {full}; merged compositor frames per frame p50 {pct(series(rows, 'merged'), .5):.0f}")
        table = [["per frame", "p50", "p90", "p95", "p99", "max"]]
        if meta:
            table.append(line("compositor rects", series(rows, "rects"), lambda x: f"{x:.0f}"))
            table.append(line("compositor area, kpx", series(rows, "area"), lambda x: f"{x / 1000:.1f}"))
            table.append(line("compositor patches BGRA, KB", series(rows, "bgra_bytes"), kb))
            lz = series(rows, "lz4_bytes")
            if lz:
                table.append(line("compositor patches LZ4, KB", lz, kb))
        d = series(rows, "diff_px")
        if d:
            table.append(line("changed tiles BGRA, KB", [x * 4 for x in d], kb))
            table.append(line("changed tiles LZ4, KB", series(rows, "diff_lz4_bytes"), kb))
            table.append(line("changed patches (rects)", series(rows, "diff_patches"), lambda x: f"{x:.0f}"))
            bb = [num(r, "diff_bbox_w") * num(r, "diff_bbox_h") for r in rows if num(r, "diff_bbox_w") is not None]
            table.append(line("bounding box BGRA, KB", [x * 4 for x in bb], kb))
            nochange = sum(1 for x in d if x == 0)
            table.append(["frames with no changed pixel", f"{nochange}", "", "", "", ""])
            table.append(line("host: diff, ms", [x / 1000 for x in series(rows, "diff_us")], lambda x: f"{x:.2f}"))
            table.append(line("host: LZ4 of patches, ms", [x / 1000 for x in series(rows, "lz4_us")], lambda x: f"{x:.2f}"))
        if a.markdown:
            print("| " + " | ".join(table[0]) + " |\n|" + "---|" * len(table[0]))
            for r in table[1:]:
                print("| " + " | ".join(r) + " |")
        else:
            for r in table:
                print(f"{r[0]:32}" + "".join(f"{c:>9}" for c in r[1:]))


if __name__ == "__main__":
    main()
