// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform layer: everything that differs between the Linux (Wayland) and Windows hosts.
//!
//! Each backend provides the same small surface, so `main.rs` and `session.rs` don't
//! carry `cfg` blocks:
//!
//! * `capture_start`: screen capture of one monitor into a [`crate::frame::Capture`];
//! * `input::{Pen, Touch}`: virtual pen and touch devices fed from the tablet;
//! * output lookup and mapping (`find_output`, `map_devices`, `announce_output`);
//! * `set_encoder` / `auto_encoder`: which zero-copy GPU path frames take;
//! * `exit_now` / `on_exit`: leaving from the Ctrl-C handler;
//! * `monotonic_ns`: the host clock of the latency statistics.

use crate::frame::StreamGeometry;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(any(target_os = "linux", windows)))]
compile_error!("Waylus supports Linux (Wayland) and Windows 10/11 only");

/// A monitor of the host, as reported by the compositor / the OS.
#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    /// Position and size in the desktop's coordinate space (Linux: logical, Windows: pixels).
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Physical mode size.
    pub mode_w: u32,
    pub mode_h: u32,
}

/// Physical size of the area the video occupies on the tablet, used to set
/// the axis resolution (units/mm). libinput wants a resolution on tablets.
#[derive(Debug, Clone, Copy)]
pub struct AreaMm {
    pub width: f32,
    pub height: f32,
}

/// What the input backend needs to know about the mirrored monitor.
pub struct InputTarget<'a> {
    pub area: AreaMm,
    /// Geometry of the captured monitor, when a capture exists.
    pub geometry: Option<StreamGeometry>,
    /// `--output`, for input-only mode.
    pub output_name: Option<&'a str>,
}

/// What `capture_start` is asked for.
pub struct CaptureRequest<'a> {
    /// Highest rate asked of the compositor; pacing to the tablet's rate happens on our side.
    pub source_max_fps: u32,
    /// Show the monitor picker again instead of reusing the remembered monitor.
    pub choose_again: bool,
    /// A virtual monitor of this size instead of mirroring one.
    pub virtual_size: Option<(u32, u32)>,
    /// `--output`.
    pub output: Option<&'a str>,
}
