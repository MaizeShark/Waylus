# Waylus on Windows

The host also runs on Windows 10 (1903 or newer; 11 recommended) with the same app, protocol and transports. Measurements: [measurements.md, Windows](measurements.md#windows).

```
 host (Rust, Windows)                                                tablet (Kotlin, unchanged)
 Windows.Graphics.Capture → D3D11 video processor (BGRA→NV12, scale) ─┐
   → FFmpeg D3D11 frames → NVENC / AMF / QSV ─────────────────────────┤  USB (AOA, adb)  ┌→ MediaCodec → SurfaceView
                                                                      ├─ or Wi-Fi ───────┤
 synthetic pointer devices (pen + touch)  ←  input events  ←──────────┘                  └← MotionEvent / raw stylus
```

## Download

The easy way: `waylus-<version>-windows-x64.zip` from the [releases page](https://github.com/MaizeShark/Waylus/releases). It contains `waylus.exe`, the FFmpeg DLLs it loads (BtbN's GPL build, see `licenses\FFmpeg-SOURCE.txt`), the app as `waylus.apk`, which Waylus installs over adb by itself, and a `README.txt` with the first steps. Unpack it, keep the files together, and continue with [USB accessory mode and its driver](#usb-accessory-mode-and-its-driver) and [Run](#run). For adb, install Google's [platform-tools](https://developer.android.com/tools/releases/platform-tools) and put them on `PATH`; it is optional with USB accessory mode or Wi-Fi.

The exe is linked with the static C runtime, so no Visual C++ redistributable is needed. If the app on the tablet came from a self-built APK, Android refuses to update it with the release APK (`INSTALL_FAILED_UPDATE_INCOMPATIBLE`, different signing key): uninstall it once and pair Wi-Fi again.

## Build

For development. CI builds the same way (`.github/workflows/ci.yml`, job *windows-host*; the FFmpeg build it uses is pinned in `.github/ffmpeg-windows.env`).

1. Install **Rust** (MSVC toolchain), the **Visual Studio Build Tools** (C++), **LLVM** (`winget install LLVM.LLVM`, for bindgen) and an **FFmpeg shared build, version 7.1 or newer**, with headers and import libraries: for example BtbN's `ffmpeg-n8.1-latest-win64-gpl-shared-8.1.zip` from <https://github.com/BtbN/FFmpeg-Builds/releases> (the one tested) or the gyan.dev "full-shared" build. Unpack it, then:
   ```powershell
   $env:FFMPEG_DIR = 'C:\ffmpeg-dev\ffmpeg-n8.1-latest-win64-gpl-shared-8.1'
   $env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
   cargo build --release            # target\release\waylus.exe
   ```
   (vcpkg works as well: `vcpkg install ffmpeg[nvcodec,amf,qsv]:x64-windows`.) At run time `waylus.exe` finds the FFmpeg DLLs on `PATH` or next to itself: copy the `bin\*.dll` files beside it.
2. The **tablet app** is the same APK as on Linux (protocol version 5), installed with `adb` (platform-tools on `PATH`, USB debugging on). Waylus installs an APK it finds next to the binary or the source tree, or uses the installed one.
3. For USB accessory mode (the default), the tablet's accessory interface needs WinUSB; see [USB accessory mode and its driver](#usb-accessory-mode-and-its-driver). Without it Waylus falls back to `adb`.

**Type-checking the Windows code from Linux** (no build, no Windows SDK): `cargo check` needs FFmpeg headers (the Linux ones do; any 7.1+ version) and MinGW's C headers for bindgen, which Fedora has in `mingw64-headers` (`dnf download mingw64-headers` and unpack it with `rpm2cpio | cpio -idm` if you don't want to install it):
```sh
rustup target add x86_64-pc-windows-msvc
mkdir -p /tmp/ffwin/lib && ln -sfn /usr/include/ffmpeg /tmp/ffwin/include
MINGW=/usr/x86_64-w64-mingw32/sys-root/mingw/include
FFMPEG_DIR=/tmp/ffwin BINDGEN_EXTRA_CLANG_ARGS="--target=x86_64-w64-mingw32 -isystem $MINGW -D__X86INTRIN_H" \
  cargo check --target x86_64-pc-windows-msvc
```
`-D__X86INTRIN_H` keeps clang's x86 intrinsics headers out of the bindings (bindgen can't express their AMX types); FFmpeg's API doesn't use them.

## Run

`waylus --list-outputs` shows the monitors (number, GDI name, size, refresh rate, GPU); `waylus --output 2` mirrors one. The numbers follow Windows' device names (`\\.\DISPLAY1`, `\\.\DISPLAY2`, …), so they don't change when you pick another main display or rearrange the monitors; `--output DISPLAY2` works too. Without `--output` Waylus asks once and remembers the choice in `%LOCALAPPDATA%\waylus\output.txt` (`--choose` asks again).

```powershell
waylus --list-outputs
waylus --output 2                # mirror monitor 2: video + pen + touch
waylus --no-video --output 2     # pen and touch only, as a plain graphics tablet
waylus --encoder h264_amf        # force the encoder (AMD); h264_qsv for Intel
```

All other flags are the same as on Linux (`--transport`, `--bitrate`, `--fps`, `--wifi`, `--hud`, `-v`, ...), except the Linux-only `--niri-setup`. The statistics lines quoted in the measurements need `-v`. Ctrl-C removes the `adb reverse`, stops the app and releases the pointer devices.

## How it works

- **Capture** is Windows.Graphics.Capture of one monitor, cursor included. Frames stay on the GPU: the D3D11 video processor converts them to NV12 (BT.709, limited range) and scales them down to what the tablet can decode, and FFmpeg hands the texture to the encoder (`h264_nvenc`, `h264_amf` or `h264_qsv`; `--encoder auto` picks by the GPU's vendor). The encoder settings are the same low-latency ones as on Linux.
- **Encoders.** AMF and QSV are implemented but **untested**: the development machine has an NVIDIA GPU only (the error paths were checked: without an AMD/Intel GPU both stop with a clear message). NVENC is the tested path. QSV needs Intel's graphics driver with the oneVPL runtime.
- **Pen and touch** use `CreateSyntheticPointerDevice` / `InjectSyntheticPointerInput` (Windows 10 1809+): a `PT_PEN` device with pressure (0–1024), tilt, hover and the barrel button, and a `PT_TOUCH` device with 10 contacts (scrolling and pinch-zoom work). Coordinates map to the mirrored monitor's rectangle in virtual-desktop pixels; the process is per-monitor DPI aware. Input can't reach windows of elevated programs unless Waylus runs elevated too.
- **Verified** with `tools/windows/PointerProbe.ps1`, a window that records the `WM_POINTER` messages it receives: `waylus --test-input --output N` (a hidden flag) injects a pressure ramp with tilt, hover with the barrel button, and a two-finger pinch, and the probe sees pressure 0–1024, tilt up to 45°/−22°, the barrel flag, and two touch contacts.

### Pen buttons, eraser and applications

- Windows drops the eraser flags of a synthetic pen, so a stylus's eraser acts like its tip.
- A Windows Ink pen has one barrel button, so both of the stylus's buttons report as that one. As with any Windows Ink pen, the button counts when the pen touches down: hold it, then touch (pressing it mid-stroke ends the stroke and starts a new one with the button).
- GTK applications such as Xournal++ see it as button 3 (in Xournal++: *Stylus → Button 2*) and ignore a press while the pen only hovers; Windows Ink applications such as Krita also read it while hovering.
- **Krita** reads tablets through WinTab by default, which Waylus's pen doesn't provide, so it sees only mouse moves (no pressure, tilt or buttons): set *Settings → Configure Krita → Tablet settings → Tablet Input API* to *Windows 8+ Pointer Input (Windows Ink)* and restart Krita (tested: pressure and tilt work).

## USB accessory mode and its driver

`--transport adb` works with Google's or the vendor's USB driver, as for any Android tooling. The default, **USB accessory mode (AOA)**, needs WinUSB on the tablet's accessory interface, which Android doesn't ask for (it advertises no Microsoft OS descriptors):

- **With Samsung's Android USB driver installed** (the usual "Samsung USB driver" download, which many people have for any Android phone), nothing to do: its package binds WinUSB to the accessory interface (18d1:2d00/2d01, "SAMSUNG Android Open Accessory(AOA) Device"). Samsung's driver also takes the parent composite device (`dg_ssudbus` instead of Microsoft's `usbccgp`), which the upstream USB library, nusb 0.2.9, refuses; Waylus ships a patched copy of nusb for that ([`vendor/nusb/WAYLUS.md`](../vendor/nusb/WAYLUS.md)).
- **Without it**, Windows leaves the accessory interface without a driver; Waylus says so and uses `adb` instead. Install WinUSB once with Zadig:
  1. Run `waylus` once with the tablet attached. It switches the tablet into accessory mode (USB ID 18d1:2d00 or 2d01), and Windows lists an "Android Open Accessory" device.
  2. Start [Zadig](https://zadig.akeo.ie/), tick *Options → List All Devices*, select that **Android Open Accessory** entry (interface 0) and install the **WinUSB** driver.
  3. Run `waylus` again: it takes the accessory link.

  This replaces the driver of that one interface; adb and the tablet's other uses are unaffected.

Samsung's driver never completes a USB transfer that is cancelled the usual way, and that is also what Windows does when a process ends. Waylus therefore aborts its accessory transfers itself when it stops (Ctrl-C, closing the window, the session ending). If it is killed hard instead (Task Manager's *End task*, `taskkill /F`), the process stays half-closed, holding `waylus.exe` and the accessory interface, until the tablet is unplugged; unplug it once and start again.

To switch the tablet into accessory mode at all, Waylus has to open the tablet's own USB interface, which Windows' adb server holds, so it runs `adb kill-server` at that point (the next adb call starts the server again).

The rest of accessory mode (permission dialog, staying in accessory mode until unplugged, reconnects) behaves as on Linux: see [linux.md, USB accessory mode](linux.md#usb-accessory-mode-and---transport-adb).

## Wi-Fi on Windows

`--wifi`, `--pair`, `--unpair`, `--listen`, `--no-usb` and `--fec` work as on Linux, with the same pairing and encryption ([wifi.md](wifi.md#using-wi-fi)). The key store is `%LOCALAPPDATA%\waylus\wifi.json` (the folder's inherited permissions apply: your account, SYSTEM and administrators). The tablet keeps one paired host, so pairing it with the Windows host replaces a Linux pairing.

- **Firewall.** Windows Defender Firewall asks the first time `waylus.exe` listens (TCP and UDP 27183, and mDNS on UDP 5353). Allow it on **Private networks only**; never tick *Public*. Windows often files a home Wi-Fi network as *Public*, and then the Private-only rule doesn't apply: switch the network to Private (Settings → Network & internet → Wi-Fi → the network → Network profile type). If the host starts but the tablet can't connect, look here first: `Get-NetConnectionProfile`, and the rules for `waylus.exe` under *Windows Defender Firewall → Allow an app*.
- **DSCP and qWAVE.** Waylus marks its UDP video datagrams (`IP_TOS`), but Windows ignores that unless a QoS policy allows it, and Waylus doesn't create a policy (that takes administrator rights). Instead it asks Windows' QoS service (qWAVE) to mark the video flow as audio/video traffic once it knows the tablet's address, and logs `qWAVE: UDP flow to … marked as audio/video traffic` when Windows accepts it (it did without administrator rights in testing). Whether the access point honours the marking was not measured. Details: [wifi.md, Windows](wifi.md#windows).
- **mDNS.** Waylus advertises `_waylus._tcp` on the private LAN addresses it listens on only. The tablet's "enter address" fallback works without it.

## Virtual monitor

Waylus can't create a monitor on Windows (that takes a display driver), but an Indirect Display driver can: install [Virtual-Display-Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) and its monitor is captured like any other. Then:

- `waylus --virtual` finds that monitor (the one on an Indirect Display adapter), switches it to the mode that fits the tablet best (same aspect ratio, as large as the tablet's decoder allows, the tablet's refresh rate) and mirrors it. Extend the desktop onto it in Windows' display settings, drag windows there and draw. Changing the mode needs no administrator rights.
- The driver offers only the modes in its `vdd_settings.xml` (by default `C:\VirtualDisplayDriver\`: every `<resolution>` entry, plus the `<g_refresh_rate>` values for each). It lists 1920×1080 and 2560×1440 among others. For a 3:2 tablet such as the NXTPAPER 14 (2400×1600) add the size the stream uses, for example
  ```xml
  <resolution><width>2160</width><height>1440</height><refresh_rate>60</refresh_rate></resolution>
  ```
  and restart the driver (Device Manager → Display adapters → disable and enable "Virtual Display Driver", which needs administrator rights, or the driver's control app). When the exact mode is missing, Waylus logs which size it wanted and uses the closest one. Editing the file could be automated, but loading it takes elevation, so Waylus leaves the file alone.
- 1920×1080 at 60 Hz is a safe choice for any tablet.
