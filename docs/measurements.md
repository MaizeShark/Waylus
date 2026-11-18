# Measurements

All numbers here come from one desktop (RTX 4060) and one tablet (TCL NXTPAPER 14, MT8781, 60 Hz panel, Android 15) unless stated otherwise. They use the generic pipeline: the opt-in, device-specific `--vendor-tweaks` were never on. Tables measured in different sessions or on different systems are kept apart; only rows within one table were measured the same way in the same session.

Contents:
- [Method](#method)
- [How Waylus differs from Weylus](#how-waylus-differs-from-weylus)
- [Linux](#linux): [Waylus vs Weylus](#waylus-vs-weylus-niri-2026-11-17), [where the time goes](#where-the-time-goes), [Wi-Fi](#wi-fi-linux-host-2026-11-17), [2.4 GHz](#24-ghz), [other Linux tools](#other-linux-tablet-as-monitor-tools-kde-2026-11-18), [Moonlight](#moonlight--sunshine-wi-fi-kde-2026-11-18), [app 0.2.0 and the stats display](#app-020-and-the-stats-display-niri-2026-11-19)
- [Windows](#windows): [built-in statistics](#built-in-statistics), [photon to photon](#photon-to-photon), [pen](#pen), [Wi-Fi](#wi-fi), [Weylus and Moonlight](#weylus-and-moonlight-on-windows-2026-11-18)

## Method

### Photon to photon (`camera.py`)

Filmed with a phone camera (Pixel 7a slow motion, 120 fps) showing [`tools/latency/barcode.html`](../tools/latency/barcode.html) on the monitor and its mirror on the tablet in the same shot. `tools/latency/camera.py` decodes both timestamps in every frame; their difference is the lag you actually see between the two screens ("tablet behind the real monitor"), including the tablet's own compositing and display panel, with no clock sync involved.

- To reproduce: open `barcode.html` fullscreen on the mirrored monitor, film both screens in slow motion, run `python3 -I tools/latency/camera.py VIDEO --dump 100 --out frame.png` to read off the four corners of each barcode, then `camera.py VIDEO --monitor x,y x,y x,y x,y --tablet x,y x,y x,y x,y`.
- For a hand-held camera add `--track 100` (the frame the corners came from): every frame's corners are moved by the camera shake, measured by phase correlation against that frame.
- The rolling shutter often catches the monitor mid-refresh, so the monitor's timestamps are fitted as a line over the camera frames and every readable tablet frame counts.
- `camera.py` drops values far from the median as chance decodes ("outliers") and prints the figures both without and with them. Where the dropped values cluster (freezes), both readings are given below.
- An earlier method, `tools/latency/probe.py`, compared tablet screenshots against an overlay showing Android's expected present time. Its bias differs per tool: it read ~34 ms above the camera for Waylus (it includes the host's own render and compositing) but 21 ms *below* it for Weylus (177 vs 198 ms p50, re-measured the same day), so a screenshot apparently shows a browser client's frames before they reach the glass. It is kept for quick checks of Waylus alone, not for comparisons.

### Pen (`pen.py`)

Pen-down on the tablet → event on the host's input device. `tools/latency/pen.py` runs while the tool is mirroring the monitor with video on. It reads the tablet's stylus device over adb and the tool's virtual pen device on the host, both with kernel event timestamps, syncs the two clocks over a persistent adb shell (±0.4 ms), and matches 30 pen taps. Everything after the host input device (compositor, app, monitor) is the same for every tool. To reproduce: `python3 -I tools/latency/pen.py --host-device "Waylus Pen"` (or `"Weylus Stylus"`), then tap.

On Windows, `pen.py --windows-monitor N` listens for the pointer message in a window on the mirrored monitor instead (see [Pen](#pen) under Windows).

### Built-in statistics

Waylus logs its own per-stage timings every 5 s (clocks synced by ping/pong). Since version 0.2.0 these lines (`latency:`, `stages:`, `tablet:`, `wifi:`, `video:`) are printed only with `waylus -v`; the figures below were taken from them. The tablet's stats display shows the same capture → shown figures over 1 s windows, but leave it off for comparisons (see [app 0.2.0](#app-020-and-the-stats-display-niri-2026-11-19)). They cover capture → the decoded frame being handed to the tablet's compositor, so they leave out what the camera sees on top: the tablet's compositing and refresh (~1–2 frames at 60 Hz), minus the real monitor's own delay.

## How Waylus differs from Weylus

[Weylus](https://github.com/H-M-H/Weylus) inspired this project and is more mature and more portable. Waylus is a narrower, latency-focused take: Wayland and Windows, Android only, USB first. The differences that explain the numbers below:

| | Weylus | Waylus |
|---|---|---|
| Tablet client | Any modern web browser | Native Android app |
| Transport | Wi-Fi / LAN, HTTP + WebSocket | USB: raw USB with Android Open Accessory (no USB debugging needed), or an `adb reverse` tunnel. Optional Wi-Fi (`--wifi`): paired and encrypted, video over UDP with FEC |
| Host OS focus | Linux (X11 first, Wayland experimental), Windows, macOS | Linux, Wayland first (xdg-desktop-portal + PipeWire); Windows 10/11 |
| Video encode | FFmpeg; software by default, VAAPI / NVENC / VideoToolbox optional | NVENC or VAAPI (Windows: NVENC, AMF, QSV), zero-copy on the GPU (compositor DMA-BUF → GL blit/scale → CUDA → NVENC, or → GL NV12 shader → VAAPI surface), low-latency tuned: no B-frames, no lookahead, small VBV, keyframes on demand |
| Video decode | In the browser | Android `MediaCodec` hardware decoder straight to a `SurfaceView` |
| Pen input | Browser PointerEvents: pressure and tilt | The tablet's raw stylus evdev device, read over adb: full sensor resolution, pressure, both tilt axes, hover, buttons, eraser. Falls back to unbuffered `MotionEvent`s |
| Touch | Multitouch | Multitouch, with palm rejection while the pen is near |
| Virtual / extra monitor | Yes on X11 (Intel VirtualHeads / xrandr) | `--virtual`: a real extra monitor at the tablet's aspect ratio via the ScreenCast portal (tested on KDE Plasma 6.8 and GNOME 51). niri doesn't implement it; an HDMI/DP dummy plug works there. Windows: through an Indirect Display driver ([windows.md](windows.md#virtual-monitor)) |
| Platforms for the tablet | Anything with a browser (iPad, Android, …) | Android 11+ |
| Licence | AGPL-3.0-or-later | GPL-3.0-or-later |

## Linux

### Waylus vs Weylus (niri, 2026-11-17)

Measured on one machine (RTX 4060, niri, mirroring a 1920×1080 output at 75 Hz) and the tablet, over the same USB 2.0 cable for both tools (Weylus through an `adb reverse` tunnel), and over the same Wi-Fi for both tools. Camera: one clip per tool with the pen idle, same camera position; Waylus at commit 0578fdd (NVENC, AOA transport), Weylus as in ¹; 1080p75 → 60 Hz panel.

| Metric | Weylus ¹ | Waylus |
|---|---|---|
| **Tablet behind the real monitor, photon to photon** | p50 **198 ms** / p95 **278 ms** (p5 139, p99 311; n = 1149) | p50 **45 ms** / p95 **59 ms** (p5 25, p99 64; n = 305) |
| **Same, both over Wi-Fi (5 GHz)** ² | p50 **183 ms** / p95 **248 ms** (p5 123, p99 273; n = 1574) | p50 **49 ms** / p95 **65 ms** (p5 26, p99 69; n = 552) |
| **Pen-down on the tablet → event on the host's input device** ³ | p50 **12.9 ms** / p95 **15.5 ms** (n = 30) | p50 **5.1 ms** / p95 **7.0 ms** (n = 30) |

¹ A local build of [Weylus](https://github.com/H-M-H/Weylus): mainly [PR #560](https://github.com/H-M-H/Weylus/pull/560) (PipeWire capture), with PipeWire adjusted to work on NVIDIA (upstream does not run on this setup). Settings: NVENC, Chrome on the tablet, "Lower Latency" on, frame-rate slider at maximum (120 fps), no stretching. Connected through an `adb reverse` tunnel rather than Wi-Fi, which favours it.

² Measured 2026-11-17, one clip per tool, pen idle, nearly the same camera position. Tablet on 5260 MHz (channel 52, 802.11ac), RSSI −66…−69 dBm, link 351 Mbit/s. The host was also on Wi-Fi via the same access point (5260 MHz, −69 dBm), so every packet crossed the air twice. Waylus at commit c5eed34, `waylus --wifi --no-usb --max-in-flight 3` (now the default; NVENC, 20 Mbit/s, 20 % FEC). With the old default of 2 it measured p50 66 / p95 96 ms (p5 30, p99 104; n = 310; no packet lost during the clip). The camera was hand-held, so the corner points come from two frames; three corner sets gave identical results. Weylus as in ¹, with Chrome connecting to the host's LAN address instead of the tunnel.

³ Method as in [Pen](#pen-penpy), with each tool mirroring DP-2 with video. Waylus reads the stylus device directly (raw pen over adb); Weylus goes through Chrome's pointer events and a WebSocket.

### Where the time goes

Waylus's [built-in statistics](#built-in-statistics), typical values at 1080p with the whole screen changing every frame (barcode page): capture → shown p50 ~21 / p95 ~25 ms with the pen idle, ~17 ms while drawing.
- capture → sent (GPU blit/scale + CUDA copy + NVENC): p50 ~2.5 / p95 ~4 ms
- sent → received (USB): p50 ~2.5 / p95 ~3 ms
- waiting for the decoder: ~0.5 ms
- hardware decode on the MT8781: ~19 ms with the pen idle, **~11.5 ms while drawing**. MediaTek's decoder speed follows the CPU clock, and Android only raises the clocks while there is touch/pen input (the same effect is discussed in [moonlight-android#1241](https://github.com/moonlight-stream/moonlight-android/issues/1241)). `--boost` (on by default) keeps the clocks up for 20 s after the last input, so the faster decode also holds between strokes. With all 8 cores clocked up, decode takes ~9 ms.
- decoded → shown: ~1.5 ms

Decode is by far the largest stage, so the tablet's chip matters most: a Pixel 7a (Tensor G2) decodes in 6–9 ms and reaches capture → shown p50 12–14 ms.

On a laptop with VAAPI (i5-8350U, UHD 620, zero-copy): encode 3.3 ms/frame, capture → sent p50 ~5 ms, capture → shown p50 ~17 ms (pen in use).

**Back-pressure** (`--max-in-flight`, default 3; 2 until 2026-11-17, used for the niri numbers above) keeps a slow decoder from building a queue: the host waits and then encodes the newest frame instead of a backlog. With 2, the encoder was held back about half the time over USB and sent a capture that had already aged; 3 still never queued frames at the decoder (built-in stats, USB, 3 alternating runs each: capture → shown p50 27.4 → 26.8 ms, p95 32–34 → 30.5–31 ms).

**USB accessory mode vs adb:** the USB hop is no faster on average (p50 ~2.5 ms either way), but over 2 × 2 alternating runs AOA had no tail spikes (p95 ≤ 5.6 ms), while adb showed ~18–20 ms p95 windows in two of three runs.

**Earlier fix:** 30–40 ms transfer stalls over adb came from messages written in several pieces, which hit Nagle + delayed ACK in adbd's socket on the tablet; messages are now written in one piece.

### Wi-Fi (Linux host, 2026-11-17)

Built-in statistics, NXTPAPER 14 and host both on Wi-Fi via the same access point. The design and the full development log are in [wifi.md](wifi.md#findings-so-far).

| | USB (AOA) | Wi-Fi, weak 5 GHz link (−72…−79 dBm) |
|---|---|---|
| Pen event → host (injected stylus input) | p50 1.6 / p95 2.5 ms | p50 3.1 / p95 33–72 ms |
| Pen-down → host input device (`pen.py`, 30 real taps, video on, Android pen events: `--no-raw-pen`) ¹ | p50 6.8 / p95 8.8 ms (max 9.3) | p50 9.3 / p95 15.7 ms (max 132) |
| Capture → shown on the tablet (1080p60, 20 Mbit/s) | p50 25 / p95 28 ms | p50 35–42 / p95 51–60 ms |

¹ Measured 2026-11-17 with `pen.py` at commit 841f4e0. Tablet on 5260 MHz, −67 dBm, with the host on the same access point over Wi-Fi. Both runs use the app's pen events (`--no-raw-pen`); with the raw stylus reader, the pen would travel over adb rather than the transport being measured. Over Wi-Fi, one of the 30 taps hit a link stall (132 ms); the other 29 stayed under 16 ms.

Photon to photon over Wi-Fi (camera, ² in the [Weylus table](#waylus-vs-weylus-niri-2026-11-17)): Waylus **p50 49 / p95 65 ms**, Weylus over the same Wi-Fi 183 / 248 ms, Waylus over USB 45 / 59 ms. What remains of the gap to USB is the link: sent → received p50 ~8.3 ms vs 2.7 over USB. A 51 KB frame takes ~4 ms on air at an effective ~100 Mbit/s, because the host's own Wi-Fi shares the channel. A wired host or a lower `--bitrate` should shrink it; not measured yet.

With the old back-pressure default of 2 frames in flight, Wi-Fi measured 66 / 96 ms. The ~6 ms longer pipeline meant two frames no longer covered it: the encoder was held back ~800 ms/s, reached only 53–56 fps and sent captures that had already aged. Built-in stats, 3 alternating runs each, same conditions: capture → shown p50 38.3 → 32.8 ms, p95 49 → 39 ms, capture → sent 7.4 → 2.3 ms, 55 → 60 fps.

### 2.4 GHz

2026-11-17, camera as in ² above with `--track`; tablet on 2462 MHz, 802.11n, −52 dBm, 72–78 Mbit/s; host on 5 GHz at the same access point.

| Photon to photon | p50 | p95 | p99 |
|---|---|---|---|
| 20 Mbit/s (default) | 103 ms (112 with freezes) | 282 ms (**807** with freezes) | 331 ms (1065) |
| `--bitrate 10` | 79 ms (81) | 134 ms (**303**) | 173 ms (461) |

Here the values `camera.py` drops clustered at 0.2–1.2 s: real freezes, 15 % of the frames at 20 Mbit/s and 8 % at 10 Mbit/s, so both readings are given. The tablet's loss log shows the cause: the 2.4 GHz link stalls for 100–450 ms at a time and loses bursts of half a frame, more than the parity can repair; each such loss costs a keyframe, which on 2.4 GHz takes long to send. A longer loss timeout didn't help ([details](wifi.md#findings-so-far)). Use 5 GHz.

### Other Linux tablet-as-monitor tools (KDE, 2026-11-18)

Measured 2026-11-18 on the same desktop, tablet and USB cable as above, in a KDE Plasma 6.8.3 session. UScreen and MoreSpace create their monitor through the EVDI kernel module, which niri can't drive while it renders on an NVIDIA GPU (`NoSupportedRendererFormat`, see [niri#3625](https://github.com/niri-wm/niri/issues/3625)), and Moreland's virtual monitor needs KWin or Hyprland. All of them extend the desktop rather than mirror it, so their extra monitor was made to show DP-2 and filmed against DP-2 exactly as in [Method](#photon-to-photon-camerapy): EVDI monitors were set to mirror it (`kscreen-doctor output.<name>.mirror.DP-2`); Moreland's KWin virtual monitor was placed exactly on top of DP-2 instead, because KWin stops streaming a virtual monitor that mirrors another one. Waylus streamed a KDE virtual monitor mirroring DP-2, the same path as the EVDI tools. Defaults were kept except where noted.

| | Waylus | [Moreland](https://github.com/adiimanav/moreland) 0.3.0 | [UScreen](https://github.com/majmichu1/UScreen) 1.2.15 | [MoreSpace](https://morespaceapp.com/) 0.6.7 |
|---|---|---|---|---|
| **Tablet behind the real monitor, photon to photon** | p50 **50 ms** / p95 **60 ms** (p5 31, p99 70; n = 517) | p50 **56 ms** / p95 **65 ms** (p5 39, p99 73; n = 790) ³ | p50 **65 ms** / p95 **79 ms** (p5 45, p99 82; n = 544) ¹ | p50 **103 ms** / p95 **117 ms** (p5 84, p99 123; n = 999) ² |
| **Pen-down on the tablet → event on the host's input device** | p50 **4.9 ms** / p95 **6.5 ms** (max 6.8; n = 30) | p50 **8.0 ms** / p95 **23.0 ms** (max 46; n = 31) | p50 **8.5 ms** / p95 **11.9 ms** (max 12.3; n = 30) | p50 **8.0 ms** / p95 **63.5 ms** (max 241; n = 31) |
| Stream | 1920×1080, 60 fps, 20 Mbit/s, NVENC H.264 | 1920×1080, 60 fps (its default is 90), 20 Mbit/s, NVENC H.264 via GStreamer | 1920×1080 (set by hand ¹), 60 fps, 20 Mbit/s, NVENC H.264 | 1920×1280 (width set to 1920; it keeps the tablet's 3:2), 60 Hz, ~8.8 Mbit/s (its automatic value), **software** H.264 ² |
| Capture | Screencast portal (PipeWire DMA-BUF) | KWin screencast protocol (PipeWire) | EVDI | EVDI |
| Transport | USB accessory (AOA) | `adb forward` | `adb` port forwarding | USB |
| Licence | GPL-3.0-or-later | Apache-2.0 | MIT | Proprietary; Google sign-in, €17 after a 48 h trial |

Waylus mirroring DP-2 directly (its normal mode, no extra monitor) measured p50 46 / p95 59 ms (p5 22, p99 69; n = 503) in the same session, the same as on niri: the KDE copy into a separate output costs it ~4 ms, and the compositor makes no difference. The pen tails (Moreland, MoreSpace) are real: the clock drift during each run was below 0.05 ms.

¹ UScreen sizes its monitor to the tablet's native resolution by default (2400×1600 here, `auto_resolution`). That measured p50 93 / p95 123 ms (p5 58, p99 134; n = 906): 2400×1600 is above what this tablet's decoder is rated for (15 000 vs 14 400 macroblocks per frame), and decode time grows with resolution (Waylus logs ~14 ms at 1080p and ~23 ms at 2160×1440 on this tablet). The table uses 1920×1080 so that all tools decode about the same amount. The pen was measured at 2400×1600; pen latency doesn't depend on the stream.

² MoreSpace encodes in hardware only through VAAPI, which NVIDIA doesn't provide, so on this machine it falls back to Cisco's OpenH264 software encoder (`morespace codec install-openh264`). It would hardware-encode on an Intel or AMD GPU with VAAPI H.264 CBR support. The test laptop's Kaby Lake iGPU has none (constant-QP only), so that comparison is still open. Its default codec preference is HEVC, which also needs VAAPI. On niri it doesn't start (the EVDI limitation above; it also needs `wlr-randr`).

³ Built from source at commit e333374 with `--features plasma`, run as `moreland --width 1920 --height 1080 --fps 60 --bitrate 20000`. Its own statistics for the run: NVENC encode p50 6.2 ms, tablet decode p50 25.4 ms. Waylus decodes 1080p on this tablet in ~14–19 ms, probably helped by keeping the tablet's clocks up while streaming (`--boost`, see [where the time goes](#where-the-time-goes)).

### Moonlight + Sunshine (Wi-Fi, KDE, 2026-11-18)

[Moonlight](https://moonlight-stream.org/) with [Sunshine](https://github.com/LizardByte/Sunshine) is a game-streaming pair, not a pen display, but it is the reference for low-latency streaming over Wi-Fi and forwards the pen. It only runs over the network, so it was compared with `waylus --wifi`, in the same session as the table above, one after the other: tablet on 5260 MHz (802.11ac, −63…−64 dBm, 351 Mbit/s), host on Wi-Fi via the same access point.

| Wi-Fi 5 GHz | Waylus `--wifi --no-usb` | Moonlight 12.2 + Sunshine 2026.914, H.264 | same, HEVC (Moonlight's default) |
|---|---|---|---|
| **Tablet behind the real monitor, photon to photon** | p50 **49 ms** / p95 **62 ms** (p5 30, p99 70; n = 629) ¹ | p50 **67 ms** / p95 **79 ms** (p5 48, p99 80; n = 721) | p50 **60 ms** / p95 **74 ms** (p5 37, p99 79; n = 595) |
| **Pen-down → host input device** | p50 **9.4 ms** / p95 **15.0 ms** (max 16.2; n = 31; Android pen events, `--no-raw-pen`) | p50 **7.4 ms** / p95 **10.3 ms** (max 12.8; n = 30) | |

Sunshine captured DP-2 through KMS (`output_name = DP-2`) and encoded with NVENC; Moonlight at 1080p, 60 fps, 20 Mbit/s (Sunshine reports ~15 Mbit/s of video after its FEC share), frame pacing "Prefer lowest latency".

¹ 25 of 654 tablet readings (3.8 %) sat at 89–283 ms, clustered around 190 ms, and are left out of the figures as outliers. Re-analysis of the clip shows that 23 of them are **one freeze of about 190 ms** (camera frames 1925–1947): the lag there climbs by 8.3 ms per camera frame, so the tablet held one picture while the monitor moved on. The other two, isolated readings around it, are probably misreads. The tablet logged **no lost frames** during the clip, so it was not a frame lost beyond what the FEC repairs. The cause is not determined: a hitch on the host (capture, encode or send), packets that were delayed but complete, or a hiccup in the tablet's decode or display. The leading guess, unverified, is the host's back-pressure: the host waits for the tablet's acknowledgements, with at most 3 frames in flight, and those may have been delayed on the uplink. With everything included: p50 49 / p95 70 / p99 239 ms. Moonlight's clip had no freeze (its 4 dropped readings out of 725 were misreads, and its p99 with everything included is 81 ms). Open items for Waylus's Wi-Fi mode: the cause of the freeze, and pen latency over Wi-Fi, which is slower than Moonlight's. Since 0.2.0 the host logs any stall over 100 ms as a warning with what held it up, so a repeat would show whether back-pressure is the cause.

### App 0.2.0 and the stats display (niri, 2026-11-19)

Version 0.2.0 replaced the app's plain text screen with a Compose UI (idle screen, pairing, settings) and added the optional stats display. Checked against the previous commit (4c8744b, host and app both built from it) on the NXTPAPER 14 over USB accessory mode, niri, release builds, the mirrored monitor's content changing about 50 times a second (a browser and a music app; not the barcode page), pen idle. Runs alternated old / new / new with the stats display, twice, 70 s each; each 5 s window of the host's `-v` statistics, without the first one (Android's launch boost still on):

| | Capture → shown p50 | p95 | Tablet decode p50 |
|---|---|---|---|
| Before (4c8744b) | 28.4–30.1 ms | 31–36 ms | 20.0–20.8 ms |
| 0.2.0, stats display off | 28.0–29.6 ms | 31–35 ms | 19.7–20.5 ms |
| 0.2.0, stats display on | 24.4–29.2 ms (15–18 in some windows) | 26–34 ms | 18.5–20.6 ms (10.7–12) |

- No change with the display off. With it on, several windows came out *faster*, because decoding was: the display redraws once a second, which apparently keeps the tablet's CPU clocks higher (decode on the MT8781 follows the CPU clock, see [where the time goes](#where-the-time-goes)). That makes it a confounder rather than a cost, so comparison numbers are taken with it off.
- SurfaceFlinger (`dumpsys SurfaceFlinger`, HWC layers) composited the video layer as `DEVICE` (hardware overlay) while streaming, with the stats display both off and on; the idle UI is removed while streaming.
- Start: process start → first video frame when the host starts the app over adb, with the accessory link already up, 3 runs each after a first one: before 513–537 ms, 0.2.0 519–531 ms. Cold, right after installing (including the accessory switch): ~3.0 / 3.3 s, one run each. Cold start to the idle screen when opened from the launcher (`am start -W`, 6 runs): before ~260 ms, 0.2.0 ~407 ms (~375 ms fully AOT-compiled); the difference is Compose's first composition.
- APK: 4.1 MB release with R8 (before: 7.8 MB without R8).
- Pen → host was not re-measured: it needs someone drawing. The input path is unchanged (same `InputCapture`, unbuffered dispatch on the root view; the only addition is that a finger gesture *starting* on the black bar beside the picture toggles the stats display and isn't forwarded).
- The new stall warning showed up during these runs once, at the start of a session: 106 ms, encoder 26 ms and 80 ms unattributed. In a separate Wi-Fi test (5 GHz, −64 dBm) it caught one 195 ms stall that was all back-pressure (3/3 frames in flight, waiting for the tablet's acknowledgements), the pattern guessed for the freeze in the [Moonlight comparison](#moonlight--sunshine-wi-fi-kde-2026-11-18) above; that test also took a tablet screenshot at about that time, which may have caused it, so it is a lead, not a finding.

## Windows

Setup: the same desktop as the Linux numbers, booted into Windows 11 (RTX 4060, driver 32.0.16.1088, NVENC), mirroring a 1920×1080 60 Hz monitor with `tools/latency/barcode.html` full screen in a browser (so the picture changes every frame), the TCL NXTPAPER 14 (60 Hz panel) over USB, 20 Mbit/s, pen idle. These are separate from the Linux tables; the monitor here runs at 60 Hz, not 75.

### Built-in statistics

| Built-in statistics, `--transport adb`, 2026-11-18 (5 s windows, 4 in a row) | Windows |
|---|---|
| Capture → shown on the tablet | p50 32.3–32.8 / p95 37–39 ms (n ≈ 300 per window) |
| Capture → sent (capture, GPU convert, NVENC, write) | p50 3.3 / p95 3.5 ms |
| NVENC encode | 2.6 ms per frame |
| Tablet: wait for decoder / decode / decoded → shown | p50 1.1 / 21.7 / 2.9 ms |
| Frame rate / bitrate | 60.0 fps, 20.0 Mbit/s |

The decode time is the MT8781's idle-clock decode described under [where the time goes](#where-the-time-goes) (19–22 ms while nothing touches the screen; ~11 ms while drawing). The host's part is about the same as on Linux: 3.3 ms from capture to the first byte on the wire.

### Photon to photon

**USB (`--transport adb`), 2026-11-18:** `camera.py` as in [Method](#photon-to-photon-camerapy) (Pixel 7a, 120 fps, hand-held, `--track 100`, one clip of ~20 s, pen idle, 1080p60 monitor to the 60 Hz panel): p50 **46 ms** / p95 **65 ms** (p5 32, p99 68; n = 194 decoded frames). The Linux reference over USB is p50 45 / p95 59 ms. One clip only, and the camera moved a lot (tracking shift up to 360 px), so the tail is less certain than the median.

**USB accessory mode (the default), 2026-11-18:** same method and setup, release build, tablet switched from a fresh plug-in with Samsung's USB driver installed (no Zadig step): fitted p50 **39 ms** / p95 **59 ms** (p5 23, p99 67; n = 209, no outliers; camera shift up to 400 px). The host's built-in statistics during the clip: capture → shown p50 28.0 / p95 29.8 ms, capture → sent 3.9 ms, sent → received p50 2.4 / p95 2.9 ms (adb: capture → shown p50 32–33 ms). One clip each, so the 7 ms gap to adb is indicative, not settled; on Linux the two transports differed in the tail rather than the median.

**Wi-Fi 5 GHz, 2026-11-18:** same method, `waylus --wifi --no-usb` (release build, 20 Mbit/s, 20 % FEC); tablet on 5260 MHz, −62 dBm, 351 Mbit/s (`adb shell cmd wifi status` before the clip); the host is on 5 GHz via the same access point. Fitted p50 **48 ms** / p95 **64 ms** (p5 29, p99 73; n = 239; 6 readings of 86–124 ms dropped as outliers, with them p95 69 / p99 108). The Linux reference over Wi-Fi is p50 49 / p95 62–65 ms. A first clip taken with an unoptimized (debug) host build, where sealing and FEC took 12–21 ms per frame, read p50 76 / p95 94 ms and is not used.

### Pen

2026-11-18, release build: `tools/latency/pen.py --windows-monitor 2`, 30 real taps with the T-Pen each, Android pen events (`--no-raw-pen`), video on, pen-down on the tablet → the pointer message in a window on the mirrored monitor:

| | p50 | p95 | min / max |
|---|---|---|---|
| USB (`--transport adb`) | 8.7 ms | 11.4 ms | 3.9 / 11.8 ms (n = 31) |
| USB accessory mode (default) | 8.4 ms | 14.8 ms | 3.7 / 22.7 ms (n = 31) |
| Wi-Fi 5 GHz (−63 dBm) | 9.5 ms | 15.6 ms | 4.5 / 37.7 ms (n = 31) |

Not the same measurement point as the Linux figures (p50 6.8 / p95 8.8 ms over USB and 9.3 / 15.7 ms over Wi-Fi with the same pen events, from the kernel timestamp on the virtual input device): on Windows the time includes delivery of the pointer message to the window, so these are upper bounds. A first USB run on a debug build read p50 8.4 / p95 10.9 ms.

### Wi-Fi

2026-11-18, built-in statistics as above, `waylus --wifi --no-usb`, release build, 4 windows of 5 s, host and tablet on the same access point at 5260 MHz, tablet −71…−72 dBm, 263 Mbit/s link: capture → shown p50 35.3–36.7 / p95 40.8–49 ms; capture → sent p50 5.0–5.2 ms (send 1.0 ms per frame, seal + FEC 0.13 ms); sent → received p50 7.4–8.0 / p95 11.6–13.9 ms; 0 of 10 800 shards lost per window, no FEC repair, no lost frames, 60 fps. mDNS discovery (the tablet listed the host), PIN pairing from a fresh key store and qWAVE flow marking (accepted without administrator rights; its effect on the air was not measured) all worked.

### Weylus and Moonlight on Windows (2026-11-18)

Same desktop, monitor, tablet and `camera.py` method as above, 2026-11-18, the barcode page on `\\.\DISPLAY2` (1920×1080, 60 Hz). Photon to photon, tablet behind the real monitor:

| | p50 | p95 | p5 / p99 | n |
|---|---|---|---|---|
| Waylus, USB accessory mode | **39 ms** | **59 ms** | 23 / 67 | 209 |
| Waylus, USB `--transport adb` | **46 ms** | **65 ms** | 32 / 68 | 194 |
| Weylus 0.11.4, USB (`adb reverse`), min. frame interval 1 ms | **393 ms** | **471 ms** | 293 / 512 | 258 |
| Weylus 0.11.4, same, min. frame interval 8 ms | **401 ms** | **494 ms** | 317 / 527 | 1651 |
| Waylus `--wifi --no-usb`, 5 GHz (earlier session) | **48 ms** | **64 ms** | 29 / 73 | 239 ¹ |
| Waylus `--wifi --no-usb`, 5 GHz (same session as Moonlight) | **55 ms** | **78 ms** | 34 / 93 | 438 ¹ |
| Moonlight 12.2 + Sunshine 2026.914, H.264, 5 GHz | **47 ms** | **71 ms** | 30 / 73 | 301 |
| Moonlight 12.2 + Sunshine 2026.914, HEVC, 5 GHz | **53 ms** | **61 ms** | 37 / 68 | 524 |

- **Weylus** is the official Windows release 0.11.4 (the Linux comparison used a newer local build). It captures only the main display, so monitor 2 was made the main display for its clips. Settings: NVENC (`--try-nvenc`), Chrome on the tablet, "Lower Latency" on, maximum resolution. 0.11.4 has no frame-rate slider; its "minimum frame interval" was set to 1 ms, then to 8 ms (≈ 120 fps, as on Linux), with the same result. About 400 ms is twice what Weylus reached on Linux (198 ms) and ten times Waylus; where Windows adds the time (capture, the browser's decoder queue) was not investigated.
- **Moonlight**: Sunshine 2026.914, the same version as on Linux, portable build, `output_name = \\.\DISPLAY2`, `capture = ddx` (Desktop Duplication), NVENC; Moonlight at 1080p, 60 fps, 20 Mbit/s, frame pacing "Prefer lowest latency". The tablet was on 5260 MHz at −60…−63 dBm, 351–390 Mbit/s. On Windows Moonlight is 13–20 ms faster than with the same versions on Linux (H.264 67 → 47 ms, HEVC 60 → 53 ms at p50), so Sunshine's Windows capture path is the quicker one.
- **Waylus over Wi-Fi**: the clip filmed right after Moonlight's caught a Wi-Fi stall (the host's statistics for that 5 s window: sent → received p95 51 ms instead of 10–13); outside it the built-in figures matched the earlier session (capture → shown p50 34–37 ms). On Windows Waylus over Wi-Fi is about level with Moonlight at the median (48–55 vs 47–53 ms) and has the longer tail. Its slow readings on Linux (¹ under [Moonlight](#moonlight--sunshine-wi-fi-kde-2026-11-18)) are an open item too.
- The pen with Moonlight was not measured on Windows.

¹ Outliers dropped by `camera.py` as for the Linux clips: 6 readings of 86–124 ms in the earlier clip, 25 of 105–293 ms in the later one (with them: p95 110 / p99 260 ms).

Not measured yet: USB taking over from Wi-Fi.
