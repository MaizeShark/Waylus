# Waylus

> [!NOTE]
> **LLM-generated code.** Most of this project, including code and documentation, was written by an AI model (Claude, by Anthropic) working with a human maintainer. It has been tested on real hardware but not audited line by line, so review it before relying on it.

*Weylus, but for Way(land).* Use an Android tablet as a **low-latency pen display** for your Linux desktop.
It's Wayland-first, uses a native Android client and runs over USB or, optionally, Wi-Fi.

Waylus mirrors one of your monitors to the tablet using hardware H.264 encode and decode. It sends the tablet's pen and fingers back as real Linux input devices:
- a tablet tool with pressure, tilt and both stylus buttons;
- a multitouch screen.

Apps such as Krita, GIMP, Xournal++ and Inkscape see a regular graphics tablet through libinput and the Wayland tablet protocol.

```
 host (Rust)                                                        tablet (Kotlin)
 ScreenCast portal → PipeWire → NVENC / VAAPI H.264 ─┐  USB (AOA,   ┌→ MediaCodec (HW) → SurfaceView
                                                     ├─ or adb;  ───┤
                                                     │  or Wi-Fi)   │
 uinput pen + touchscreen  ← input events  ←─────────┘              └← MotionEvent (unbuffered)
```

## Waylus vs Weylus

[Weylus](https://github.com/H-M-H/Weylus) inspired this project and is more mature and more portable. Waylus is a narrower, latency-focused take: Linux and Wayland first, Android only, USB first.

| | Weylus | Waylus |
|---|---|---|
| Tablet client | Any modern web browser | Native Android app |
| Transport | Wi-Fi / LAN, HTTP + WebSocket | USB: raw USB with Android Open Accessory (no USB debugging needed), or an `adb reverse` tunnel. Optional Wi-Fi (`--wifi`): paired and encrypted, video over UDP with FEC |
| Host OS focus | Linux (X11 first, Wayland experimental), Windows, macOS | Linux, Wayland first (xdg-desktop-portal + PipeWire) |
| Video encode | FFmpeg; software by default, VAAPI / NVENC / VideoToolbox optional | NVENC or VAAPI, zero-copy on the GPU (compositor DMA-BUF → GL blit/scale → CUDA → NVENC, or → GL NV12 shader → VAAPI surface), low-latency tuned: no B-frames, no lookahead, small VBV, keyframes on demand |
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
| **Tablet behind the real monitor, photon to photon** ² | p50 **198 ms** / p95 **278 ms** (p5 139, p99 311; n = 1149) | p50 **45 ms** / p95 **59 ms** (p5 25, p99 64; n = 305) |
| **Pen-down on the tablet → event on the host's input device** ³ | p50 **12.9 ms** / p95 **15.5 ms** (n = 30) | p50 **5.1 ms** / p95 **7.0 ms** (n = 30) |

¹ A local build of [Weylus](https://github.com/H-M-H/Weylus): mainly [PR #560](https://github.com/H-M-H/Weylus/pull/560) (PipeWire capture), with PipeWire adjusted to work on NVIDIA (upstream does not run on this setup). Settings: NVENC, Chrome on the tablet, "Lower Latency" on, frame-rate slider at maximum (120 fps), no stretching. Connected through an `adb reverse` tunnel rather than Wi-Fi, which favours it.

² Filmed with a phone camera (Pixel 7a slow motion, 120 fps) showing [`tools/latency/barcode.html`](tools/latency) on the monitor and its mirror on the tablet in the same shot. `tools/latency/camera.py` decodes both timestamps in every frame; their difference is the lag you actually see between the two screens, including the tablet's own compositing and display panel, with no clock sync involved. Measured 2026-11-17, one clip per tool with the pen idle, same camera position: Waylus at commit 0578fdd (NVENC, AOA transport), Weylus as in ¹; 1080p75 → 60 Hz panel.
- To reproduce: open `barcode.html` fullscreen on the mirrored monitor, film both screens in slow motion, run `python3 -I tools/latency/camera.py VIDEO --dump 100 --out frame.png` to read off the four corners of each barcode, then `camera.py VIDEO --monitor x,y x,y x,y x,y --tablet x,y x,y x,y x,y`.
- The rolling shutter often catches the monitor mid-refresh, so the monitor's timestamps are fitted as a line over the camera frames and every readable tablet frame counts.
- An earlier method, `tools/latency/probe.py`, compared tablet screenshots against an overlay showing Android's expected present time. Its bias differs per tool: it read ~34 ms above the camera for Waylus (it includes the host's own render and compositing) but 21 ms *below* it for Weylus (177 vs 198 ms p50, re-measured the same day), so a screenshot apparently shows a browser client's frames before they reach the glass. It is kept for quick checks of Waylus alone, not for comparisons.

³ Measured with `tools/latency/pen.py` while each tool was mirroring DP-2 with video: it reads the tablet's stylus device over adb and the tool's virtual pen device on the host, both with kernel event timestamps, syncs the two clocks over a persistent adb shell (±0.4 ms), and matches 30 pen taps. Everything after the host input device (compositor, app, monitor) is the same for both tools. Waylus reads the stylus device directly (raw pen over adb); Weylus goes through Chrome's pointer events and a WebSocket. To reproduce: `python3 -I tools/latency/pen.py --host-device "Waylus Pen"` (or `"Weylus Stylus"`), then tap.

#### Where Waylus's time goes

Waylus logs its own per-stage timings every 5 s (clocks synced by ping/pong). They cover capture → the decoded frame being handed to the tablet's compositor, so they leave out what the camera sees on top: the tablet's compositing and refresh (~1–2 frames at 60 Hz), minus the real monitor's own delay. Typical values at 1080p with the whole screen changing every frame (barcode page), capture → shown p50 ~21 / p95 ~25 ms with the pen idle, ~17 ms while drawing:
- capture → sent (GPU blit/scale + CUDA copy + NVENC): p50 ~2.5 / p95 ~4 ms
- sent → received (USB): p50 ~2.5 / p95 ~3 ms
- waiting for the decoder: ~0.5 ms
- hardware decode on the MT8781: ~19 ms with the pen idle, **~11.5 ms while drawing**. MediaTek's decoder speed follows the CPU clock, and Android only raises the clocks while there is touch/pen input (the same effect is discussed in [moonlight-android#1241](https://github.com/moonlight-stream/moonlight-android/issues/1241)). `--boost` (on by default) keeps the clocks up for 20 s after the last input, so the faster decode also holds between strokes
- decoded → shown: ~1.5 ms

Decode is by far the largest stage, so the tablet's chip matters most: a Pixel 7a (Tensor G2) decodes in 6–9 ms and reaches capture → shown p50 12–14 ms.

Back-pressure (`--max-in-flight`, default 2) keeps a slow decoder from building a queue: the host waits and then encodes the newest frame instead of a backlog.

## Requirements

**Host**
- Linux with a Wayland compositor that implements the ScreenCast portal. Tested on niri (xdg-desktop-portal-gnome), KDE Plasma 6.8 and GNOME 51. Hyprland should work.
- PipeWire, FFmpeg ≥ 7.1 with `h264_nvenc` (NVIDIA) or `h264_vaapi` (Intel / AMD; tested on Intel UHD 620 with Debian 13). `adb` is optional: it installs and starts the app and enables raw pen input.
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
- `--encoder auto|h264_nvenc|h264_vaapi` (default `auto`: NVENC if the NVIDIA driver is present, else VAAPI). Both use a zero-copy GPU path. If a VAAPI driver offers no bitrate control (Debian's free `intel-media-va-driver` on older Intel GPUs only does constant QP), Waylus falls back to CQP and says so; `LIBVA_DRIVER_NAME=i965` or `intel-media-va-driver-non-free` give CBR. `WAYLUS_VAAPI_DEVICE=/dev/dri/renderD129` picks another GPU
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

### Wi-Fi (`--wifi`)

Waylus can also run over the LAN. USB stays the default: with `--wifi` the host still uses USB when the tablet is plugged in, and additionally accepts the paired tablet over Wi-Fi. Design, the Sunshine/Moonlight research behind it and all measurements: [docs/wifi.md](docs/wifi.md).

**Pairing (once).** Run `waylus --wifi --pair`. In the Waylus app on the tablet, tap the host under "Pair with …" (found via mDNS), or type its address and tap "Pair with this host". The host prints a 6-digit PIN (and shows a desktop notification); enter it on the tablet. Pairing stays possible for 5 minutes or until one tablet has paired, and closes after 3 wrong PINs. After that, `waylus --wifi` is enough: open the app on the tablet and it connects.

```sh
waylus --wifi --pair          # pair a tablet (shows a PIN)
waylus --wifi                 # USB if plugged in, else Wi-Fi
waylus --wifi --no-usb        # Wi-Fi only (adb, if present, still installs/starts the app)
waylus --unpair all           # forget paired tablets (or a name / id prefix)
```

Other flags: `--listen ADDR` (default: every private IPv4 address of the host, never public or VPN addresses such as Tailscale), `--fec PCT` (video parity, default 20 %, at least 2 parity packets per block). Ports: TCP and UDP 27183 (`--port`) and mDNS (UDP 5353); open them in your firewall if it blocks them. Fedora Workstation's default zone already allows them.

**Security model.**
- Only paired tablets can connect. Pairing uses SPAKE2 (RFC 9382) with the PIN: someone on the network who watches the pairing learns nothing that would let them guess the PIN offline, and each attempt tests a single PIN.
- Each session authenticates both sides with the 256-bit pairing key and an ephemeral ECDH exchange (forward secrecy).
- Everything after that is AES-256-GCM: input, control and video. The host drops any packet that fails authentication and never takes a peer's address on trust, so spoofed UDP source addresses or a reused DHCP address get nowhere.
- Keys: `~/.local/state/waylus/wifi.json` (mode 0600) on the host; on the tablet, in app storage, wrapped by an Android Keystore key.
- Video is always encrypted. Decrypting costs the tablet 0.2–0.5 ms per frame, and an unencrypted mode would save at most that, so there is none. Anyone on the LAN can still jam or flood the stream (denial of service), as with any Wi-Fi traffic.

**How it works.** Pen and touch go over UDP. Each packet repeats the previous two samples and the final state is resent shortly after, so a lost packet costs no retransmission wait. Video frames are encrypted, split into 1408-byte packets and protected with Reed-Solomon parity. A frame that can't be recovered is reported at once, and the host answers with a keyframe; the tablet keeps the last good picture meanwhile. While a Wi-Fi session runs in the foreground, the app holds Android's low-latency Wi-Fi lock (no power saving).

**Getting low latency.**
- Use 5 GHz (or 6 GHz) with a strong signal near the access point; check with `adb shell cmd wifi status` (Frequency, RSSI).
- At a weak spot (−78 dBm in the measurements below), Android keeps scanning for a better access point, and the link stalls for ~80–200 ms every few seconds. A wired host helps, since otherwise every packet crosses the air twice.
- On 2.4 GHz, lower the bitrate (e.g. `--bitrate 10`).
- The raw pen reader (full tilt on the NXTPAPER) needs adb; over Wi-Fi, without adb, the pen uses Android's events.

**Measured so far** (2026-11-17, built-in statistics, NXTPAPER 14 and host both on Wi-Fi via the same AP; [details](docs/wifi.md#findings-so-far)):

| | USB (AOA) | Wi-Fi, weak 5 GHz link (−72…−79 dBm) |
|---|---|---|
| Pen event → host (injected stylus input) | p50 1.6 / p95 2.5 ms | p50 3.1 / p95 33–72 ms |
| Capture → shown on the tablet (1080p60, 20 Mbit/s) | p50 25 / p95 28 ms | p50 35–42 / p95 51–60 ms |

The camera (photon-to-photon) and `pen.py` comparison on a good 5 GHz link is still to come.

## Status / roadmap

- [x] Pen (pressure, tilt, hover, buttons, eraser) and multitouch over USB
- [x] Portal / PipeWire capture → NVENC → MediaCodec video
- [x] Built-in latency statistics
- [x] Reproducible latency comparison against Weylus (`tools/latency`)
- [x] Per-stage latency stats, back-pressure, and a fix for 30–40 ms transfer stalls (messages are now written in one piece; split writes hit Nagle + delayed ACK in adbd's socket on the tablet)
- [ ] Hardware decode is the largest stage: on the MT8781 it is CPU-clock-bound (19 ms idle, 11.5 ms while drawing thanks to Android's input boost, 9 ms with all cores clocked up). `--boost` keeps clocks up for 20 s after input
- [x] Raw stylus input over adb (full tilt on devices whose Android config drops it, e.g. NXTPAPER 14)
- [x] VAAPI encoder (Intel / AMD), zero-copy: GL renders NV12 straight into exported VAAPI surfaces. On an i5-8350U (UHD 620): encode 3.3 ms/frame, capture→sent p50 ~5 ms, capture→shown p50 ~17 ms (pen in use)
- [x] Zero-copy GPU path (DMA-BUF → GL → CUDA → NVENC) and GPU downscaling to fit the tablet
- [x] AOA transport (default; no adb needed)
- [x] Virtual monitor on KDE Plasma and GNOME (`--virtual`), with automatic pen/touch mapping
- [ ] Virtual monitor on niri (needs `RecordVirtual` in niri; use a dummy plug meanwhile)
- [x] Wi-Fi transport (`--wifi`): PIN pairing, encrypted input and video, UDP with FEC, mDNS discovery
- [ ] Wi-Fi photon-to-photon and pen measurements on a good 5 GHz link and on 2.4 GHz

## Licence

Waylus is free software, licensed under the **GNU General Public License v3.0 or later**; see [LICENSE](LICENSE).
It is an independent implementation and contains no code from Weylus.
