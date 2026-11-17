// SPDX-License-Identifier: GPL-3.0-or-later
//! Connection handling: one link per tablet session (TCP tunnelled over USB by
//! `adb reverse`, or AOA bulk endpoints). Video flows host→tablet, input and stats
//! flow back.

use std::{
    io::Write,
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};

use crate::{
    capture::{Capture, Frame},
    encode::{Encoder, EncoderOpts, Packet},
    input::{AreaMm, Pen, Touch},
    link::Link,
    proto::{self, DeviceMsg, Hello, codec, now_ns, video_flags},
};

/// Pixel density assumed when the client can't report a plausible one (TCL NXTPAPER 14,
/// whose firmware reports bogus dpi values; also a typical tablet density).
const FALLBACK_PX_PER_MM: f32 = 7.95;

/// How the tablet connects.
pub enum Transport {
    /// TCP on 127.0.0.1:port, forwarded by `adb reverse`.
    Tcp(u16),
    Aoa(crate::aoa::Aoa),
}

pub struct Opts {
    pub enc: EncoderOpts,
    /// Forced frame rate; None = follow the tablet's refresh rate.
    pub fps: Option<u32>,
    /// Back-pressure: max frames sent but not yet shown on the tablet (0 = off).
    pub max_in_flight: usize,
    /// proto::tweaks bits: opt-in vendor-specific decoder settings.
    pub vendor_tweaks: u8,
    /// Output (connector) name given with --output, for input mapping in --no-video mode.
    pub output_name: Option<String>,
    /// Raw stylus stream (rawpen.rs); when set, the app's pen messages are ignored.
    pub raw_pen: Option<crate::rawpen::Sink>,
    /// Size reported to the tablet in input-only mode (aspect ratio of the output).
    pub output_size: (u32, u32),
}

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;
/// Capture timestamps of refresh frames, so they can be excluded from latency stats.
type RefreshIds = Arc<Mutex<std::collections::VecDeque<u64>>>;

const REFRESH_INTERVAL_MS: u64 = 100;

/// Frames sent to the tablet but not yet reported shown (capture timestamps, oldest
/// first). Used for back-pressure: encoding the next frame waits until the decoder has
/// caught up, so a slow decoder costs frame rate instead of piling up latency.
#[derive(Default)]
struct InFlight {
    /// (capture_ns, sent_ns) of frames not yet reported shown.
    sent: Mutex<std::collections::VecDeque<(u64, u64)>>,
    cv: std::sync::Condvar,
}

impl InFlight {
    fn sent(&self, capture_ns: u64) {
        self.sent.lock().unwrap().push_back((capture_ns, now_ns()));
    }

    /// Frame `capture_ns` was shown; anything older was shown or dropped by the tablet.
    /// Returns when that frame was sent.
    fn shown(&self, capture_ns: u64) -> Option<u64> {
        let mut q = self.sent.lock().unwrap();
        let mut sent_at = None;
        while let Some(&(c, t)) = q.front() {
            if c > capture_ns {
                break;
            }
            if c == capture_ns {
                sent_at = Some(t);
            }
            q.pop_front();
        }
        self.cv.notify_all();
        sent_at
    }

    /// Blocks while `max` or more frames are in flight. Returns the time spent
    /// waiting. Gives up after 250 ms (a lost report must not stall the stream).
    fn wait_below(&self, max: usize, stop: &AtomicBool) -> Duration {
        let start = Instant::now();
        let mut q = self.sent.lock().unwrap();
        while q.len() >= max && !stop.load(Ordering::Relaxed) {
            if start.elapsed() > Duration::from_millis(250) {
                q.clear();
                break;
            }
            q = self.cv.wait_timeout(q, Duration::from_millis(20)).unwrap().0;
        }
        start.elapsed()
    }
}
const REFRESH_FRAMES: u32 = 3;

fn send(w: &Writer, f: impl FnOnce(&mut Box<dyn Write + Send>) -> std::io::Result<()>) -> std::io::Result<()> {
    let mut g = w.lock().unwrap();
    f(&mut g)
}

/// Creates the capture once the first tablet has said hello (virtual monitors are
/// sized after the tablet).
pub type LazyCapture = Box<dyn Fn(&Hello) -> Result<Capture>>;

pub fn serve(opts: Opts, transport: Transport, mut capture: Option<Capture>, lazy: Option<LazyCapture>) -> Result<()> {
    let mut next_link: Box<dyn FnMut() -> Result<Link>> = match transport {
        Transport::Tcp(port) => {
            let listener = TcpListener::bind(("127.0.0.1", port)).with_context(|| format!("bind port {port}"))?;
            info!("waiting for the tablet on 127.0.0.1:{port} (adb reverse)");
            Box::new(move || loop {
                match listener.accept() {
                    Ok((s, _)) => return Ok(Link::tcp(s)?),
                    Err(e) => warn!("accept: {e}"),
                }
            })
        }
        Transport::Aoa(mut aoa) => {
            if !aoa.is_connected() {
                aoa.connect(None)?;
            }
            info!("waiting for the Waylus app on the USB accessory link");
            Box::new(move || aoa.next_link())
        }
    };
    let mut inputs: Option<(Arc<Mutex<Pen>>, Touch)> = None;
    let mut last_frame: Option<Frame> = None;
    loop {
        let link = next_link()?;
        info!("tablet connected");
        match session(&opts, &mut capture, lazy.as_deref(), link, &mut inputs, &mut last_frame) {
            Ok(()) => info!("tablet disconnected"),
            Err(e) => warn!("session ended: {e:#}"),
        }
        if let Some(sink) = &opts.raw_pen {
            sink.lock().unwrap().take();
        }
        if let Some((pen, touch)) = inputs.as_mut() {
            let _ = pen.lock().unwrap().reset();
            let _ = touch.reset();
        }
        if capture.as_ref().is_some_and(|c| !c.running.load(Ordering::SeqCst)) {
            bail!("screen capture stopped");
        }
    }
}

fn wait_source_size(cap: &Capture) -> Option<(u32, u32)> {
    for _ in 0..40 {
        let v = cap.source_size.load(Ordering::SeqCst);
        if v != 0 {
            return Some(((v >> 32) as u32, v as u32));
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Stream size for a `src`-sized capture: never larger than the area the video
/// covers on the tablet, within the decoder's size and macroblock-rate limits
/// at `fps`, never upscaled, aspect ratio kept.
pub fn stream_size(src: (u32, u32), h: &Hello, fps: u32) -> (u32, u32) {
    let (sw, sh) = (src.0 as f64, src.1 as f64);
    let (rw, rh) = fit(src, (h.surface_w as u32, h.surface_h as u32));
    let mut s = (rw as f64 / sw).min(rh as f64 / sh).min(1.0);
    if h.max_w > 0 && h.max_h > 0 {
        s = s.min(h.max_w as f64 / sw).min(h.max_h as f64 / sh);
    }
    loop {
        let (w, hh) = if s >= 1.0 { src } else { (((sw * s).round() as u32) & !1, ((sh * s).round() as u32) & !1) };
        let mb = w.div_ceil(16) * hh.div_ceil(16);
        let fits_frame = h.max_mb_per_frame == 0 || mb <= h.max_mb_per_frame;
        let fits_rate = h.max_mb_per_sec == 0 || mb as u64 * fps as u64 <= h.max_mb_per_sec as u64;
        if (fits_frame && fits_rate) || s < 0.1 {
            return (w.max(2), hh.max(2));
        }
        s = s.min(1.0) * 0.98;
    }
}

/// Largest rectangle with the video's aspect ratio that fits the surface.
fn fit(video: (u32, u32), surface: (u32, u32)) -> (f32, f32) {
    let (vw, vh) = (video.0 as f32, video.1 as f32);
    let (sw, sh) = (surface.0 as f32, surface.1 as f32);
    let s = (sw / vw).min(sh / vh);
    (vw * s, vh * s)
}

fn session(
    opts: &Opts,
    capture_slot: &mut Option<Capture>,
    lazy: Option<&dyn Fn(&Hello) -> Result<Capture>>,
    link: Link,
    inputs: &mut Option<(Arc<Mutex<Pen>>, Touch)>,
    last_frame: &mut Option<Frame>,
) -> Result<()> {
    let Link { mut reader, writer, close, sync } = link;
    // The app repeats SYNC + hello until it sees our answer; extra copies may follow.
    let mut repeats = 0u32;
    let mut buf = Vec::new();
    let hello = match proto::read_msg(&mut reader, &mut buf).context("waiting for hello")? {
        DeviceMsg::Hello(h) => h,
        m => bail!("expected hello, got {m:?}"),
    };
    info!(
        "client v{} surface {}x{} @ {:.1} Hz, decoder {} (max {}x{}, {} MB/frame, {} MB/s)",
        hello.version,
        hello.surface_w,
        hello.surface_h,
        hello.refresh_hz,
        hello.decoder,
        hello.max_w,
        hello.max_h,
        hello.max_mb_per_frame,
        hello.max_mb_per_sec
    );
    if hello.version != proto::PROTO_VERSION {
        bail!("protocol mismatch: host {} client {}", proto::PROTO_VERSION, hello.version);
    }
    if capture_slot.is_none()
        && let Some(make) = lazy
    {
        *capture_slot = Some(make(&hello)?);
    }
    let capture = capture_slot.as_ref();

    let video_size = match (capture, last_frame.as_ref()) {
        (Some(_), Some(f)) => (f.width, f.height),
        _ => opts.output_size,
    };
    if inputs.is_none() {
        let (w, h) = fit(video_size, (hello.surface_w as u32, hello.surface_h as u32));
        let ppmm = if (2.0..=60.0).contains(&hello.px_per_mm) { hello.px_per_mm } else { FALLBACK_PX_PER_MM };
        let area = AreaMm { width: w / ppmm, height: h / ppmm };
        info!(
            "creating virtual pen + touchscreen ({:.0}x{:.0} mm, {})",
            area.width,
            area.height,
            if ppmm == hello.px_per_mm { format!("{ppmm:.2} px/mm reported by the tablet") } else { format!("assumed {ppmm} px/mm") }
        );
        *inputs = Some((Arc::new(Mutex::new(Pen::new(area)?)), Touch::new(area)?));
    }
    let (pen, touch) = inputs.as_mut().unwrap();
    if crate::kde::is_kde() {
        let target = match capture {
            Some(cap) => match cap.geometry.position {
                Some(p) => crate::kde::output_at(p, cap.geometry.size),
                // Virtual monitors have no position yet; find KWin's output by size.
                None => wait_source_size(cap).and_then(crate::kde::virtual_output),
            },
            None => opts.output_name.clone(),
        };
        match target {
            Some(t) => crate::kde::map_devices(t),
            None => warn!("KDE: couldn't tell which output is shown; map the Waylus pen/touch in System Settings"),
        }
    }
    if crate::gnome::is_gnome() {
        let pos = capture.and_then(|c| c.geometry.position);
        let virtual_size = if capture.is_some() && pos.is_none() { capture.and_then(wait_source_size) } else { None };
        let named = opts.output_name.clone();
        crate::gnome::map_devices_with(move || match (pos, virtual_size) {
            (Some(p), _) => crate::gnome::output_at(p),
            (None, Some(sz)) => crate::gnome::virtual_output(sz),
            _ => named.clone(),
        });
    }

    let writer: Writer = Arc::new(Mutex::new(writer));
    let stop = Arc::new(AtomicBool::new(false));
    let keyframe = Arc::new(AtomicBool::new(false));
    let refresh_ids: RefreshIds = Default::default();
    let in_flight: Arc<InFlight> = Default::default();

    if capture.is_none() {
        send(&writer, |w| {
            proto::write_config(w, codec::NONE, opts.output_size.0 as u16, opts.output_size.1 as u16, 0)
        })?;
    }

    let video = capture.map(|cap| {
        let (writer, stop, keyframe, close) = (writer.clone(), stop.clone(), keyframe.clone(), close.clone());
        let frames = cap.frames.clone();
        let recycle = cap.recycle.clone();
        let capture_running = cap.running.clone();
        let refresh_ids = refresh_ids.clone();
        let in_flight = in_flight.clone();
        let max_in_flight = opts.max_in_flight;
        let vendor_tweaks = opts.vendor_tweaks;
        let mut enc_opts = opts.enc.clone();
        enc_opts.fps = opts.fps.unwrap_or(if hello.refresh_hz >= 1.0 { hello.refresh_hz.round() as u32 } else { 60 });
        let pace = cap.pace_ns.clone();
        let mut stream_dims = None;
        if let Some(src) = wait_source_size(cap) {
            let (w, h) = stream_size(src, &hello, enc_opts.fps);
            cap.target_size.store(if (w, h) == src { 0 } else { (w as u64) << 32 | h as u64 }, Ordering::SeqCst);
            if (w, h) != src {
                info!("scaling {}x{} → {w}x{h} to fit the tablet's screen/decoder", src.0, src.1);
            }
            stream_dims = Some((w, h));
        }
        // Frames captured before the target size was set (or kept from a previous
        // session) may have the wrong size; they would start a throwaway encoder.
        let wrong_size = |f: &Frame| stream_dims.is_some_and(|d| (f.width, f.height) != d);
        if let Some(f) = cap.frames.take_timeout(Duration::ZERO)
            && !wrong_size(&f)
        {
            cap.frames.put(f);
        }
        let hello = hello.clone();
        let first = last_frame.take().filter(|f| !wrong_size(f));
        thread::Builder::new()
            .name("video".into())
            .spawn(move || {
                let r = video_loop(&mut enc_opts, &pace, &hello, &frames, &recycle, first, &writer, &stop, &keyframe, &capture_running, &refresh_ids, &in_flight, max_in_flight, vendor_tweaks);
                stop.store(true, Ordering::SeqCst);
                close();
                r
            })
            .expect("spawn video thread")
    });

    let pinger = {
        let (writer, stop) = (writer.clone(), stop.clone());
        thread::Builder::new().name("ping".into()).spawn(move || {
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) {
                if send(&writer, |w| proto::write_ping(w, now_ns())).is_err() {
                    break;
                }
                n += 1;
                // Burst at start for a quick clock estimate, then 2 Hz.
                thread::sleep(Duration::from_millis(if n < 10 { 50 } else { 500 }));
            }
        })?
    };

    let mut stats = Stats::default();
    let res: Result<()> = (|| loop {
        let msg = proto::read_msg(&mut reader, &mut buf)?;
        match msg {
            DeviceMsg::Pen(s) => {
                if opts.raw_pen.is_none() {
                    pen.lock().unwrap().handle(&s)?;
                    stats.pen(&s);
                }
            }
            DeviceMsg::Layout(layout) => {
                debug!("tablet layout {layout:?}");
                if let Some(sink) = &opts.raw_pen {
                    let mut g = sink.lock().unwrap();
                    match g.as_mut() {
                        Some((l, _)) => *l = layout,
                        None => {
                            let pen = pen.clone();
                            *g = Some((
                                layout,
                                Box::new(move |s| {
                                    if let Err(e) = pen.lock().unwrap().handle(&s) {
                                        warn!("pen: {e}");
                                    }
                                }),
                            ));
                        }
                    }
                }
            }
            DeviceMsg::Touch { points, .. } => touch.handle(&points)?,
            DeviceMsg::Pong { host_ns, dev_ns } => stats.pong(host_ns, dev_ns),
            DeviceMsg::FrameShown { capture_ns, recv_ns, render_ns, queued_ns, decoded_ns } => {
                if let Some(sent) = in_flight.shown(capture_ns) {
                    stats.sent(capture_ns, sent, recv_ns);
                }
                stats.tablet(recv_ns, queued_ns, decoded_ns, render_ns);
                let mut ids = refresh_ids.lock().unwrap();
                if let Some(i) = ids.iter().position(|&x| x == capture_ns) {
                    ids.remove(i);
                } else {
                    stats.frame(capture_ns, recv_ns, render_ns);
                }
                while ids.len() > 64 {
                    ids.pop_front();
                }
            }
            DeviceMsg::RequestKeyframe => {
                debug!("tablet requested keyframe");
                keyframe.store(true, Ordering::SeqCst);
            }
            DeviceMsg::Hello(_) if repeats > 0 => repeats -= 1,
            DeviceMsg::Hello(_) => warn!("duplicate hello"),
            DeviceMsg::Sync(n) if Some(n) == sync => repeats += 1,
            DeviceMsg::Sync(_) => {
                info!("tablet restarted the session");
                return Ok(());
            }
            DeviceMsg::Unknown(t) => debug!("ignoring message type {t}"),
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        stats.maybe_report();
    })();

    stop.store(true, Ordering::SeqCst);
    close();
    let _ = pinger.join();
    if let Some(v) = video {
        match v.join() {
            Ok(Ok(f)) => *last_frame = f,
            Ok(Err(e)) => warn!("video: {e:#}"),
            Err(_) => warn!("video thread panicked"),
        }
    }
    match res {
        Err(e) if is_disconnect(&e) => Ok(()),
        r => r,
    }
}

fn is_disconnect(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn video_loop(
    opts: &mut EncoderOpts,
    pace: &std::sync::atomic::AtomicU64,
    hello: &Hello,
    frames: &crate::slot::LatestSlot<Frame>,
    recycle: &crossbeam_channel::Sender<Vec<u8>>,
    first: Option<Frame>,
    writer: &Writer,
    stop: &AtomicBool,
    keyframe: &AtomicBool,
    capture_running: &AtomicBool,
    refresh_ids: &RefreshIds,
    in_flight: &InFlight,
    max_in_flight: usize,
    vendor_tweaks: u8,
) -> Result<Option<Frame>> {
    let mut enc: Option<Encoder> = None;
    let mut packets: Vec<Packet> = Vec::new();
    let mut last: Option<Frame> = None;
    let mut pending = first;
    // Static screens produce no new frames; re-encode the last one a few times so the
    // quality converges (rate control can spend bits on residuals instead of new content).
    let mut refreshes_left = 0u32;
    let mut window = Instant::now();
    let (mut n, mut bytes, mut enc_ns, mut sent_lag_ns, mut pkts, mut empty) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut held = Duration::ZERO;

    while !stop.load(Ordering::Relaxed) {
        // Back-pressure: wait for the tablet to catch up *before* picking a frame, so
        // the one we encode next is the newest capture rather than a stale backlog.
        if max_in_flight > 0 {
            held += in_flight.wait_below(max_in_flight, stop);
        }
        let frame = match pending.take() {
            Some(f) => f,
            None => match frames.take_timeout(Duration::from_millis(REFRESH_INTERVAL_MS)) {
                Some(f) => f,
                None if !capture_running.load(Ordering::Relaxed) => bail!("screen capture stopped"),
                None => {
                    if let (Some(e), Some(f)) = (enc.as_mut(), last.as_ref())
                        && refreshes_left > 0
                    {
                        refreshes_left -= 1;
                        let ns = now_ns();
                        refresh_ids.lock().unwrap().push_back(ns);
                        packets.clear();
                        e.encode(f, ns, &mut packets)?;
                        for p in &packets {
                            let flags = if p.key { video_flags::KEY } else { 0 };
                            send(writer, |w| {
                                proto::write_video(w, p.capture_ns, flags, &p.data)?;
                                w.flush()
                            })?;
                            in_flight.sent(p.capture_ns);
                        }
                    }
                    continue;
                }
            },
        };
        refreshes_left = REFRESH_FRAMES;
        if enc.as_ref().is_none_or(|e| !e.matches(&frame)) {
            let mb = frame.width.div_ceil(16) * frame.height.div_ceil(16);
            if hello.max_mb_per_frame > 0 && mb > hello.max_mb_per_frame {
                warn!(
                    "{}x{} ({mb} macroblocks) exceeds the tablet decoder limit of {}; decoding may fail",
                    frame.width, frame.height, hello.max_mb_per_frame
                );
            }
            // Frame rate: the tablet's refresh rate, limited by its decoder's throughput.
            if hello.max_mb_per_sec > 0 && mb > 0 {
                let dec_max = (hello.max_mb_per_sec / mb).max(1);
                if dec_max < opts.fps {
                    warn!("{}x{} decodes at most {dec_max} fps on the tablet; capping from {}", frame.width, frame.height, opts.fps);
                    opts.fps = dec_max;
                }
            }
            pace.store(1_000_000_000 / opts.fps.max(1) as u64, Ordering::Relaxed);
            info!("streaming at {} fps", opts.fps);
            let hw = frame.gpu.as_ref().map(crate::cuda::frames_ctx_of).transpose()?;
            let e = Encoder::new(opts, frame.width, frame.height, frame.format, hw)?;
            send(writer, |w| proto::write_config(w, codec::H264, frame.width as u16, frame.height as u16, vendor_tweaks))?;
            if let Some(x) = e.extradata() {
                send(writer, |w| proto::write_video(w, 0, video_flags::CONFIG, &x))?;
            }
            enc = Some(e);
        }
        let took = now_ns();
        let frame_capture_ns = frame.capture_ns;
        let e = enc.as_mut().unwrap();
        if keyframe.swap(false, Ordering::SeqCst) {
            e.request_keyframe();
        }
        let t0 = now_ns();
        packets.clear();
        e.encode(&frame, frame.capture_ns, &mut packets)?;
        let t1 = now_ns();
        enc_ns += t1 - t0;
        if packets.is_empty() {
            empty += 1;
        }
        for p in &packets {
            let flags = if p.key { video_flags::KEY } else { 0 };
            bytes += p.data.len() as u64;
            send(writer, |w| {
                proto::write_video(w, p.capture_ns, flags, &p.data)?;
                w.flush()
            })?;
            in_flight.sent(p.capture_ns);
            tracing::trace!("sent frame {} bytes={} at {}", p.capture_ns, p.data.len(), now_ns());
            sent_lag_ns += now_ns().saturating_sub(p.capture_ns);
            tracing::trace!(
                "frame: slot wait {:.2} ms, encode {:.2} ms, write {:.2} ms",
                (took - frame_capture_ns) as f64 / 1e6,
                (t1 - t0) as f64 / 1e6,
                (now_ns() - t1) as f64 / 1e6
            );
            pkts += 1;
        }
        n += 1;
        if let Some(old) = last.replace(frame) {
            let _ = recycle.try_send(old.data);
        }
        if window.elapsed() >= Duration::from_secs(5) {
            let secs = window.elapsed().as_secs_f64();
            info!(
                "video: {:.1} fps, {:.1} Mbit/s, encode {:.2} ms/frame, capture→sent {:.2} ms, {} frames without output, held back {:.0} ms/s for the tablet",
                n as f64 / secs,
                bytes as f64 * 8.0 / secs / 1e6,
                enc_ns as f64 / n.max(1) as f64 / 1e6,
                sent_lag_ns as f64 / pkts.max(1) as f64 / 1e6,
                empty,
                held.as_secs_f64() * 1e3 / secs
            );
            window = Instant::now();
            held = Duration::ZERO;
            (n, bytes, enc_ns, sent_lag_ns, pkts, empty) = (0, 0, 0, 0, 0, 0);
        }
    }
    Ok(last)
}

#[derive(Default)]
struct Stats {
    /// device_clock - host_clock, from the ping with the smallest RTT.
    offset: Option<i64>,
    best_rtt: u64,
    pongs: u32,
    frame_lat: Vec<f64>,
    net_lat: Vec<f64>,
    pen_lat: Vec<f64>,
    /// Host: capture → sent; link: sent → received (needs the clock offset).
    h_send: Vec<f64>,
    link: Vec<f64>,
    /// On the tablet: received → queued into decoder → decoded → shown.
    t_queue: Vec<f64>,
    t_decode: Vec<f64>,
    t_display: Vec<f64>,
    last_report: Option<Instant>,
}

impl Stats {
    fn pong(&mut self, host_ns: u64, dev_ns: u64) {
        let now = now_ns();
        let rtt = now.saturating_sub(host_ns);
        self.pongs += 1;
        // Prefer low-RTT samples, but let the estimate age so drift is tracked.
        if self.offset.is_none() || rtt <= self.best_rtt || self.pongs.is_multiple_of(32) {
            self.best_rtt = rtt;
            self.offset = Some(dev_ns as i64 - (host_ns + rtt / 2) as i64);
        }
    }

    fn to_host(&self, dev_ns: u64) -> Option<i64> {
        self.offset.map(|o| dev_ns as i64 - o)
    }

    fn frame(&mut self, capture_ns: u64, recv_ns: u64, render_ns: u64) {
        if let (Some(r), Some(v)) = (self.to_host(render_ns), self.to_host(recv_ns)) {
            self.frame_lat.push((r - capture_ns as i64) as f64 / 1e6);
            self.net_lat.push((v - capture_ns as i64) as f64 / 1e6);
        }
    }

    fn sent(&mut self, capture_ns: u64, sent_ns: u64, recv_dev_ns: u64) {
        self.h_send.push(sent_ns.saturating_sub(capture_ns) as f64 / 1e6);
        if let Some(r) = self.to_host(recv_dev_ns) {
            self.link.push((r - sent_ns as i64) as f64 / 1e6);
            tracing::trace!("link frame {capture_ns} {:.2} ms", (r - sent_ns as i64) as f64 / 1e6);
        }
    }

    /// Stage durations on the tablet (all device clock, no offset needed).
    fn tablet(&mut self, recv: u64, queued: u64, decoded: u64, render: u64) {
        if queued == 0 || decoded == 0 || render < decoded || decoded < queued || queued < recv {
            return;
        }
        let ms = |a: u64, b: u64| (b - a) as f64 / 1e6;
        self.t_queue.push(ms(recv, queued));
        self.t_decode.push(ms(queued, decoded));
        self.t_display.push(ms(decoded, render));
    }

    fn pen(&mut self, s: &proto::PenSample) {
        if let Some(t) = self.to_host(s.t_ns) {
            self.pen_lat.push((now_ns() as i64 - t) as f64 / 1e6);
        }
    }

    fn maybe_report(&mut self) {
        let due = self.last_report.is_none_or(|t| t.elapsed() >= Duration::from_secs(5));
        if !due {
            return;
        }
        self.last_report = Some(Instant::now());
        let fmt = |v: &mut Vec<f64>| -> String {
            if v.is_empty() {
                return "-".into();
            }
            v.sort_by(|a, b| a.total_cmp(b));
            let p = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
            let s = format!("p50 {:.1} / p95 {:.1} ms (n={})", p(0.5), p(0.95), v.len());
            v.clear();
            s
        };
        if !self.frame_lat.is_empty() || !self.pen_lat.is_empty() {
            info!(
                "latency: capture→shown {} | capture→received {} | pen→host {} | rtt {:.2} ms",
                fmt(&mut self.frame_lat),
                fmt(&mut self.net_lat),
                fmt(&mut self.pen_lat),
                self.best_rtt as f64 / 1e6
            );
        }
        if !self.h_send.is_empty() {
            info!("stages: capture→sent {} | sent→received {}", fmt(&mut self.h_send), fmt(&mut self.link));
        }
        if !self.t_decode.is_empty() {
            info!(
                "tablet: wait for decoder {} | decode {} | decoded→shown {}",
                fmt(&mut self.t_queue),
                fmt(&mut self.t_decode),
                fmt(&mut self.t_display)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nxtpaper() -> Hello {
        Hello {
            version: 2,
            surface_w: 2400,
            surface_h: 1600,
            max_w: 2560,
            max_h: 1440,
            max_mb_per_frame: 14400,
            max_mb_per_sec: 864000,
            refresh_hz: 60.0,
            decoder: "c2.mtk.avc.decoder".into(),
            px_per_mm: 8.1,
        }
    }

    #[test]
    fn stream_sizes() {
        let t = nxtpaper();
        // 1080p fits as is (smaller than the 2400x1350 video area).
        assert_eq!(stream_size((1920, 1080), &t, 60), (1920, 1080));
        // 1440p and 4K shrink to the area the video covers on the tablet.
        assert_eq!(stream_size((2560, 1440), &t, 60), (2400, 1350));
        assert_eq!(stream_size((3840, 2160), &t, 60), (2400, 1350));
        // A 120 Hz tablet with the same decoder must trade resolution for rate.
        let (w, h) = stream_size((3840, 2160), &t, 120);
        assert!(w.div_ceil(16) * h.div_ceil(16) * 120 <= 864000, "{w}x{h}");
        // Lenovo TB125FU: 2000x1200 screen, 4K decoder.
        let l = Hello { surface_w: 2000, surface_h: 1200, max_w: 3840, max_h: 2160, max_mb_per_frame: 32400, max_mb_per_sec: 983040, ..nxtpaper() };
        assert_eq!(stream_size((3840, 2160), &l, 60), (2000, 1124));
    }
}
