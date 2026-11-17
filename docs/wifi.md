# Wi-Fi transport: research and design

Status: **design agreed** (2026-11-17): PIN + SPAKE2 pairing, `--wifi` next to USB, pairing only with `--pair`, IDR recovery first. Implementation in progress.

Goal: a LAN transport next to AOA/adb with latency close to USB and, above all, a low
tail. Target on a good 5/6 GHz link: capture→photon p50 ≤ USB + ~10 ms (USB: 45 ms),
p95 ≤ ~80 ms, pen→host close to USB (5.1 / 7.0 ms), no multi-hundred-ms stalls after
loss. USB stays the default whenever a cable is connected.

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
  date), `waylus --unpair NAME|all`. Tablet: app-private storage, key wrapped with an
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
  the README will recommend `--bitrate 10`. Adaptation is out of scope for the first version.

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
  in the README table, with method and date.

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

## 4. Licensing
- Waylus stays GPL-3.0-or-later. The design follows Sunshine/Moonlight (GPL-3.0), but
  the plan is to write the code fresh. Any file that ends up porting their code gets a
  header note naming the source and the combined licence.
- New dependencies (RustCrypto, `reed-solomon-erasure`, `mdns-sd`, BouncyCastle,
  JavaReedSolomon) are MIT/Apache-2.0, compatible with GPL-3.0.
