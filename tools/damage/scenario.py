#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Repeatable input for the damage measurements (docs/damage-path.md, phase 1).

Creates its own uinput pen, mouse and keyboard and plays one scenario on the mirrored
output. The pen is a tablet like the "Waylus Pen", so the compositor maps it to the
output Waylus mirrors (niri: Waylus's config.d drop-in, while Waylus runs; KDE/GNOME:
map it by hand or use --output-rect). Coordinates are fractions of that output.

  scenario.py stroke   [--seconds 10]   pen strokes (zig-zags and loops) with contact
  scenario.py write    [--seconds 10]   handwriting-like: short small loops along lines
  scenario.py hover    [--seconds 10]   pen hovering only: moves the cursor
  scenario.py mouse    [--seconds 10]   mouse motion only (relative), cursor only
  scenario.py scroll   [--seconds 10]   mouse wheel, 1 notch every 100 ms, down then up
  scenario.py drag     [--seconds 10]   Super + left button drag (niri/KDE: move window)
  scenario.py keys K [K ...]              press keys/chords (e.g. 1, RIGHTBRACE, CTRL+Z)
  scenario.py tap X,Y [X,Y ...]           pen taps at these points (fractions of the output)
  scenario.py mclick --at X,Y             a mouse click there
  scenario.py mstroke / mwrite            the stroke / write paths drawn with the mouse
                                          (left button), for apps that ignore a second tablet

Start with the pointer over the window under test (`--at X,Y` puts it there with a
hover first). Needs write access to /dev/uinput.
"""
import argparse
import math
import time

import evdev
from evdev import AbsInfo, UInput, ecodes as e

AXIS = 65535
RATE = 120  # events per second, like a fast pen


def make_pen():
    caps = {
        e.EV_KEY: [e.BTN_TOOL_PEN, e.BTN_TOUCH, e.BTN_STYLUS],
        e.EV_ABS: [
            (e.ABS_X, AbsInfo(0, 0, AXIS, 0, 0, 200)),
            (e.ABS_Y, AbsInfo(0, 0, AXIS, 0, 0, 300)),
            (e.ABS_PRESSURE, AbsInfo(0, 0, 4095, 0, 0, 0)),
        ],
    }
    return UInput(caps, name="Waylus Scenario Pen", bustype=e.BUS_VIRTUAL, vendor=0x1209, product=0x5751,
                  input_props=[e.INPUT_PROP_DIRECT])


def make_mouse():
    caps = {e.EV_KEY: [e.BTN_LEFT, e.BTN_RIGHT, e.BTN_MIDDLE], e.EV_REL: [e.REL_X, e.REL_Y, e.REL_WHEEL, e.REL_WHEEL_HI_RES]}
    return UInput(caps, name="Waylus Scenario Mouse", bustype=e.BUS_VIRTUAL, vendor=0x1209, product=0x5752)


def make_kbd():
    return UInput({e.EV_KEY: list(range(1, 249))}, name="Waylus Scenario Keyboard", bustype=e.BUS_VIRTUAL,
                  vendor=0x1209, product=0x5753)


def keys(kbd, a):
    """Presses each argument: a key name (LEFTBRACE, 1, ...) or a chord such as CTRL+SHIFT+A."""
    for chord in a.keys:
        codes = [getattr(e, "KEY_" + ("LEFT" + k if k in ("CTRL", "SHIFT", "ALT", "META") else k).upper()) for k in chord.split("+")]
        for c in codes:
            kbd.write(e.EV_KEY, c, 1)
        kbd.syn()
        time.sleep(0.03)
        for c in reversed(codes):
            kbd.write(e.EV_KEY, c, 0)
        kbd.syn()
        time.sleep(0.08)


class Pen:
    def __init__(self, dev):
        self.d = dev
        self.prox = False
        self.down = False

    def move(self, fx, fy, pressure=None):
        d = self.d
        if not self.prox:
            d.write(e.EV_KEY, e.BTN_TOOL_PEN, 1)
            self.prox = True
        d.write(e.EV_ABS, e.ABS_X, int(max(0, min(1, fx)) * AXIS))
        d.write(e.EV_ABS, e.ABS_Y, int(max(0, min(1, fy)) * AXIS))
        if pressure is not None:
            if pressure > 0 and not self.down:
                d.write(e.EV_KEY, e.BTN_TOUCH, 1)
                self.down = True
            d.write(e.EV_ABS, e.ABS_PRESSURE, int(pressure * 4095))
            if pressure == 0 and self.down:
                d.write(e.EV_KEY, e.BTN_TOUCH, 0)
                self.down = False
        d.syn()

    def leave(self):
        if self.down:
            self.d.write(e.EV_ABS, e.ABS_PRESSURE, 0)
            self.d.write(e.EV_KEY, e.BTN_TOUCH, 0)
            self.down = False
        if self.prox:
            self.d.write(e.EV_KEY, e.BTN_TOOL_PEN, 0)
            self.prox = False
        self.d.syn()


def ticks(seconds):
    """Yields (i, t) at RATE Hz for `seconds`, sleeping to the schedule."""
    t0 = time.monotonic()
    n = int(seconds * RATE)
    for i in range(n):
        due = t0 + i / RATE
        dt = due - time.monotonic()
        if dt > 0:
            time.sleep(dt)
        yield i, i / RATE


def stroke(pen, a):
    """Zig-zags and loops over the middle of the output, pen down for 1.5 s, up for 0.25 s."""
    x0, y0, x1, y1 = a.box
    for i, t in ticks(a.seconds):
        phase = t % 1.75
        k = t * 0.6
        fx = x0 + (x1 - x0) * (0.5 + 0.45 * math.sin(2 * math.pi * k))
        fy = y0 + (y1 - y0) * (0.5 + 0.4 * math.sin(2 * math.pi * k * 2.3 + 1))
        if phase < 1.5:
            pen.move(fx, fy, 0.35 + 0.3 * math.sin(t * 7) ** 2)
        else:
            pen.move(fx, fy, 0)


def write(pen, a):
    """Handwriting: small loops (letters ~1.5 % of the output high) along lines, word gaps."""
    x0, y0, x1, y1 = a.box
    line_h = 0.06
    x, y = x0, y0 + 0.05
    for i, t in ticks(a.seconds):
        word = (t % 1.2) < 1.0
        x += 0.0012  # ~ 17 cm/s on a 30 cm wide screen... sideways drift
        if x > x1:
            x, y = x0, y + line_h
            if y > y1:
                y = y0 + 0.05
        lx = x + 0.006 * math.cos(2 * math.pi * t * 6)
        ly = y + 0.012 * math.sin(2 * math.pi * t * 6) + 0.004 * math.sin(2 * math.pi * t * 1.7)
        pen.move(lx, ly, 0.5 if word else 0)


def hover(pen, a):
    x0, y0, x1, y1 = a.box
    for i, t in ticks(a.seconds):
        k = t * 0.5
        pen.move(x0 + (x1 - x0) * (0.5 + 0.45 * math.sin(2 * math.pi * k)),
                 y0 + (y1 - y0) * (0.5 + 0.45 * math.cos(2 * math.pi * k * 1.3)))


def mouse_move(mouse, a):
    for i, t in ticks(a.seconds):
        mouse.write(e.EV_REL, e.REL_X, int(round(6 * math.cos(2 * math.pi * t * 0.5))))
        mouse.write(e.EV_REL, e.REL_Y, int(round(6 * math.sin(2 * math.pi * t * 0.5))))
        mouse.syn()


def mouse_path(mouse, a, fn):
    """Moves the mouse along fn(t) -> (fx, fy, down) in output fractions, as relative motion.
    Assumes the pointer starts at fn(0) (use --at) and that pointer acceleration is flat
    or that small steps are close enough; `--px` is the output width in pixels."""
    w, h = a.px
    fx0, fy0, _ = fn(0)
    cx, cy = fx0 * w, fy0 * h
    down = False
    for i, t in ticks(a.seconds):
        fx, fy, want = fn(t)
        dx, dy = int(round(fx * w - cx)), int(round(fy * h - cy))
        cx += dx
        cy += dy
        if want != down:
            mouse.write(e.EV_KEY, e.BTN_LEFT, int(want))
            down = want
        if dx or dy:
            mouse.write(e.EV_REL, e.REL_X, dx)
            mouse.write(e.EV_REL, e.REL_Y, dy)
        mouse.syn()
    if down:
        mouse.write(e.EV_KEY, e.BTN_LEFT, 0)
        mouse.syn()


def stroke_fn(a):
    x0, y0, x1, y1 = a.box

    def f(t):
        k = t * 0.6
        return (x0 + (x1 - x0) * (0.5 + 0.45 * math.sin(2 * math.pi * k)),
                y0 + (y1 - y0) * (0.5 + 0.4 * math.sin(2 * math.pi * k * 2.3 + 1)),
                (t % 1.75) < 1.5)
    return f


def write_fn(a):
    x0, y0, x1, y1 = a.box
    span = x1 - x0
    lines = max(1, int((y1 - y0 - 0.05) / 0.06))

    def f(t):
        x = 0.144 * t  # output widths per second sideways
        line = int(x / span)
        lx = x0 + (x % span) + 0.006 * math.cos(2 * math.pi * t * 6)
        ly = y0 + 0.05 + 0.06 * (line % lines) + 0.012 * math.sin(2 * math.pi * t * 6) + 0.004 * math.sin(2 * math.pi * t * 1.7)
        return lx, ly, (t % 1.2) < 1.0
    return f


def mouse_click(mouse):
    """A left click where the pointer is (put it there with --at)."""
    mouse.write(e.EV_KEY, e.BTN_LEFT, 1)
    mouse.syn()
    time.sleep(0.05)
    mouse.write(e.EV_KEY, e.BTN_LEFT, 0)
    mouse.syn()
    time.sleep(0.1)


def scroll(mouse, a):
    half = a.seconds / 2
    for i, t in ticks(a.seconds):
        if i % (RATE // 10) == 0:
            v = -1 if t < half else 1
            mouse.write(e.EV_REL, e.REL_WHEEL, v)
            mouse.write(e.EV_REL, e.REL_WHEEL_HI_RES, v * 120)
            mouse.syn()


def drag(mouse, kbd, a):
    kbd.write(e.EV_KEY, e.KEY_LEFTMETA, 1)
    kbd.syn()
    time.sleep(0.1)
    mouse.write(e.EV_KEY, e.BTN_LEFT, 1)
    mouse.syn()
    try:
        for i, t in ticks(a.seconds):
            mouse.write(e.EV_REL, e.REL_X, int(round(5 * math.cos(2 * math.pi * t * 0.4))))
            mouse.write(e.EV_REL, e.REL_Y, int(round(3 * math.sin(2 * math.pi * t * 0.4))))
            mouse.syn()
    finally:
        mouse.write(e.EV_KEY, e.BTN_LEFT, 0)
        mouse.syn()
        kbd.write(e.EV_KEY, e.KEY_LEFTMETA, 0)
        kbd.syn()


def tap(pen, a):
    """Taps the pen at each X,Y (fractions) given as arguments."""
    for pt in a.keys:
        fx, fy = (float(v) for v in pt.split(","))
        for _ in range(5):
            pen.move(fx, fy)
            time.sleep(0.01)
        pen.move(fx, fy, 0.5)
        time.sleep(0.06)
        pen.move(fx, fy, 0)
        time.sleep(0.05)
        pen.leave()
        time.sleep(0.2)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("scenario", choices=["stroke", "write", "hover", "mouse", "scroll", "drag", "keys", "tap", "mstroke", "mwrite", "mclick"])
    ap.add_argument("keys", nargs="*", help="for `keys`")
    ap.add_argument("--seconds", type=float, default=10)
    ap.add_argument("--box", default="0.2,0.2,0.8,0.8", help="area for pen scenarios, fractions x0,y0,x1,y1")
    ap.add_argument("--at", help="hover the pen to X,Y (fractions) first, to put the pointer there")
    ap.add_argument("--px", default="1920,1080", help="output size in pixels (mouse scenarios)")
    ap.add_argument("--settle", type=float, default=1.0, help="seconds to wait after creating the devices")
    a = ap.parse_args()
    a.box = [float(v) for v in a.box.split(",")]
    a.px = [int(v) for v in a.px.split(",")]
    pen_dev, mouse, kbd = make_pen(), make_mouse(), make_kbd()
    pen = Pen(pen_dev)
    try:
        time.sleep(a.settle)  # let the compositor pick the devices up
        if not a.at and a.scenario in ("mstroke", "mwrite"):
            fx0, fy0, _ = (stroke_fn if a.scenario == "mstroke" else write_fn)(a)(0)
            a.at = f"{fx0},{fy0}"
        if a.at:
            fx, fy = (float(v) for v in a.at.split(","))
            for _ in range(10):
                pen.move(fx, fy)
                time.sleep(0.01)
            pen.leave()
            time.sleep(0.2)
        {
            "stroke": lambda: stroke(pen, a),
            "write": lambda: write(pen, a),
            "hover": lambda: hover(pen, a),
            "mouse": lambda: mouse_move(mouse, a),
            "scroll": lambda: scroll(mouse, a),
            "drag": lambda: drag(mouse, kbd, a),
            "keys": lambda: keys(kbd, a),
            "tap": lambda: tap(pen, a),
            "mstroke": lambda: mouse_path(mouse, a, stroke_fn(a)),
            "mwrite": lambda: mouse_path(mouse, a, write_fn(a)),
            "mclick": lambda: mouse_click(mouse),
        }[a.scenario]()
    finally:
        pen.leave()
        time.sleep(0.2)
        for d in (pen_dev, mouse, kbd):
            d.close()


if __name__ == "__main__":
    main()
