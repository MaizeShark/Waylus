// SPDX-License-Identifier: GPL-3.0-or-later
//! Screen capture through the xdg-desktop-portal ScreenCast API and PipeWire.
//!
//! The portal hands us a PipeWire remote and a node id. We consume that node
//! with a PipeWire stream and publish the newest frame into a [`LatestSlot`];
//! stale frames are dropped, never queued.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

use anyhow::{Context, Result, anyhow, bail};
use ashpd::desktop::{
    PersistMode,
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
};
use pipewire as pw;
use pw::spa;
use tracing::{debug, info, warn};

use std::os::fd::RawFd;

use crate::{
    dmabuf::{Importer, Plane, fourcc},
    proto::now_ns,
    slot::LatestSlot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixFmt {
    Bgrx,
    Bgra,
    Rgbx,
    Rgba,
}

pub struct Frame {
    /// On the GPU (zero-copy path): an FFmpeg CUDA frame; `data` is then empty.
    pub gpu: Option<ffmpeg_next::frame::Video>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixFmt,
    pub capture_ns: u64,
    pub data: Vec<u8>,
}

/// Geometry of the captured monitor in compositor (logical) coordinates.
#[derive(Debug, Clone, Copy)]
pub struct StreamGeometry {
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

pub struct Capture {
    pub frames: Arc<LatestSlot<Frame>>,
    /// Target frame interval for pacing (0 = pass every frame). Set per session
    /// from the tablet's refresh rate.
    pub pace_ns: Arc<AtomicU64>,
    /// Captured size as `w << 32 | h` (0 until negotiated).
    pub source_size: Arc<AtomicU64>,
    /// Size to scale frames to, `w << 32 | h` (0 = native). Only the DMA-BUF path scales.
    pub target_size: Arc<AtomicU64>,
    /// Recycled frame buffers, sent back by the encoder.
    pub recycle: crossbeam_channel::Sender<Vec<u8>>,
    pub geometry: StreamGeometry,
    pub running: Arc<AtomicBool>,
}

fn token_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("waylus").join("screencast-token")
}

/// Opens a portal session and starts the PipeWire consumer thread.
/// Blocks until the user picked a monitor (first run) or the restore token was accepted.
/// `source_max_fps` is the highest rate we ask the compositor for; pacing to the
/// tablet's rate happens on our side (see [`Capture::pace_ns`]).
/// `virtual_size`: instead of mirroring a monitor, ask the compositor for a new
/// virtual monitor of that size (portal source type VIRTUAL).
pub fn start(source_max_fps: u32, choose_again: bool, virtual_size: Option<(u32, u32)>) -> Result<Capture> {
    let (geo_tx, geo_rx) = crossbeam_channel::bounded::<Result<(StreamGeometry, u32, std::os::fd::OwnedFd)>>(1);
    let frames = Arc::new(LatestSlot::new());
    let running = Arc::new(AtomicBool::new(true));
    let pace_ns = Arc::new(AtomicU64::new(0));
    let pace2 = pace_ns.clone();
    let source_size = Arc::new(AtomicU64::new(0));
    let target_size = Arc::new(AtomicU64::new(0));
    let sizes = (source_size.clone(), target_size.clone());
    let (recycle_tx, recycle_rx) = crossbeam_channel::unbounded::<Vec<u8>>();

    let frames2 = frames.clone();
    let running2 = running.clone();
    thread::Builder::new().name("capture".into()).spawn(move || {
        // The portal session lives as long as this thread, so the runtime and
        // the session objects stay alive while PipeWire runs.
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = geo_tx.send(Err(e.into()));
                return;
            }
        };
        let portal = rt.block_on(open_portal(choose_again, virtual_size.is_some()));
        let (proxy, session, geometry, node, fd) = match portal {
            Ok(v) => v,
            Err(e) => {
                let _ = geo_tx.send(Err(e));
                return;
            }
        };
        let _keep = (proxy, session);
        let _ = geo_tx.send(Ok((geometry, node, fd.try_clone().expect("dup fd"))));
        if let Err(e) = run_pipewire(fd, node, source_max_fps, virtual_size, pace2, sizes, frames2, recycle_rx, running2.clone()) {
            warn!("capture stopped: {e:#}");
        }
        running2.store(false, Ordering::SeqCst);
    })?;

    let (geometry, node, _fd) = geo_rx.recv().context("capture thread died")??;
    info!("screencast node {node}, geometry {geometry:?}");
    Ok(Capture { frames, pace_ns, source_size, target_size, recycle: recycle_tx, geometry, running })
}

async fn open_portal(choose_again: bool, virtual_monitor: bool) -> Result<(
    Screencast,
    ashpd::desktop::Session<Screencast>,
    StreamGeometry,
    u32,
    std::os::fd::OwnedFd,
)> {
    let proxy = Screencast::new().await.context("ScreenCast portal unavailable")?;
    let session = proxy.create_session(Default::default()).await?;
    let token_file = token_path();
    let token = if choose_again || virtual_monitor { None } else { std::fs::read_to_string(&token_file).ok().map(|s| s.trim().to_owned()) };
    let cursor = if proxy.available_cursor_modes().await?.contains(CursorMode::EMBEDDED) {
        CursorMode::EMBEDDED
    } else {
        warn!("portal does not support embedded cursor");
        CursorMode::HIDDEN
    };
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor)
                .set_sources(if virtual_monitor { SourceType::VIRTUAL } else { SourceType::MONITOR })
                .set_multiple(false)
                .set_restore_token(token.as_deref())
                .set_persist_mode(if virtual_monitor { PersistMode::DoNot } else { PersistMode::ExplicitlyRevoked }),
        )
        .await?
        .response()?;
    if token.is_none() {
        info!("choose the monitor to mirror in the screencast dialog");
    }
    let streams = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .context("screencast was cancelled or failed")?;
    if let Some(t) = streams.restore_token().filter(|_| !virtual_monitor) {
        if let Some(dir) = token_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&token_file, t) {
            warn!("could not save restore token: {e}");
        }
    }
    let stream = streams.streams().first().ok_or_else(|| anyhow!("portal returned no stream"))?;
    let geometry = StreamGeometry { position: stream.position(), size: stream.size() };
    let node = stream.pipe_wire_node_id();
    let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
    Ok((proxy, session, geometry, node, fd))
}

struct UserData {
    info: spa::param::video::VideoInfoRaw,
    format: Option<PixFmt>,
    dmabuf: bool,
    importer: Option<Importer>,
    interop: Option<crate::cuda::Interop>,
    frames: Arc<LatestSlot<Frame>>,
    recycle: crossbeam_channel::Receiver<Vec<u8>>,
    source_size: Arc<AtomicU64>,
    target_size: Arc<AtomicU64>,
    /// Frame pacing: target interval (shared, 0 = off) and the earliest time the next frame is due.
    pace_ns: Arc<AtomicU64>,
    next_due_ns: u64,
    logged_type: bool,
    read_ns: u64,
    reads: u64,
    /// process() calls / frames with a buffer / frames passed on, for diagnostics.
    stat_calls: u64,
    stat_bufs: u64,
    stat_out: u64,
    stat_t: u64,
}

fn map_format(f: spa::param::video::VideoFormat) -> Option<PixFmt> {
    use spa::param::video::VideoFormat as V;
    Some(match f {
        V::BGRx => PixFmt::Bgrx,
        V::BGRA => PixFmt::Bgra,
        V::RGBx => PixFmt::Rgbx,
        V::RGBA => PixFmt::Rgba,
        _ => return None,
    })
}

fn drm_fourcc(f: PixFmt) -> u32 {
    match f {
        PixFmt::Bgrx => fourcc(b"XR24"),
        PixFmt::Bgra => fourcc(b"AR24"),
        PixFmt::Rgbx => fourcc(b"XB24"),
        PixFmt::Rgba => fourcc(b"AB24"),
    }
}

fn serialize(obj: spa::pod::Object) -> Result<Vec<u8>> {
    Ok(spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
        .map_err(|e| anyhow!("pod serialize: {e:?}"))?
        .0
        .into_inner())
}

/// EnumFormat for one or more raw formats; with `modifiers` it describes a DMA-BUF format.
/// `size`: a fixed size (virtual monitors take their size from the consumer), else any.
fn format_pod(
    formats: &[spa::param::video::VideoFormat],
    modifiers: Option<&[u64]>,
    max_fps: u32,
    size: Option<(u32, u32)>,
) -> Result<Vec<u8>> {
    use spa::param::{format::FormatProperties as FP, format::MediaSubtype, format::MediaType};
    use spa::pod::{ChoiceValue, Property, PropertyFlags, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle};
    let mut props = vec![
        Property::new(FP::MediaType.as_raw(), Value::Id(Id(MediaType::Video.as_raw()))),
        Property::new(FP::MediaSubtype.as_raw(), Value::Id(Id(MediaSubtype::Raw.as_raw()))),
        Property::new(
            FP::VideoFormat.as_raw(),
            Value::Choice(ChoiceValue::Id(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: Id(formats[0].as_raw()),
                    alternatives: formats.iter().map(|f| Id(f.as_raw())).collect(),
                },
            ))),
        ),
    ];
    if let Some(mods) = modifiers {
        props.push(Property {
            key: FP::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum { default: mods[0] as i64, alternatives: mods.iter().map(|&m| m as i64).collect() },
            ))),
        });
    }
    props.push(Property::new(
        FP::VideoSize.as_raw(),
        match size {
            Some((width, height)) => Value::Rectangle(Rectangle { width, height }),
            None => Value::Choice(ChoiceValue::Rectangle(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Rectangle { width: 1920, height: 1080 },
                    min: Rectangle { width: 1, height: 1 },
                    max: Rectangle { width: 8192, height: 8192 },
                },
            ))),
        },
    ));
    props.push(Property::new(
        FP::VideoFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Fraction { num: max_fps, denom: 1 },
                min: Fraction { num: 0, denom: 1 },
                max: Fraction { num: 1000, denom: 1 },
            },
        ))),
    ));
    // Compositors throttle by minimum frame interval (niri does), which turns a 75 Hz
    // output into 37.5 fps when we ask for 60. Ask for plenty and pace precisely ourselves.
    let source_max = max_fps.max(1);
    props.push(Property::new(
        FP::VideoMaxFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Fraction { num: source_max, denom: 1 },
                min: Fraction { num: 1, denom: 1 },
                max: Fraction { num: source_max, denom: 1 },
            },
        ))),
    ));
    serialize(spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: props,
    })
}

fn buffers_pod(dmabuf: bool) -> Result<Vec<u8>> {
    use spa::pod::{ChoiceValue, Property, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags};
    let types = if dmabuf {
        1 << spa::buffer::DataType::DmaBuf.as_raw()
    } else {
        (1 << spa::buffer::DataType::MemFd.as_raw()) | (1 << spa::buffer::DataType::MemPtr.as_raw())
    };
    serialize(spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![Property::new(
            spa::sys::SPA_PARAM_BUFFERS_dataType,
            Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::from_bits_retain(spa::sys::SPA_CHOICE_Flags), ChoiceEnum::Flags {
                default: types,
                flags: vec![types],
            }))),
        )],
    })
}

fn run_pipewire(
    fd: std::os::fd::OwnedFd,
    node: u32,
    max_fps: u32,
    virtual_size: Option<(u32, u32)>,
    pace_ns: Arc<AtomicU64>,
    (source_size, target_size): (Arc<AtomicU64>, Arc<AtomicU64>),
    frames: Arc<LatestSlot<Frame>>,
    recycle: crossbeam_channel::Receiver<Vec<u8>>,
    running: Arc<AtomicBool>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;

    let importer = match Importer::new() {
        Ok(i) => Some(i),
        Err(e) => {
            warn!("DMA-BUF import unavailable ({e:#}); only shared-memory capture will work");
            None
        }
    };

    // Offer DMA-BUF formats (with the modifiers our GPU can import) first, SHM as fallback.
    use spa::param::video::VideoFormat as VF;
    let mut pods = Vec::new();
    if let Some(imp) = &importer {
        for f in [VF::BGRx, VF::BGRA] {
            let mods = imp.modifiers(drm_fourcc(map_format(f).unwrap()));
            if !mods.is_empty() {
                debug!("{f:?}: {} importable modifiers", mods.len());
                pods.push(format_pod(&[f], Some(&mods), max_fps, virtual_size)?);
            }
        }
    }
    pods.push(format_pod(&[VF::BGRx, VF::BGRx, VF::BGRA, VF::RGBx, VF::RGBA], None, max_fps, virtual_size)?);

    let stream = pw::stream::StreamBox::new(
        &core,
        "waylus-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let data = UserData {
        info: Default::default(),
        format: None,
        dmabuf: false,
        interop: if importer.is_some() && std::env::var_os("WAYLUS_NO_ZEROCOPY").is_none() {
            crate::cuda::Interop::new().map_err(|e| info!("zero-copy unavailable ({e:#}); using readback")).ok()
        } else {
            None
        },
        importer,
        frames,
        recycle,
        pace_ns,
        source_size,
        target_size,
        next_due_ns: 0,
        logged_type: false,
        read_ns: 0,
        reads: 0,
        stat_calls: 0,
        stat_bufs: 0,
        stat_out: 0,
        stat_t: 0,
    };

    let ml = mainloop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, _, old, new| {
            debug!("pipewire stream {old:?} -> {new:?}");
            if let pw::stream::StreamState::Error(e) = &new {
                warn!("pipewire stream error: {e}");
                ml.quit();
            }
        })
        .param_changed(|stream, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if ud.info.parse(param).is_err() {
                return;
            }
            ud.format = map_format(ud.info.format());
            let sz = ud.info.size();
            ud.source_size.store((sz.width as u64) << 32 | sz.height as u64, Ordering::SeqCst);
            ud.dmabuf = ud.info.flags().contains(spa::param::video::VideoFlags::MODIFIER);
            if let Some(imp) = ud.importer.as_mut() {
                imp.clear();
            }
            info!(
                "negotiated {:?} {}x{} @ {}/{} (max {}/{}), {}",
                ud.info.format(),
                ud.info.size().width,
                ud.info.size().height,
                ud.info.framerate().num,
                ud.info.framerate().denom,
                ud.info.max_framerate().num,
                ud.info.max_framerate().denom,
                if ud.dmabuf { format!("DMA-BUF modifier {:#x}", ud.info.modifier()) } else { "shared memory".into() }
            );
            match buffers_pod(ud.dmabuf) {
                Ok(b) => {
                    if let Some(pod) = spa::pod::Pod::from_bytes(&b) {
                        let _ = stream.update_params(&mut [pod]);
                    }
                }
                Err(e) => warn!("buffers param: {e:#}"),
            }
        })
        .remove_buffer(|_, ud, _| {
            if let Some(imp) = ud.importer.as_mut() {
                imp.clear();
            }
        })
        .process(|stream, ud| {
            // Drain to the newest buffer; older ones are stale.
            let mut newest = None;
            let mut n = 0;
            while let Some(b) = stream.dequeue_buffer() {
                newest = Some(b);
                n += 1;
            }
            ud.stat_calls += 1;
            ud.stat_bufs += n;
            let t = now_ns();
            if t - ud.stat_t > 5_000_000_000 {
                if ud.stat_t != 0 {
                    let secs = (t - ud.stat_t) as f64 / 1e9;
                    debug!(
                        "pipewire: {:.1} process/s, {:.1} buffers/s, {:.1} frames/s passed on",
                        ud.stat_calls as f64 / secs,
                        ud.stat_bufs as f64 / secs,
                        ud.stat_out as f64 / secs
                    );
                }
                (ud.stat_calls, ud.stat_bufs, ud.stat_out, ud.stat_t) = (0, 0, 0, t);
            }
            let Some(mut buffer) = newest else { return };
            let Some(fmt) = ud.format else { return };
            let now = now_ns();
            // Pacing that keeps the average rate at the target: a 75 Hz source passes 60 of
            // every 75 frames instead of every other one. Half an interval of slack absorbs jitter.
            let interval = ud.pace_ns.load(Ordering::Relaxed);
            if now + interval / 2 < ud.next_due_ns {
                return;
            }
            let width = ud.info.size().width;
            let height = ud.info.size().height;
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            if !ud.logged_type {
                info!("pipewire buffer type {:?}, {} plane(s)", datas[0].type_(), datas.len());
                ud.logged_type = true;
            }
            let mut buf = ud.recycle.try_recv().unwrap_or_default();
            let (fw, fh, stride, format) = if datas[0].type_() == spa::buffer::DataType::DmaBuf {
                let t = ud.target_size.load(Ordering::Relaxed);
                let (ow, oh) = if t == 0 { (width, height) } else { ((t >> 32) as u32, t as u32) };
                let Some(imp) = ud.importer.as_mut() else { return };
                if datas[0].chunk().size() == 0 && datas[0].chunk().flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
                    return;
                }
                let planes: Vec<Plane> = datas
                    .iter()
                    .map(|d| Plane { fd: d.fd() as RawFd, offset: d.chunk().offset(), stride: d.chunk().stride() as u32 })
                    .collect();
                let t0 = now_ns();
                if let Some(interop) = ud.interop.as_mut() {
                    match imp.read_gpu(width, height, ow, oh, drm_fourcc(fmt), ud.info.modifier(), &planes, interop) {
                        Ok(gpu) => {
                            ud.next_due_ns = (ud.next_due_ns + interval).max(now.saturating_sub(interval / 2));
                            ud.stat_out += 1;
                            tracing::trace!("zero-copy frame {:.2} ms", (now_ns() - t0) as f64 / 1e6);
                            ud.frames.put(Frame {
                                gpu: Some(gpu),
                                width: ow,
                                height: oh,
                                stride: 0,
                                format: PixFmt::Rgbx,
                                capture_ns: now,
                                data: buf,
                            });
                            return;
                        }
                        Err(e) => {
                            warn!("zero-copy failed ({e:#}); falling back to readback");
                            ud.interop = None;
                        }
                    }
                }
                if let Err(e) = imp.read(width, height, ow, oh, drm_fourcc(fmt), ud.info.modifier(), &planes, &mut buf) {
                    warn!("dmabuf read failed: {e:#}");
                    return;
                }
                ud.read_ns += now_ns() - t0;
                tracing::trace!("dmabuf readback {:.2} ms (process start → read start {:.2} ms)", (now_ns() - t0) as f64 / 1e6, (t0 - now) as f64 / 1e6);
                ud.reads += 1;
                if ud.reads % 600 == 0 {
                    debug!("dmabuf readback {:.2} ms avg", ud.read_ns as f64 / ud.reads as f64 / 1e6);
                }
                // glReadPixels(GL_BGRA) always yields B,G,R,A byte order.
                (ow, oh, ow as usize * 4, if fmt == PixFmt::Bgra || fmt == PixFmt::Rgba { PixFmt::Bgra } else { PixFmt::Bgrx })
            } else {
                let d = &mut datas[0];
                let (offset, size, stride) = {
                    let c = d.chunk();
                    (c.offset() as usize, c.size() as usize, c.stride())
                };
                if size == 0 {
                    return;
                }
                let stride = if stride > 0 { stride as usize } else { width as usize * 4 };
                let Some(src) = d.data() else { return };
                let need = stride * height as usize;
                if src.len() < offset + need {
                    return;
                }
                buf.clear();
                buf.extend_from_slice(&src[offset..offset + need]);
                (width, height, stride, fmt)
            };
            ud.next_due_ns = (ud.next_due_ns + interval).max(now.saturating_sub(interval / 2));
            ud.stat_out += 1;
            ud.frames.put(Frame { gpu: None, width: fw, height: fh, stride, format, capture_ns: now, data: buf });
        })
        .register()?;

    let mut params: Vec<&spa::pod::Pod> =
        pods.iter().map(|b| spa::pod::Pod::from_bytes(b).ok_or_else(|| anyhow!("bad pod"))).collect::<Result<_>>()?;

    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    // Poll the running flag so the session can be torn down from outside.
    let ml = mainloop.clone();
    let flag = running.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if !flag.load(Ordering::Relaxed) {
            ml.quit();
        }
    });
    timer
        .update_timer(Some(std::time::Duration::from_millis(200)), Some(std::time::Duration::from_millis(200)))
        .into_result()?;

    mainloop.run();
    if !running.load(Ordering::Relaxed) {
        return Ok(());
    }
    bail!("pipewire main loop exited")
}
