// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows 10/11 backend: Windows.Graphics.Capture into D3D11 textures, hardware encoding
//! through FFmpeg D3D11 frames (NVENC / AMF / QSV), and synthetic pointer devices for the
//! pen and touch.

mod capture;
mod d3d;
pub mod input;
pub mod monitors;

use std::{path::PathBuf, sync::OnceLock};

use anyhow::Result;
use tracing::warn;
use windows::Win32::{
    Media::timeBeginPeriod,
    System::{
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        Threading::{GetCurrentProcess, TerminateProcess},
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

use super::{CaptureRequest, Output};
use crate::frame::Capture;

/// Per-process setup, first thing in `main`: per-monitor DPI awareness (pointer injection
/// and capture both work in physical pixels) and a 1 ms timer resolution (pacing sleeps).
pub fn init() {
    unsafe {
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            warn!("could not make the process per-monitor DPI aware: {e}");
        }
        timeBeginPeriod(1);
    }
}

/// `%LOCALAPPDATA%\waylus`: pairing keys, remembered monitor.
pub fn state_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("waylus")
}

pub fn capture_start(req: &CaptureRequest) -> Result<Capture> {
    capture::start(req)
}

/// FFmpeg encoder for `--encoder auto`, by the vendor of the GPU that drives the display.
pub fn auto_encoder() -> &'static str {
    d3d::auto_encoder()
}

/// Selects the encoder the capture's GPU frames are meant for (nothing to do on Windows:
/// D3D11 frames feed NVENC, AMF and QSV alike).
pub fn set_encoder(_encoder: &str) {}

/// `--output` (index or name); without one, the remembered monitor, or what the capture shows.
pub fn find_output(name: Option<&str>, cap: Option<&Capture>) -> Option<Output> {
    if let Some(n) = name {
        return monitors::find(n).map(|m| m.output());
    }
    if let Some(c) = cap
        && let (Some((x, y)), Some((w, h))) = (c.geometry.position, c.geometry.size)
    {
        return monitors::list().into_iter().map(|m| m.output()).find(|o| (o.x, o.y, o.width, o.height) == (x, y, w, h));
    }
    let remembered = std::fs::read_to_string(monitors::choice_file()).ok()?;
    monitors::list().into_iter().find(|m| m.name == remembered.trim()).map(|m| m.output())
}

/// Nothing to tell the desktop: the pointer devices are mapped by coordinates.
pub fn announce_output(_: &Output) {}

pub fn map_devices(_: Option<&Capture>, _: Option<&str>) {}

/// Releases the synthetic pointer devices. Safe from the Ctrl-C handler.
pub fn on_exit() {
    input::release_all();
}

/// Leaves the process without running destructors (encoder teardown can hang).
pub fn exit_now(code: i32) -> ! {
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), code as u32);
    }
    std::process::exit(code)
}

/// Monotonic clock in nanoseconds (QueryPerformanceCounter), the host clock of the latency stats.
pub fn monotonic_ns() -> u64 {
    static FREQ: OnceLock<u64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1) as u64
    });
    let mut t = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    (t as u128 * 1_000_000_000 / freq as u128) as u64
}

/// `waylus --test-input`.
pub fn test_input(output: Option<&str>) -> Result<()> {
    input::selftest(output)
}

/// `waylus --list-outputs`.
pub fn list_outputs() {
    monitors::print();
}
