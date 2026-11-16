#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""End-to-end latency probe for screen mirroring onto an Android tablet.

Setup:
  * barcode.html is open fullscreen on the mirrored monitor (host wall clock, per frame)
  * the Waylus APK's LatencyOverlay shows, on the tablet, the expected presentation time
    of each frame converted to the host wall clock (synced through this script)
  * any mirroring tool (Waylus, Weylus in a browser, ...) shows the monitor on the tablet

Each tablet screenshot then contains both barcodes; their difference is the time from
the host page rendering a frame to the tablet presenting it. Run with `python3 -I`.
"""
import argparse, io, socket, statistics, struct, subprocess, sys, threading, time

from PIL import Image

PKG = "dev.waylus"
CELL_W, CELL_H = 40, 24  # LatencyOverlay cell size in px


def adb(*args, check=True, capture=True):
    return subprocess.run(["adb", *args], check=check, capture_output=capture)


def sync_server(port):
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(4)

    def serve(c):
        c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        with c:
            while c.recv(1):
                c.sendall(struct.pack("<q", time.time_ns()))

    while True:
        c, _ = srv.accept()
        threading.Thread(target=serve, args=(c,), daemon=True).start()


def decode_bits(img, x0, y0, cw, ch, strict=False):
    """Read a 4x8 grid; returns the 24-bit ms value or None if the checksum fails.
    strict=True (the overlay) samples near the cell corners and requires uniform cells,
    so a misaligned grid is rejected instead of producing a lucky checksum match."""
    bits = 0
    mx, my = (cw / 2 - 3, ch / 2 - 3) if strict else (3, 3)
    for i in range(32):
        cx = x0 + (i % 8 + 0.5) * cw
        cy = y0 + (i // 8 + 0.5) * ch
        lum = [sum(img.getpixel((int(cx + dx), int(cy + dy)))[:3]) / 3
               for dx in (-mx, 0, mx) for dy in (-my, 0, my)]
        if strict and (max(lum) - min(lum) > 25 or 90 < lum[4] < 150):
            return None
        if sum(lum) / len(lum) > 140:
            bits |= 1 << i
    ms = bits & 0xFFFFFF
    s = ((ms & 0xFF) + ((ms >> 8) & 0xFF) + ((ms >> 16) & 0xFF)) & 0xFF ^ 0xA5
    return ms if (bits >> 24) == s else None


def find_overlay(img):
    w, h = img.size
    ow, oh = CELL_W * 8, CELL_H * 4
    # Normally flush bottom-right; search a little in case of insets.
    for dy in range(0, 200, 2):
        for dx in range(0, 200, 2):
            v = decode_bits(img, w - ow - dx, h - oh - dy, CELL_W, CELL_H, strict=True)
            if v is not None:
                return v, (w - ow - dx, h - oh - dy, w - dx, h - dy)
    return None, None


def find_mirror(img, overlay_box):
    """Bounding box of the mirrored page. Its dark cells are #555 (~85), well above
    letterbox bars (black in Waylus, ~33 grey in Weylus)."""
    g = img.convert("L").point(lambda v: 255 if v > 60 else 0)
    if overlay_box:
        g.paste(0, overlay_box)
    box = g.getbbox()
    if not box:
        return None
    x0, y0, x1, y1 = box
    return decode_bits(img, x0, y0, (x1 - x0) / 8, (y1 - y0) / 4)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--samples", type=int, default=40)
    ap.add_argument("--port", type=int, default=27184)
    ap.add_argument("--label", default="")
    ap.add_argument("--keep", help="save failed screenshots to this directory")
    a = ap.parse_args()

    threading.Thread(target=sync_server, args=(a.port,), daemon=True).start()
    adb("reverse", f"tcp:{a.port}", f"tcp:{a.port}")
    adb("shell", "appops", "set", PKG, "SYSTEM_ALERT_WINDOW", "allow")
    adb("shell", "am", "start-foreground-service", "-n", f"{PKG}/.LatencyOverlay", "--ei", "port", str(a.port))
    time.sleep(3)  # clock sync settles

    lat, fails = [], 0
    try:
        while len(lat) < a.samples and fails < a.samples * 2:
            png = adb("exec-out", "screencap", "-p").stdout
            img = Image.open(io.BytesIO(png)).convert("RGB")
            shown, box = find_overlay(img)
            drawn = find_mirror(img, box) if shown is not None else None
            if shown is None or drawn is None:
                fails += 1
                if a.keep:
                    img.save(f"{a.keep}/fail-{fails}.png")
                continue
            d = (shown - drawn) % (1 << 24)
            if d > 2000:  # stale / misdecoded
                fails += 1
                continue
            lat.append(d)
            print(f"\r{len(lat)}/{a.samples}: {d} ms   ", end="", file=sys.stderr, flush=True)
    finally:
        adb("shell", "am", "stopservice", "-n", f"{PKG}/.LatencyOverlay", check=False)
        adb("reverse", "--remove", f"tcp:{a.port}", check=False)
    print(file=sys.stderr)
    if not lat:
        sys.exit("no valid samples (is barcode.html fullscreen on the mirrored monitor?)")
    q = statistics.quantiles(lat, n=20)
    print(f"{a.label or 'result'}: n={len(lat)} min {min(lat)} / p50 {statistics.median(lat):.0f} / "
          f"p95 {q[18]:.0f} / max {max(lat)} ms  (failed decodes: {fails})")


if __name__ == "__main__":
    main()
