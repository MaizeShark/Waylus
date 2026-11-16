// SPDX-License-Identifier: GPL-3.0-or-later
//! Hardware H.264 encoding through FFmpeg (NVENC first, VAAPI fallback later).
//!
//! RGB frames from PipeWire go straight into NVENC, which does the colour
//! conversion on the GPU. Low-latency settings: no B-frames, no lookahead,
//! one-frame VBV, infinite GOP with keyframes only on request.

use anyhow::{Context, Result, anyhow, bail};
use ffmpeg_next as ff;
use ff::{codec, format::Pixel, frame, picture};
use tracing::info;

use crate::capture::{Frame, PixFmt};

#[derive(Debug, Clone)]
pub struct EncoderOpts {
    pub encoder: String,
    /// Bits per second; None = scale with resolution (20 Mbit/s at 1080p).
    pub bitrate: Option<usize>,
    pub fps: u32,
}

pub struct Packet {
    pub data: Vec<u8>,
    pub key: bool,
    pub capture_ns: u64,
}

pub struct Encoder {
    enc: ff::encoder::video::Encoder,
    frame: frame::Video,
    width: u32,
    height: u32,
    format: PixFmt,
    pending_ns: std::collections::VecDeque<(i64, u64)>,
    pkt: ff::Packet,
    force_key: bool,
}

fn pixel(f: PixFmt) -> Pixel {
    match f {
        PixFmt::Bgrx => Pixel::BGRZ,
        PixFmt::Bgra => Pixel::BGRA,
        PixFmt::Rgbx => Pixel::RGBZ,
        PixFmt::Rgba => Pixel::RGBA,
    }
}

impl Encoder {
    pub fn new(opts: &EncoderOpts, width: u32, height: u32, format: PixFmt) -> Result<Self> {
        ff::init().context("ffmpeg init")?;
        let codec = ff::encoder::find_by_name(&opts.encoder)
            .ok_or_else(|| anyhow!("encoder {} not found in this ffmpeg build", opts.encoder))?;
        let bitrate = opts.bitrate.unwrap_or_else(|| {
            let scaled = 20e6 * (width * height) as f64 / (1920.0 * 1080.0);
            scaled.clamp(8e6, 60e6) as usize
        });
        let mut ctx = codec::context::Context::new_with_codec(codec);
        ctx.set_flags(codec::Flags::GLOBAL_HEADER | codec::Flags::LOW_DELAY);
        let mut enc = ctx.encoder().video()?;
        enc.set_width(width);
        enc.set_height(height);
        enc.set_format(pixel(format));
        enc.set_time_base((1, 1_000_000));
        enc.set_frame_rate(Some((opts.fps as i32, 1)));
        enc.set_bit_rate(bitrate);
        enc.set_max_bit_rate(bitrate);
        enc.set_max_b_frames(0);
        enc.set_gop(i32::MAX as u32);
        enc.set_colorspace(ff::color::Space::BT709);
        enc.set_color_primaries(ff::color::Primaries::BT709);
        enc.set_color_transfer_characteristic(ff::color::TransferCharacteristic::BT709);
        enc.set_color_range(ff::color::Range::MPEG);
        unsafe {
            // A few frames of VBV: lets keyframes reach usable quality while keeping the
            // worst-case frame small (~170 KB at 20 Mbit/s, ~5 ms over USB 2.0).
            (*enc.as_mut_ptr()).rc_buffer_size = (bitrate / opts.fps.max(1) as usize * 4) as i32;
        }

        let mut dict = ff::Dictionary::new();
        if opts.encoder.contains("nvenc") {
            dict.set("preset", "p1");
            dict.set("tune", "ull");
            dict.set("zerolatency", "1");
            dict.set("delay", "0");
            dict.set("rc", "cbr");
            dict.set("rc-lookahead", "0");
            dict.set("forced-idr", "1");
            dict.set("profile", "high");
        }
        let enc = enc.open_as_with(codec, dict).with_context(|| format!("open {}", opts.encoder))?;
        info!("encoder {} {}x{} {:?} {} kbit/s", opts.encoder, width, height, format, bitrate / 1000);
        let frame = frame::Video::new(pixel(format), width, height);
        Ok(Self {
            enc,
            frame,
            width,
            height,
            format,
            pending_ns: Default::default(),
            pkt: ff::Packet::empty(),
            force_key: true,
        })
    }

    /// SPS/PPS in Annex-B form, to be sent as a codec-config buffer.
    pub fn extradata(&self) -> Option<Vec<u8>> {
        unsafe {
            let p = self.enc.as_ptr();
            if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
                return None;
            }
            Some(std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec())
        }
    }

    pub fn matches(&self, f: &Frame) -> bool {
        f.width == self.width && f.height == self.height && f.format == self.format
    }

    pub fn request_keyframe(&mut self) {
        self.force_key = true;
    }

    /// Encodes one frame and returns the produced packets (usually exactly one).
    /// Encodes `f`, stamped with `capture_ns` (normally `f.capture_ns`; a fresh time for refresh frames).
    pub fn encode(&mut self, f: &Frame, capture_ns: u64, out: &mut Vec<Packet>) -> Result<()> {
        if !self.matches(f) {
            bail!("frame format changed");
        }
        let row = self.width as usize * 4;
        let dst_stride = self.frame.stride(0);
        let dst = self.frame.data_mut(0);
        for y in 0..self.height as usize {
            let s = &f.data[y * f.stride..y * f.stride + row];
            dst[y * dst_stride..y * dst_stride + row].copy_from_slice(s);
        }
        let pts = (capture_ns / 1000) as i64;
        self.frame.set_pts(Some(pts));
        self.frame.set_kind(if self.force_key { picture::Type::I } else { picture::Type::None });
        self.force_key = false;
        self.pending_ns.push_back((pts, capture_ns));
        self.enc.send_frame(&self.frame)?;
        loop {
            match self.enc.receive_packet(&mut self.pkt) {
                Ok(()) => {
                    let pts = self.pkt.pts().unwrap_or(pts);
                    let mut out_ns = capture_ns;
                    while let Some(&(p, ns)) = self.pending_ns.front() {
                        if p <= pts {
                            self.pending_ns.pop_front();
                            if p == pts {
                                out_ns = ns;
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    out.push(Packet {
                        data: self.pkt.data().unwrap_or_default().to_vec(),
                        key: self.pkt.is_key(),
                        capture_ns: out_ns,
                    });
                }
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => break,
                Err(ff::Error::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}
