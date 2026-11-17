// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire protocol shared with the Android client (`android/app/src/main/java/dev/waylus/Proto.kt`).
//!
//! Every message is framed as `[u8 type][u32 len][payload]`, little-endian.
//! Device timestamps are Android `System.nanoTime()` (CLOCK_MONOTONIC on the
//! tablet), host timestamps are CLOCK_MONOTONIC on the host.

use std::io::{self, Read, Write};

pub const PROTO_VERSION: u16 = 4;
pub const DEFAULT_PORT: u16 = 27183;
/// Upper bound for a single message, protects against garbage on the wire.
const MAX_LEN: u32 = 16 * 1024 * 1024;

pub mod ty {
    pub const HELLO: u8 = 1;
    pub const CONFIG: u8 = 2;
    pub const VIDEO: u8 = 3;
    pub const PEN: u8 = 4;
    pub const TOUCH: u8 = 5;
    pub const PING: u8 = 6;
    pub const PONG: u8 = 7;
    pub const FRAME_SHOWN: u8 = 8;
    pub const REQUEST_KEYFRAME: u8 = 9;
    pub const LAYOUT: u8 = 10;
    /// AOA only: marks the start of a session in the byte stream (see aoa.rs).
    pub const SYNC: u8 = 11;
}

/// Payload of a SYNC message: this magic followed by the client's u64 session nonce.
pub const SYNC_MAGIC: [u8; 8] = *b"WAYLUSYN";
/// Header plus magic of a SYNC message, the byte pattern searched for in the stream.
pub const SYNC_PREFIX: [u8; 13] = {
    let mut p = [ty::SYNC, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut i = 0;
    while i < 8 {
        p[5 + i] = SYNC_MAGIC[i];
        i += 1;
    }
    p
};

pub mod video_flags {
    pub const KEY: u8 = 1;
    pub const CONFIG: u8 = 2;
}

#[allow(dead_code)]
pub mod pen_state {
    pub const OUT: u8 = 0;
    pub const HOVER: u8 = 1;
    pub const CONTACT: u8 = 2;
}

pub mod pen_buttons {
    pub const PRIMARY: u8 = 1;
    pub const SECONDARY: u8 = 2;
    /// The tool is an eraser (TOOL_TYPE_ERASER or inverted pen).
    pub const ERASER: u8 = 4;
}

#[derive(Debug, Clone)]
pub struct Hello {
    pub version: u16,
    /// Size of the drawable surface in pixels (landscape).
    pub surface_w: u16,
    pub surface_h: u16,
    /// Decoder limits.
    pub max_w: u16,
    pub max_h: u16,
    pub max_mb_per_frame: u32,
    pub max_mb_per_sec: u32,
    /// Display refresh rate in Hz (0 = unknown).
    pub refresh_hz: f32,
    pub decoder: String,
    /// Physical pixel density of the panel (0 = unknown, older clients).
    pub px_per_mm: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct PenSample {
    pub t_ns: u64,
    pub state: u8,
    pub buttons: u8,
    /// 0..=65535 across the video rectangle.
    pub x: u16,
    pub y: u16,
    /// 0..=65535
    pub pressure: u16,
    /// Centidegrees, -9000..=9000.
    pub tilt_x: i16,
    pub tilt_y: i16,
}

/// Where the video sits on the tablet's display, so raw sensor coordinates can be
/// mapped (see rawpen.rs). Rotation is Android's Surface.ROTATION_* (0..3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub rotation: u8,
    /// Display size in its natural orientation, pixels.
    pub natural_w: u16,
    pub natural_h: u16,
    /// Video rectangle in current-orientation display pixels.
    pub left: i16,
    pub top: i16,
    pub right: i16,
    pub bottom: i16,
}

#[derive(Debug, Clone, Copy)]
pub struct TouchPoint {
    pub id: u8,
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone)]
pub enum DeviceMsg {
    Hello(Hello),
    Pen(PenSample),
    /// Full list of current contacts; missing ids are lifted.
    Touch {
        #[allow(dead_code)]
        t_ns: u64,
        points: Vec<TouchPoint>,
    },
    Pong { host_ns: u64, dev_ns: u64 },
    /// Device timestamps of one frame: received, rendered, queued into the decoder, decoded.
    FrameShown { capture_ns: u64, recv_ns: u64, render_ns: u64, queued_ns: u64, decoded_ns: u64 },
    RequestKeyframe,
    Layout(Layout),
    /// The client (re)started a session on the same USB link (AOA), with this nonce.
    Sync(u64),
    Unknown(u8),
}

struct Cur<'a>(&'a [u8]);
impl Cur<'_> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        if self.0.len() < N {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short message"));
        }
        let (a, b) = self.0.split_at(N);
        self.0 = b;
        Ok(a.try_into().unwrap())
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn i16(&mut self) -> io::Result<i16> {
        Ok(i16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
}

/// Reads one framed message. Reuses `buf` for the payload.
pub fn read_msg(r: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<DeviceMsg> {
    let mut hdr = [0u8; 5];
    r.read_exact(&mut hdr)?;
    let ty = hdr[0];
    let len = u32::from_le_bytes(hdr[1..5].try_into().unwrap());
    if len > MAX_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("message too large: {len}")));
    }
    buf.resize(len as usize, 0);
    r.read_exact(buf)?;
    let mut c = Cur(buf);
    Ok(match ty {
        ty::HELLO => {
            let version = c.u16()?;
            let surface_w = c.u16()?;
            let surface_h = c.u16()?;
            let max_w = c.u16()?;
            let max_h = c.u16()?;
            let max_mb_per_frame = c.u32()?;
            let max_mb_per_sec = c.u32()?;
            let refresh_hz = c.u16()? as f32 / 100.0;
            let n = c.u8()? as usize;
            let name = c.0.get(..n).unwrap_or(c.0);
            c.0 = c.0.get(n..).unwrap_or(&[]);
            let px_per_mm = c.u16().map(|v| v as f32 / 100.0).unwrap_or(0.0);
            DeviceMsg::Hello(Hello {
                version,
                surface_w,
                surface_h,
                max_w,
                max_h,
                max_mb_per_frame,
                max_mb_per_sec,
                refresh_hz,
                decoder: String::from_utf8_lossy(name).into_owned(),
                px_per_mm,
            })
        }
        ty::PEN => DeviceMsg::Pen(PenSample {
            t_ns: c.u64()?,
            state: c.u8()?,
            buttons: c.u8()?,
            x: c.u16()?,
            y: c.u16()?,
            pressure: c.u16()?,
            tilt_x: c.i16()?,
            tilt_y: c.i16()?,
        }),
        ty::TOUCH => {
            let t_ns = c.u64()?;
            let n = c.u8()? as usize;
            let mut points = Vec::with_capacity(n);
            for _ in 0..n {
                points.push(TouchPoint { id: c.u8()?, x: c.u16()?, y: c.u16()? });
            }
            DeviceMsg::Touch { t_ns, points }
        }
        ty::PONG => DeviceMsg::Pong { host_ns: c.u64()?, dev_ns: c.u64()? },
        ty::FRAME_SHOWN => DeviceMsg::FrameShown {
            capture_ns: c.u64()?,
            recv_ns: c.u64()?,
            render_ns: c.u64()?,
            queued_ns: c.u64().unwrap_or(0),
            decoded_ns: c.u64().unwrap_or(0),
        },
        ty::REQUEST_KEYFRAME => DeviceMsg::RequestKeyframe,
        ty::LAYOUT => DeviceMsg::Layout(Layout {
            rotation: c.u8()?,
            natural_w: c.u16()?,
            natural_h: c.u16()?,
            left: c.i16()?,
            top: c.i16()?,
            right: c.i16()?,
            bottom: c.i16()?,
        }),
        ty::SYNC => {
            c.take::<8>()?;
            DeviceMsg::Sync(u64::from_le_bytes(c.take()?))
        }
        other => DeviceMsg::Unknown(other),
    })
}

/// Writes one framed message with a single `write_all`. Splitting a message into
/// several writes made adbd forward it as several small socket writes on the
/// tablet, where Nagle + delayed ACK held a frame back for up to ~40 ms.
fn message(w: &mut impl Write, ty: u8, parts: &[&[u8]]) -> io::Result<()> {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let mut buf = Vec::with_capacity(5 + len);
    buf.push(ty);
    buf.extend_from_slice(&(len as u32).to_le_bytes());
    for p in parts {
        buf.extend_from_slice(p);
    }
    w.write_all(&buf)?;
    // No-op on TCP; on USB this submits the transfer (ending it with a short packet).
    w.flush()
}

pub mod codec {
    pub const H264: u8 = 0;
    /// Input-only session: no video, width/height only give the aspect ratio.
    pub const NONE: u8 = 255;
}

/// Opt-in, device-specific decoder tweaks (Config flags). Not used for comparisons.
pub mod tweaks {
    pub const MTK_VPP_OFF: u8 = 1;
    pub const MTK_CPU_BOOST: u8 = 2;
}

pub fn write_config(w: &mut impl Write, codec: u8, width: u16, height: u16, flags: u8) -> io::Result<()> {
    let mut p = [0u8; 6];
    p[0] = codec;
    p[1..3].copy_from_slice(&width.to_le_bytes());
    p[3..5].copy_from_slice(&height.to_le_bytes());
    p[5] = flags;
    message(w, ty::CONFIG, &[&p])
}

pub fn write_video(w: &mut impl Write, capture_ns: u64, flags: u8, data: &[u8]) -> io::Result<()> {
    let mut p = [0u8; 9];
    p[..8].copy_from_slice(&capture_ns.to_le_bytes());
    p[8] = flags;
    message(w, ty::VIDEO, &[&p, data])
}

pub fn write_sync(w: &mut impl Write, nonce: u64) -> io::Result<()> {
    message(w, ty::SYNC, &[&SYNC_MAGIC, &nonce.to_le_bytes()])
}

pub fn write_ping(w: &mut impl Write, host_ns: u64) -> io::Result<()> {
    message(w, ty::PING, &[&host_ns.to_le_bytes()])
}

/// CLOCK_MONOTONIC in nanoseconds.
pub fn now_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
