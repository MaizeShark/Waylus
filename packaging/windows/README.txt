Waylus @VERSION@ for Windows 10/11 (x64)
========================================

LLM-generated code: most of Waylus, including this file, was written by an AI model
(Claude, by Anthropic) working with a human maintainer. It has been tested on real
hardware but not audited line by line.

Waylus uses an Android tablet (Android 11 or newer) as a low-latency pen display, over
USB or Wi-Fi. Documentation: https://github.com/MaizeShark/Waylus/blob/v@VERSION@/docs/windows.md


First run
---------
1. Unpack this folder anywhere and keep the files together: waylus.exe loads the FFmpeg
   DLLs and installs waylus.apk from the folder it is in.
2. Tablet app:
   - With adb (Android platform-tools on PATH, USB debugging on in the tablet's developer
     options): nothing to do. Waylus installs waylus.apk and starts the app itself.
   - Without adb: copy waylus.apk to the tablet once and open it to install it.
3. Plug in the tablet and run, in a terminal in this folder:
       .\waylus.exe --list-outputs      (monitor numbers)
       .\waylus.exe --output 2          (mirror monitor 2: video, pen and touch)
   Without --output Waylus asks once and remembers the choice. Ctrl-C stops it.
   .\waylus.exe --help lists all options.


USB accessory mode: the driver step
-----------------------------------
By default Waylus talks to the tablet in USB accessory mode, which needs the WinUSB
driver on the tablet's accessory interface. Without it Waylus says so and uses adb.

- If Samsung's Android USB driver is installed, nothing to do.
- Otherwise, once:
  1. Run waylus.exe with the tablet attached. It switches the tablet into accessory mode
     and Windows lists an "Android Open Accessory" device.
  2. Start Zadig (https://zadig.akeo.ie/), tick Options > List All Devices, select the
     "Android Open Accessory" entry (interface 0) and install the WinUSB driver.
  3. Run waylus.exe again.
  This changes the driver of that one interface only; adb keeps working.


Wi-Fi
-----
    .\waylus.exe --wifi --pair    once: in the app's Wi-Fi tab tap Pair, enter the PIN
    .\waylus.exe --wifi           USB if plugged in, else Wi-Fi

The first time Waylus listens, Windows Defender Firewall asks whether to allow it. Allow
it on Private networks only, never Public. If the tablet still can't connect, the Wi-Fi
network is probably filed as Public: Settings > Network & internet > Wi-Fi > the network
> Network profile type > Private.


Good to know
------------
- Krita: Settings > Configure Krita > Tablet settings > Tablet Input API > "Windows 8+
  Pointer Input (Windows Ink)", then restart Krita. Otherwise it gets no pressure.
- "INSTALL_FAILED_UPDATE_INCOMPATIBLE": the app on the tablet was signed with another key
  (for example a self-built one). Uninstall it once (adb uninstall de.hut9.waylus, or in
  the tablet's settings), run waylus.exe again and pair Wi-Fi again.
- If waylus.exe was killed with Task Manager's End task and stays locked, unplug the
  tablet once.


Files and licences
------------------
waylus.exe, waylus.apk   Waylus @VERSION@, GNU GPL v3.0 or later (LICENSE)
*.dll                    FFmpeg @FFMPEG_VERSION@, BtbN's GPL shared build, GNU GPL v3.0
                         or later (licenses\FFmpeg-LICENSE.txt)
licenses\                licences of FFmpeg, the Rust crates in waylus.exe (including the
                         patched nusb) and the libraries in the app

Source code: Waylus https://github.com/MaizeShark/Waylus/tree/v@VERSION@ ;
FFmpeg and the libraries built into its DLLs: licenses\FFmpeg-SOURCE.txt
