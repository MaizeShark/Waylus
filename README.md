# Waylus

> [!NOTE]
> **LLM-generated code.** Most of this project, including code and documentation, was written by an AI model (Claude, by Anthropic) working with a human maintainer. It has been tested on real hardware but not audited line by line, so review it before relying on it.

*Weylus, but for Way(land).* Use an Android tablet as a **low-latency pen display** for your Linux (Wayland) or Windows 10/11 desktop.
It uses a native Android client and runs over USB or, optionally, Wi-Fi.

Waylus mirrors one of your monitors to the tablet using hardware H.264 encode and decode. It sends the tablet's pen and fingers back as real Linux input devices:
- a tablet tool with pressure, tilt and both stylus buttons;
- a multitouch screen.

Apps such as Krita, GIMP, Xournal++ and Inkscape see a regular graphics tablet through libinput and the Wayland tablet protocol. On Windows the pen and touch are synthetic pointer devices, so Windows Ink applications see a real pen (see [Windows](#windows)).

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

Measured on one machine (RTX 4060, niri, mirroring a 1920×1080 output at 75 Hz) and one tablet (TCL NXTPAPER 14, MT8781, 60 Hz), over the same USB 2.0 cable for both tools (Weylus through an `adb reverse` tunnel), and over the same Wi-Fi for both tools.

| Metric | Weylus ¹ | Waylus |
|---|---|---|
| **Tablet behind the real monitor, photon to photon** ² | p50 **198 ms** / p95 **278 ms** (p5 139, p99 311; n = 1149) | p50 **45 ms** / p95 **59 ms** (p5 25, p99 64; n = 305) |
| **Same, both over Wi-Fi (5 GHz)** ⁴ | p50 **183 ms** / p95 **248 ms** (p5 123, p99 273; n = 1574) | p50 **49 ms** / p95 **65 ms** (p5 26, p99 69; n = 552) |
| **Pen-down on the tablet → event on the host's input device** ³ | p50 **12.9 ms** / p95 **15.5 ms** (n = 30) | p50 **5.1 ms** / p95 **7.0 ms** (n = 30) |

¹ A local build of [Weylus](https://github.com/H-M-H/Weylus): mainly [PR #560](https://github.com/H-M-H/Weylus/pull/560) (PipeWire capture), with PipeWire adjusted to work on NVIDIA (upstream does not run on this setup). Settings: NVENC, Chrome on the tablet, "Lower Latency" on, frame-rate slider at maximum (120 fps), no stretching. Connected through an `adb reverse` tunnel rather than Wi-Fi, which favours it.

² Filmed with a phone camera (Pixel 7a slow motion, 120 fps) showing [`tools/latency/barcode.html`](tools/latency) on the monitor and its mirror on the tablet in the same shot. `tools/latency/camera.py` decodes both timestamps in every frame; their difference is the lag you actually see between the two screens, including the tablet's own compositing and display panel, with no clock sync involved. Measured 2026-11-17, one clip per tool with the pen idle, same camera position: Waylus at commit 0578fdd (NVENC, AOA transport), Weylus as in ¹; 1080p75 → 60 Hz panel.
- To reproduce: open `barcode.html` fullscreen on the mirrored monitor, film both screens in slow motion, run `python3 -I tools/latency/camera.py VIDEO --dump 100 --out frame.png` to read off the four corners of each barcode, then `camera.py VIDEO --monitor x,y x,y x,y x,y --tablet x,y x,y x,y x,y`. For a hand-held camera add `--track 100` (the frame the corners came from): every frame's corners are moved by the camera shake, measured by phase correlation against that frame.
- The rolling shutter often catches the monitor mid-refresh, so the monitor's timestamps are fitted as a line over the camera frames and every readable tablet frame counts.
- An earlier method, `tools/latency/probe.py`, compared tablet screenshots against an overlay showing Android's expected present time. Its bias differs per tool: it read ~34 ms above the camera for Waylus (it includes the host's own render and compositing) but 21 ms *below* it for Weylus (177 vs 198 ms p50, re-measured the same day), so a screenshot apparently shows a browser client's frames before they reach the glass. It is kept for quick checks of Waylus alone, not for comparisons.

³ Measured with `tools/latency/pen.py` while each tool was mirroring DP-2 with video: it reads the tablet's stylus device over adb and the tool's virtual pen device on the host, both with kernel event timestamps, syncs the two clocks over a persistent adb shell (±0.4 ms), and matches 30 pen taps. Everything after the host input device (compositor, app, monitor) is the same for both tools. Waylus reads the stylus device directly (raw pen over adb); Weylus goes through Chrome's pointer events and a WebSocket. To reproduce: `python3 -I tools/latency/pen.py --host-device "Waylus Pen"` (or `"Weylus Stylus"`), then tap.

⁴ Measured 2026-11-17 with `camera.py` as in ², one clip per tool, pen idle, nearly the same camera position. Tablet on 5260 MHz (channel 52, 802.11ac), RSSI −66…−69 dBm, link 351 Mbit/s. The host was also on Wi-Fi via the same access point (5260 MHz, −69 dBm), so every packet crossed the air twice. Waylus at commit c5eed34, `waylus --wifi --no-usb --max-in-flight 3` (now the default; NVENC, 20 Mbit/s, 20 % FEC). With the old default of 2 it measured p50 66 / p95 96 ms (p5 30, p99 104; n = 310; no packet lost during the clip). The camera was hand-held, so the corner points come from two frames; three corner sets gave identical results. Weylus as in ¹, with Chrome connecting to the host's LAN address instead of the tunnel.

#### Where Waylus's time goes

Waylus logs its own per-stage timings every 5 s (clocks synced by ping/pong). They cover capture → the decoded frame being handed to the tablet's compositor, so they leave out what the camera sees on top: the tablet's compositing and refresh (~1–2 frames at 60 Hz), minus the real monitor's own delay. Typical values at 1080p with the whole screen changing every frame (barcode page), capture → shown p50 ~21 / p95 ~25 ms with the pen idle, ~17 ms while drawing:
- capture → sent (GPU blit/scale + CUDA copy + NVENC): p50 ~2.5 / p95 ~4 ms
- sent → received (USB): p50 ~2.5 / p95 ~3 ms
- waiting for the decoder: ~0.5 ms
- hardware decode on the MT8781: ~19 ms with the pen idle, **~11.5 ms while drawing**. MediaTek's decoder speed follows the CPU clock, and Android only raises the clocks while there is touch/pen input (the same effect is discussed in [moonlight-android#1241](https://github.com/moonlight-stream/moonlight-android/issues/1241)). `--boost` (on by default) keeps the clocks up for 20 s after the last input, so the faster decode also holds between strokes
- decoded → shown: ~1.5 ms

Decode is by far the largest stage, so the tablet's chip matters most: a Pixel 7a (Tensor G2) decodes in 6–9 ms and reaches capture → shown p50 12–14 ms.

Back-pressure (`--max-in-flight`, default 3; 2 until 2026-11-17, used for the numbers above) keeps a slow decoder from building a queue: the host waits and then encodes the newest frame instead of a backlog. With 2, the encoder was held back about half the time over USB and sent a capture that had already aged; 3 still never queued frames at the decoder (built-in stats, USB, 3 alternating runs each: capture → shown p50 27.4 → 26.8 ms, p95 32–34 → 30.5–31 ms).

### Other Linux tablet-as-monitor tools

Measured 2026-11-18 on the same desktop, tablet and USB cable as above, in a KDE Plasma 6.8.3 session. UScreen and MoreSpace create their monitor through the EVDI kernel module, which niri can't drive while it renders on an NVIDIA GPU (`NoSupportedRendererFormat`, see [niri#3625](https://github.com/niri-wm/niri/issues/3625)), and Moreland's virtual monitor needs KWin or Hyprland. All of them extend the desktop rather than mirror it, so their extra monitor was made to show DP-2 and filmed against DP-2 exactly as in ²: EVDI monitors were set to mirror it (`kscreen-doctor output.<name>.mirror.DP-2`); Moreland's KWin virtual monitor was placed exactly on top of DP-2 instead, because KWin stops streaming a virtual monitor that mirrors another one. Waylus streamed a KDE virtual monitor mirroring DP-2, the same path as the EVDI tools. Defaults were kept except where noted.

| | Waylus | [Moreland](https://github.com/adiimanav/moreland) 0.3.0 | [UScreen](https://github.com/majmichu1/UScreen) 1.2.15 | [MoreSpace](https://morespaceapp.com/) 0.6.7 |
|---|---|---|---|---|
| **Tablet behind the real monitor, photon to photon** ² | p50 **50 ms** / p95 **60 ms** (p5 31, p99 70; n = 517) | p50 **56 ms** / p95 **65 ms** (p5 39, p99 73; n = 790) ⁸ | p50 **65 ms** / p95 **79 ms** (p5 45, p99 82; n = 544) ⁶ | p50 **103 ms** / p95 **117 ms** (p5 84, p99 123; n = 999) ⁷ |
| **Pen-down on the tablet → event on the host's input device** ³ | p50 **4.9 ms** / p95 **6.5 ms** (max 6.8; n = 30) | p50 **8.0 ms** / p95 **23.0 ms** (max 46; n = 31) | p50 **8.5 ms** / p95 **11.9 ms** (max 12.3; n = 30) | p50 **8.0 ms** / p95 **63.5 ms** (max 241; n = 31) |
| Stream | 1920×1080, 60 fps, 20 Mbit/s, NVENC H.264 | 1920×1080, 60 fps (its default is 90), 20 Mbit/s, NVENC H.264 via GStreamer | 1920×1080 (set by hand ⁶), 60 fps, 20 Mbit/s, NVENC H.264 | 1920×1280 (width set to 1920; it keeps the tablet's 3:2), 60 Hz, ~8.8 Mbit/s (its automatic value), **software** H.264 ⁷ |
| Capture | Screencast portal (PipeWire DMA-BUF) | KWin screencast protocol (PipeWire) | EVDI | EVDI |
| Transport | USB accessory (AOA) | `adb forward` | `adb` port forwarding | USB |
| Licence | GPL-3.0-or-later | Apache-2.0 | MIT | Proprietary; Google sign-in, €17 after a 48 h trial |

Waylus mirroring DP-2 directly (its normal mode, no extra monitor) measured p50 46 / p95 59 ms (p5 22, p99 69; n = 503) in the same session, the same as on niri: the KDE copy into a separate output costs it ~4 ms, and the compositor makes no difference. The pen tails (Moreland, MoreSpace) are real: the clock drift during each run was below 0.05 ms.

⁶ UScreen sizes its monitor to the tablet's native resolution by default (2400×1600 here, `auto_resolution`). That measured p50 93 / p95 123 ms (p5 58, p99 134; n = 906): 2400×1600 is above what this tablet's decoder is rated for (15 000 vs 14 400 macroblocks per frame), and decode time grows with resolution (Waylus logs ~14 ms at 1080p and ~23 ms at 2160×1440 on this tablet). The table uses 1920×1080 so that all tools decode about the same amount. The pen was measured at 2400×1600; pen latency doesn't depend on the stream.

⁷ MoreSpace encodes in hardware only through VAAPI, which NVIDIA doesn't provide, so on this machine it falls back to Cisco's OpenH264 software encoder (`morespace codec install-openh264`). It would hardware-encode on an Intel or AMD GPU with VAAPI H.264 CBR support. The test laptop's Kaby Lake iGPU has none (constant-QP only), so that comparison is still open. Its default codec preference is HEVC, which also needs VAAPI. On niri it doesn't start (the EVDI limitation above; it also needs `wlr-randr`).

⁸ Built from source at commit e333374 with `--features plasma`, run as `moreland --width 1920 --height 1080 --fps 60 --bitrate 20000`. Its own statistics for the run: NVENC encode p50 6.2 ms, tablet decode p50 25.4 ms. Waylus decodes 1080p on this tablet in ~14–19 ms, probably helped by keeping the tablet's clocks up while streaming (`--boost`, see below).

#### Moonlight (Wi-Fi)

[Moonlight](https://moonlight-stream.org/) with [Sunshine](https://github.com/LizardByte/Sunshine) is a game-streaming pair, not a pen display, but it is the reference for low-latency streaming over Wi-Fi and forwards the pen. It only runs over the network, so it was compared with `waylus --wifi`, in the same session as the table above, one after the other: tablet on 5260 MHz (802.11ac, −63…−64 dBm, 351 Mbit/s), host on Wi-Fi via the same access point.

| Wi-Fi 5 GHz | Waylus `--wifi --no-usb` | Moonlight 12.2 + Sunshine 2026.914, H.264 | same, HEVC (Moonlight's default) |
|---|---|---|---|
| **Tablet behind the real monitor, photon to photon** ² | p50 **49 ms** / p95 **62 ms** (p5 30, p99 70; n = 629) ⁹ | p50 **67 ms** / p95 **79 ms** (p5 48, p99 80; n = 721) | p50 **60 ms** / p95 **74 ms** (p5 37, p99 79; n = 595) |
| **Pen-down → host input device** ³ | p50 **9.4 ms** / p95 **15.0 ms** (max 16.2; n = 31; Android pen events, `--no-raw-pen`) | p50 **7.4 ms** / p95 **10.3 ms** (max 12.8; n = 30) | |

Sunshine captured DP-2 through KMS (`output_name = DP-2`) and encoded with NVENC; Moonlight at 1080p, 60 fps, 20 Mbit/s (Sunshine reports ~15 Mbit/s of video after its FEC share), frame pacing "Prefer lowest latency".

⁹ 25 of 654 tablet readings (3.8 %) sat at 89–283 ms, clustered around 190 ms, and are left out of the figures as outliers. They look like real stalls (a frame lost beyond what the FEC repairs, then a keyframe), not misreads; with them included: p50 49 / p95 70 / p99 239 ms. Moonlight's run had no such cluster (its 4 dropped readings out of 725 were misreads, and its p99 with everything included is 81 ms). Its recovery over Wi-Fi is the more robust of the two, and its pen over Wi-Fi is faster; both are open items for Waylus's Wi-Fi mode.

## Requirements

**Host (Linux; for Windows see [Windows](#windows))**
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

The pen and touchscreen must be mapped to the output shown on the tablet. **On KDE Plasma and GNOME Waylus does this automatically**, for both mirrored and virtual monitors: through KWin's input-device D-Bus settings on KDE, and through the per-device GSettings that Settings → Wacom Tablet uses on GNOME (keyed by the monitor's EDID). On GNOME this matters beyond convenience: an unmapped direct touchscreen crashed gnome-shell 51 when a touch started a window move. **On niri** Waylus writes the mapping to `~/.config/niri/config.d/90-waylus.kdl` while it runs and deletes it on exit, so niri goes back to your own tablet/touch settings (`map-to-output` applies to every tablet, so this also keeps a second drawing tablet where you put it). Your config has to include that file once, at the end; `waylus --niri-setup` appends the line, or add it yourself:

```kdl
include optional=true "config.d/90-waylus.kdl"
```

`optional=true` keeps the config valid while the file doesn't exist. Elsewhere, map the pen and touch in your compositor's settings.

Useful flags:
- `--choose`: show the monitor picker again (switch the mirrored monitor)
- `--bitrate <Mbit/s>` (default: 20 at 1080p, scaled with resolution)
- `--fps` (default: the tablet's refresh rate)
- `--encoder auto|h264_nvenc|h264_vaapi` (default `auto`: NVENC if the NVIDIA driver is present, else VAAPI). Both use a zero-copy GPU path. If a VAAPI driver offers no bitrate control (Debian's free `intel-media-va-driver` on older Intel GPUs only does constant QP), Waylus falls back to CQP and says so; `LIBVA_DRIVER_NAME=i965` or `intel-media-va-driver-non-free` give CBR. `WAYLUS_VAAPI_DEVICE=/dev/dri/renderD129` picks another GPU
- `--serial <adb serial>`
- `--virtual[=WxH]`: create a virtual monitor instead of mirroring (GNOME / KDE). The size defaults to the tablet's screen within its decoder limits
- `--no-raw-pen`: use Android's pen events instead of reading the stylus device over adb
- `--boost <s>`: keep the tablet's CPU clocks up for this long after pen/touch input (default 20, 0 = off). Decoding on MediaTek tablets is much faster at high clocks, which Android only grants while you touch the screen. Costs some tablet battery while active
- `--max-in-flight <n>`: back-pressure, max frames sent but not yet shown (default 3, 0 = off)
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
- Avoid 2.4 GHz if you can (measured below: freezes of 0.2–1 s). If you must, use `--bitrate 10`.
- The raw pen reader (full tilt on the NXTPAPER) needs adb; over Wi-Fi, without adb, the pen uses Android's events.

**Measured so far** (2026-11-17, built-in statistics, NXTPAPER 14 and host both on Wi-Fi via the same AP; [details](docs/wifi.md#findings-so-far)):

| | USB (AOA) | Wi-Fi, weak 5 GHz link (−72…−79 dBm) |
|---|---|---|
| Pen event → host (injected stylus input) | p50 1.6 / p95 2.5 ms | p50 3.1 / p95 33–72 ms |
| Pen-down → host input device (`pen.py`, 30 real taps, video on, Android pen events: `--no-raw-pen`) ⁵ | p50 6.8 / p95 8.8 ms (max 9.3) | p50 9.3 / p95 15.7 ms (max 132) |
| Capture → shown on the tablet (1080p60, 20 Mbit/s) | p50 25 / p95 28 ms | p50 35–42 / p95 51–60 ms |

Photon to photon over Wi-Fi (camera, see ⁴ above): Waylus **p50 49 / p95 65 ms**, Weylus over the same Wi-Fi 183 / 248 ms, Waylus over USB 45 / 59 ms. What remains of the gap to USB is the link: sent → received p50 ~8.3 ms vs 2.7 over USB. A 51 KB frame takes ~4 ms on air at an effective ~100 Mbit/s, because the host's own Wi-Fi shares the channel. A wired host or a lower `--bitrate` should shrink it; not measured yet.

With the old back-pressure default of 2 frames in flight, Wi-Fi measured 66 / 96 ms. The ~6 ms longer pipeline meant two frames no longer covered it: the encoder was held back ~800 ms/s, reached only 53–56 fps and sent captures that had already aged. Built-in stats, 3 alternating runs each, same conditions: capture → shown p50 38.3 → 32.8 ms, p95 49 → 39 ms, capture → sent 7.4 → 2.3 ms, 55 → 60 fps.

**2.4 GHz** (2026-11-17, camera as in ⁴ with `--track`; tablet on 2462 MHz, 802.11n, −52 dBm, 72–78 Mbit/s; host on 5 GHz at the same access point):

| Photon to photon | p50 | p95 | p99 |
|---|---|---|---|
| 20 Mbit/s (default) | 103 ms (112 with freezes) | 282 ms (**807** with freezes) | 331 ms (1065) |
| `--bitrate 10` | 79 ms (81) | 134 ms (**303**) | 173 ms (461) |

`camera.py` drops values far from the median as chance decodes. Here they clustered at 0.2–1.2 s: real freezes, 15 % of the frames at 20 Mbit/s and 8 % at 10 Mbit/s, so both readings are given. The tablet's loss log shows the cause: the 2.4 GHz link stalls for 100–450 ms at a time and loses bursts of half a frame, more than the parity can repair; each such loss costs a keyframe, which on 2.4 GHz takes long to send. A longer loss timeout didn't help ([details](docs/wifi.md)). Use 5 GHz.

⁵ Measured 2026-11-17 with `tools/latency/pen.py` as in ³, at commit 841f4e0. Tablet on 5260 MHz, −67 dBm, with the host on the same access point over Wi-Fi. Both runs use the app's pen events (`--no-raw-pen`); with the raw stylus reader, the pen would travel over adb rather than the transport being measured. Over Wi-Fi, one of the 30 taps hit a link stall (132 ms); the other 29 stayed under 16 ms.

## Windows

The host also runs on Windows 10 (1903 or newer; 11 recommended) with the same app, protocol and transports. The Linux measurements and tables above don't apply to it; its own are [further down](#measured-on-windows).

```
 host (Rust, Windows)                                                tablet (Kotlin, unchanged)
 Windows.Graphics.Capture → D3D11 video processor (BGRA→NV12, scale) ─┐
   → FFmpeg D3D11 frames → NVENC / AMF / QSV ─────────────────────────┤  USB (AOA, adb)  ┌→ MediaCodec → SurfaceView
                                                                      ├─ or Wi-Fi ───────┤
 synthetic pointer devices (pen + touch)  ←  input events  ←──────────┘                  └← MotionEvent / raw stylus
```

- **Capture** is Windows.Graphics.Capture of one monitor, cursor included. Frames stay on the GPU: the D3D11 video processor converts them to NV12 (BT.709, limited range) and scales them down to what the tablet can decode, and FFmpeg hands the texture to the encoder (`h264_nvenc`, `h264_amf` or `h264_qsv`; `--encoder auto` picks by the GPU's vendor). The encoder settings are the same low-latency ones as on Linux.
- **Pen and touch** use `CreateSyntheticPointerDevice` / `InjectSyntheticPointerInput` (Windows 10 1809+): a `PT_PEN` device with pressure (0–1024), tilt, hover and the barrel button, and a `PT_TOUCH` device with 10 contacts (scrolling and pinch-zoom work). Coordinates map to the mirrored monitor's rectangle in virtual-desktop pixels; the process is per-monitor DPI aware. Windows drops the eraser flags of a synthetic pen, so a stylus's eraser acts like its tip. A Windows Ink pen has one barrel button, so both of the stylus's buttons report as that one. As with any Windows Ink pen, the button counts when the pen touches down: hold it, then touch (pressing it mid-stroke ends the stroke and starts a new one with the button). GTK applications such as Xournal++ see it as button 3 (in Xournal++: *Stylus → Button 2*) and ignore a press while the pen only hovers; Windows Ink applications such as Krita also read it while hovering. **Krita** reads tablets through WinTab by default, which Waylus's pen doesn't provide, so it sees only mouse moves (no pressure, tilt or buttons): set *Settings → Configure Krita → Tablet settings → Tablet Input API* to *Windows 8+ Pointer Input (Windows Ink)* and restart Krita (tested: pressure and tilt work). Input can't reach windows of elevated programs unless Waylus runs elevated too.
- Verified with `tools/windows/PointerProbe.ps1`, a window that records the `WM_POINTER` messages it receives: `waylus --test-input --output N` injects a pressure ramp with tilt, hover with the barrel button, and a two-finger pinch, and the probe sees pressure 0–1024, tilt up to 45°/−22°, the barrel flag, and two touch contacts.

### Setup

1. Install **Rust** (MSVC toolchain), the **Visual Studio Build Tools** (C++), **LLVM** (`winget install LLVM.LLVM`, for bindgen) and an **FFmpeg shared build, version 7.1 or newer**, with headers and import libraries: for example BtbN's `ffmpeg-n8.1-latest-win64-gpl-shared-8.1.zip` from <https://github.com/BtbN/FFmpeg-Builds/releases> (the one tested) or the gyan.dev "full-shared" build. Unpack it, then:
   ```powershell
   $env:FFMPEG_DIR = 'C:\ffmpeg-dev\ffmpeg-n8.1-latest-win64-gpl-shared-8.1'
   $env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
   cargo build --release            # target\release\waylus.exe
   ```
   (vcpkg works as well: `vcpkg install ffmpeg[nvcodec,amf,qsv]:x64-windows`.) At run time `waylus.exe` finds the FFmpeg DLLs on `PATH` or next to itself: copy the `bin\*.dll` files beside it.
2. The **tablet app** is the same APK as on Linux (protocol version 5), installed with `adb` (platform-tools on `PATH`, USB debugging on). Waylus installs an APK it finds next to the binary or the source tree, or uses the installed one.
3. Run it. `waylus --list-outputs` shows the monitors (number, GDI name, size, refresh rate, GPU); `waylus --output 2` mirrors one. The numbers follow Windows' device names (`\\.\DISPLAY1`, `\\.\DISPLAY2`, …), so they don't change when you pick another main display or rearrange the monitors; `--output DISPLAY2` works too. Without `--output` Waylus asks once and remembers the choice in `%LOCALAPPDATA%\waylus\output.txt` (`--choose` asks again).

```powershell
waylus --list-outputs
waylus --output 2                # mirror monitor 2: video + pen + touch
waylus --no-video --output 2     # pen and touch only, as a plain graphics tablet
waylus --encoder h264_amf        # force the encoder (AMD); h264_qsv for Intel
```

All other flags are the same as on Linux (`--transport`, `--bitrate`, `--fps`, `--wifi`, ...), except the Linux-only `--niri-setup`. Ctrl-C removes the `adb reverse`, stops the app and releases the pointer devices.

AMF and QSV are implemented but **untested**: the development machine has an NVIDIA GPU only (the error paths were checked: without an AMD/Intel GPU both stop with a clear message). NVENC is the tested path. QSV needs Intel's graphics driver with the oneVPL runtime.

### USB accessory mode and its driver

`--transport adb` works with Google's or the vendor's USB driver, as for any Android tooling. The default, **USB accessory mode (AOA)**, needs WinUSB on the tablet's accessory interface, which Android doesn't ask for (it advertises no Microsoft OS descriptors):

- **With Samsung's Android USB driver installed** (the usual "Samsung USB driver" download, which many people have for any Android phone), nothing to do: its package binds WinUSB to the accessory interface (18d1:2d00/2d01, "SAMSUNG Android Open Accessory(AOA) Device"). Samsung's driver also takes the parent composite device (`dg_ssudbus` instead of Microsoft's `usbccgp`), which the upstream USB library, nusb 0.2.9, refuses; Waylus ships nusb with a one-line change for that (`vendor/nusb/WAYLUS.md`).
- **Without it**, Windows leaves the accessory interface without a driver; Waylus says so and uses `adb` instead. Install WinUSB once:
  1. Run `waylus` once with the tablet attached. It switches the tablet into accessory mode (USB ID 18d1:2d00 or 2d01), and Windows lists an "Android Open Accessory" device.
  2. Start [Zadig](https://zadig.akeo.ie/), tick *Options → List All Devices*, select that **Android Open Accessory** entry (interface 0) and install the **WinUSB** driver.
  3. Run `waylus` again: it takes the accessory link.

  This replaces the driver of that one interface; adb and the tablet's other uses are unaffected.

Samsung's driver never completes a USB transfer that is cancelled the usual way, and that is also what Windows does when a process ends. Waylus therefore aborts its accessory transfers itself when it stops (Ctrl-C, closing the window, the session ending). If it is killed hard instead (Task Manager's *End task*, `taskkill /F`), the process stays half-closed, holding `waylus.exe` and the accessory interface, until the tablet is unplugged; unplug it once and start again.

To switch the tablet into accessory mode at all, Waylus has to open the tablet's own USB interface, which Windows' adb server holds, so it runs `adb kill-server` at that point (the next adb call starts the server again).

### Wi-Fi on Windows

`--wifi`, `--pair`, `--unpair`, `--listen`, `--no-usb` and `--fec` work as on Linux, with the same pairing and encryption. The key store is `%LOCALAPPDATA%\waylus\wifi.json` (the folder's inherited permissions apply: your account, SYSTEM and administrators). The tablet keeps one paired host, so pairing it with the Windows host replaces a Linux pairing.

- **Firewall.** Windows Defender Firewall asks the first time `waylus.exe` listens (TCP and UDP 27183, and mDNS on UDP 5353). Allow it on **Private networks only**; never tick *Public*. Windows often files a home Wi-Fi network as *Public*, and then the Private-only rule doesn't apply: switch the network to Private (Settings → Network & internet → Wi-Fi → the network → Network profile type). If the host starts but the tablet can't connect, look here first: `Get-NetConnectionProfile`, and the rules for `waylus.exe` under *Windows Defender Firewall → Allow an app*.
- **DSCP.** Waylus marks its UDP video datagrams (`IP_TOS`), but Windows ignores that unless a QoS policy allows it. Waylus doesn't create a policy (that takes administrator rights), so on Windows the packets go out unmarked.
- **mDNS.** Waylus advertises `_waylus._tcp` on the private LAN addresses it listens on only. The tablet's "enter address" fallback works without it.

### Virtual monitor

Waylus can't create a monitor on Windows (that takes a display driver), but an Indirect Display driver can: install [Virtual-Display-Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) and its monitor is captured like any other. Then:

- `waylus --virtual` finds that monitor (the one on an Indirect Display adapter), switches it to the mode that fits the tablet best (same aspect ratio, as large as the tablet's decoder allows, the tablet's refresh rate) and mirrors it. Extend the desktop onto it in Windows' display settings, drag windows there and draw. Changing the mode needs no administrator rights.
- The driver offers only the modes in its `vdd_settings.xml` (by default `C:\VirtualDisplayDriver\`: every `<resolution>` entry, plus the `<g_refresh_rate>` values for each). It lists 1920×1080 and 2560×1440 among others. For a 3:2 tablet such as the NXTPAPER 14 (2400×1600) add the size the stream uses, for example
  ```xml
  <resolution><width>2160</width><height>1440</height><refresh_rate>60</refresh_rate></resolution>
  ```
  and restart the driver (Device Manager → Display adapters → disable and enable "Virtual Display Driver", which needs administrator rights, or the driver's control app). When the exact mode is missing, Waylus logs which size it wanted and uses the closest one. Editing the file could be automated, but loading it takes elevation, so Waylus leaves the file alone.
- 1920×1080 at 60 Hz is a safe choice for any tablet.

### Measured on Windows

Setup: the same desktop as the Linux numbers, booted into Windows 11 (RTX 4060, driver 32.0.16.1088, NVENC), mirroring a 1920×1080 60 Hz monitor with `tools/latency/barcode.html` full screen in a browser (so the picture changes every frame), the TCL NXTPAPER 14 (60 Hz panel) over USB, 20 Mbit/s, pen idle. These are separate from the Linux tables above; the monitor here runs at 60 Hz, not 75.

| Built-in statistics, `--transport adb`, 2026-11-18 (5 s windows, 4 in a row) | Windows |
|---|---|
| Capture → shown on the tablet | p50 32.3–32.8 / p95 37–39 ms (n ≈ 300 per window) |
| Capture → sent (capture, GPU convert, NVENC, write) | p50 3.3 / p95 3.5 ms |
| NVENC encode | 2.6 ms per frame |
| Tablet: wait for decoder / decode / decoded → shown | p50 1.1 / 21.7 / 2.9 ms |
| Frame rate / bitrate | 60.0 fps, 20.0 Mbit/s |

The decode time is the MT8781's idle-clock decode that the Linux section already describes (19–22 ms while nothing touches the screen; ~11 ms while drawing). The host's part is about the same as on Linux: 3.3 ms from capture to the first byte on the wire.

**Photon to photon, USB (`--transport adb`), 2026-11-18:** `camera.py` as in the Linux notes (Pixel 7a, 120 fps, hand-held, `--track 100`, one clip of ~20 s, pen idle, 1080p60 monitor to the 60 Hz panel): p50 **46 ms** / p95 **65 ms** (p5 32, p99 68; n = 194 decoded frames). The Linux reference over USB is p50 45 / p95 59 ms. One clip only, and the camera moved a lot (tracking shift up to 360 px), so the tail is less certain than the median.

**Photon to photon, USB accessory mode (the default), 2026-11-18:** same method and setup, release build, tablet switched from a fresh plug-in with Samsung's USB driver installed (no Zadig step): fitted p50 **39 ms** / p95 **59 ms** (p5 23, p99 67; n = 209, no outliers; camera shift up to 400 px). The host's built-in statistics during the clip: capture → shown p50 28.0 / p95 29.8 ms, capture → sent 3.9 ms, sent → received p50 2.4 / p95 2.9 ms (adb: capture → shown p50 32–33 ms). One clip each, so the 7 ms gap to adb is indicative, not settled; on Linux the two transports differed in the tail rather than the median.

**Photon to photon, Wi-Fi 5 GHz, 2026-11-18:** same method, `waylus --wifi --no-usb` (release build, 20 Mbit/s, 20 % FEC); tablet on 5260 MHz, −62 dBm, 351 Mbit/s (`adb shell cmd wifi status` before the clip); the host is on 5 GHz via the same access point. Fitted p50 **48 ms** / p95 **64 ms** (p5 29, p99 73; n = 239; 6 readings of 86–124 ms dropped as outliers, with them p95 69 / p99 108). The Linux reference over Wi-Fi is p50 49 / p95 62–65 ms. A first clip taken with an unoptimized (debug) host build, where sealing and FEC took 12–21 ms per frame, read p50 76 / p95 94 ms and is not used.

**Pen, 2026-11-18** (release build): `tools/latency/pen.py --windows-monitor 2`, 30 real taps with the T-Pen each, Android pen events (`--no-raw-pen`), video on, pen-down on the tablet → the pointer message in a window on the mirrored monitor:

| | p50 | p95 | min / max |
|---|---|---|---|
| USB (`--transport adb`) | 8.7 ms | 11.4 ms | 3.9 / 11.8 ms (n = 31) |
| USB accessory mode (default) | 8.4 ms | 14.8 ms | 3.7 / 22.7 ms (n = 31) |
| Wi-Fi 5 GHz (−63 dBm) | 9.5 ms | 15.6 ms | 4.5 / 37.7 ms (n = 31) |

Not the same measurement point as the Linux figures (p50 6.8 / p95 8.8 ms over USB and 9.3 / 15.7 ms over Wi-Fi with the same pen events, from the kernel timestamp on the virtual input device): on Windows the time includes delivery of the pointer message to the window, so these are upper bounds. A first USB run on a debug build read p50 8.4 / p95 10.9 ms.

**Wi-Fi, 2026-11-18** (built-in statistics as above, `waylus --wifi --no-usb`, release build, 4 windows of 5 s, host and tablet on the same access point at 5260 MHz, tablet −71…−72 dBm, 263 Mbit/s link): capture → shown p50 35.3–36.7 / p95 40.8–49 ms; capture → sent p50 5.0–5.2 ms (send 1.0 ms per frame, seal + FEC 0.13 ms); sent → received p50 7.4–8.0 / p95 11.6–13.9 ms; 0 of 10 800 shards lost per window, no FEC repair, no lost frames, 60 fps. mDNS discovery (the tablet listed the host), PIN pairing from a fresh key store and qWAVE flow marking (accepted without administrator rights; its effect on the air was not measured) all worked.

#### Weylus and Moonlight on Windows

Same desktop, monitor, tablet and `camera.py` method as above, 2026-11-18, the barcode page on `\\.\DISPLAY2` (1920×1080, 60 Hz). Photon to photon, tablet behind the real monitor:

| | p50 | p95 | p5 / p99 | n |
|---|---|---|---|---|
| Waylus, USB accessory mode | **39 ms** | **59 ms** | 23 / 67 | 209 |
| Waylus, USB `--transport adb` | **46 ms** | **65 ms** | 32 / 68 | 194 |
| Weylus 0.11.4, USB (`adb reverse`), min. frame interval 1 ms | **393 ms** | **471 ms** | 293 / 512 | 258 |
| Weylus 0.11.4, same, min. frame interval 8 ms | **401 ms** | **494 ms** | 317 / 527 | 1651 |
| Waylus `--wifi --no-usb`, 5 GHz (earlier session) | **48 ms** | **64 ms** | 29 / 73 | 239 ᵂ |
| Waylus `--wifi --no-usb`, 5 GHz (same session as Moonlight) | **55 ms** | **78 ms** | 34 / 93 | 438 ᵂ |
| Moonlight 12.2 + Sunshine 2026.914, H.264, 5 GHz | **47 ms** | **71 ms** | 30 / 73 | 301 |
| Moonlight 12.2 + Sunshine 2026.914, HEVC, 5 GHz | **53 ms** | **61 ms** | 37 / 68 | 524 |

- **Weylus** is the official Windows release 0.11.4 (the Linux comparison used a newer local build). It captures only the main display, so monitor 2 was made the main display for its clips. Settings: NVENC (`--try-nvenc`), Chrome on the tablet, "Lower Latency" on, maximum resolution. 0.11.4 has no frame-rate slider; its "minimum frame interval" was set to 1 ms, then to 8 ms (≈ 120 fps, as on Linux), with the same result. About 400 ms is twice what Weylus reached on Linux (198 ms) and ten times Waylus; where Windows adds the time (capture, the browser's decoder queue) was not investigated.
- **Moonlight**: Sunshine 2026.914, the same version as on Linux, portable build, `output_name = \\.\DISPLAY2`, `capture = ddx` (Desktop Duplication), NVENC; Moonlight at 1080p, 60 fps, 20 Mbit/s, frame pacing "Prefer lowest latency". The tablet was on 5260 MHz at −60…−63 dBm, 351–390 Mbit/s. On Windows Moonlight is 13–20 ms faster than with the same versions on Linux (H.264 67 → 47 ms, HEVC 60 → 53 ms at p50), so Sunshine's Windows capture path is the quicker one.
- **Waylus over Wi-Fi**: the clip filmed right after Moonlight's caught a Wi-Fi stall (the host's statistics for that 5 s window: sent → received p95 51 ms instead of 10–13); outside it the built-in figures matched the earlier session (capture → shown p50 34–37 ms). On Windows Waylus over Wi-Fi is about level with Moonlight at the median (48–55 vs 47–53 ms) and has the longer tail when the link stalls, the same open item as on Linux (⁹ above).
- The pen with Moonlight was not measured on Windows.

ᵂ Outliers dropped as described for Linux (⁹ above): 6 readings of 86–124 ms in the earlier clip, 25 of 105–293 ms in the later one (with them: p95 110 / p99 260 ms).

Not measured yet: USB taking over from Wi-Fi.

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
