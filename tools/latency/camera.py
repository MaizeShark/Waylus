#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Glass-to-glass latency from a camera recording of the monitor and the tablet.

Open barcode.html fullscreen on the mirrored monitor, mirror it to the tablet, and
film both screens in one shot (slow motion, e.g. 240 fps). Every camera frame then
shows two timestamps of the same page: the monitor's current one and the older one
the tablet is still showing. Their difference is how far the tablet lags the real
monitor, photon to photon; no clock sync is involved, and the camera frame rate
only sets the number of samples.

1. Dump a frame and note the four corners of each barcode (the 4x8 grid, not the
   letterbox bars), clockwise from top-left, in pixels of the dumped image:
     camera.py VIDEO --dump 100 frame.png
2. Analyse:
     camera.py VIDEO --monitor 102,80 900,95 890,540 98,520 --tablet 1000,200 ...

Frames where a barcode is mid-change (blended cells) fail the checksum and are skipped.
Needs ffmpeg/ffprobe and numpy.
"""

import argparse
import json
import statistics
import subprocess
import sys

import numpy as np

ROWS, COLS = 4, 8


def probe(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "stream=width,height,r_frame_rate,nb_frames:stream_side_data=rotation", "-of", "json", path],
        check=True, capture_output=True, text=True).stdout
    s = json.loads(out)["streams"][0]
    w, h = s["width"], s["height"]
    rot = 0
    for sd in s.get("side_data_list", []):
        rot = int(sd.get("rotation", 0))
    if rot % 180:
        w, h = h, w  # ffmpeg auto-rotates when decoding
    num, den = s["r_frame_rate"].split("/")
    return w, h, float(num) / float(den), s.get("nb_frames")


def frames(path, w, h):
    p = subprocess.Popen(["ffmpeg", "-v", "error", "-i", path, "-f", "rawvideo", "-pix_fmt", "gray", "-"],
                         stdout=subprocess.PIPE)
    n = w * h
    while True:
        buf = p.stdout.read(n)
        if len(buf) < n:
            break
        yield np.frombuffer(buf, np.uint8).reshape(h, w)
    p.wait()


def homography(quad):
    """Maps unit-square coords (u, v) to image coords for quad = TL, TR, BR, BL."""
    src = [(0, 0), (1, 0), (1, 1), (0, 1)]
    a = []
    for (u, v), (x, y) in zip(src, quad):
        a.append([u, v, 1, 0, 0, 0, -u * x, -v * x, -x])
        a.append([0, 0, 0, u, v, 1, -u * y, -v * y, -y])
    _, _, vt = np.linalg.svd(np.array(a, float))
    return vt[-1].reshape(3, 3)


def cell_samples(quad):
    """Pixel offsets to sample for each of the 32 cells (a small patch at each centre)."""
    hm = homography(quad)
    pts = []
    for r in range(ROWS):
        for c in range(COLS):
            patch = []
            for du in (-0.15, 0, 0.15):
                for dv in (-0.15, 0, 0.15):
                    u, v = (c + 0.5 + du) / COLS, (r + 0.5 + dv) / ROWS
                    x, y, z = hm @ (u, v, 1)
                    patch.append((int(round(y / z)), int(round(x / z))))
            pts.append(patch)
    ys = np.array([[p[0] for p in cell] for cell in pts])
    xs = np.array([[p[1] for p in cell] for cell in pts])
    return ys, xs


def decode(img, sample):
    ys, xs = sample
    v = np.median(img[ys, xs], axis=1)
    lo, hi = v.min(), v.max()
    if hi - lo < 25:
        return None
    bits = 0
    for i, x in enumerate(v):
        if x > (lo + hi) / 2:
            bits |= 1 << i
    ms, sum_ = bits & 0xFFFFFF, bits >> 24
    if (((ms & 0xFF) + ((ms >> 8) & 0xFF) + ((ms >> 16) & 0xFF)) & 0xFF) ^ 0xA5 != sum_:
        return None
    return ms


def shifted(sample, dx, dy, w, h):
    ys, xs = sample
    ys, xs = ys + dy, xs + dx
    if ys.min() < 0 or xs.min() < 0 or ys.max() >= h or xs.max() >= w:
        return None
    return ys, xs


class Shake:
    """Hand-held camera: the global shift of each frame relative to a reference frame
    (the one the corners were read from), by phase correlation on a downscaled image.
    The desk, bezels and other static objects carry it; it doesn't depend on the
    barcodes decoding."""

    S = 4  # downscale factor

    def __init__(self, ref):
        h, w = ref.shape[0] // self.S, ref.shape[1] // self.S
        self.win = np.outer(np.hanning(h), np.hanning(w)).astype(np.float32)
        self.F = np.conj(np.fft.rfft2(self._prep(ref)))

    def _prep(self, img):
        h, w = self.win.shape
        small = img[: h * self.S, : w * self.S].reshape(h, self.S, w, self.S).mean(axis=(1, 3), dtype=np.float32)
        return (small - small.mean()) * self.win

    def shift(self, img):
        """(dx, dy) in full-resolution pixels such that img(x + dx, y + dy) ≈ ref(x, y)."""
        r = np.fft.rfft2(self._prep(img)) * self.F
        r /= np.abs(r) + 1e-9
        c = np.fft.irfft2(r, s=self.win.shape)
        py, px = np.unravel_index(np.argmax(c), c.shape)
        if py > c.shape[0] // 2:
            py -= c.shape[0]
        if px > c.shape[1] // 2:
            px -= c.shape[1]
        return int(px * self.S), int(py * self.S)


def quad(args):
    pts = [tuple(float(c) for c in p.split(",")) for p in args]
    if len(pts) != 4:
        raise SystemExit("give four corners: x,y x,y x,y x,y (TL TR BR BL)")
    return pts


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p / 100 * len(xs)))]


def fitted(rows):
    """Uses every tablet decode, not just frames where both barcodes read cleanly.

    The rolling shutter often catches the monitor mid-refresh, so few frames decode
    both screens. The monitor's value is linear in the camera frame index, though: a
    robust line fit through its decodes gives its value at every frame (averaging
    out its refresh steps), and also exposes the occasional garbage decode that
    passes the 8-bit checksum by chance.
    """
    pts = [(i, m) for i, m, _, _ in rows if m is not None]
    if len(pts) < 10:
        return
    x = np.array([p[0] for p in pts], float)
    y = np.array([p[1] for p in pts], float)
    y = np.unwrap(y, period=1 << 24)
    keep = np.ones(len(x), bool)
    for _ in range(5):
        a, b = np.polyfit(x[keep], y[keep], 1)
        keep = np.abs(y - (a * x + b)) < 25
    a, b = np.polyfit(x[keep], y[keep], 1)
    print(f"monitor fit: {a:.2f} ms per camera frame (≈ {1000 / a:.0f} fps), "
          f"{keep.sum()}/{len(x)} monitor decodes on the line")
    lat = []
    for i, _, t, _ in rows:
        if t is None:
            continue
        d = ((a * i + b) - t) % (1 << 24)
        if d < 2000:
            lat.append(d)
    if not lat:
        return
    # Chance checksum passes land far off; scale the window with the spread so a
    # tool with a wide (but real) latency distribution keeps its tails.
    med = statistics.median(lat)
    mad = statistics.median(abs(d - med) for d in lat)
    good = [d for d in lat if abs(d - med) < max(35, 6 * mad)]
    bad = sorted(d for d in lat if abs(d - med) >= max(35, 6 * mad))
    if bad:
        print(f"dropped as outliers: {len(bad)} values, {bad[0]:.0f}..{bad[-1]:.0f} ms "
              f"(median {statistics.median(bad):.0f}; window {med:.0f} ± {max(35, 6 * mad):.0f} ms)")
        # Clustered "outliers" are usually real freezes, not chance decodes: show both.
        print(f"all tablet decodes, outliers kept: p50 {statistics.median(lat):.0f} / p95 {pct(lat, 95):.0f} / "
              f"p99 {pct(lat, 99):.0f} ms (n={len(lat)})")
    print(f"fitted (all tablet decodes): p50 {statistics.median(good):.0f} / p95 {pct(good, 95):.0f} / "
          f"p5 {pct(good, 5):.0f} / p99 {pct(good, 99):.0f} / "
          f"min {min(good):.0f} / max {max(good):.0f} ms (n={len(good)}, {len(lat) - len(good)} outliers dropped)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("video")
    ap.add_argument("--dump", type=int, metavar="N", help="write frame N as a PNG (see --out) and exit")
    ap.add_argument("--out", default="frame.png")
    ap.add_argument("--monitor", nargs=4, metavar="X,Y", help="barcode corners on the monitor (TL TR BR BL)")
    ap.add_argument("--tablet", nargs=4, metavar="X,Y", help="barcode corners on the tablet (TL TR BR BL)")
    ap.add_argument("--csv", help="write per-frame values here")
    ap.add_argument("--track", type=int, metavar="N",
                    help="hand-held camera: N is the frame the corners were read from (--dump N); "
                         "every frame's corners are moved by its camera shift relative to it")
    args = ap.parse_args()

    w, h, fps, nb = probe(args.video)
    print(f"{args.video}: {w}x{h} @ {fps:.2f} fps ({nb or '?'} frames)", file=sys.stderr)

    if args.dump is not None:
        for i, img in enumerate(frames(args.video, w, h)):
            if i == args.dump:
                from PIL import Image
                Image.fromarray(img).save(args.out)
                print(f"wrote {args.out}", file=sys.stderr)
                return
        raise SystemExit("video has fewer frames")

    if not (args.monitor and args.tablet):
        raise SystemExit("need --monitor and --tablet corners (use --dump first)")
    mon, tab = cell_samples(quad(args.monitor)), cell_samples(quad(args.tablet))
    for ys, xs in (mon, tab):
        if ys.min() < 0 or xs.min() < 0 or ys.max() >= h or xs.max() >= w:
            raise SystemExit("corners outside the frame")

    def windows():
        """(frame index, image, monitor sample, tablet sample), following the shake if asked."""
        if args.track is None:
            for i, img in enumerate(frames(args.video, w, h)):
                yield i, img, mon, tab
            return
        ref = next(img for i, img in enumerate(frames(args.video, w, h)) if i == args.track)
        shake, seen = Shake(ref), []
        dx = dy = 0
        for i, img in enumerate(frames(args.video, w, h)):
            # The shake changes smoothly at slow-motion frame rates: every 4th frame is enough.
            if i % 4 == 0:
                dx, dy = shake.shift(img)
            seen.append((dx, dy))
            sm, st = shifted(mon, dx, dy, w, h), shifted(tab, dx, dy, w, h)
            if sm is None or st is None:
                continue
            yield i, img, sm, st
        xs, ys = [p[0] for p in seen], [p[1] for p in seen]
        print(f"tracking: camera shift x {min(xs)}..{max(xs)}, y {min(ys)}..{max(ys)} px", file=sys.stderr)

    lat, rows, total, ok_m, ok_t = [], [], 0, 0, 0
    for i, img, mon_s, tab_s in windows():
        total += 1
        m, t = decode(img, mon_s), decode(img, tab_s)
        ok_m += m is not None
        ok_t += t is not None
        d = None
        if m is not None and t is not None:
            d = (m - t) % (1 << 24)
            if d > 1 << 23:  # tablet "ahead": a decoding error, not a measurement
                d = None
            elif d < 2000:
                lat.append(d)
        rows.append((i, m, t, d))

    if args.csv:
        with open(args.csv, "w") as f:
            f.write("frame,monitor_ms,tablet_ms,latency_ms\n")
            for r in rows:
                f.write(",".join("" if x is None else str(x) for x in r) + "\n")

    print(f"frames {total}: monitor decoded {ok_m}, tablet decoded {ok_t}, both {len(lat)}")
    if not lat:
        raise SystemExit("no frame with both barcodes readable; check the corners (--dump)")
    print(f"tablet behind monitor: p50 {statistics.median(lat):.0f} / p95 {pct(lat, 95):.0f} / "
          f"min {min(lat)} / max {max(lat)} ms (n={len(lat)})")
    fitted(rows)
    # Each distinct monitor frame counts once, so slow motion doesn't over-weight static stretches.
    seen = {}
    for _, m, _, d in rows:
        if d is not None and d < 2000:
            seen.setdefault(m, d)
    per = list(seen.values())
    print(f"per monitor frame: p50 {statistics.median(per):.0f} / p95 {pct(per, 95):.0f} ms (n={len(per)})")


if __name__ == "__main__":
    main()
