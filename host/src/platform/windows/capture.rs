// SPDX-License-Identifier: GPL-3.0-or-later
//! Screen capture with Windows.Graphics.Capture (WGC) of one monitor.
//!
//! WGC hands us each new desktop frame (cursor included) as a D3D11 texture. We copy it
//! into our own texture, convert/scale it to NV12 on the GPU with the D3D11 video
//! processor straight into an FFmpeg D3D11 frame, and publish that in the
//! [`LatestSlot`](crate::slot::LatestSlot); stale frames are dropped, never queued. The
//! encoder then reads the same texture (NVENC / AMF / QSV), so pixels never reach system
//! memory. WGC only produces frames when the screen changes, like the Linux screencast;
//! the video loop re-sends the last frame when the screen is static.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use windows::{
    Foundation::{TimeSpan, TypedEventHandler},
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Graphics::{Direct3D11::ID3D11Texture2D, Dxgi::IDXGIDevice, Gdi::HMONITOR},
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize,
        },
    },
    core::{IInspectable, Interface},
};

use super::{
    d3d::{Converter, Gpu, HwFrames, gpu_for_monitor},
    monitors,
};
use crate::{
    frame::{Capture, Frame, PixFmt, StreamGeometry},
    platform::CaptureRequest,
    proto::now_ns,
    slot::LatestSlot,
};

/// The WinRT Direct3D device is agile (usable from any thread) but not marked `Send`.
struct AgileDevice(IDirect3DDevice);
unsafe impl Send for AgileDevice {}
unsafe impl Sync for AgileDevice {}
impl AgileDevice {
    // A method, so closures capture the whole wrapper (edition 2024 captures disjoint fields).
    fn get(&self) -> &IDirect3DDevice {
        &self.0
    }
}

/// Everything the frame callback needs; one frame is processed at a time.
struct State {
    gpu: Gpu,
    frames: Arc<LatestSlot<Frame>>,
    pace_ns: Arc<AtomicU64>,
    source_size: Arc<AtomicU64>,
    target_size: Arc<AtomicU64>,
    converter: Option<Converter>,
    hw: Option<HwFrames>,
    next_due_ns: u64,
    logged: bool,
    stat_in: u64,
    stat_out: u64,
    stat_t: std::time::Instant,
}

impl State {
    fn on_frame(&mut self, tex: &ID3D11Texture2D, content: (u32, u32), arrived_ns: u64) -> Result<()> {
        self.stat_in += 1;
        let interval = self.pace_ns.load(Ordering::Relaxed);
        let now = now_ns();
        // Pacing that keeps the average rate at the target (a 75 Hz source passes 60 of
        // every 75 frames); half an interval of slack absorbs jitter. Same as on Linux.
        if now + interval / 2 < self.next_due_ns {
            return Ok(());
        }
        let t = self.target_size.load(Ordering::Relaxed);
        let dst = if t == 0 { content } else { ((t >> 32) as u32, t as u32) };
        if self.converter.as_ref().is_none_or(|c| c.src_size != content || c.dst_size != dst) {
            self.converter = Some(Converter::new(&self.gpu, content, dst).context("video processor")?);
            if self.hw.as_ref().is_none_or(|h| h.size != dst) {
                self.hw = Some(HwFrames::new(&self.gpu, dst).context("GPU frame pool")?);
            }
            self.source_size.store((content.0 as u64) << 32 | content.1 as u64, Ordering::SeqCst);
            info!("capturing {}x{} → streaming {}x{} (NV12 D3D11 frames)", content.0, content.1, dst.0, dst.1);
        }
        let conv = self.converter.as_ref().unwrap();
        let (mut video, out_tex) = self.hw.as_ref().unwrap().get()?;
        unsafe {
            self.gpu.context.CopyResource(&conv.src, tex);
        }
        conv.convert(&out_tex)?;
        // Time of the screen content: when the frame reached us (its SystemRelativeTime
        // is the compositor's presentation time, a few ms earlier at most).
        let capture_ns = arrived_ns.min(now);
        video.set_pts(None);
        self.next_due_ns = (self.next_due_ns + interval).max(now.saturating_sub(interval / 2));
        self.stat_out += 1;
        if !self.logged {
            info!("first frame captured and converted ({:.2} ms after arrival)", (now_ns() - arrived_ns) as f64 / 1e6);
            self.logged = true;
        }
        self.frames.put(Frame { gpu: Some(video), width: dst.0, height: dst.1, stride: 0, format: PixFmt::Bgrx, capture_ns, data: Vec::new() });
        if self.stat_t.elapsed() >= Duration::from_secs(5) {
            let secs = self.stat_t.elapsed().as_secs_f64();
            debug!("wgc: {:.1} frames/s arrived, {:.1} frames/s passed on", self.stat_in as f64 / secs, self.stat_out as f64 / secs);
            (self.stat_in, self.stat_out, self.stat_t) = (0, 0, std::time::Instant::now());
        }
        Ok(())
    }
}

pub fn start(req: &CaptureRequest) -> Result<Capture> {
    if req.virtual_size.is_some() {
        anyhow::bail!(
            "--virtual is not available on Windows: install an Indirect Display driver such as \
             Virtual-Display-Driver (see the README) and mirror its monitor with --output"
        );
    }
    let monitor = monitors::choose(req.output, req.choose_again)?;
    let out = monitor.output();
    info!(
        "capturing {} ({}x{} at {},{} @ {} Hz)",
        monitor.name, out.width, out.height, out.x, out.y, monitor.refresh_hz
    );
    let frames = Arc::new(LatestSlot::new());
    let running = Arc::new(AtomicBool::new(true));
    let pace_ns = Arc::new(AtomicU64::new(0));
    let source_size = Arc::new(AtomicU64::new(0));
    let target_size = Arc::new(AtomicU64::new(0));
    let (recycle_tx, recycle_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<()>>(1);

    let (frames2, running2, pace2, src2, tgt2) =
        (frames.clone(), running.clone(), pace_ns.clone(), source_size.clone(), target_size.clone());
    let handle = monitor.handle;
    thread::Builder::new().name("capture".into()).spawn(move || {
        let _keep_receiver = recycle_rx;
        let r = run(handle, frames2, running2.clone(), pace2, src2, tgt2, &ready_tx);
        if let Err(e) = r {
            let _ = ready_tx.send(Err(e));
        }
        running2.store(false, Ordering::SeqCst);
    })?;
    ready_rx.recv().context("capture thread died")??;

    Ok(Capture {
        frames,
        pace_ns,
        source_size,
        target_size,
        recycle: recycle_tx,
        geometry: StreamGeometry { position: Some((out.x, out.y)), size: Some((out.width, out.height)) },
        running,
    })
}

fn run(
    monitor: isize,
    frames: Arc<LatestSlot<Frame>>,
    running: Arc<AtomicBool>,
    pace_ns: Arc<AtomicU64>,
    source_size: Arc<AtomicU64>,
    target_size: Arc<AtomicU64>,
    ready: &crossbeam_channel::Sender<Result<()>>,
) -> Result<()> {
    unsafe {
        RoInitialize(RO_INIT_MULTITHREADED).context("RoInitialize")?;
    }
    let gpu = gpu_for_monitor(monitor)?;
    info!("GPU: {} (vendor {:#06x})", gpu.name, gpu.vendor);
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(HMONITOR(monitor as *mut _)) }
        .context("GraphicsCaptureItem for the monitor (needs Windows 10 1903 or newer)")?;
    let dxgi: IDXGIDevice = gpu.device.cast()?;
    let winrt_device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?.cast()?;
    let size = item.Size()?;
    let pool =
        Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 3, size)?;
    source_size.store((size.Width as u64) << 32 | size.Height as u64, Ordering::SeqCst);

    let state = Arc::new(Mutex::new(State {
        gpu: gpu.clone(),
        frames,
        pace_ns,
        source_size,
        target_size,
        converter: None,
        hw: None,
        next_due_ns: 0,
        logged: false,
        stat_in: 0,
        stat_out: 0,
        stat_t: std::time::Instant::now(),
    }));
    let cur_size = Arc::new(Mutex::new((size.Width, size.Height)));
    let running_cb = running.clone();
    let state_cb = state.clone();
    let winrt_cb = AgileDevice(winrt_device.clone());
    pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
        let Some(pool) = pool.as_ref() else { return Ok(()) };
        let arrived = now_ns();
        // Drain: only the newest frame matters.
        let mut newest = None;
        while let Ok(f) = pool.TryGetNextFrame() {
            newest = Some(f);
        }
        let Some(frame) = newest else { return Ok(()) };
        let content = frame.ContentSize()?;
        {
            // The monitor's mode changed: recreate the pool at the new size.
            let mut cur = cur_size.lock().unwrap();
            if *cur != (content.Width, content.Height) {
                *cur = (content.Width, content.Height);
                pool.Recreate(winrt_cb.get(), DirectXPixelFormat::B8G8R8A8UIntNormalized, 3, content)?;
                return Ok(());
            }
        }
        let result = (|| -> Result<()> {
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let tex: ID3D11Texture2D = unsafe { access.GetInterface() }?;
            state_cb.lock().unwrap().on_frame(&tex, (content.Width as u32, content.Height as u32), arrived)
        })();
        if let Err(e) = result {
            // Once, then at most every 5 s: a persistent failure would flood the log.
            static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
            let mut last = LAST.lock().unwrap();
            if last.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
                warn!("capture frame dropped: {e:#}");
                *last = Some(std::time::Instant::now());
            }
        }
        drop(frame);
        let _ = &running_cb;
        Ok(())
    }))?;

    let session = pool.CreateCaptureSession(&item)?;
    if let Err(e) = session.SetIsCursorCaptureEnabled(true) {
        warn!("cannot capture the cursor: {e}");
    }
    // No yellow border around the monitor (Windows 11); not available everywhere.
    if let Err(e) = session.SetIsBorderRequired(false) {
        debug!("capture border stays: {e}");
    }
    // Up to 240 frames/s instead of the default 60 (Windows 11 24H2).
    if let Err(e) = session.SetMinUpdateInterval(TimeSpan { Duration: 40_000 }) {
        debug!("MinUpdateInterval: {e}");
    }
    session.StartCapture()?;
    let _ = ready.send(Ok(()));
    info!("Windows.Graphics.Capture started");

    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }
    let _ = session.Close();
    let _ = pool.Close();
    let _ = (&item, &winrt_device);
    Ok(())
}
