#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Pen latency of any tablet-mirroring tool: stylus event on the tablet → host input device.

Reads the tablet's stylus device over adb and the tool's virtual input device on the
host, both with kernel event timestamps (CLOCK_REALTIME), and syncs the two clocks over
a persistent adb shell (offset from the round trip with the smallest delay). Tap the pen
on the tablet; each pen-down appears once on each side, and the difference is how long
the tool takes to deliver it. Everything after that (compositor, apps, monitor) is the
same for every tool.

    pen.py --host-device "Waylus Pen"       # or "Weylus Stylus"

Needs adb (USB debugging) and read access to /dev/input on the host (group `input`).
Run with `python3 -I`.
"""

import argparse
import re
import statistics
import struct
import subprocess
import sys
import threading
import time

EV_KEY, BTN_TOUCH = 1, 0x14A
EVENT = struct.Struct("<qqHHi")  # 64-bit timeval, type, code, value (x86_64 and arm64)


def host_device(name):
    text = open("/proc/bus/input/devices").read()
    for block in text.split("\n\n"):
        if f'N: Name="' in block and name in block.split('N: Name="', 1)[1].split('"', 1)[0]:
            m = re.search(r"\bevent(\d+)\b", block)
            if m:
                return f"/dev/input/event{m.group(1)}"
    return None


def tablet_device():
    out = subprocess.run(["adb", "shell", "getevent -pl 2>/dev/null"], capture_output=True, text=True).stdout
    for dev, name in re.findall(r'add device \d+: (\S+)\s+name:\s+"([^"]*)"', out):
        if re.search(r"pen|stylus", name, re.I):
            return dev, name
    return None, None


def clock_offset(samples=80):
    """Tablet CLOCK_REALTIME minus host CLOCK_REALTIME, in ns, from the fastest round trip."""
    sh = subprocess.Popen(["adb", "shell"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
    best = None
    for _ in range(samples):
        t0 = time.time_ns()
        # A shell builtin (mksh): spawning `date` added ~15 ms per round trip.
        sh.stdin.write("echo $EPOCHREALTIME\n")
        sh.stdin.flush()
        line = sh.stdout.readline()
        t1 = time.time_ns()
        try:
            sec, _, frac = line.strip().partition(".")
            tab = int(sec) * 1_000_000_000 + int(frac.ljust(9, "0")[:9])
        except ValueError:
            continue
        rtt = t1 - t0
        if best is None or rtt < best[1]:
            best = (tab - (t0 + t1) // 2, rtt)
    sh.stdin.close()
    sh.wait()
    if best is None:
        sys.exit("clock sync failed (adb shell echo $EPOCHREALTIME)")
    return best


def reader(stream, out, stop):
    buf = b""
    while not stop.is_set():
        chunk = stream.read1(4096) if hasattr(stream, "read1") else stream.read(EVENT.size)
        if not chunk:
            break
        buf += chunk
        while len(buf) >= EVENT.size:
            sec, usec, typ, code, value = EVENT.unpack_from(buf)
            buf = buf[EVENT.size:]
            if typ == EV_KEY and code == BTN_TOUCH and value == 1:
                out.append(sec * 1_000_000_000 + usec * 1000)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host-device", required=True, help='name (substring) of the tool\'s pen device, e.g. "Waylus Pen"')
    ap.add_argument("--tablet-device", help="stylus device on the tablet (default: the one named *pen*/*stylus*)")
    ap.add_argument("--taps", type=int, default=30)
    ap.add_argument("--timeout", type=float, default=180)
    ap.add_argument("--label", default="")
    a = ap.parse_args()

    host = host_device(a.host_device)
    if not host:
        sys.exit(f"no host input device named like {a.host_device!r} (is the tool running?)")
    tab_dev, tab_name = (a.tablet_device, "") if a.tablet_device else tablet_device()
    if not tab_dev:
        sys.exit("no stylus device found on the tablet; pass --tablet-device")
    off1, rtt1 = clock_offset()
    print(f"host {host}, tablet {tab_dev} {tab_name}; clock offset {off1 / 1e6:.2f} ms (rtt {rtt1 / 1e6:.2f} ms)",
          file=sys.stderr)

    stop = threading.Event()
    tab_downs, host_downs = [], []
    # dd rather than cat: Waylus kills stray `cat <device>` readers on start.
    tab = subprocess.Popen(["adb", "exec-out", f"dd if={tab_dev} bs={EVENT.size} 2>/dev/null"], stdout=subprocess.PIPE)
    hostf = open(host, "rb", buffering=0)
    threading.Thread(target=reader, args=(tab.stdout, tab_downs, stop), daemon=True).start()
    threading.Thread(target=reader, args=(hostf, host_downs, stop), daemon=True).start()

    print(f"tap the pen on the tablet {a.taps} times (about one tap per second)", file=sys.stderr)
    start = time.time()
    seen = 0
    while len(tab_downs) < a.taps and time.time() - start < a.timeout:
        if len(tab_downs) != seen:
            seen = len(tab_downs)
            print(f"\r{seen}/{a.taps}", end="", file=sys.stderr, flush=True)
        time.sleep(0.05)
    time.sleep(1.0)
    stop.set()
    tab.kill()
    print(file=sys.stderr)

    off2, rtt2 = clock_offset()
    off = (off1 + off2) // 2
    print(f"clock offset after: {off2 / 1e6:.2f} ms (drift {(off2 - off1) / 1e6:+.2f} ms)", file=sys.stderr)

    lat, used = [], set()
    for t in tab_downs:
        t_host = t - off
        cand = [h for h in host_downs if h not in used and -5e6 <= h - t_host < 1e9]
        if cand:
            h = min(cand)
            used.add(h)
            lat.append((h - t_host) / 1e6)
    if not lat:
        sys.exit(f"no matching pen-downs (tablet {len(tab_downs)}, host {len(host_downs)}); "
                 "if the numbers are huge, the tablet's evdev clock may not be CLOCK_REALTIME")
    lat.sort()
    p95 = lat[min(len(lat) - 1, int(0.95 * len(lat)))]
    print(f"{a.label or a.host_device}: pen-down → host device p50 {statistics.median(lat):.1f} / p95 {p95:.1f} / "
          f"min {lat[0]:.1f} / max {lat[-1]:.1f} ms (n={len(lat)} of {len(tab_downs)} taps; "
          f"clock sync ±{max(rtt1, rtt2) / 2e6:.1f} ms)")


if __name__ == "__main__":
    main()
