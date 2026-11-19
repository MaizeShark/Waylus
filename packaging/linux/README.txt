Waylus @VERSION@ for Linux (x86_64), built on Fedora 45
======================================================

LLM-generated code: most of Waylus, including this file, was written by an AI model
(Claude, by Anthropic) working with a human maintainer. It has been tested on real
hardware but not audited line by line.

Waylus uses an Android tablet (Android 11 or newer) as a low-latency pen display, over
USB or Wi-Fi. Documentation: https://github.com/MaizeShark/Waylus/blob/v@VERSION@/docs/linux.md

This binary is linked against Fedora 45's FFmpeg 9 (libavcodec.so.63) and PipeWire. It
runs on Fedora 45 and distributions with the same library versions; elsewhere, build from
source (https://github.com/MaizeShark/Waylus#build-from-source).

Requirements
------------
- A Wayland compositor with the ScreenCast portal (niri, KDE Plasma, GNOME, ...).
- FFmpeg with h264_nvenc (NVIDIA) or h264_vaapi (Intel/AMD). Fedora's ffmpeg-free and
  RPM Fusion's ffmpeg both work.
- Write access to /dev/uinput, for example:
      sudo groupadd -r uinput; sudo usermod -aG uinput $USER
      echo 'KERNEL=="uinput", MODE="0660", GROUP="uinput", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/60-waylus-uinput.rules
  then log in again.
- adb (android-tools) is optional: with it Waylus installs waylus.apk from this folder
  and starts the app. Without it, install waylus.apk on the tablet once by other means.

Run
---
    ./waylus                 pick a monitor once; pen, touch and video over USB
    ./waylus --wifi --pair   once, for Wi-Fi; then ./waylus --wifi
    ./waylus --help          all options
On niri, run ./waylus --niri-setup once.

If installing the app fails with INSTALL_FAILED_UPDATE_INCOMPATIBLE, the app on the tablet
was signed with another key (for example a self-built one): uninstall it once
(adb uninstall de.hut9.waylus), run waylus again and pair Wi-Fi again.

Files and licences
------------------
waylus, waylus.apk   Waylus @VERSION@, GNU GPL v3.0 or later (LICENSE)
licenses/            the Rust crates in waylus (including the patched nusb) and the
                     libraries in the app
Source code: https://github.com/MaizeShark/Waylus/tree/v@VERSION@
