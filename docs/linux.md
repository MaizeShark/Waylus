# Waylus on Linux

The README has the [quick start](../README.md#running-on-linux). This page has the details: requirements, the per-compositor pen/touch mapping, virtual monitors, the USB transports and all options.

```
 host (Rust)                                                        tablet (Kotlin)
 ScreenCast portal → PipeWire → NVENC / VAAPI H.264 ─┐  USB (AOA,   ┌→ MediaCodec (HW) → SurfaceView
                                                     ├─ or adb;  ───┤
                                                     │  or Wi-Fi)   │
 uinput pen + touchscreen  ← input events  ←─────────┘              └← MotionEvent (unbuffered)
```

Waylus sends the tablet's pen and fingers back as real Linux input devices: a tablet tool with pressure, tilt and both stylus buttons, and a multitouch screen. Apps such as Krita, GIMP, Xournal++ and Inkscape see a regular graphics tablet through libinput and the Wayland tablet protocol.

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

The monitor you pick is remembered via a portal restore token stored in `~/.local/state/waylus/`; `--choose` shows the picker again.

## Pen and touch mapping

The pen and touchscreen must be mapped to the output shown on the tablet.

- **KDE Plasma and GNOME:** Waylus does this automatically, for both mirrored and virtual monitors: through KWin's input-device D-Bus settings on KDE, and through the per-device GSettings that Settings → Wacom Tablet uses on GNOME (keyed by the monitor's EDID). On GNOME this matters beyond convenience: an unmapped direct touchscreen crashed gnome-shell 51 when a touch started a window move.
- **niri:** Waylus writes the mapping to `~/.config/niri/config.d/90-waylus.kdl` while it runs and deletes it on exit, so niri goes back to your own tablet/touch settings (`map-to-output` applies to every tablet, so this also keeps a second drawing tablet where you put it). Your config has to include that file once, at the end; `waylus --niri-setup` appends the line, or add it yourself:
  ```kdl
  include optional=true "config.d/90-waylus.kdl"
  ```
  `optional=true` keeps the config valid while the file doesn't exist.
- **Elsewhere**, map the pen and touch in your compositor's settings.

## Virtual monitor (`--virtual`)

`--virtual[=WxH]` creates an extra monitor through the ScreenCast portal instead of mirroring one, and maps pen and touch to it. Tested on KDE Plasma 6.8 and GNOME 51. The size defaults to the tablet's screen within its decoder limits.

niri doesn't implement it (its portal advertises the virtual source type, but the call fails with `Unknown method 'RecordVirtual'`); use an HDMI/DP dummy plug there and mirror that output.

On KDE, KWin stops streaming a virtual output that is set to mirror another output. Use the virtual monitor as an extension of the desktop, or mirror the real monitor directly.

## USB accessory mode and `--transport adb`

By default the host switches the tablet into Android Open Accessory (AOA) mode and exchanges data over raw USB bulk transfers, skipping adbd and both TCP stacks. With `--transport adb`, or automatically when the tablet doesn't support AOA, it uses an `adb reverse` TCP tunnel instead:
- **No USB debugging needed.** Without adb, the app must already be installed, and Android offers to open it when the host connects; tick "always" in that dialog. If adb is available it is still used to install and start the app and to read the raw stylus device.
- The first time, Android asks for permission to access the accessory.
- The tablet stays in accessory mode (USB ID 18d1:2d00/2d01) until it is unplugged, so MTP file transfer is unavailable meanwhile; adb keeps working.
- The USB hop is no faster on average (p50 ~2.5 ms either way, measured on the NXTPAPER 14), but AOA has no tail spikes, while adb showed ~18–20 ms p95 windows ([measurements](measurements.md#where-the-time-goes)).
- The link outlives the host and app processes. Each session therefore starts with a SYNC handshake, and a restarted host is picked up by a running app within ~5 s. On Ctrl-C the host first tells the app that it is stopping, so the app says so at once instead of waiting for the 5 s idle timeout.
- The host needs access to the USB device. With USB debugging on, systemd's `70-uaccess.rules` already grants it (the accessory still exposes adb). Without adb, the tablet comes up as 18d1:2d00, which may need a rule like this in `/etc/udev/rules.d/70-waylus.rules`:
  ```
  SUBSYSTEM=="usb", ATTR{idVendor}=="18d1", ATTR{idProduct}=="2d00|2d01", TAG+="uaccess"
  ```

## Wi-Fi

`--wifi` adds a paired, encrypted LAN transport next to USB. Pairing, the security model and tips: [wifi.md, Using Wi-Fi](wifi.md#using-wi-fi).

## Options

All options, as printed by `waylus --help`:

| Option | |
|---|---|
| `-o, --output <NAME>` | Connector of the mirrored output, such as `DP-2`: used for the niri mapping and, with `--no-video`, for the aspect ratio |
| `--no-video` | Only forward pen and touch (a plain graphics tablet) |
| `--choose` | Show the monitor picker again (switch the mirrored monitor) |
| `--virtual[=WxH]` | Create a virtual monitor instead of mirroring (GNOME / KDE), see above |
| `--niri-setup` | Add the include line for the pen/touch mapping to the niri config and exit |
| `--encoder auto\|h264_nvenc\|h264_vaapi` | Default `auto`: NVENC if the NVIDIA driver is present, else VAAPI. Both use a zero-copy GPU path, see [VAAPI](#vaapi) |
| `--bitrate <Mbit/s>` | Default: 20 at 1080p, scaled with resolution |
| `--fps <n>` | Default: the tablet's refresh rate, within decoder limits |
| `--transport aoa\|adb` | USB accessory mode (default) or the `adb reverse` tunnel, see above |
| `--port <n>` | TCP port (adb tunnel; Wi-Fi uses it for TCP and UDP). Default 27183 |
| `-s, --serial <serial>` | adb serial of the tablet, if several devices are attached |
| `--apk <path>` | Client APK to install (default: search next to the binary / source tree) |
| `--no-adb` | Don't touch adb at all (you run `adb reverse` and start the app yourself) |
| `--keep-app` | Leave the app running on the tablet on exit |
| `--no-raw-pen` | Use Android's pen events instead of reading the stylus device over adb |
| `--boost <s>` | Keep the tablet's CPU clocks up for this long after pen/touch input (default 20, 0 = off). Decoding on MediaTek tablets is much faster at high clocks, which Android only grants while you touch the screen. Costs some tablet battery while active |
| `--max-in-flight <n>` | Back-pressure: max frames sent but not yet shown (default 3, 0 = off) |
| `--vendor-tweaks mtk-cpu-boost,mtk-vpp-off` | **Opt-in, device-specific** MediaTek decoder settings. `mtk-cpu-boost` saved ~1.9 ms of decode on the MT8781; `mtk-vpp-off` made no measurable difference. Never used for the published comparison numbers |
| `--wifi`, `--pair`, `--unpair <name>`, `--listen <addr>`, `--fec <pct>`, `--no-usb` | Wi-Fi, see [wifi.md](wifi.md#using-wi-fi) |
| `--hud` | Show the stats display (frame rate, latency, Wi-Fi reception) on the tablet. Also a switch in the app's settings, or tap the black bar beside the picture |
| `-v, --verbose` | Repeatable. `-v`: a status line and the detailed statistics every 5 s; `-vv`: debug; `-vvv`: trace, plus FFmpeg's own log |
| `-q, --quiet` | Warnings and errors only |

## Raw pen

With adb available, Waylus reads the tablet's stylus evdev device directly: full sensor resolution, pressure, both tilt axes, hover, buttons and the eraser. This matters on devices whose Android configuration drops part of it; on the NXTPAPER 14 Android reports no X tilt. `--no-raw-pen` (or no adb, as over Wi-Fi without a cable) uses Android's unbuffered `MotionEvent`s.

## VAAPI

- If a VAAPI driver offers no bitrate control (Debian's free `intel-media-va-driver` on older Intel GPUs only does constant QP), Waylus falls back to CQP and says so. `LIBVA_DRIVER_NAME=i965` or `intel-media-va-driver-non-free` may give CBR; this is not verified, and the test laptop's Kaby Lake iGPU (i5-8350U) offered constant QP only.
- `WAYLUS_VAAPI_DEVICE=/dev/dri/renderD129` picks another GPU.
- Measured on an i5-8350U (UHD 620): [measurements](measurements.md#where-the-time-goes).

## Scaling, environment variables and logs

- Monitors larger than the tablet can show or decode (e.g. 4K) are scaled down on the GPU automatically. The tablet reports its decoder's limits at connect; the host logs them (`decoder … (max WxH, N MB/frame, N MB/s)`).
- `WAYLUS_NO_ZEROCOPY=1` forces the CPU readback path.
- Log levels: see `-v`/`-q` under [options](#options). By default there are no periodic statistics, only events and warnings; a video stall over 100 ms is logged as a warning with what held it up (at most one every 10 s, with a count of the others). `-v` prints, every 5 s:
  ```
  53 fps | capture→shown p50 25 / p95 27 ms | pen - ms | usb
  video: 52.8 fps, 17.6 Mbit/s, encode 1.96 ms/frame, capture→sent 2.46 ms, 0 frames without output, held back 0 ms/s for the tablet
  latency: capture→shown p50 24.7 / p95 26.6 ms (n=265) | capture→received p50 4.5 / p95 5.6 ms (n=265) | pen→host - | rtt 0.49 ms
  stages: … | tablet: … (and wifi: … over Wi-Fi)
  ```
  `RUST_LOG` (e.g. `RUST_LOG=waylus=debug`) overrides the level when set; the statistics lines use the target `waylus::stats`. `WAYLUS_FFMPEG_LOG=1` sets FFmpeg's own log level to debug without `-vvv`.
- `pen` in the status line and the stats display is Android's pen events to the host. With the raw stylus reader (adb) there is no such figure; `tools/latency/pen.py` measures that path.
