# Waylus

> [!NOTE]
> **LLM-generated code.** Most of this project, including code and documentation, was written by an AI model (Claude, by Anthropic) working with a human maintainer. It has been tested on real hardware but not audited line by line, so review it before relying on it.

*Weylus, but for Way(land).* Use an Android tablet as a **low-latency pen display** for your Linux desktop.
It's Wayland-first, uses a native Android client and runs over USB.

Waylus mirrors one of your monitors to the tablet using hardware H.264 encode and decode. It sends the tablet's pen and fingers back as real Linux input devices:
- a tablet tool with pressure, tilt and both stylus buttons;
- a multitouch screen.

Apps such as Krita, GIMP, Xournal++ and Inkscape see a regular graphics tablet through libinput and the Wayland tablet protocol.

```
 host (Rust)                                                   tablet (Kotlin)
 ScreenCast portal → PipeWire → NVENC H.264 ─┐   adb reverse   ┌→ MediaCodec (HW) → SurfaceView
                                             ├── USB / TCP ────┤
 uinput pen + touchscreen  ← input events  ←─┘                 └← MotionEvent (unbuffered)
```

## Waylus vs Weylus

[Weylus](https://github.com/H-M-H/Weylus) inspired this project and is more mature and more portable. Waylus is a narrower, latency-focused take: Linux and Wayland first, Android only, USB only.

| | Weylus | Waylus |
|---|---|---|
| Tablet client | Any modern web browser | Native Android app |
| Transport | Wi-Fi / LAN, HTTP + WebSocket | USB: raw USB with Android Open Accessory (no USB debugging needed), or an `adb reverse` tunnel |
| Host OS focus | Linux (X11 first, Wayland experimental), Windows, macOS | Linux, Wayland first (xdg-desktop-portal + PipeWire) |
| Video encode | FFmpeg; software by default, VAAPI / NVENC / VideoToolbox optional | NVENC, zero-copy on the GPU (compositor DMA-BUF → GL blit/scale → CUDA → NVENC), low-latency tuned: no B-frames, no lookahead, small VBV, keyframes on demand; VAAPI planned |
| Video decode | In the browser | Android `MediaCodec` hardware decoder straight to a `SurfaceView` |
| Pen input | Browser PointerEvents: pressure and tilt | The tablet's raw stylus evdev device, read over adb: full sensor resolution, pressure, both tilt axes, hover, buttons, eraser. Falls back to unbuffered `MotionEvent`s |
| Touch | Multitouch | Multitouch, with palm rejection while the pen is near |
| Virtual / extra monitor | Yes on X11 (Intel VirtualHeads / xrandr) | `--virtual`: a real extra monitor at the tablet's aspect ratio via the ScreenCast portal (tested on KDE Plasma 6.8 and GNOME 51). niri doesn't implement it; an HDMI/DP dummy plug works there |
| Platforms for the tablet | Anything with a browser (iPad, Android, …) | Android 11+ |
| Licence | AGPL-3.0-or-later | GPL-3.0-or-later |

### Latency

Measured on one machine (RTX 4060, niri, mirroring a 1920×1080 output at 75 Hz) and one tablet (TCL NXTPAPER 14, MT8781, 60 Hz), over the same USB 2.0 cable for both tools. Weylus ran over an `adb reverse` tunnel rather than Wi-Fi, which favours it.

| Metric | Weylus ¹ | Waylus |
|---|---|---|
| Host page frame → presented on tablet ² | p50 **166 ms** / p95 **229 ms** | p50 **74–84 ms** / p95 **106–116 ms** (pen idle); p50 72 / p95 83 ms right after pen input (`--boost`) |
| Capture → frame shown (built-in stats) ³ | – | p50 ~17 ms (while drawing) / ~21 ms (pen idle), full-screen motion |
| Pen event → host input device ³ | – | p50 **2 ms** / p95 3 ms |

¹ A local build of [Weylus](https://github.com/H-M-H/Weylus) with niri/NVIDIA fixes (upstream does not run on this setup). Settings: NVENC, Chrome on the tablet, "Lower Latency" on, frame-rate slider at maximum, no stretching.

² Measured with [`tools/latency`](tools/latency).
- A page on the mirrored monitor shows the host clock as a barcode, once per frame.
- An overlay on the tablet shows when Android expects to present each frame, synced to the host clock.
- Tablet screenshots contain both barcodes, and the difference between them is the latency.
- The number includes Chrome on the host rendering the page and niri compositing it (roughly 40–60 ms, the same for both tools). It is therefore higher than what you perceive relative to the real monitor.
- 40–60 samples per run. Each tool was measured with the other one stopped.
- To reproduce: open `tools/latency/barcode.html` fullscreen on the mirrored monitor, start the mirroring tool, then run `python3 -I tools/latency/probe.py --label <name>` (needs Pillow and the Waylus APK installed).

³ Waylus logs these every 5 s, split into stages. It syncs the clocks with ping/pong. "Shown" is the decoder's render callback, so it excludes scan-out. Typical breakdown at 1080p with the whole screen changing every frame (barcode page), capture→shown p50 ~21 / p95 ~25 ms:
- capture → sent (GPU blit/scale + CUDA copy + NVENC): p50 ~2.5 / p95 ~4 ms
- sent → received (USB via adb): p50 ~2 / p95 ~3.5 ms
- waiting for the decoder: ~0.5 ms
- hardware decode on the MT8781: ~19 ms with the pen idle, **~11.5 ms while drawing**. MediaTek's decoder speed follows the CPU clock, and Android only raises the clocks while there is touch/pen input (the same effect is discussed in [moonlight-android#1241](https://github.com/moonlight-stream/moonlight-android/issues/1241)). With the pen in use, capture→shown is p50 ~17 ms. `--boost` (on by default) keeps the clocks up for 20 s after the last input, so this also holds between strokes
- decoded → shown: ~1.5 ms

Back-pressure (`--max-in-flight`, default 2) keeps a slow decoder from building a queue: the host waits and then encodes the newest frame instead of a backlog.

The Waylus probe numbers were re-measured on 2026-11-16 (commit 994ec89: zero-copy, single-write transfer fix, back-pressure, clock boost); the Weylus numbers are from the earlier session with the same setup and method. The probe takes `adb screencap`s over the same USB link while it runs, which loads the tablet and inflates both tools' numbers somewhat.

## Requirements

**Host**
- Linux with a Wayland compositor that implements the ScreenCast portal. Tested on niri (xdg-desktop-portal-gnome), KDE Plasma 6.8 and GNOME 51. Hyprland should work.
- PipeWire, FFmpeg ≥ 9 with `h264_nvenc` (an NVIDIA GPU). `adb` is optional: it installs and starts the app and enables raw pen input.
- Write access to `/dev/uinput`, for example:
  ```sh
  sudo groupadd -r uinput; sudo usermod -aG uinput $USER
  echo 'KERNEL=="uinput", MODE="0660", GROUP="uinput", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/60-waylus-uinput.rules
  ```
- Build dependencies (Fedora): `pipewire-devel ffmpeg-devel clang-devel`, a Rust toolchain, and the Android SDK for the client.

**Tablet**
- Android 11 or newer. USB debugging is optional: without it, install the APK once by other means; the app opens when the host connects.

## Build

```sh
cargo build --release                       # host: target/release/waylus
cd android && ./gradlew assembleDebug       # client: android/app/build/outputs/apk/debug/app-debug.apk
```

## Usage

```sh
waylus                      # mirror a monitor (the portal asks once which one), pen + touch + video
waylus --no-video -o DP-2   # input only: use the tablet as a plain graphics tablet for DP-2
```

`waylus` does the setup for you:
- installs or updates the APK when it has changed;
- switches the tablet into USB accessory mode (or sets up `adb reverse`);
- launches the app on the tablet;
- serves reconnects until you press Ctrl-C.

The monitor you pick is remembered via a portal restore token stored in `~/.local/state/waylus/`.

The pen and touchscreen must be mapped to the output shown on the tablet. **On KDE Plasma and GNOME Waylus does this automatically**, for both mirrored and virtual monitors: through KWin's input-device D-Bus settings on KDE, and through the per-device GSettings that Settings → Wacom Tablet uses on GNOME (keyed by the monitor's EDID). On GNOME this matters beyond convenience: an unmapped direct touchscreen crashed gnome-shell 51 when a touch started a window move. Elsewhere, map them in your compositor's settings. For niri, Waylus prints this snippet on startup:

```kdl
input {
    tablet { map-to-output "DP-2"; }
    touch  { map-to-output "DP-2"; }
}
```

Useful flags:
- `--choose`: show the monitor picker again (switch the mirrored monitor)
- `--bitrate <Mbit/s>` (default: 20 at 1080p, scaled with resolution)
- `--fps` (default: the tablet's refresh rate)
- `--encoder h264_nvenc`
- `--serial <adb serial>`
- `--virtual[=WxH]`: create a virtual monitor instead of mirroring (GNOME / KDE). The size defaults to the tablet's screen within its decoder limits
- `--no-raw-pen`: use Android's pen events instead of reading the stylus device over adb
- `--boost <s>`: keep the tablet's CPU clocks up for this long after pen/touch input (default 20, 0 = off). Decoding on MediaTek tablets is much faster at high clocks, which Android only grants while you touch the screen. Costs some tablet battery while active
- `--max-in-flight <n>`: back-pressure, max frames sent but not yet shown (default 2, 0 = off)
- `--vendor-tweaks mtk-cpu-boost,mtk-vpp-off`: **opt-in, device-specific** MediaTek decoder settings. `mtk-cpu-boost` saved ~1.9 ms of decode on the MT8781; `mtk-vpp-off` made no measurable difference. Never used for the published comparison numbers
- `--transport adb`: use the `adb reverse` tunnel instead of USB accessory mode (see below)
- `--keep-app`
- `--no-adb`

### USB accessory mode (default) and `--transport adb`

By default the host switches the tablet into Android Open Accessory (AOA) mode and exchanges data over raw USB bulk transfers, skipping adbd and both TCP stacks. With `--transport adb`, or automatically when the tablet doesn't support AOA, it uses an `adb reverse` TCP tunnel instead:
- **No USB debugging needed.** Without adb, the app must already be installed, and Android offers to open it when the host connects; tick "always" in that dialog. If adb is available it is still used to install and start the app and to read the raw stylus device.
- The first time, Android asks for permission to access the accessory.
- The tablet stays in accessory mode (USB ID 18d1:2d00/2d01) until it is unplugged, so MTP file transfer is unavailable meanwhile; adb keeps working.
- The USB hop is no faster on average (p50 ~2.5 ms either way, measured on the NXTPAPER 14), but over 2 × 2 alternating runs AOA had no tail spikes (p95 ≤ 5.6 ms), while adb showed ~18–20 ms p95 windows in two of three runs.
- The link outlives the host and app processes. Each session therefore starts with a SYNC handshake, and a restarted host is picked up by a running app within ~5 s.
- The host needs access to the USB device. With USB debugging on, systemd's `70-uaccess.rules` already grants it (the accessory still exposes adb). Without adb, the tablet comes up as 18d1:2d00, which may need a rule like this in `/etc/udev/rules.d/70-waylus.rules`:
  ```
  SUBSYSTEM=="usb", ATTR{idVendor}=="18d1", ATTR{idProduct}=="2d00|2d01", TAG+="uaccess"
  ```

Monitors larger than the tablet can show or decode (e.g. 4K) are scaled down on the GPU automatically. Set `WAYLUS_NO_ZEROCOPY=1` to force the CPU readback path.

Set `RUST_LOG=waylus=debug` for verbose logs.

## Status / roadmap

- [x] Pen (pressure, tilt, hover, buttons, eraser) and multitouch over USB
- [x] Portal / PipeWire capture → NVENC → MediaCodec video
- [x] Built-in latency statistics
- [x] Reproducible latency comparison against Weylus (`tools/latency`)
- [x] Per-stage latency stats, back-pressure, and a fix for 30–40 ms transfer stalls (messages are now written in one piece; split writes hit Nagle + delayed ACK in adbd's socket on the tablet)
- [ ] Hardware decode is the largest stage: on the MT8781 it is CPU-clock-bound (19 ms idle, 11.5 ms while drawing thanks to Android's input boost, 9 ms with all cores clocked up). `--boost` keeps clocks up for 20 s after input
- [x] Raw stylus input over adb (full tilt on devices whose Android config drops it, e.g. NXTPAPER 14)
- [ ] VAAPI encoder fallback (AMD / Intel)
- [x] Zero-copy GPU path (DMA-BUF → GL → CUDA → NVENC) and GPU downscaling to fit the tablet
- [x] AOA transport (default; no adb needed)
- [x] Virtual monitor on KDE Plasma and GNOME (`--virtual`), with automatic pen/touch mapping
- [ ] Virtual monitor on niri (needs `RecordVirtual` in niri; use a dummy plug meanwhile)

## Licence

Waylus is free software, licensed under the **GNU General Public License v3.0 or later**; see [LICENSE](LICENSE).
It is an independent implementation and contains no code from Weylus.
