# Waylus

> [!NOTE]
> **LLM-generated code.** Most of this project, including code and documentation, was written by an AI model (Claude, by Anthropic) working with a human maintainer. It has been tested on real hardware but not audited line by line, so review it before relying on it.

*Weylus, but for Wayland.* Use an Android tablet as a **low-latency pen display** for your Linux desktop.
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

Measured on the same machine (RTX 4060, niri, 1920×1080 output) and the same tablet (TCL NXTPAPER 14, MT8781, USB 2.0).

| Metric | Weylus (Wi-Fi) | Waylus (USB) |
|---|---|---|
| Pen event → host input device | TBD | **p50 2.2 ms / p95 3.0 ms** ¹ |
| Screen capture → frame shown on tablet | TBD | **p50 24 ms / p95 27–34 ms** ¹ |
| Glass-to-glass (240 fps camera) | TBD | TBD |

¹ Built-in measurement. Waylus syncs the two clocks with ping/pong and logs these numbers every 5 seconds. "Shown" is when Android reports the frame as rendered (`OnFrameRenderedListener`, or the callback time on MediaTek, whose codec reports a bogus timestamp), so it does not include the panel's scan-out time. Breakdown at 1080p: DMA-BUF import plus readback about 1.6 ms, NVENC including the copy about 6 ms, USB about 2 ms, decode plus present about 13 ms.

Cells stay "TBD" until they are measured. No numbers are estimated.

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
- `--bitrate <Mbit/s>` (default 20)
- `--fps` (default 60)
- `--encoder h264_nvenc`
- `--serial <adb serial>`
- `--keep-app`
- `--no-adb`

Set `RUST_LOG=waylus=debug` for verbose logs.

## Status / roadmap

- [x] Pen (pressure, tilt, hover, buttons, eraser) and multitouch over USB
- [x] Portal / PipeWire capture → NVENC → MediaCodec video
- [x] Built-in latency statistics
- [ ] Latency tuning to below 50 ms glass-to-glass, then measure against Weylus
- [ ] Full tilt direction on devices whose Android input config drops `AXIS_ORIENTATION` (e.g. NXTPAPER 14), via a raw evdev helper running as the adb shell user
- [ ] VAAPI encoder fallback (AMD / Intel)
- [ ] DMA-BUF zero-copy capture (PipeWire dmabuf → GPU encoder)
- [ ] AOA transport (no adb needed)
- [ ] Virtual monitor where the portal supports it (GNOME / KDE)

## Licence

Waylus is free software, licensed under the **GNU General Public License v3.0 or later**; see [LICENSE](LICENSE).
It is an independent implementation and contains no code from Weylus.
