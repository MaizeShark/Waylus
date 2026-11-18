// SPDX-License-Identifier: GPL-3.0-or-later
//! Zero-copy hand-off to VAAPI encoders (Intel / AMD).
//!
//! NV12 surfaces come from a VAAPI frames pool that the encoder uses directly. Each
//! surface is exported once as DMA-BUFs (one R8 layer for luma, one GR88 layer for
//! chroma) and imported into GL as render targets, so the captured frame is
//! converted to NV12 by a shader straight into encoder memory (see dmabuf.rs).

use std::ptr;

use anyhow::{Result, bail};
use ffmpeg_next as ff;
use ff::ffi;
use tracing::info;

use super::dmabuf::Plane;

pub struct Interop {
    device: *mut ffi::AVBufferRef,
    frames: *mut ffi::AVBufferRef,
    size: (u32, u32),
    /// Bumped whenever the pool is recreated (surface IDs may be reused).
    pub generation: u64,
}

/// One exported plane layer of a surface.
pub struct Layer {
    pub drm_format: u32,
    pub modifier: u64,
    pub plane: Plane,
}

/// Keeps the export (and its file descriptors) alive while GL imports it.
pub struct Exported {
    mapped: *mut ffi::AVFrame,
    pub layers: Vec<Layer>,
}

impl Drop for Exported {
    fn drop(&mut self) {
        unsafe { ffi::av_frame_free(&mut self.mapped) };
    }
}

impl Interop {
    pub fn new() -> Result<Self> {
        let path = std::env::var("WAYLUS_VAAPI_DEVICE").ok().map(|p| std::ffi::CString::new(p).unwrap());
        let mut device = ptr::null_mut();
        let r = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut device,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                path.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
                ptr::null_mut(),
                0,
            )
        };
        if r < 0 || device.is_null() {
            bail!("VAAPI device: {}", ff::Error::from(r));
        }
        info!("zero-copy: GL → VAAPI surfaces enabled");
        Ok(Self { device, frames: ptr::null_mut(), size: (0, 0), generation: 0 })
    }

    fn ensure_frames(&mut self, w: u32, h: u32) -> Result<()> {
        if !self.frames.is_null() && self.size == (w, h) {
            return Ok(());
        }
        unsafe {
            ffi::av_buffer_unref(&mut self.frames);
            let mut frames = ffi::av_hwframe_ctx_alloc(self.device);
            if frames.is_null() {
                bail!("av_hwframe_ctx_alloc failed");
            }
            let fc = (*frames).data as *mut ffi::AVHWFramesContext;
            (*fc).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*fc).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
            (*fc).width = w as i32;
            (*fc).height = h as i32;
            // Enough for the encoder's references plus frames in flight here.
            (*fc).initial_pool_size = 8;
            let r = ffi::av_hwframe_ctx_init(frames);
            if r < 0 {
                ffi::av_buffer_unref(&mut frames);
                bail!("VAAPI frames context: {}", ff::Error::from(r));
            }
            self.frames = frames;
        }
        self.size = (w, h);
        self.generation += 1;
        Ok(())
    }

    /// A free NV12 surface of size `w`x`h` and its VASurfaceID.
    pub fn get(&mut self, w: u32, h: u32) -> Result<(ff::frame::Video, usize)> {
        self.ensure_frames(w, h)?;
        let mut frame = ff::frame::Video::empty();
        let r = unsafe { ffi::av_hwframe_get_buffer(self.frames, frame.as_mut_ptr(), 0) };
        if r < 0 {
            bail!("VAAPI surface: {}", ff::Error::from(r));
        }
        let id = unsafe { (*frame.as_ptr()).data[3] as usize };
        Ok((frame, id))
    }

    /// Exports a surface as DMA-BUF layers (luma R8, chroma GR88).
    pub fn export(&self, frame: &ff::frame::Video) -> Result<Exported> {
        unsafe {
            let mapped = ffi::av_frame_alloc();
            (*mapped).format = ffi::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            let flags = ffi::AV_HWFRAME_MAP_READ as i32 | ffi::AV_HWFRAME_MAP_WRITE as i32;
            let r = ffi::av_hwframe_map(mapped, frame.as_ptr(), flags);
            let mut exp = Exported { mapped, layers: vec![] };
            if r < 0 {
                bail!("export VAAPI surface: {}", ff::Error::from(r));
            }
            let desc = &*((*mapped).data[0] as *const ffi::AVDRMFrameDescriptor);
            for l in &desc.layers[..desc.nb_layers as usize] {
                if l.nb_planes != 1 {
                    bail!("unexpected VAAPI export: layer with {} planes", l.nb_planes);
                }
                let p = &l.planes[0];
                let obj = &desc.objects[p.object_index as usize];
                exp.layers.push(Layer {
                    drm_format: l.format,
                    modifier: obj.format_modifier,
                    plane: Plane { fd: obj.fd, offset: p.offset as u32, stride: p.pitch as u32 },
                });
            }
            if exp.layers.len() != 2 {
                bail!("unexpected VAAPI export: {} layers", exp.layers.len());
            }
            Ok(exp)
        }
    }
}

impl Drop for Interop {
    fn drop(&mut self) {
        unsafe {
            ffi::av_buffer_unref(&mut self.frames);
            ffi::av_buffer_unref(&mut self.device);
        }
    }
}
