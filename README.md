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
| Transport | Wi-Fi / LAN, HTTP + WebSocket | USB (`adb reverse` tunnel); AOA planned |
| Host OS focus | Linux (X11 first, Wayland experimental), Windows, macOS | Linux, Wayland first (xdg-desktop-portal + PipeWire) |
| Video encode | FFmpeg; software by default, VAAPI / NVENC / VideoToolbox optional | FFmpeg NVENC by default (VAAPI planned), low-latency tuned: no B-frames, no lookahead, 1-frame VBV, keyframes on demand |
| Video decode | In the browser | Android `MediaCodec` hardware decoder straight to a `SurfaceView` |
| Pen input | Browser PointerEvents: pressure and tilt | `MotionEvent` with unbuffered dispatch and every batched sample forwarded: pressure (4096 levels on the T-Pen), tilt, hover, both side buttons, eraser |
| Touch | Multitouch | Multitouch, with palm rejection while the pen is near |
| Virtual / extra monitor | Yes on X11 (Intel VirtualHeads / xrandr) | Not yet (mirrors an existing output) |
| Platforms for the tablet | Anything with a browser (iPad, Android, …) | Android 11+ |
| Licence | AGPL-3.0-or-later | GPL-3.0-or-later |

### Latency

Measured on one machine (RTX 4060, niri, mirroring a 1920×1080 output at 75 Hz) and one tablet (TCL NXTPAPER 14, MT8781, 60 Hz), over the same USB 2.0 cable for both tools. Weylus ran over an `adb reverse` tunnel rather than Wi-Fi, which favours it.

| Metric | Weylus ¹ | Waylus |
|---|---|---|
| Host page frame → presented on tablet ² | p50 **166 ms** / p95 **229 ms** | p50 **94–106 ms** / p95 **108–118 ms** |
| Capture → frame shown (built-in stats) ³ | – | p50 24 ms (desktop) to 40 ms (full-screen motion) |
| Pen event → host input device ³ | – | p50 **2 ms** / p95 3 ms |

¹ A local build of [Weylus](https://github.com/H-M-H/Weylus) with niri/NVIDIA fixes (upstream does not run on this setup). Settings: NVENC, Chrome on the tablet, "Lower Latency" on, frame-rate slider at maximum, no stretching.

² Measured with [`tools/latency`](tools/latency).
- A page on the mirrored monitor shows the host clock as a barcode, once per frame.
- An overlay on the tablet shows when Android expects to present each frame, synced to the host clock.
- Tablet screenshots contain both barcodes, and the difference between them is the latency.
- The number includes Chrome on the host rendering the page and niri compositing it (roughly 40–60 ms, the same for both tools). It is therefore higher than what you perceive relative to the real monitor.
- 40–60 samples per run. Each tool was measured with the other one stopped.
- To reproduce: open `tools/latency/barcode.html` fullscreen on the mirrored monitor, start the mirroring tool, then run `python3 -I tools/latency/probe.py --label <name>` (needs Pillow and the Waylus APK installed).

³ Waylus logs these every 5 s. It syncs the clocks with ping/pong. "Shown" is the decoder's render callback, so it excludes scan-out. Breakdown at 1080p:
- DMA-BUF import + readback: ~1.6 ms
- NVENC incl. copy: ~6 ms
- USB: ~2 ms
- Decode + present: 12–25 ms (higher under full-screen motion)

## Requirements

**Host**
- Linux with a Wayland compositor that implements the ScreenCast portal. Tested on niri with xdg-desktop-portal-gnome. GNOME, KDE and Hyprland should work.
- PipeWire, FFmpeg ≥ 9 with `h264_nvenc` (an NVIDIA GPU), and `adb`.
- Write access to `/dev/uinput`, for example:
  ```sh
  sudo groupadd -r uinput; sudo usermod -aG uinput $USER
  echo 'KERNEL=="uinput", MODE="0660", GROUP="uinput", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/60-waylus-uinput.rules
  ```
- Build dependencies (Fedora): `pipewire-devel ffmpeg-devel clang-devel`, a Rust toolchain, and the Android SDK for the client.

**Tablet**
- Android 11 or newer with USB debugging enabled and authorised for this computer.

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
- sets up `adb reverse`;
- launches the app on the tablet;
- serves reconnects until you press Ctrl-C.

The monitor you pick is remembered via a portal restore token stored in `~/.local/state/waylus/`.

Map the pen and touchscreen to the mirrored output in your compositor. For niri, Waylus prints this snippet on startup:

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
- `--keep-app`
- `--no-adb`

Set `RUST_LOG=waylus=debug` for verbose logs.

## Status / roadmap

- [x] Pen (pressure, tilt, hover, buttons, eraser) and multitouch over USB
- [x] Portal / PipeWire capture → NVENC → MediaCodec video
- [x] Built-in latency statistics
- [x] Reproducible latency comparison against Weylus (`tools/latency`)
- [ ] Faster decode under full-screen motion (received → shown grows from ~12 to ~25 ms)
- [ ] Full tilt direction on devices whose Android input config drops `AXIS_ORIENTATION` (e.g. NXTPAPER 14), via a raw evdev helper running as the adb shell user
- [ ] VAAPI encoder fallback (AMD / Intel)
- [ ] DMA-BUF zero-copy capture (PipeWire dmabuf → GPU encoder)
- [ ] AOA transport (no adb needed)
- [ ] Virtual monitor where the portal supports it (GNOME / KDE)

## Licence

Waylus is free software, licensed under the **GNU General Public License v3.0 or later**; see [LICENSE](LICENSE).
It is an independent implementation and contains no code from Weylus.
