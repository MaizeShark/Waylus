// SPDX-License-Identifier: GPL-3.0-or-later
//! Types shared by every capture backend (see `platform`): what a captured frame looks
//! like and the handle the session uses to pull frames and steer the capture.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::{thread, time::Duration};

use crate::slot::LatestSlot;

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

/// Geometry of the captured monitor in compositor (Linux: logical) or virtual-desktop
/// (Windows: pixel) coordinates.
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

impl Capture {
    /// Waits (up to 2 s) until the capture knows the size of the frames it produces.
    pub fn wait_source_size(&self) -> Option<(u32, u32)> {
        for _ in 0..40 {
            let v = self.source_size.load(Ordering::SeqCst);
            if v != 0 {
                return Some(((v >> 32) as u32, v as u32));
            }
            thread::sleep(Duration::from_millis(50));
        }
        None
    }
}
