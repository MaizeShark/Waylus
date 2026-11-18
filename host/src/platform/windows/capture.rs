// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows.Graphics.Capture (placeholder until milestone 3).

use anyhow::{Result, bail};

use crate::frame::Capture;
use crate::platform::CaptureRequest;

pub fn start(_req: &CaptureRequest) -> Result<Capture> {
    bail!("screen capture is not implemented on Windows yet; use --no-video")
}
