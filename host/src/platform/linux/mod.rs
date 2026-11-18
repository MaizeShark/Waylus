// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux backend: xdg-desktop-portal + PipeWire capture, DMA-BUF/EGL import with CUDA or
//! VAAPI hand-off to the encoder, uinput pen and touch devices, and output mapping for
//! KDE, GNOME and niri.

mod capture;
mod cuda;
mod dmabuf;
mod gnome;
pub mod input;
mod kde;
pub mod niri;
mod vaapi;

use std::sync::atomic::Ordering;

use anyhow::Result;
use tracing::{info, warn};

use super::{CaptureRequest, Output};
use crate::frame::Capture;

pub fn capture_start(req: &CaptureRequest) -> Result<Capture> {
    if req.virtual_size.is_some() && gnome::is_gnome() {
        gnome::remember_connectors();
    }
    capture::start(req.source_max_fps, req.choose_again, req.virtual_size)
}

/// `$XDG_STATE_HOME/waylus` (default `~/.local/state/waylus`): pairing keys.
pub fn state_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("waylus")
}

/// Per-process setup (nothing on Linux).
pub fn init() {}

/// FFmpeg encoder for `--encoder auto`: NVENC when the NVIDIA driver is present, else VAAPI.
pub fn auto_encoder() -> &'static str {
    let nvidia = unsafe { libloading::Library::new("libcuda.so.1") }.is_ok();
    if nvidia { "h264_nvenc" } else { "h264_vaapi" }
}

/// Selects the GPU path frames take to `encoder`.
pub fn set_encoder(encoder: &str) {
    capture::ZERO_COPY.store(
        if encoder.contains("nvenc") {
            capture::ZERO_COPY_CUDA
        } else if encoder.contains("vaapi") {
            capture::ZERO_COPY_VAAPI
        } else {
            capture::ZERO_COPY_NONE
        },
        Ordering::Relaxed,
    );
}

/// The monitor named `name` (`--output`), or, without a name, the one a capture shows.
pub fn find_output(name: Option<&str>, cap: Option<&Capture>) -> Option<Output> {
    let found = name.and_then(niri::by_name);
    if name.is_some() && found.is_none() && niri::is_niri() {
        warn!("output {name:?} not found via niri IPC");
    }
    found.or_else(|| match cap.map(|c| (c.geometry.position, c.geometry.size)) {
        Some((Some(p), Some(s))) => niri::by_geometry(p, s),
        _ => None,
    })
}

/// Tells the compositor (niri) which output the pen/touch belong to.
pub fn announce_output(o: &Output) {
    if !niri::is_niri() {
        return;
    }
    match niri::write_mapping(&o.name) {
        Ok(_) if niri::is_included() => info!("niri: pen/touch mapped to {} while Waylus runs", o.name),
        Ok(p) => warn!(
            "niri: wrote the pen/touch mapping to {}, but your niri config doesn't include it; \
             run `waylus --niri-setup` once, or add this line at the end of your config:\n{}",
            p.display(),
            niri::include_line()
        ),
        Err(e) => warn!("niri: {e:#}; map the pen/touch yourself:\n{}", niri::config_hint(&o.name)),
    }
}

/// Maps the virtual pen/touch devices to the mirrored output (KDE, GNOME).
pub fn map_devices(capture: Option<&Capture>, output_name: Option<&str>) {
    if kde::is_kde() {
        let target = match capture {
            Some(cap) => match cap.geometry.position {
                Some(p) => kde::output_at(p, cap.geometry.size),
                // Virtual monitors have no position yet; find KWin's output by size.
                None => cap.wait_source_size().and_then(kde::virtual_output),
            },
            None => output_name.map(str::to_owned),
        };
        match target {
            Some(t) => kde::map_devices(t),
            None => warn!("KDE: couldn't tell which output is shown; map the Waylus pen/touch in System Settings"),
        }
    }
    if gnome::is_gnome() {
        let pos = capture.and_then(|c| c.geometry.position);
        let virtual_size = if capture.is_some() && pos.is_none() { capture.and_then(|c| c.wait_source_size()) } else { None };
        let named = output_name.map(str::to_owned);
        gnome::map_devices_with(move || match (pos, virtual_size) {
            (Some(p), _) => gnome::output_at(p),
            (None, Some(sz)) => gnome::virtual_output(sz),
            _ => named.clone(),
        });
    }
}

/// Undoes what the host set up on the desktop (niri mapping). Safe from the Ctrl-C handler.
pub fn on_exit() {
    niri::remove_mapping();
}

/// Leaves the process without running destructors (CUDA/NVENC teardown can hang).
pub fn exit_now(code: i32) -> ! {
    unsafe { libc::_exit(code) }
}

/// `waylus --niri-setup`.
pub fn niri_setup() -> Result<()> {
    niri::setup()
}

/// Monotonic clock in nanoseconds (CLOCK_MONOTONIC), the host clock of the latency stats.
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
