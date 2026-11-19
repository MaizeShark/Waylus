# Damage path: small screen changes without the video decoder

An experiment: send the small parts of the screen that change while drawing (the stroke, the cursor) to the tablet as raw pixels, next to the H.264 stream, so they skip the tablet's hardware decoder. The decoder is the largest stage on the TCL NXTPAPER 14 (MT8781): ~19 ms per frame with the pen idle, ~11.5 ms while drawing ([where the time goes](measurements.md#where-the-time-goes)).

Status (2026-11-19): **phase 1 done on niri and the NXTPAPER 14; the decision gate passed.** Phase 2 (prototype, `--fast-patches`, off by default) is in progress. Still open from phase 1: compositor damage on KDE and GNOME, Windows dirty regions, and the tablet benchmark on the Pixel 7a (see [open items](#open-items)).

Contents:
- [Phase 1: measurements](#phase-1-measurements): [is the damage small?](#1-is-the-damage-small), [the tablet path](#2-the-tablet-path), [what USB carries](#3-what-usb-carries), [decision](#decision)
- [Open items](#open-items)
- [Dev flags and tools](#dev-flags-and-tools)

## Phase 1: measurements

### 1. Is the damage small?

**What the compositors report.** PipeWire can attach `SPA_META_VideoDamage` (a list of changed rectangles) to each buffer. The host now asks for it, when a dev flag is set (`WAYLUS_DAMAGE_LOG`, see [below](#dev-flags-and-tools)).

| Compositor | Damage metadata | How we know |
|---|---|---|
| niri 26.04 | **None.** Its screencast offers only the header and cursor metadata, so every frame counts as fully changed. Also none in niri `main` as of 2026-11-18. | Measured: no damage metadata on any buffer. Source: `src/screencasting/pw_utils.rs`. |
| KWin (Plasma) | Fills it: up to its region limit the real rectangles, else their bounding box. | Source only (`src/plugins/screencast/screencaststream.cpp`, `addDamage`); **not measured yet**. |
| mutter (GNOME) | Fills it: the rectangles, or the extents if there are more than its buffer holds. | Source only (`src/backends/meta-stream-source.c`); **not measured yet**. |
| Windows 11 (WGC) | `Direct3D11CaptureFrame.DirtyRegions` with `DirtyRegionMode = ReportOnly` (Windows 11 24H2). | Code added (logged per frame like on Linux); **not measured yet**. |

**What actually changes.** Because niri reports nothing, the measurements below compare pixels instead: with `WAYLUS_NO_ZEROCOPY=1` the host reads every frame back to the CPU, and the damage log compares it with the previous frame passed on, in 16×16 tiles. That is the true change in the picture, which compositor damage can only match or exceed. The changed tiles are merged into rectangles (runs of tiles per tile row, merged with identical runs below), and each rectangle is LZ4-compressed on its own, as BGRA (`lz4_flex` block format).

Method, 2026-11-19: niri 26.04, mirroring DP-2 (1920×1080 at 75 Hz) to the NXTPAPER 14 over USB accessory mode, host paced to the tablet's 60 Hz as in normal use (damage of skipped compositor frames is merged into the next frame passed on). Input from `tools/damage/scenario.py` (uinput), 20 s per scenario, the first 3.5 s dropped. Strokes are drawn with a synthetic *mouse*: Krita ignored the synthetic second tablet once the host's own "Waylus Pen" device existed (see [open items](#open-items)). The stroke speed, from the moving damage, was a median of ~1300 px/s (p90 2700), handwriting ~600 px/s. Per frame passed on:

| Scenario | Frames | fps | Changed tiles as BGRA, KB p50 / p95 / p99 | Same, LZ4, KB p50 / p95 / p99 | Rectangles p50 / p95 | Host LZ4 time p95, ms |
|---|---|---|---|---|---|---|
| Krita, 4 px brush, 100 % | 1159 | 59 | 4.0 / 9.0 / 12 | 0.9 / **2.7** / 3.4 | 2 / 4 | 0.01 |
| Krita, handwriting, 4 px | 1167 | 59 | 3.0 / 5.0 / 22 | 0.6 / **1.4** / 2.1 | 1 / 3 | 0.01 |
| Krita, 132 px brush, 100 % | 1164 | 59 | 54 / 71 / 84 | 1.5 / **11** / 17 | 14 / 22 | 0.06 |
| Krita, 43 px brush, 50 % zoom | 1144 | 60 | 10 / 17 / 22 | 1.3 / **2.7** / 3.3 | 3 / 6 | 0.02 |
| Cursor only (mouse) over a terminal | 1164 | 59 | 4.0 / 6.0 / 7.0 | 0.2 / 1.0 / 1.3 | 1 / 3 | 0.01 |
| Cursor only (pen hover) over a terminal | 1157 | 60 | 5.0 / 8.0 / 9.0 | 0.5 / 1.3 / 1.6 | 2 / 3 | 0.01 |
| Scrolling a page (Chrome, mouse wheel) | 1140 | 57 | 3137 / 3396 / 3462 | 327 / 387 / 408 | 67 / 111 | 2.29 |
| Dragging a floating window (Super + drag) | 1157 | 59 | 2272 / 2990 / 3105 | 191 / 249 / 259 | 39 / 180 | 1.45 |
| Dragging a tiled window (Super + drag) | 1165 | 59 | 1962 / 2458 / 4952 | 348 / 419 / 810 | 67 / 99 | 2.17 |

- **Drawing is tiny**: p95 1.4–11 KB per frame after LZ4, a few rectangles. Even the 132 px brush stays under 20 KB at p99.
- **Scrolling and dragging windows are not**: 250–420 KB per frame after LZ4 at p95, i.e. 15–25 MB/s at 60 fps. They have to stay on H.264.
- **The cursor shows up.** With `CursorMode::Embedded` the cursor is drawn into the frame; moving it changes 4–8 KB of tiles per frame (here found by the pixel diff, since niri reports no damage). Krita's brush outline is part of the drawing numbers.
- **Animated wallpapers.** The first cursor run was over an empty workspace with an animated (Wallpaper Engine) wallpaper: it changed ~150 scattered rectangles in every frame, 226 / 264 KB BGRA and 157 / 183 KB LZ4 (p50 / p95), whether or not the cursor moved. Anything animated on screen (wallpaper, video, a spinner) adds damage that has nothing to do with the pen, so a fast path should look only at the area around the pen rather than at the whole frame.
- An exploratory run before these (pen instead of mouse, Krita at 59.9 % zoom after switching it to full screen) had ~1.4 s in which Krita repainted most of the canvas, 1.2–1.6 MB per frame; it didn't recur in the runs above. Cause not determined (a late re-layout or Krita's instant preview).
- Host cost of the measurement itself (CPU readback path only): the tile diff takes 1.6–2.3 ms per 1080p frame, LZ4 of the drawing patches under 0.1 ms.

### 2. The tablet path

A hidden benchmark in the app (`DamageBench.kt`, started with `am start -n de.hut9.waylus/dev.waylus.MainActivity --es bench_damage LABEL`) times each step of putting a patch on screen without the decoder: LZ4 decompression (a Kotlin implementation of the block format, `Lz4.kt`), `glTexSubImage2D` into a 1920×1080 RGBA texture, drawing that texture over a full 1920×1080 buffer, and the whole path as a prototype would run it: decompress, upload, draw into a buffer from an `ImageReader`, hand that buffer to SurfaceFlinger on a child `SurfaceControl` above a `SurfaceView`, then wait for SurfaceFlinger's latch time and the present fence (`Transaction.addTransactionCompletedListener`, API 35). Test content: dark anti-aliased strokes on white, the centre crop of the size given.

TCL NXTPAPER 14 (MT8781, Mali-G57 MC2, Android 15), release build, 2026-11-19, three states: idle clocks; the app's own clock boost (`--boost`, one busy thread per CPU cluster); Android's input boost (a finger swipe every ~0.3 s through `adb shell input`, which also starts the app's boost, as when drawing). p50, or p50 / p95, in ms:

| Step | Size | Idle | App boost | Input boost |
|---|---|---|---|---|
| LZ4 decompress (strokes, 7.3:1 at 64², 6.4:1 at 256², 6.6:1 at 512², 8.3:1 at 1080p) | 64² / 256² / 512² / 1080p | 0.06 / 0.41 / 1.67 / 10.6 | – / 0.41 / 1.67 / 10.6 | – / 0.41 / 1.69 / 10.7 |
| `glTexSubImage2D` + `glFinish` | 64² / 256² / 512² / 1080p | 0.17 / 0.26 / 0.54 / 3.37 | 0.21 / 0.27 / 0.77 / 3.53 | 0.32 / 0.42 / 0.78 / 2.65 |
| Draw the texture over 1920×1080 + `glFinish` | | 1.75 | 1.59 | 0.83 |
| **Decompress + upload + draw + swap** | 64² | 3.1 / 3.6 | 2.6 / 2.7 | **2.4 / 2.8** |
| | 256² | 3.6 / 4.9 | 3.1 / 3.9 | **2.9 / 3.5** |
| | 512² | 5.0–8.0 / 9.2–9.7 | 4.8 / 6.8 | 4.9 / 6.1 |
| | 1080p | 16.4 / 21.9 | 16.0 / 19.8 | 17.7 / 24.2 |
| Swap → SurfaceFlinger latch | any | 11–13.6 / 18.5–21.9 | 10.4–13.2 / 18.2–20.2 | 9.7–10.5 / 17.7–18.7 |
| Latch → present fence | any | 14.4 or 31 (two modes) | 15.0 / 15.2 | 15.0 / 15.1 |

- Kotlin LZ4 runs at ~630 MB/s of output; it doesn't follow the clock boosts (the benchmark thread already runs on a fast core). A typical drawing patch (≤ 20 KB LZ4, ≤ ~85 KB of pixels) decompresses in ~0.1 ms.
- For small patches most of the ~2.5 ms is the full-screen draw and the swap, not the patch. Drawing only the changed area (scissor or a smaller layer) would cut it; not tried yet.
- **Compared with the decoder**: ~2.4–2.9 ms for drawing-sized patches against ~11.5 ms decode while drawing (and 3.1–3.6 against ~19 ms at idle clocks). Both paths then wait for SurfaceFlinger's next latch, so the patch should reach the glass about 8–9 ms earlier while drawing. That is an estimate from these numbers, measured in phase 3.
- Swap → latch is the wait for SurfaceFlinger's next frame plus the GPU finishing; uniform phase against vsync gives ~8 ms on average, the rest is the extra hop through the `ImageReader` and the transaction. The video path has the same kind of wait after the decoder, which the built-in "decoded → shown" (~1.5 ms) doesn't include: on this decoder that timestamp is the callback time.
- At idle clocks the present fence came either one frame (14.4 ms) or two frames (31 ms) after the latch; with either boost always one. In this benchmark the patch layer sits above an empty `SurfaceView` and the app's window, so the extra frame is probably GPU composition at low clocks; phase 2 checks the composition type with the real video underneath (`dumpsys SurfaceFlinger`).
- Pixel 7a: not measured yet (its screen was locked).

### 3. What USB carries

`WAYLUS_BENCH_LINK=1` makes the host measure the link instead of streaming. It needs nothing new in the app: the host sends messages of a type the app doesn't know (which it reads and drops), each followed by a PING; the PONG's receive time, mapped to the host clock through the offset of the fastest plain pings, says when the whole message had arrived. NXTPAPER 14 (USB 2.0, 480M in `lsusb -t`), same cable as above, 2026-11-19. Arrival time of one message on an idle link, p50 in ms (ranges over separate runs: 2 for AOA, 5 for adb), and throughput for 32 MB sent back to back:

| Message size | 4 KB | 16 KB | 64 KB | 128 KB | 256 KB | 1 MB | 4 MB | Throughput (64 KB–1 MB messages) |
|---|---|---|---|---|---|---|---|---|
| USB accessory mode (AOA) | 0.4–1.3 | 0.7–2.1 | 2.1–4.8 | 4.0–8.1 | 8.2–14.3 | 48–53 | 184–206 | **19–21 MB/s** (16 KB messages: 15–17) |
| adb (`adb reverse`) | 0.7–2.1 | 1.0–2.5 | 2.2–4.0 | 3.7–6.4 | 7.0–10.4 | 31–34 | 119–121 | **35–38 MB/s** |

- AOA carries only ~20 MB/s, about half of adb. The likely cause is on the tablet: Android's accessory gadget driver (`f_accessory`) reads in 16 KB requests with only two queued; adbd uses FunctionFS with larger asynchronous transfers. The app's read loop (16 KB chunks) matches the driver. Not investigated further.
- The H.264 stream takes 2.5 MB/s at 20 Mbit/s, so at 60 fps AOA leaves ~290 KB per frame and adb ~550 KB, *if* nothing else waits. Latency is the tighter limit: patches and video frames share one ordered stream, so a patch delays the frame behind it by its own transfer time. A 16 KB patch costs 0.7–2.1 ms, a 64 KB one 2–5 ms.
- First AOA run vs later ones: the first had a 0.24 ms ping and twice the speed for small messages, the later ones 0.72 ms; probably the tablet's clocks (the first run came right after the app started). Throughput was the same.
- Pixel 7a: shows as 5000M (USB 3) in `lsusb -t`; its link throughput is not measured yet.

### Decision

The gate was: typical drawing damage small (p95 under ~100 KB per frame after LZ4) and the tablet path clearly faster than decode (under ~4 ms for that size).
- Drawing damage: p95 1.4–11 KB after LZ4. **Passed** by a wide margin.
- Tablet path for drawing-sized patches: 2.4–2.9 ms p50 (p95 2.8–3.5) while drawing, against ~11.5 ms decode. **Passed.**
- Link: drawing patches take ~1–2 ms on AOA.

So phase 2 goes ahead, with **design A (overlay patches)**: H.264 stays the ground truth for every frame and the patches only cover the time until the decoder catches up, so a lost or late patch costs nothing and the video layer stays a hardware overlay. Design B (one mixed stream composed in GL) would need resync logic and loses the hardware overlay; nothing measured here says the encoder or decoder can't keep up, which was its only reason.

Consequences for the prototype, from the numbers above:
- Look only at a window around the pen (and the cursor), not the whole frame: animated content elsewhere would otherwise fill every frame with damage, and large changes go through H.264 anyway.
- On niri there is no compositor damage, so the host compares that window with the previous frame itself. Where the compositor reports damage (KDE, GNOME, Windows), the window can be skipped when nothing in it changed.
- Cap a frame's patches at a few tens of KB after LZ4 and drop (never queue) patches the link can't take right away.

## Open items

- KDE Plasma and GNOME: run the same scenarios with `WAYLUS_DAMAGE_LOG` (without `WAYLUS_NO_ZEROCOPY` it logs the compositor's damage only) for a monitor and a virtual output, to see whether the reported damage is as small as the pixel diff.
- Windows 11: the same on the Windows host (dirty regions; the LZ4 sizes need a readback that isn't there on Windows yet).
- Pixel 7a: the tablet benchmark and the link benchmark (USB 3: how much larger patches can get, and whether raw 1080p60 NV12 at ~190 MB/s is realistic).
- Krita and a second tablet: with the host's "Waylus Pen" present, Krita (6.0.4, Wayland) moved the cursor for the synthetic pen but drew nothing, even after the host exited; synthetic mouse strokes worked. Real use has one tablet, so this only affects the test setup.

## Dev flags and tools

None of these are for normal use; all are off unless set.

- `WAYLUS_DAMAGE_LOG=file.csv` (host, Linux and Windows): asks the compositor for damage and writes one CSV line per frame passed on: the compositor's rectangles, their 16×16 tile patches as BGRA bytes, and (with `WAYLUS_NO_ZEROCOPY=1`, Linux) the pixel diff and the LZ4 sizes. `tools/damage/analyze.py file.csv` prints percentiles.
- `tools/damage/scenario.py stroke|write|hover|mouse|scroll|drag|mstroke|mwrite|keys|tap`: synthetic input through uinput for repeatable scenarios (`--help`).
- `WAYLUS_BENCH_LINK=1` (host): the link benchmark above instead of streaming.
- `am start -n de.hut9.waylus/dev.waylus.MainActivity --es bench_damage LABEL [--ez bench_boost true]` (app): the tablet benchmark above; results in logcat, lines starting with `dbench`.
