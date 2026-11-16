// SPDX-License-Identifier: GPL-3.0-or-later
//! Waylus: use an Android tablet as a low-latency pen display on Wayland, over USB.

mod adb;
mod capture;
mod cuda;
mod dmabuf;
mod encode;
mod gnome;
mod input;
mod kde;
mod niri;
mod proto;
mod rawpen;
mod session;
mod slot;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Only forward pen/touch input, do not stream the screen.
    #[arg(long)]
    no_video: bool,
    /// Output (connector name) being mirrored, e.g. DP-2. Used for the niri
    /// mapping hint and, with --no-video, for the aspect ratio.
    #[arg(long, short)]
    output: Option<String>,
    /// FFmpeg encoder to use.
    #[arg(long, default_value = "h264_nvenc")]
    encoder: String,
    /// Video bitrate in Mbit/s (default: 20 at 1080p, scaled with resolution).
    #[arg(long)]
    bitrate: Option<f64>,
    /// Show the monitor picker again instead of reusing the remembered monitor.
    #[arg(long)]
    choose: bool,
    /// Create a virtual monitor instead of mirroring one (GNOME/KDE; niri lacks it).
    /// Size defaults to the tablet's screen within its decoder limits, or give WIDTHxHEIGHT.
    #[arg(long, num_args = 0..=1, default_missing_value = "auto", value_name = "SIZE")]
    r#virtual: Option<String>,
    /// Frame rate (default: the tablet's display refresh rate, within decoder limits).
    #[arg(long)]
    fps: Option<u32>,
    /// TCP port tunnelled through adb.
    #[arg(long, default_value_t = proto::DEFAULT_PORT)]
    port: u16,
    /// adb serial of the tablet (if several devices are attached).
    #[arg(long, short)]
    serial: Option<String>,
    /// Client APK to install (default: search next to the binary / source tree).
    #[arg(long)]
    apk: Option<PathBuf>,
    /// Do not touch adb at all (you run `adb reverse` and start the app yourself).
    #[arg(long)]
    no_adb: bool,
    /// Leave the app running on the tablet on exit.
    #[arg(long)]
    keep_app: bool,
    /// Opt-in, device-specific decoder tweaks, comma separated: mtk-vpp-off, mtk-cpu-boost
    /// (MediaTek). Off by default; not used for the published comparisons.
    #[arg(long, value_delimiter = ',')]
    vendor_tweaks: Vec<String>,
    /// Back-pressure: max frames sent but not yet shown on the tablet (0 = off).
    #[arg(long, default_value_t = 2)]
    max_in_flight: usize,
    /// Don't read the stylus device directly over adb; use Android's pen events.
    #[arg(long)]
    no_raw_pen: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "waylus=info".into()),
        )
        .init();
    let args = Args::parse();

    let adb = (!args.no_adb).then(|| adb::Adb::new(args.serial.clone()));
    if let Some(adb) = &adb {
        adb.wait_for_device()?;
        match args.apk.clone().or_else(adb::find_apk) {
            Some(apk) => adb.ensure_installed(&apk)?,
            None if adb.is_installed() => warn!("client APK not found locally, using the installed one"),
            None => bail!("client APK not found; build it with `cd android && ./gradlew assembleDebug` or pass --apk"),
        }
        adb.reverse(args.port)?;
    }

    install_exit_handler(adb.clone(), args.port, args.keep_app)?;

    let pen_sink: rawpen::Sink = Default::default();
    let _raw_pen = match &adb {
        Some(adb) if !args.no_raw_pen => match rawpen::start(adb, pen_sink.clone()) {
            Ok(r) => Some(r),
            Err(e) => {
                info!("raw pen unavailable ({e:#}); using Android pen events");
                None
            }
        },
        _ => None,
    };

    let mut output = args.output.as_deref().and_then(niri::by_name);
    if args.output.is_some() && output.is_none() && niri::is_niri() {
        warn!("output {:?} not found via niri IPC", args.output);
    }

    let source_max_fps = args.fps.map_or(240, |f| f * 2);
    let virtual_size = match args.r#virtual.as_deref() {
        None => None,
        Some("auto") => Some(None),
        Some(v) => Some(Some(parse_size(v).map_err(anyhow::Error::msg)?)),
    };
    let mut lazy: Option<session::LazyCapture> = None;
    let capture = if args.no_video {
        None
    } else if let Some(size) = virtual_size {
        let fps = args.fps;
        lazy = Some(Box::new(move |hello: &proto::Hello| {
            let fps = fps.unwrap_or(hello.refresh_hz.round().max(1.0) as u32);
            let size = size
                .unwrap_or_else(|| session::stream_size((hello.surface_w as u32, hello.surface_h as u32), hello, fps));
            info!("creating a {}x{} virtual monitor", size.0, size.1);
            if gnome::is_gnome() {
                gnome::remember_connectors();
            }
            // Virtual monitors take their refresh rate from what we ask for (Mutter made
            // a 240 Hz one with the mirroring headroom), so ask for the tablet's rate.
            let cap = capture::start(fps, true, Some(size))?;
            std::thread::sleep(std::time::Duration::from_millis(500));
            if !cap.running.load(std::sync::atomic::Ordering::SeqCst) {
                bail!("virtual monitor capture failed to start (see warnings above)");
            }
            Ok(cap)
        }));
        None
    } else {
        let cap = capture::start(source_max_fps, args.choose, None)?;
        // Give PipeWire a moment to negotiate so format errors surface before the tablet connects.
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !cap.running.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("screen capture failed to start (see warnings above)");
        }
        if output.is_none()
            && let (Some(p), Some(s)) = (cap.geometry.position, cap.geometry.size)
        {
            output = niri::by_geometry(p, s);
        }
        Some(cap)
    };

    match &output {
        Some(o) => {
            info!("mirroring {} ({}x{})", o.name, o.mode_w, o.mode_h);
            if niri::is_niri() {
                info!("make sure your niri config maps the pen/touch to it:\n{}", niri::config_hint(&o.name));
            }
        }
        None if args.no_video => warn!("no --output given, assuming 16:9"),
        None => {}
    }
    let output_size = output.as_ref().map(|o| (o.mode_w, o.mode_h)).unwrap_or((1920, 1080));

    if let Some(adb) = &adb {
        adb.start_app(args.port)?;
    }

    session::serve(
        session::Opts {
            port: args.port,
            enc: encode::EncoderOpts {
                encoder: args.encoder.clone(),
                bitrate: args.bitrate.map(|b| (b * 1e6) as usize),
                fps: args.fps.unwrap_or(60),
            },
            fps: args.fps,
            max_in_flight: args.max_in_flight,
            vendor_tweaks: args.vendor_tweaks.iter().try_fold(0u8, |acc, t| match t.as_str() {
                "mtk-vpp-off" => Ok(acc | proto::tweaks::MTK_VPP_OFF),
                "mtk-cpu-boost" => Ok(acc | proto::tweaks::MTK_CPU_BOOST),
                other => Err(anyhow::anyhow!("unknown vendor tweak {other:?}")),
            })?,
            output_name: args.output.clone(),
            raw_pen: _raw_pen.as_ref().map(|_| pen_sink.clone()),
            output_size,
        },
        capture,
        lazy,
    )
}

/// Ctrl-C: undo the adb setup, then leave with `_exit`. A normal `exit` runs
/// CUDA/NVENC teardown while the encoder thread is still active, which can
/// hang. A second Ctrl-C, or a stuck cleanup, exits immediately.
fn install_exit_handler(adb: Option<adb::Adb>, port: u16, keep_app: bool) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static STOPPING: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(move || {
        if STOPPING.swap(true, Ordering::SeqCst) {
            unsafe { libc::_exit(130) };
        }
        eprintln!("\nstopping…");
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(3));
            unsafe { libc::_exit(1) };
        });
        if let Some(adb) = &adb {
            adb.remove_reverse(port);
            if !keep_app {
                adb.stop_app();
            }
        }
        unsafe { libc::_exit(0) };
    })
    .context("install Ctrl-C handler")
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    Ok((w.parse().map_err(|e| format!("{e}"))?, h.parse().map_err(|e| format!("{e}"))?))
}
