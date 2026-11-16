// SPDX-License-Identifier: GPL-3.0-or-later
//! Waylus: use an Android tablet as a low-latency pen display on Wayland, over USB.

mod adb;
mod capture;
mod dmabuf;
mod encode;
mod input;
mod niri;
mod proto;
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
    /// Video bitrate in Mbit/s.
    #[arg(long, default_value_t = 20.0)]
    bitrate: f64,
    /// Maximum frame rate.
    #[arg(long, default_value_t = 60)]
    fps: u32,
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
        let (adb2, port, keep) = (adb.clone(), args.port, args.keep_app);
        ctrlc::set_handler(move || {
            adb2.remove_reverse(port);
            if !keep {
                adb2.stop_app();
            }
            std::process::exit(0);
        })
        .context("install Ctrl-C handler")?;
    }

    let mut output = args.output.as_deref().and_then(niri::by_name);
    if args.output.is_some() && output.is_none() && niri::is_niri() {
        warn!("output {:?} not found via niri IPC", args.output);
    }

    let capture = if args.no_video {
        None
    } else {
        let cap = capture::start(args.fps)?;
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
                bitrate: (args.bitrate * 1e6) as usize,
                fps: args.fps,
            },
            output_size,
        },
        capture,
    )
}
