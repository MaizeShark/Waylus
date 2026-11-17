// SPDX-License-Identifier: GPL-3.0-or-later
//! Waylus: use an Android tablet as a low-latency pen display on Wayland, over USB or Wi-Fi.

mod adb;
mod aoa;
mod capture;
mod cuda;
mod dmabuf;
mod encode;
mod gnome;
mod input;
mod kde;
mod link;
mod niri;
mod proto;
mod rawpen;
mod session;
mod slot;
mod vaapi;
mod wifi;

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
    /// FFmpeg encoder: h264_nvenc, h264_vaapi, or auto (NVENC if an NVIDIA driver is
    /// present, else VAAPI).
    #[arg(long, default_value = "auto")]
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
    /// How the tablet talks to the host: `aoa` (Android Open Accessory: raw USB, works
    /// without USB debugging; adb is still used, if available, to install/start the app
    /// and read the raw pen; falls back to `adb` if the tablet lacks AOA) or `adb`
    /// (TCP through `adb reverse`).
    #[arg(long, value_enum, default_value_t = TransportArg::Aoa)]
    transport: TransportArg,
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
    /// Keep the tablet's CPU clocks up for this many seconds after pen/touch input
    /// (0 = off). Decoding on some tablets (e.g. MediaTek) is much faster at high clocks,
    /// which Android only grants while you touch the screen. Costs tablet battery.
    #[arg(long, default_value_t = 20)]
    boost: u32,
    /// Busy threads per CPU cluster the clock boost uses on the tablet.
    #[arg(long, default_value_t = 1, hide = true)]
    boost_threads: u32,
    /// Back-pressure: max frames sent but not yet shown on the tablet (0 = off).
    #[arg(long, default_value_t = 2)]
    max_in_flight: usize,
    /// Don't read the stylus device directly over adb; use Android's pen events.
    #[arg(long)]
    no_raw_pen: bool,
    /// Also accept the tablet over Wi-Fi (LAN). Only paired tablets can connect; USB
    /// is still used when the tablet is plugged in.
    #[arg(long)]
    wifi: bool,
    /// Allow one tablet to pair over Wi-Fi (for 5 minutes): the host shows a PIN that
    /// you enter on the tablet.
    #[arg(long, requires = "wifi")]
    pair: bool,
    /// Forget paired Wi-Fi tablets (a name, an id prefix, or "all") and exit.
    #[arg(long, value_name = "NAME")]
    unpair: Option<String>,
    /// Wi-Fi: address to listen on (repeatable; default: every private IPv4 address).
    #[arg(long, value_name = "ADDR", requires = "wifi")]
    listen: Vec<std::net::IpAddr>,
    /// Wi-Fi video FEC: parity shards as a percentage of data shards (at least 2 per
    /// block; 0 = off).
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u8).range(0..=100), requires = "wifi")]
    fec: u8,
    /// Don't use USB as a transport (with --wifi; adb, if present, is still used to
    /// install/start the app and read the raw pen).
    #[arg(long, requires = "wifi")]
    no_usb: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq)]
enum TransportArg {
    Adb,
    Aoa,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "waylus=info".into()),
        )
        .init();
    let args = Args::parse();

    let encoder = match args.encoder.as_str() {
        "auto" => encode::auto_encoder().to_owned(),
        e => e.to_owned(),
    };
    if !args.no_video {
        info!("encoder: {encoder}");
    }
    capture::ZERO_COPY.store(
        if encoder.contains("nvenc") {
            capture::ZERO_COPY_CUDA
        } else if encoder.contains("vaapi") {
            capture::ZERO_COPY_VAAPI
        } else {
            capture::ZERO_COPY_NONE
        },
        std::sync::atomic::Ordering::Relaxed,
    );
    if let Some(which) = &args.unpair {
        return wifi::unpair(which);
    }
    let usb = !args.no_usb;
    let mut aoa_mode = usb && args.transport == TransportArg::Aoa;
    let mut adb = (!args.no_adb).then(|| adb::Adb::new(args.serial.clone()));
    let mut serial = args.serial.clone();
    if (aoa_mode || args.wifi) && adb.as_ref().is_some_and(|a| !a.is_available()) {
        if args.wifi {
            info!("no adb device; the app must be installed already");
        } else {
            info!("no adb device; the app must be installed already and is started by the accessory");
        }
        adb = None;
    }
    if let Some(adb) = &adb {
        adb.wait_for_device()?;
        match args.apk.clone().or_else(adb::find_apk) {
            Some(apk) => adb.ensure_installed(&apk)?,
            None if adb.is_installed() => warn!("client APK not found locally, using the installed one"),
            None => bail!("client APK not found; build it with `cd android && ./gradlew assembleDebug` or pass --apk"),
        }
        if aoa_mode {
            serial = serial.or_else(|| adb.serial_no());
        }
    }
    let mut aoa = aoa_mode.then(|| aoa::Aoa::new(serial.clone()));
    if let Some(a) = aoa.as_mut() {
        // Switch now: the tablet re-enumerates, so adb drops out for a moment.
        // With adb (or Wi-Fi) at hand, fall back rather than wait for an AOA tablet forever.
        let give_up = (adb.is_some() || args.wifi).then_some(std::time::Duration::from_secs(if adb.is_some() { 5 } else { 2 }));
        match a.connect(give_up) {
            Ok(()) => {
                if let Some(adb) = &adb {
                    adb.wait_for_device()?;
                }
            }
            Err(e) if adb.is_none() => {
                info!("no tablet on USB ({e:#}); Wi-Fi only");
                aoa = None;
                aoa_mode = false;
            }
            Err(e) => {
                warn!("USB accessory mode unavailable ({e:#}); using adb instead");
                aoa = None;
                aoa_mode = false;
            }
        }
    }
    // adb reverse when USB is wanted but AOA isn't available (needs an adb device).
    let adb_tcp = usb && !aoa_mode && adb.is_some();
    if adb_tcp && let Some(adb) = &adb {
        adb.reverse(args.port)?;
    }
    let wifi = if args.wifi {
        Some(wifi::Server::start(wifi::Opts { port: args.port, fec_percent: args.fec as usize, listen: args.listen.clone(), pair: args.pair })?)
    } else {
        None
    };
    if !aoa_mode && !adb_tcp && wifi.is_none() {
        bail!("no transport: no tablet on USB and Wi-Fi is off (use --wifi)");
    }

    install_exit_handler(adb.clone(), adb_tcp.then_some(args.port), args.keep_app)?;

    let pen_sink: rawpen::Sink = Default::default();
    let _raw_pen = match &adb {
        Some(adb) if !args.no_raw_pen => match rawpen::start(adb, pen_sink.clone()) {
            Ok(r) => {
                let _ = RAW_PEN_DEVICE.set(r.device.clone());
                Some(r)
            }
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
        let wifi_host = wifi.as_ref().and_then(|w| w.addrs.first()).map(|a| a.ip().to_string());
        match (aoa_mode, adb_tcp, &wifi_host) {
            (true, _, _) => adb.start_app(args.port, "aoa", None, args.boost, args.boost_threads)?,
            (_, true, _) => adb.start_app(args.port, "tcp", None, args.boost, args.boost_threads)?,
            (_, _, Some(h)) => adb.start_app(args.port, "wifi", Some(h), args.boost, args.boost_threads)?,
            _ => {}
        }
    }

    session::serve(
        session::Opts {
            enc: encode::EncoderOpts {
                encoder: encoder.clone(),
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
        {
            let mut t = Vec::new();
            if let Some(a) = aoa {
                t.push(session::Transport::Aoa(a));
            }
            if adb_tcp {
                t.push(session::Transport::Tcp(args.port));
            }
            if let Some(w) = wifi {
                t.push(session::Transport::Wifi(w));
            }
            t
        },
        capture,
        lazy,
    )
}

/// Ctrl-C: undo the adb setup, then leave with `_exit`. A normal `exit` runs
/// CUDA/NVENC teardown while the encoder thread is still active, which can
/// hang. A second Ctrl-C, or a stuck cleanup, exits immediately.
/// Stylus device the raw pen reader streams on the tablet, for cleanup on Ctrl-C.
static RAW_PEN_DEVICE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn install_exit_handler(adb: Option<adb::Adb>, reverse_port: Option<u16>, keep_app: bool) -> Result<()> {
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
            if let Some(port) = reverse_port {
                adb.remove_reverse(port);
            }
            if !keep_app {
                adb.stop_app();
            }
            // _exit skips RawPen's Drop; end the remote stylus reader here.
            if let Some(dev) = RAW_PEN_DEVICE.get() {
                adb.kill_remote_cat(dev);
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
