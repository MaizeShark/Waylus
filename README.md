# Waylus

> [!NOTE]
> **LLM-generated code.** Most of this project, including code and documentation, was written by an AI model (Claude, by Anthropic) working with a human maintainer. It has been tested on real hardware but not audited line by line, so review it before relying on it.

*Weylus, but for Way(land).* Use an Android tablet as a **low-latency pen display** for your Linux (Wayland) or Windows 10/11 desktop, over USB or Wi-Fi, with a native Android client.

```
 host (Rust)                                                        tablet (Kotlin)
 ScreenCast portal → PipeWire → NVENC / VAAPI H.264 ─┐  USB (AOA,   ┌→ MediaCodec (HW) → SurfaceView
 (Windows: Graphics Capture → NVENC / AMF / QSV)     ├─ or adb;  ───┤
                                                     │  or Wi-Fi)   │
 virtual pen + touchscreen  ← input events  ←────────┘              └← MotionEvent / raw stylus
```

## What it does

- Mirrors one of your monitors to the tablet with hardware H.264 encode and decode, zero-copy on the GPU. On KDE, GNOME and Windows it can also drive an extra, virtual monitor.
- Sends the pen back with pressure, tilt, hover and both stylus buttons, plus multitouch. On Linux they are real input devices (uinput), so Krita, GIMP, Xournal++ and Inkscape see a regular graphics tablet; on Windows they are Windows Ink pointer devices.
- Connects over USB, either raw USB with Android Open Accessory (no USB debugging needed) or an `adb reverse` tunnel, or over Wi-Fi (paired and encrypted, video over UDP with FEC).
- Runs on Linux with Wayland (tested on niri, KDE Plasma 6.8 and GNOME 51) and on Windows 10/11.

## Latency

Tablet behind the real monitor, filmed at 120 fps (photon to photon), and pen-down on the tablet to the host's input event. TCL NXTPAPER 14 tablet (MediaTek MT8781, 60 Hz), RTX 4060 desktop with NVENC, 1080p at 20 Mbit/s, USB accessory mode, Wi-Fi on 5 GHz with the host on Wi-Fi too. Linux: niri, 75 Hz monitor, 2026-11-17. Windows 11: 60 Hz monitor, 2026-11-18.

| | Photon to photon p50 / p95 | Pen p50 / p95 |
|---|---|---|
| Linux, USB | 45 / 59 ms | 5.1 / 7.0 ms |
| Linux, Wi-Fi | 49 / 65 ms | 9.3 / 15.7 ms |
| Windows, USB | 39 / 59 ms | 8.4 / 14.8 ms |
| Windows, Wi-Fi | 48 / 64 ms | 9.5 / 15.6 ms |

The pen rows were measured at different points: on Linux to the kernel event of the virtual device (USB with the raw stylus reader, Wi-Fi with Android's pen events), on Windows to the pointer message in a window, so the Windows figures are upper bounds. Weylus measured 198 / 278 ms on the same Linux setup. Method, conditions, the per-stage breakdown and comparisons with Weylus, Moreland, UScreen, MoreSpace and Moonlight + Sunshine: [docs/measurements.md](docs/measurements.md).

## Quick start

The tablet needs Android 11 or newer and the Waylus app (`android/`). With `adb` available (USB debugging on), the host installs and starts the app itself; without it, install the APK once by other means.

### Linux

Requirements: a Wayland compositor with the ScreenCast portal, PipeWire, FFmpeg ≥ 7.1 with `h264_nvenc` (NVIDIA) or `h264_vaapi` (Intel / AMD), and write access to `/dev/uinput` ([udev rule](docs/linux.md#requirements)). Build dependencies on Fedora: `pipewire-devel ffmpeg-devel clang-devel`, Rust, and the Android SDK for the app.

```sh
cargo build --release                       # host: target/release/waylus
cd android && ./gradlew assembleDebug       # app: android/app/build/outputs/apk/debug/app-debug.apk
waylus                                      # pick a monitor once; pen, touch and video over USB
```

- **KDE and GNOME** map pen and touch to the mirrored monitor automatically. `waylus --virtual` creates an extra monitor at the tablet's aspect ratio instead.
- **niri:** run `waylus --niri-setup` once. It adds `include optional=true "config.d/90-waylus.kdl"` to your niri config, and Waylus writes its pen/touch mapping to that file while it runs. niri has no virtual monitors; use a dummy plug.

Details, udev rules and all options: [docs/linux.md](docs/linux.md).

### Windows

Requirements: Rust (MSVC), Visual Studio Build Tools, LLVM and an FFmpeg 7.1+ shared build ([setup](docs/windows.md#build)). For USB accessory mode the tablet's accessory interface needs WinUSB: Samsung's Android USB driver provides it, or install it once with Zadig. Without it Waylus uses `adb`.

```powershell
cargo build --release            # with FFMPEG_DIR and LIBCLANG_PATH set
waylus --list-outputs            # monitor numbers
waylus --output 2                # mirror monitor 2: video + pen + touch
```

Krita needs the Windows Ink tablet API. Driver, firewall, virtual monitor and Krita details: [docs/windows.md](docs/windows.md).

### Wi-Fi

```sh
waylus --wifi --pair             # once: the host shows a PIN, enter it in the app
waylus --wifi                    # USB if plugged in, else Wi-Fi
```

Use 5 GHz with a strong signal. Security model and tips: [docs/wifi.md](docs/wifi.md#using-wi-fi).

## Common options

| Option | |
|---|---|
| `--output <name>`, `-o` | Linux: connector such as `DP-2` (niri mapping, `--no-video`). Windows: number or name from `--list-outputs` |
| `--choose` | Show the monitor picker again |
| `--virtual[=WxH]` | Create a virtual monitor instead of mirroring (KDE, GNOME; Windows with an Indirect Display driver) |
| `--no-video` | Pen and touch only, as a plain graphics tablet |
| `--transport aoa\|adb` | USB accessory mode (default) or the `adb reverse` tunnel |
| `--wifi`, `--pair`, `--no-usb`, `--unpair <name>` | Wi-Fi: accept paired tablets, pair one, Wi-Fi only, forget pairings |
| `--encoder <name>` | `auto` (default), `h264_nvenc`, `h264_vaapi` (Linux), `h264_amf`, `h264_qsv` (Windows) |
| `--bitrate <Mbit/s>` | Default 20 at 1080p, scaled with resolution |
| `--fps <n>` | Default: the tablet's refresh rate |
| `--boost <s>` | Keep the tablet's CPU clocks up after input, for faster decoding (default 20, 0 = off) |
| `--no-raw-pen` | Use Android's pen events instead of reading the stylus device over adb |
| `--serial <serial>`, `-s` | adb serial, if several devices are attached |

The full list is in `waylus --help` and [docs/linux.md](docs/linux.md#options).

## Troubleshooting

- **The picture is scaled down.** The stream is never larger than what the tablet's hardware decoder is rated for, and Waylus scales the picture down on the GPU to fit. The NXTPAPER 14's decoder is rated for 14 400 macroblocks per frame, less than its 2400×1600 screen (15 000). The host logs the tablet's decoder limits when it connects. Larger streams also decode more slowly ([measurements](docs/measurements.md#other-linux-tablet-as-monitor-tools-kde-2026-11-18), footnote ¹).
- **Waylus uses adb instead of USB accessory mode.** The tablet doesn't support AOA, or (Windows) the accessory interface has no WinUSB driver: see [docs/windows.md](docs/windows.md#usb-accessory-mode-and-its-driver). On Linux without USB debugging, the accessory device may need a [udev rule](docs/linux.md#usb-accessory-mode-and---transport-adb). The tablet stays in accessory mode until unplugged.
- **Windows: the tablet can't connect over Wi-Fi.** Allow `waylus.exe` in Windows Defender Firewall on Private networks only, and make sure the Wi-Fi network's profile is Private, not Public ([details](docs/windows.md#wi-fi-on-windows)).
- **Windows: `waylus.exe` stays locked after Task Manager's *End task*.** With Samsung's driver a hard-killed process can't finish; unplug the tablet once. Ctrl-C or closing the window exits cleanly.
- **KDE: the stream stops** when a virtual output is set to mirror another output; KWin stops streaming it. Extend the desktop onto the virtual monitor, or mirror the real monitor directly.
- **niri: no virtual monitor.** niri doesn't implement the portal's virtual source; mirror a dummy plug's output instead.
- **No tilt on one axis.** Some tablets' Android configuration drops it (the NXTPAPER 14 reports no X tilt). With adb, Waylus reads the stylus device directly and gets both axes; over Wi-Fi without adb it can't.
- **Krita on Windows has no pressure.** Set *Tablet Input API* to *Windows 8+ Pointer Input (Windows Ink)* ([details](docs/windows.md#pen-buttons-eraser-and-applications)).

Verbose logs: `RUST_LOG=waylus=debug`.

## Status / roadmap

Done: pen, touch and video over USB (AOA and adb) and Wi-Fi; Linux (NVENC, VAAPI) and Windows (NVENC) hosts; virtual monitors on KDE, GNOME and Windows; built-in latency statistics and reproducible measurements, including Wi-Fi on 5 GHz and 2.4 GHz.

Open:
- Wi-Fi: one ~190 ms freeze of undetermined cause in the Moonlight comparison clip, and pen latency over Wi-Fi trails Moonlight's ([details](docs/measurements.md#moonlight--sunshine-wi-fi-kde-2026-11-18)). 2.4 GHz still freezes under loss; intra refresh instead of keyframes is a candidate.
- Hardware decode is the largest stage: on the MT8781 it is CPU-clock-bound (19 ms idle, 11.5 ms while drawing); `--boost` keeps the clocks up for 20 s after input.
- Virtual monitor on niri (needs `RecordVirtual` in niri; use a dummy plug meanwhile).
- AMF and QSV encoders on Windows are untested. A comparison with MoreSpace's hardware (VAAPI) encoder is still open.
- Not measured yet: a wired host over Wi-Fi, USB taking over from Wi-Fi.

## Licence

Waylus is free software, licensed under the **GNU General Public License v3.0 or later**; see [LICENSE](LICENSE).
It is an independent implementation and contains no code from Weylus. [`vendor/nusb`](vendor/nusb/WAYLUS.md) is a patched copy of nusb (MIT or Apache-2.0).
