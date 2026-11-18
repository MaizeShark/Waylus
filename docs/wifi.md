# Wi-Fi transport

How to use Waylus over Wi-Fi, then the research and design behind it, and the development
measurements. The measured results are collected in [measurements.md](measurements.md#wi-fi-linux-host-2026-11-17).

Status: **implemented** (2026-11-17, Windows host 2026-11-18). Design agreed 2026-11-17: PIN + SPAKE2 pairing, `--wifi` next to USB, pairing only with `--pair`, IDR recovery first.

Goal: a LAN transport next to AOA/adb with latency close to USB and, above all, a low
tail. Target on a good 5/6 GHz link: capture→photon p50 ≤ USB + ~10 ms (USB: 45 ms),
p95 ≤ ~80 ms, pen→host close to USB (5.1 / 7.0 ms), no multi-hundred-ms stalls after
loss. USB stays the default whenever a cable is connected.

## Using Wi-Fi

USB stays the default: with `--wifi` the host still uses USB when the tablet is plugged in, and additionally accepts the paired tablet over Wi-Fi.

**Pairing (once).** Run `waylus --wifi --pair`. In the Waylus app on the tablet, tap the host under "Pair with …" (found via mDNS), or type its address and tap "Pair with this host". The host prints a 6-digit PIN (and shows a desktop notification); enter it on the tablet. Pairing stays possible for 5 minutes or until one tablet has paired, and closes after 3 wrong PINs. After that, `waylus --wifi` is enough: open the app on the tablet and it connects. The tablet keeps one paired host; pairing it with another host replaces the old pairing.

```sh
waylus --wifi --pair          # pair a tablet (shows a PIN)
waylus --wifi                 # USB if plugged in, else Wi-Fi
waylus --wifi --no-usb        # Wi-Fi only (adb, if present, still installs/starts the app)
waylus --unpair all           # forget paired tablets (or a name / id prefix)
```

Other flags: `--listen ADDR` (default: every private IPv4 address of the host, never public or VPN addresses such as Tailscale), `--fec PCT` (video parity, default 20 %, at least 2 parity packets per block). Ports: TCP and UDP 27183 (`--port`) and mDNS (UDP 5353); open them in your firewall if it blocks them. Fedora Workstation's default zone already allows them. On Windows, see [windows.md, Wi-Fi on Windows](windows.md#wi-fi-on-windows) for the firewall dialog.

**Security model.**
- Only paired tablets can connect. Pairing uses SPAKE2 (RFC 9382) with the PIN: someone on the network who watches the pairing learns nothing that would let them guess the PIN offline, and each attempt tests a single PIN.
- Each session authenticates both sides with the 256-bit pairing key and an ephemeral ECDH exchange (forward secrecy).
- Everything after that is AES-256-GCM: input, control and video. The host drops any packet that fails authentication and never takes a peer's address on trust, so spoofed UDP source addresses or a reused DHCP address get nowhere.
- Keys: `~/.local/state/waylus/wifi.json` (mode 0600) on the host (Windows: `%LOCALAPPDATA%\waylus\wifi.json`); on the tablet, in app storage, wrapped by an Android Keystore key.
- Video is always encrypted. Decrypting costs the tablet 0.2–0.5 ms per frame, and an unencrypted mode would save at most that, so there is none. Anyone on the LAN can still jam or flood the stream (denial of service), as with any Wi-Fi traffic.

**How it works** (details in [section 2](#2-waylus-wi-fi-design)). Pen and touch go over UDP. Each packet repeats the previous two samples and the final state is resent shortly after, so a lost packet costs no retransmission wait. Video frames are encrypted, split into 1408-byte packets and protected with Reed-Solomon parity. A frame that can't be recovered is reported at once, and the host answers with a keyframe; the tablet keeps the last good picture meanwhile. While a Wi-Fi session runs in the foreground, the app holds Android's low-latency Wi-Fi lock (no power saving).

**Getting low latency.**
- Use 5 GHz (or 6 GHz) with a strong signal near the access point; check with `adb shell cmd wifi status` (Frequency, RSSI).
- At a weak spot (−78 dBm in the measurements below), Android keeps scanning for a better access point, and the link stalls for ~80–200 ms every few seconds. A wired host helps, since otherwise every packet crosses the air twice.
- Avoid 2.4 GHz if you can (measured: freezes of 0.2–1 s, [measurements](measurements.md#24-ghz)). If you must, use `--bitrate 10`.
- The raw pen reader (full tilt on the NXTPAPER) needs adb; over Wi-Fi, without adb, the pen uses Android's events.

## 1. What Sunshine / Moonlight do

Read from source on 2026-11-17: LizardByte/Sunshine (`src/stream.cpp`, `src/video.cpp`,
`src/nvenc/nvenc_base.cpp`, `src/nvhttp.cpp`, `src/crypto.cpp`, `src/platform/linux/misc.cpp`),
moonlight-common-c (`src/RtpVideoQueue.c`, `src/ControlStream.c`, `src/InputStream.c`,
`src/Video.h`, `src/Connection.c`, `src/PlatformSockets.c`), moonlight-android
(`Game.java`, `MediaCodecHelper.java`, `MediaCodecDecoderRenderer.java`).

### Video packetization and FEC
- RTP-like UDP packets: 12-byte RTP header (+4 extension), then `NV_VIDEO_PACKET`
  (stream packet index, **frame index**, flags SOF/EOF/PIC_DATA, multi-FEC block
  number/count, `fecInfo` = shard index | data shards | FEC percentage).
- Packet size from the client: 1392 bytes on a LAN (1024 remote IPv4), rounded down to a
  multiple of 16 ("FEC only works in 16 byte chunks").
- Reed-Solomon over GF(2⁸) with **nanors** (sleepybishop, MIT), on both sides. Max 255
  shards per block, so a frame is split into up to **4 FEC blocks** (2-bit field);
  frames needing more get FEC turned off.
- Defaults: **20 % parity** (`fec_percentage`), and the client asks for
  `minRequiredFecPackets = 2`, so even a 1-packet frame gets 2 parity shards. Sunshine
  lowers the encoder bitrate by the FEC share so the on-air rate matches the user's
  setting.
- Pacing: shards go out in `sendmmsg` batches of ≤ 64 packets / 64 KB (GSO limit), at
  most 80 % of 1 Gbit/s in 1 ms groups, i.e. hardly paced on a LAN.
- Client reassembly (`RtpVideoQueue.c`): one frame in flight. Packets of older frames are
  dropped; when a packet of a newer frame arrives, the incomplete frame is abandoned. It
  recovers via RS as soon as `data` shards of any mix have arrived. No reordering buffer,
  no waiting for retransmissions.

### Loss handling
- The client reports a lost frame range (`connectionDetectedFrameLoss`). If RFI is
  enabled it sends an **RFI request** (first..last frame) on the urgent control channel;
  otherwise, or if queuing fails, an **IDR request**. It also predicts loss early
  ("speculative RFI"): once more shards are missing than there is parity, it reports
  the frame lost before the next one arrives (disabled for 5 min after any reordering).
- The decoder output is frozen until recovery: frames that depend on lost data are not
  decoded.
- Encoder side: **RFI exists only with the native NVENC SDK path (Windows)**
  (`nvEncInvalidateRefFrames` for each lost frame, DPB of several refs, one active ref;
  an IDR if the range is longer than the DPB). The FFmpeg paths used on Linux
  (`h264_nvenc`, `h264_vaapi`) don't support it: `invalidate_ref_frames()` just forces an IDR.
- RFI also needs decoder support: moonlight-android enables H.264 RFI only on a few old
  Tegra/Qualcomm OMX decoders (`refFrameInvalidationAvcPrefixes`), and HEVC RFI on
  Exynos/MediaTek/Amlogic. **For Waylus (Linux, FFmpeg, H.264, MediaTek) the Moonlight
  behaviour is: FEC, then IDR.**
- Loss statistics: the client sends a loss-stats message every 50 ms and a per-frame FEC
  status after any recovery (`SS_FRAME_FEC_STATUS`). Sunshine only logs them; there is
  no bitrate or FEC adaptation.
- FFmpeg NVENC encoder settings: `preset` p1-p7, `tune ull`, `zerolatency 1`,
  `delay 0`, `forced-idr 1`, CBR, VBV = one frame. Same as Waylus, except Waylus
  allows a 4-frame VBV. VAAPI: `async_depth 1`, SEI off.

### Input/control channel (ENet)
- ENet = reliable, sequenced UDP with several channels. Moonlight uses it for control
  *and* input, separate from video because they need reliable delivery and ordering
  while video must never wait for a retransmission. Separate channels (generic,
  **urgent** for IDR/RFI, keyboard, mouse, pen, touch, ...) avoid head-of-line blocking
  between them.
- Pen and touch are sent **reliable**. Motion events are batched to ≤ 1 per ms
  ("paradoxically decreases latency by avoiding queuing in ENet"). Only gyro motion is
  sent unreliable.
- A lost packet therefore waits for an ENet retransmission (RTT-based timeout, which on
  Wi-Fi is easily tens of ms). That is a tail-latency cost Waylus can avoid for pen
  input: pen and touch are absolute state, so redundancy works better than
  retransmission (see 2.4).

### Pairing, keys and encryption
- GameStream pairing: the client shows a **4-digit PIN** that the user enters on the
  host. AES key = SHA-256(salt ‖ PIN)[0..16], AES-ECB challenge/response, then both sides
  exchange and pin X.509 certificates. Later sessions use mutual TLS (HTTPS), which also
  carries a random per-session AES-128 key (`rikey`).
- Weakness: someone who records the pairing exchange can brute-force a 4-digit PIN
  offline (10⁴ tries). Waylus should use a PAKE instead.
- Control stream: AES-GCM per ENet packet, IV from a sequence number. Video:
  AES-GCM per shard (12-byte IV, 16-byte tag, IV = 64-bit counter + 'V'), but
  **Sunshine's default on a LAN is no video encryption** (`lan_encryption_mode = NEVER`);
  only remote streams use it by default. No published cost numbers.

### Android specifics (moonlight-android)
- Wi-Fi locks: `WIFI_MODE_FULL_HIGH_PERF` and, on Android 10+,
  `WIFI_MODE_FULL_LOW_LATENCY`, held for the stream (`Game.java`). Low-latency mode
  turns off Wi-Fi power save while the app is in the foreground with the screen on. In
  the measurements above, power save turned out to be the largest Wi-Fi latency effect.
- Receive socket: `SO_RCVBUF` asked as 2048 × packet size (~2.8 MB), stepped down until
  accepted. QoS: `SO_PRIORITY` 5 (video) / 6 (audio) on Linux; Sunshine tags DSCP
  CS5 (40) for video and CS6 (48) for audio.
- Decoder: `KEY_LOW_LATENCY`, vendor low-latency keys (qti, hisi, rtc, generic
  `vendor.low-latency.enable`), `KEY_PRIORITY` 0, operating rate. MediaTek: decode time
  follows the CPU clock (moonlight-android#1241). Waylus already handles this with `--boost`.
- Render: "lowest latency" mode releases every decoded frame immediately. Pacing modes
  only add a buffer. No jitter buffer for video.
- Bitrate: fixed, chosen by the user (default by resolution/fps: 1080p60 → 20 Mbit/s). No
  adaptation.

## 2. Waylus Wi-Fi design

### 2.1 Overview

```
 tablet ──TCP :27183── host     pairing, session handshake (ECDH), then AEAD records with
                                the existing framed messages: Hello, Config, codec config, Layout
 tablet ══UDP :27183══ host     host→tablet: video shards (FEC, per-frame AES-GCM), Ping
                                tablet→host: Pen, Touch, FrameShown, Pong, loss feedback,
                                each datagram AES-GCM sealed
```
- The tablet connects to the host. The host listens only with `--wifi`, and only on
  addresses of LAN interfaces (RFC 1918 / ULA / link-local, not tailscale). `--listen
  ADDR` overrides this.
- The `Link` abstraction stays. The Wi-Fi `Link`'s writer dispatches each message (one
  `write()` = one message, as now) by type: VIDEO → video sender (UDP),
  PING → UDP, everything else → TCP record. Its reader merges TCP records and UDP
  datagrams into one message stream. `session()` and `video_loop()` stay almost
  unchanged. USB keeps its byte format exactly, and `PROTO_VERSION` becomes 5.

### 2.2 Pairing (PIN + SPAKE2)
- `waylus --wifi --pair` turns pairing on until one device has paired (or 5 minutes).
  Without `--pair` the host rejects every unpaired device.
- The tablet (manual address or mDNS) sends `PAIR_REQUEST{device id, name}`. The host
  picks a random **6-digit PIN** and prints it ("Pair 'TCL 9491G'? PIN 482 913"; also
  a desktop notification). The user types it on the tablet.
- **SPAKE2 (RFC 9382) on P-256**, PIN as password. An eavesdropper learns nothing
  usable, and each attempt tests one PIN. A failed attempt invalidates the PIN, and
  after 3 failures pairing turns off. Both sides confirm with key-confirmation MACs.
  Result: a 256-bit `pair_key` per (host, tablet), plus random 128-bit host and device ids.
- Storage: host `~/.local/state/waylus/paired.json` (mode 0600: device id, name, key,
  date; built as `wifi.json`, which also holds the host's identity), `waylus --unpair NAME|all`. Tablet: app-private storage, key wrapped with an
  Android Keystore AES key.
- Libraries: host `p256`, `sha2`, `hkdf`, `hmac` (RustCrypto). Tablet: BouncyCastle
  (`bcprov`, MIT) for the P-256 point arithmetic SPAKE2 needs (the JCA has no point
  addition). Everything else uses the platform JCA (Conscrypt/BoringSSL).

### 2.3 Session handshake and crypto
- Over TCP: tablet → `{device id, nonce_c, ephemeral P-256 pubkey}`, host →
  `{host id, nonce_h, ephemeral pubkey, HMAC(pair_key, transcript)}`, tablet →
  `HMAC(pair_key, transcript ‖ 1)`. Unknown device id or a bad MAC closes the
  connection before any input is read.
- Keys = HKDF-SHA256(ECDH secret ‖ pair_key, nonces): separate AES-256-GCM keys per
  direction and for video, with forward secrecy from the ephemeral ECDH.
- TCP after the handshake: records `[u32 len][ciphertext+tag]`, nonce = direction ‖
  counter, each written with one `write_all` (Nagle off).
- UDP control/input datagram: `[u8 kind][u8 rsv][u16 rsv][u64 seq]` (AAD) +
  AES-GCM(framed messages) + tag. Overhead 28 bytes. Replay window of 64 on `seq`. The
  host sends to, and accepts from, only the source address of the latest
  **authenticated** datagram (handles DHCP changes and roaming, ignores spoofed
  sources). The tablet sends an authenticated UDP hello right after the handshake so
  the host learns its port.
- Video: **one AES-GCM operation per frame** (nonce = 'V' ‖ frame number, AAD = frame
  header), with FEC applied to the ciphertext. One cipher call per frame instead of
  per packet is cheaper on the JCA, where per-call setup dominates small packets. A
  forged or corrupted shard makes the frame fail authentication; that counts as loss
  (IDR), which an attacker on the LAN could cause by jamming anyway. I'll measure the
  cost first (encrypt on host, decrypt on tablet per frame). `--insecure-video` is added
  only if that shows a measurable gain.

### 2.4 Input over UDP: redundancy, not retransmission
- Pen and touch messages are already absolute state (touch = full contact list). Each
  input datagram carries the newest message plus the previous 2 pen/touch messages, and
  the host drops samples whose `t_ns` is not newer than the last one applied. One lost
  datagram is covered by the next (~4 ms later while drawing) without waiting for an RTT.
- The final state (pen lift, out of range, last finger up) is repeated at +5 and +15 ms,
  so a lost last datagram cannot leave a stroke stuck down. The host also already
  resets input when a session ends.
- Each writer flush (already coalesced) = one datagram.
- QoS: DSCP EF (46) on the tablet's input socket (`setTrafficClass`) and on the host's
  feedback/ping, which maps to WMM AC_VO (RFC 8325, cfg80211). Video gets AF41 (34) →
  AC_VI. I'll measure pen latency with and without the tags.
- Raw pen (stylus evdev over adb) still works when adb is reachable (cable or wireless
  debugging). Otherwise the pen uses Android MotionEvents (on the NXTPAPER: no tilt_x).

### 2.5 Video over UDP
- Shard datagram header (16 bytes, plaintext):
  `u8 kind=VIDEO | u8 flags (KEY) | u16 shard index | u32 frame number |
   u8 block | u8 blocks | u8 data shards | u8 parity shards | u32 ciphertext length`,
  then 1408 bytes of shard (16-aligned; datagram 1424 B → IPv4 1452 B, IPv6 1472 B, no
  fragmentation at MTU 1500).
- FEC: Reed-Solomon GF(2⁸), Vandermonde/Backblaze construction: host crate
  `reed-solomon-erasure` (MIT), tablet a port of Backblaze JavaReedSolomon (MIT). Both
  use the same matrix, which I'll verify with shared test vectors. Defaults as
  Sunshine/Moonlight: **20 % parity, at least 2 parity shards per block**, ≤ 200 data
  shards per block (several blocks for large keyframes). `--fec <percent>`.
- Sizes at 20 Mbit/s, 1080p60: average frame ~42 KB = 30 shards + 6 parity; a VBV-limited
  keyframe ~170 KB = 121 + 25.
- Send: `sendmmsg` of the whole frame. Optional in-frame pacing (`--pace <Mbit/s>`,
  default off) if the AP drops bursts. The host is on Wi-Fi itself here, so its own queue
  already paces at its link rate.
- Tablet reassembly, after Moonlight's model: one frame in progress (plus the next, to
  handle reordering within ~1 frame). The frame is complete when `data` shards per block
  have arrived → RS decode only if a data shard is missing → AES-GCM open → decoder.
  A packet of frame N+2 abandons frame N.
- Unrecoverable frame → `FRAME_LOST{frame}`. The host releases it from back-pressure
  (`InFlight`; today a frame lost forever would hold the encoder for the 250 ms
  timeout) and forces an IDR. The tablet freezes on the last good picture and drops
  frames until the keyframe. At most one IDR request per 100 ms, and only while no
  keyframe has arrived.
- Recovery options for later (only if the 2.4 GHz measurements show the IDR bursts
  hurt): NVENC `intra-refresh` (gradual refresh instead of IDRs, no size spike), or the
  native NVENC SDK for real RFI. FFmpeg's VAAPI H.264 encoder has neither.
- Codec config (SPS/PPS) and Config go over TCP. If they arrive after the first UDP
  keyframe, the decoder restart requests a new one (as it does today).

### 2.6 Stats
- The tablet sends `WIFI_STATS` once per second: shards received / missing, FEC
  recoveries (shards and frames), unrecoverable frames, keyframe requests, max gap
  between frames, receive spread per frame (first → last shard).
- The host adds to its 5 s report: loss %, FEC recoveries, unrecoverable frames,
  IDRs sent (requested / initial), encrypt + FEC time per frame, and the Wi-Fi band
  where it can be read (tablet: `WifiInfo.frequency`, link speed, RSSI; sent in `Hello`).

### 2.7 Android
- `WifiManager.createWifiLock(WIFI_MODE_FULL_LOW_LATENCY)`: acquired when a Wi-Fi
  session starts, released when it ends and in `onPause`. USB sessions don't take it.
- UDP receive thread at `THREAD_PRIORITY_URGENT_DISPLAY` (as the USB reader), `SO_RCVBUF`
  4 MB (log the effective size), FEC/decrypt on that thread (≲ 1 ms per frame expected;
  I'll measure), decoder unchanged.
- Transport choice: accessory attached → USB (already switches over in `onNewIntent`);
  started by the host over adb → as requested; else → Wi-Fi to the paired host. UI:
  status text plus a list of discovered hosts, "Pair" with PIN entry, manual address.
- Discovery: `NsdManager` browsing `_waylus._tcp`; the host advertises with the
  `mdns-sd` crate (TXT: host id, proto version), only with `--wifi`. Manual address as
  a fallback. No QR code for now.

### 2.8 Host CLI
- `--wifi`: also listen on the LAN. USB setup works as before when a tablet is plugged
  in. Without a USB tablet (no adb device), the host skips USB and waits on Wi-Fi only.
  Sessions are served from either link, one at a time.
- `--pair`, `--unpair`, `--listen ADDR`, `--fec PCT`, `--pace MBIT`, and maybe
  `--insecure-video` (see 2.3).
- Bitrate: 20 Mbit/s at 1080p as now (+20 % FEC on air). On 2.4 GHz (~40 Mbit/s usable)
  the docs recommend `--bitrate 10`. Adaptation is out of scope for the first version.

### 2.9 Implementation notes (where the build differs from or adds to the plan)
- QoS tags: the host's UDP socket (video, pings) uses AF41 (WMM video); the tablet's
  input/feedback socket uses EF (WMM voice). No clear effect measured yet (§3).
- The app, when started from the launcher while the tablet is still in USB accessory
  mode from an earlier session, uses AUTO: Wi-Fi for the connection, plus a watcher
  thread that sends a SYNC over USB and switches to USB once a host echoes it (~1.3 s
  after the host starts serving USB). A write to the accessory blocks while no host
  reads the pipe and can't be cancelled, so it must never happen on the connection
  thread. A first version probed with a small write and a timeout, but the driver
  buffers a few writes, so the probe passed and the real handshake then blocked
  forever (the app never reconnected; reported by the user 2026-11-17).
- A newer authenticated link preempts the running session on the host (tablet
  reconnects after a Wi-Fi drop or a host restart without waiting for the 5 s idle
  timeout).
- Reconnects measured 2026-11-17: host restart → tablet back ~1 s after the host is
  up; Wi-Fi off/on on the tablet → back ~3.8 s after Wi-Fi is enabled (association +
  DHCP; the tablet got a new address, found again via mDNS).
- Fixed on the way: `Connection` never called `onDisconnected()` after a session
  ended with an error (its `finally` cleared `connected` first).
- Developer aids: `WAYLUS_TEST_DROP=<percent>` drops video shards on the host;
  `am start ... --ez bench_crypto true` benchmarks the tablet's ciphers;
  `--ez wifi_lock false` disables the low-latency lock for comparisons.

## 3. Measurement plan
- Before every run: `adb shell cmd wifi status` (frequency, RSSI, link speed); record the
  host's `iw dev wlp3s0 link`.
- Baseline 2026-11-17, app not running: host on 5260 MHz, −67 dBm, 720/288 Mbit/s tx/rx.
  Tablet on 5260 MHz, −79 dBm, 175 Mbit/s. Ping host→tablet every 200 ms: avg 31 ms,
  max 129 ms (Wi-Fi power save); every 10 ms: avg 3.8, max 40 ms. Host→AP: 1.3 ms.
- Milestone 2: `pen.py` with `--no-video` over Wi-Fi (adb for measurement only), with
  and without DSCP, and with/without the Wi-Fi lock.
- Milestone 3: per-frame crypto cost with/without encryption; loss/FEC stats; injected
  loss on the host (`tc qdisc … netem loss 2%`) to exercise FEC and IDR recovery.
- Milestone 5: `camera.py` (120 fps) and `pen.py` on 5 GHz and 2.4 GHz, compared with USB
  in the measurements table, with method and date (done 2026-11-17, see below and
  [measurements.md](measurements.md#wi-fi-linux-host-2026-11-17)).

### Findings so far

2026-11-17, milestone 2 (paired, encrypted input channel, `--no-video --no-raw-pen`).
Tablet on 5260 MHz, RSSI −77…−79 dBm, link 54–81 Mbit/s (a weak spot). Host on the same
AP, −67 dBm.
- Pairing (PIN + SPAKE2) works end to end. A wrong PIN is rejected on both sides and
  the next attempt gets a new PIN. Rust and Kotlin both pass the RFC 9382 test vector.
- Built-in pen→host statistic (Android event time → host, clocks synced by ping/pong)
  with injected stylus swipes (`adb shell input stylus swipe`, ~800 samples/s):
  **USB (AOA) p50 1.6 / p95 2.5 ms, Wi-Fi p50 3.1 / p95 33–72 ms**. This is a proxy;
  the `pen.py` numbers with real taps are still to do.
- The tail is the tablet's radio link at this spot, not the protocol. Ping host →
  tablet at 250 Hz during a session (lock held): p50 3.0 / p95 13 / p99 153 ms;
  host → AP p99 2.5 ms. The delays come in ~80 ms stall episodes every 2–3 s. Without
  a session (no low-latency lock) they get worse (p95 90 / p99 233 ms).
- Android also runs a Wi-Fi scan every 10–40 s at this RSSI (0.5–3.4 s each, looking
  for a better AP), another source of stalls. A better spot (stronger RSSI) should
  reduce both; to be checked.
- DSCP EF (WMM voice) on pings: no clear effect in 3 alternating runs (spikes both ways).

2026-11-17, milestone 3 (video over UDP, FEC, loss feedback), 1080p60 at 20 Mbit/s,
NVENC. Tablet first on 5680 MHz −79 dBm (39–52 Mbit/s), later 5260 MHz −72 dBm
(263–351 Mbit/s):
- Host per frame: AES-GCM seal + RS parity 0.16–0.22 ms, sending 36 datagrams
  0.2–0.3 ms. Tablet: reassembly + decrypt 0.5–0.6 ms per frame without loss.
- A frame completes as soon as its data shards are in, so parity costs no latency
  without loss. Spread first → last shard of a 51 KB frame: 7 ms at ~45 Mbit/s,
  4–5 ms at ~300 Mbit/s.
- Built-in stats, no loss, weak link: capture→shown p50 38–40 / p95 51 ms
  (USB in the same session setup: 25 / 28 ms).
- Injected loss (`WAYLUS_TEST_DROP`, test only): at 3 % of shards, FEC recovered all
  of them (~210 shards in ~144 frames per 5 s, no frame lost, no keyframe). At 10 %,
  ~650 shards recovered per 5 s, 11–14 frames lost, each answered by one forced IDR
  with ~1 frame skipped before it. No stalls (48–53 fps, capture→shown p95 54–60 ms).
  RS reconstruction in Kotlin adds ~1–1.5 ms to frames that need it.
- Crypto cost (`--ez bench_crypto true`, 41 KB frame, MT8781): AES-GCM open
  0.19–0.21 ms on a high-priority thread, 0.6–0.8 ms at normal priority (clock bound,
  little cores). AES-CTR without authentication costs about the same, and
  ChaCha20-Poly1305 is no faster. In the stream: 0.43–0.51 ms per frame. Host AES-NI:
  negligible. Decision: **video stays encrypted; no `--insecure-video`**. An
  authenticated-only mode would save nothing, and dropping authentication would save
  ≤ 0.5 ms (~1 % of the latency, below what the camera can resolve) for no integrity.

2026-11-17, camera (`camera.py`, 120 fps, pen idle). Tablet 5260 MHz, −66…−69 dBm,
351 Mbit/s; host on the same AP over Wi-Fi (−69 dBm):
- Waylus `--wifi --no-usb`: **p50 66 / p95 96 ms** (p5 30, p99 104; n = 310). No
  shard lost during the clip. Weylus over the same Wi-Fi (Chrome → LAN address):
  **p50 183 / p95 248 ms** (p5 123, p99 273; n = 1574). Waylus USB for reference:
  45 / 59 ms. Target was p50 ≤ ~55 / p95 ≤ ~80: missed by ~10 / ~16 ms on this setup.
- Built-in stats during the clip: capture→sent p50 6–9 ms (USB 2.5; back-pressure,
  the encoder was held ~820 ms/s and sent 42–53 fps), sent→received 8.5–9 ms (p95
  12–17, USB 2.5), decode 18.5–20 ms (pen idle, same as USB), FEC+decrypt 0.7 ms,
  capture→shown p50 ~39 ms (USB ~21–25). The camera adds the same ~25 ms on top as
  for USB (the tablet's compositing and panel).
- Next: `--max-in-flight 3`, a lower bitrate, a wired host; `pen.py` with real taps;
  2.4 GHz.

2026-11-17, back-pressure (`--max-in-flight`): with 2, the longer Wi-Fi pipeline held
the encoder back ~800 ms/s. A/B with built-in stats, `barcode.html`, 3 alternating
runs each, 5260 MHz −67 dBm:
- Wi-Fi: 2 → capture→shown p50 37.4–39.0 / p95 48.8–50.1, capture→sent 6.5–8.2 / ~17,
  53–56 fps; 3 → p50 32.6–32.9 / p95 38.0–39.2, capture→sent 2.3 / 4.6, ~60 fps.
  Sent→received unchanged (~8.3), no queueing at the decoder (wait 0.5 ms).
- USB: 2 → p50 27.4 / p95 32–34; 3 → p50 26.7–26.9 / p95 30.5–30.9.
- Camera (hand-held), Wi-Fi with 3: **p50 49 / p95 65 ms** (p5 26, p99 69; n = 552),
  vs 66 / 96 with 2; three different corner sets gave identical numbers. Within the
  target (USB: 45 / 59). Default changed to 3.

2026-11-17, `pen.py`, 30 real taps each, video on, `--no-raw-pen` (app pen events),
5260 MHz −67 dBm, host on the same AP over Wi-Fi, commit 841f4e0:
- USB (AOA): pen-down → host input device p50 6.8 / p95 8.8 ms (min 2.6, max 9.3).
- Wi-Fi: p50 9.3 / p95 15.7 ms (min 2.8, max 132). The 132 ms tap fell into one of the
  link stalls seen in the ping tests; redundancy can't help when the radio is off the
  channel. Raw pen over a USB adb cable ([measurements](measurements.md#waylus-vs-weylus-niri-2026-11-17)): 5.1 / 7.0 ms.

2026-11-17, 2.4 GHz (tablet on 2462 MHz, 802.11n, −52…−56 dBm, 72–78 Mbit/s; host
still on 5 GHz at the same AP). Built-in stats, `barcode.html`, 2 runs each:
- 20 Mbit/s: capture→shown p50 46–47 / p95 103–105 ms, sent→received p50 16–18 / p95
  76 ms, 41–42 fps, 7–13 frames lost per 35 s (FEC recovered almost nothing).
  10 Mbit/s: p50 40–42 / p95 91–101 ms, 44–47 fps, 1–5 frames lost; a frame takes
  ~5 ms on air instead of ~11.
- Loss diagnostics (logged by the tablet per lost frame): some "losses" were only late
  (declared lost by the 100 ms timeout, the next shard arrived 1–4 ms later), and
  others were real bursts (about half a frame missing while the next frame was
  complete, beyond what 20 % parity can repair). The link stalls for 100–450 ms at
  times. Timeout 100 vs 500 ms (`adb shell setprop debug.waylus.lostms`): p95
  109–114 vs 113–122 ms, within run-to-run noise; the default stays 100 ms.

2026-11-17, 2.4 GHz camera (hand-held; `camera.py --track`), tablet 2462 MHz −52 dBm,
host on 5 GHz at the same AP, `--max-in-flight 3`:
- 20 Mbit/s: p50 103 / p95 282 / p99 331 ms after dropping 188 "outliers" (348–1229
  ms); with them kept p50 112 / p95 807 / p99 1065 ms (n = 1260).
- 10 Mbit/s: p50 79 / p95 134 / p99 173 ms; with outliers kept 81 / 303 / 461 ms
  (n = 971; 75 dropped, 203–523 ms).
- The dropped values cluster: they are freezes (link stalls + keyframe recovery), not
  chance decodes. `camera.py` now prints both readings.
- Conclusion: 5 GHz is the band for Waylus over Wi-Fi; on 2.4 GHz use `--bitrate 10`.
  Possible future work for 2.4 GHz: intra refresh instead of keyframes (smaller
  recovery frames), adaptive bitrate.

## Windows

The Wi-Fi code is shared; what differs on a Windows host (2026-11-18, Windows 11, RTX 4060 desktop on 5 GHz Wi-Fi):

- **Sockets.** `IP_TOS` and `SO_SNDBUF` go through `socket2`, interface addresses through `if-addrs` (up interfaces, private IPv4 only, never loopback, public or CGNAT/VPN addresses). The video UDP socket gets the same 4 MiB send buffer as on Linux.
- **Key store.** `%LOCALAPPDATA%\waylus\wifi.json`. Waylus doesn't change its permissions: the file inherits the folder's ACL (the user, SYSTEM, Administrators; checked with `icacls`), which is the Windows counterpart of mode 0600.
- **DSCP.** Windows ignores `IP_TOS` unless a QoS policy allows the application to set DSCP. The alternative without a policy is qWAVE (`QOSAddSocketToFlow`, `QOSTrafficTypeAudioVideo`). The host tries it for the video flow once the tablet's UDP address is known and logs `qWAVE: UDP flow to … marked` when Windows accepts it; the qWAVE service (`QWAVE`) is manual-start and not running by default, and whether the access point honours the marking was not measured, so the result is reported in the [Windows measurements](measurements.md#wi-fi), not assumed.
- **mDNS.** `mdns-sd` starts and registers the service next to Windows' own mDNS responder (the `Dnscache` service) on port 5353 without errors (whether the tablet finds it: see the measurements). The responder is restricted to the interfaces the host listens on (`IfKind::Addr`), also on Linux, so it never answers on public or VPN interfaces.
- **Firewall.** The first listen raises Windows Defender Firewall's dialog for `waylus.exe`; its default offers Public and Private. If the Wi-Fi network is classified *Public* (it often is), a Private-only rule doesn't cover it and the tablet can't connect. See [windows.md, Wi-Fi on Windows](windows.md#wi-fi-on-windows).

## 4. Licensing
- Waylus stays GPL-3.0-or-later. The design follows Sunshine/Moonlight (GPL-3.0), but
  the plan is to write the code fresh. Any file that ends up porting their code gets a
  header note naming the source and the combined licence.
- New dependencies (RustCrypto, `reed-solomon-erasure`, `mdns-sd`, BouncyCastle,
  JavaReedSolomon) are MIT/Apache-2.0, compatible with GPL-3.0.
