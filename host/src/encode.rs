// SPDX-License-Identifier: GPL-3.0-or-later
//! Hardware H.264 encoding through FFmpeg: NVENC (NVIDIA) or VAAPI (Intel / AMD).
//!
//! RGB frames go straight into NVENC, which does the colour conversion on the GPU.
//! VAAPI encoders want NV12: frames are converted with swscale and uploaded to a
//! VAAPI surface. Low-latency settings: no B-frames, no lookahead, a short VBV,
//! infinite GOP with keyframes only on request.

use anyhow::{Context, Result, anyhow, bail};
use ffmpeg_next as ff;
use ff::{codec, format::Pixel, frame, picture};
use tracing::{info, warn};

use crate::frame::{Frame, PixFmt};

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
    gpu: bool,
    pending_ns: std::collections::VecDeque<(i64, u64)>,
    pkt: ff::Packet,
    force_key: bool,
    vaapi: Option<Vaapi>,
}

/// The hw frames context of a GPU frame, for configuring the encoder.
pub fn frames_ctx_of(frame: &frame::Video) -> Result<*mut ff::ffi::AVBufferRef> {
    let r = unsafe { (*frame.as_ptr()).hw_frames_ctx };
    if r.is_null() { Err(anyhow!("frame has no hw_frames_ctx")) } else { Ok(r) }
}

/// CPU frames → NV12 VAAPI surfaces.
struct Vaapi {
    frames_ctx: *mut ff::ffi::AVBufferRef,
    sws: *mut ff::ffi::SwsContext,
    nv12: frame::Video,
}

impl Vaapi {
    fn new(width: u32, height: u32, format: PixFmt) -> Result<Self> {
        use ff::ffi;
        unsafe {
            let dev_path = std::env::var("WAYLUS_VAAPI_DEVICE").ok().map(|p| std::ffi::CString::new(p).unwrap());
            let mut dev = std::ptr::null_mut();
            let r = ffi::av_hwdevice_ctx_create(
                &mut dev,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                dev_path.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                std::ptr::null_mut(),
                0,
            );
            if r < 0 {
                bail!("VAAPI device: {}", ff::Error::from(r));
            }
            let mut fr = ffi::av_hwframe_ctx_alloc(dev);
            ffi::av_buffer_unref(&mut dev);
            if fr.is_null() {
                bail!("av_hwframe_ctx_alloc failed");
            }
            let fc = (*fr).data as *mut ffi::AVHWFramesContext;
            (*fc).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*fc).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
            (*fc).width = width as i32;
            (*fc).height = height as i32;
            (*fc).initial_pool_size = 8;
            let r = ffi::av_hwframe_ctx_init(fr);
            if r < 0 {
                ffi::av_buffer_unref(&mut fr);
                bail!("VAAPI frames context: {}", ff::Error::from(r));
            }
            let (w, h) = (width as i32, height as i32);
            let src: ffi::AVPixelFormat = pixel(format).into();
            const SWS_BILINEAR: i32 = 2;
            let sws = ffi::sws_getContext(w, h, src, w, h, ffi::AVPixelFormat::AV_PIX_FMT_NV12, SWS_BILINEAR, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
            if sws.is_null() {
                ffi::av_buffer_unref(&mut fr);
                bail!("sws_getContext failed");
            }
            // Full-range RGB → limited-range BT.709, matching what the stream signals.
            let bt709 = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709);
            ffi::sws_setColorspaceDetails(sws, bt709, 1, bt709, 0, 0, 1 << 16, 1 << 16);
            Ok(Self { frames_ctx: fr, sws, nv12: frame::Video::new(Pixel::NV12, width, height) })
        }
    }

    /// Converts and uploads one frame; returns the VAAPI frame.
    fn upload(&mut self, f: &Frame) -> Result<frame::Video> {
        use ff::ffi;
        unsafe {
            let src = [f.data.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null()];
            let stride = [f.stride as i32, 0, 0, 0];
            let dst = self.nv12.as_mut_ptr();
            ffi::sws_scale(self.sws, src.as_ptr(), stride.as_ptr(), 0, f.height as i32, (*dst).data.as_ptr(), (*dst).linesize.as_ptr());
            let mut hw = frame::Video::empty();
            let r = ffi::av_hwframe_get_buffer(self.frames_ctx, hw.as_mut_ptr(), 0);
            if r < 0 {
                bail!("VAAPI surface: {}", ff::Error::from(r));
            }
            let r = ffi::av_hwframe_transfer_data(hw.as_mut_ptr(), dst, 0);
            if r < 0 {
                bail!("upload to VAAPI surface: {}", ff::Error::from(r));
            }
            Ok(hw)
        }
    }
}

impl Drop for Vaapi {
    fn drop(&mut self) {
        unsafe {
            ff::ffi::sws_freeContext(self.sws);
            ff::ffi::av_buffer_unref(&mut self.frames_ctx);
        }
    }
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
    /// `hw_frames`: the CUDA frames context of zero-copy input frames (None = system memory).
    pub fn new(
        opts: &EncoderOpts,
        width: u32,
        height: u32,
        format: PixFmt,
        hw_frames: Option<*mut ff::ffi::AVBufferRef>,
    ) -> Result<Self> {
        ff::init().context("ffmpeg init")?;
        if std::env::var_os("WAYLUS_FFMPEG_LOG").is_some() {
            ff::util::log::set_level(ff::util::log::Level::Debug);
        }
        let codec = ff::encoder::find_by_name(&opts.encoder)
            .ok_or_else(|| anyhow!("encoder {} not found in this ffmpeg build", opts.encoder))?;
        let bitrate = opts.bitrate.unwrap_or_else(|| {
            let scaled = 20e6 * (width * height) as f64 / (1920.0 * 1080.0);
            scaled.clamp(8e6, 60e6) as usize
        });
        let hw_format = hw_frames.map(|h| unsafe { (*((*h).data as *mut ff::ffi::AVHWFramesContext)).format });
        let vaapi = if opts.encoder.contains("vaapi") && hw_frames.is_none() {
            Some(Vaapi::new(width, height, format)?)
        } else {
            None
        };
        let vaapi_ref = vaapi.as_ref();
        // cqp: constant quantiser instead of CBR, for VAAPI drivers without rate control.
        let open = |cqp: bool| -> Result<ff::encoder::video::Encoder> {
            let vaapi = vaapi_ref;
            let mut ctx = codec::context::Context::new_with_codec(codec);
            ctx.set_flags(codec::Flags::GLOBAL_HEADER | codec::Flags::LOW_DELAY);
            let mut enc = ctx.encoder().video()?;
            enc.set_width(width);
            enc.set_height(height);
            enc.set_format(pixel(format));
            if let Some(hw) = hw_frames {
                unsafe {
                    let c = enc.as_mut_ptr();
                    (*c).pix_fmt = hw_format.unwrap();
                    (*c).hw_frames_ctx = ff::ffi::av_buffer_ref(hw);
                }
            }
            if let Some(v) = vaapi {
                unsafe {
                    let c = enc.as_mut_ptr();
                    (*c).pix_fmt = ff::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
                    (*c).hw_frames_ctx = ff::ffi::av_buffer_ref(v.frames_ctx);
                }
            }
            enc.set_time_base((1, 1_000_000));
            enc.set_frame_rate(Some((opts.fps as i32, 1)));
            if !cqp {
                enc.set_bit_rate(bitrate);
                enc.set_max_bit_rate(bitrate);
            }
            enc.set_max_b_frames(0);
            enc.set_gop(i32::MAX as u32);
            enc.set_colorspace(ff::color::Space::BT709);
            enc.set_color_primaries(ff::color::Primaries::BT709);
            enc.set_color_transfer_characteristic(ff::color::TransferCharacteristic::BT709);
            enc.set_color_range(ff::color::Range::MPEG);
            unsafe {
                // A few frames of VBV: lets keyframes reach usable quality while keeping the
                // worst-case frame small (~170 KB at 20 Mbit/s, ~5 ms over USB 2.0).
                if !cqp {
                    (*enc.as_mut_ptr()).rc_buffer_size = (bitrate / opts.fps.max(1) as usize * 4) as i32;
                }
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
            } else if opts.encoder.contains("vaapi") {
                // Default depth 2 keeps a frame queued in the driver: one frame of latency.
                dict.set("async_depth", "1");
                if cqp {
                    dict.set("rc_mode", "CQP");
                    dict.set("qp", "24");
                } else {
                    dict.set("rc_mode", "CBR");
                }
                dict.set("profile", "high");
            }
            enc.open_as_with(codec, dict).with_context(|| format!("open {}", opts.encoder))
        };
        let mut cqp = false;
        let enc = match open(false) {
            Ok(e) => e,
            Err(e) if opts.encoder.contains("vaapi") => {
                // Debian's free intel-media-va-driver only offers CQP on older Intel GPUs.
                warn!(
                    "{e:#}: this VAAPI driver has no bitrate control, using constant QP. For CBR, \
                     try LIBVA_DRIVER_NAME=i965 (older Intel) or intel-media-va-driver-non-free"
                );
                cqp = true;
                open(true)?
            }
            Err(e) => return Err(e),
        };
        info!(
            "encoder {} {}x{} {} {}",
            opts.encoder,
            width,
            height,
            if hw_format == Some(ff::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI) {
                "NV12 VAAPI surfaces (zero-copy)".to_string()
            } else if hw_format == Some(ff::ffi::AVPixelFormat::AV_PIX_FMT_D3D11) {
                "NV12 D3D11 textures (zero-copy)".to_string()
            } else if hw_frames.is_some() {
                "CUDA (zero-copy)".to_string()
            } else if vaapi.is_some() {
                format!("{format:?} → NV12 (CPU) → VAAPI")
            } else {
                format!("{format:?}")
            },
            if cqp { "CQP 24".to_string() } else { format!("{} kbit/s", bitrate / 1000) }
        );
        let frame = frame::Video::new(pixel(format), width, height);
        Ok(Self {
            enc,
            frame,
            width,
            height,
            format,
            gpu: hw_frames.is_some(),
            pending_ns: Default::default(),
            pkt: ff::Packet::empty(),
            force_key: true,
            vaapi,
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
        f.width == self.width && f.height == self.height && f.format == self.format && f.gpu.is_some() == self.gpu
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
        let pts = (capture_ns / 1000) as i64;
        let kind = if self.force_key { picture::Type::I } else { picture::Type::None };
        self.force_key = false;
        self.pending_ns.push_back((pts, capture_ns));
        if let Some(gpu) = &f.gpu {
            // New reference to the same GPU buffer. (ffmpeg-next's clone() would try
            // a deep CPU copy, which fails for hardware frames.)
            let mut fr = unsafe { frame::Video::wrap(ff::ffi::av_frame_clone(gpu.as_ptr())) };
            fr.set_pts(Some(pts));
            fr.set_kind(kind);
            self.enc.send_frame(&fr).context("send GPU frame to encoder")?;
        } else if let Some(v) = self.vaapi.as_mut() {
            let mut fr = v.upload(f)?;
            fr.set_pts(Some(pts));
            fr.set_kind(kind);
            self.enc.send_frame(&fr).context("send VAAPI frame to encoder")?;
        } else {
            let row = self.width as usize * 4;
            let dst_stride = self.frame.stride(0);
            let dst = self.frame.data_mut(0);
            for y in 0..self.height as usize {
                let s = &f.data[y * f.stride..y * f.stride + row];
                dst[y * dst_stride..y * dst_stride + row].copy_from_slice(s);
            }
            self.frame.set_pts(Some(pts));
            self.frame.set_kind(kind);
            self.enc.send_frame(&self.frame).context("send frame to encoder")?;
        }
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
                Err(e) => return Err(anyhow::Error::from(e).context("receive packet from encoder")),
            }
        }
        Ok(())
    }
}
