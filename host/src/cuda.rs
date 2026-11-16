// SPDX-License-Identifier: GPL-3.0-or-later
//! Zero-copy hand-off of the captured GL texture to NVENC via CUDA–GL interop.
//!
//! The staging texture (the compositor's DMA-BUF, blitted/scaled on the GPU) is
//! registered with CUDA once; per frame it is mapped and copied device-to-device
//! into an FFmpeg CUDA frame, which h264_nvenc encodes without touching system
//! memory. The CUDA driver API is loaded at runtime, so no CUDA SDK is needed.

use std::{ffi::c_void, ptr};

use anyhow::{Context, Result, anyhow, bail};
use ffmpeg_next::{self as ff, ffi};
use libloading::Library;
use tracing::info;

type CuResult = i32;
type CuContext = *mut c_void;
type CuResource = *mut c_void;
type CuArray = *mut c_void;

#[repr(C)]
struct Memcpy2D {
    src_x_in_bytes: usize,
    src_y: usize,
    src_memory_type: u32,
    src_host: *const c_void,
    src_device: u64,
    src_array: CuArray,
    src_pitch: usize,
    dst_x_in_bytes: usize,
    dst_y: usize,
    dst_memory_type: u32,
    dst_host: *mut c_void,
    dst_device: u64,
    dst_array: CuArray,
    dst_pitch: usize,
    width_in_bytes: usize,
    height: usize,
}

const CU_MEMORYTYPE_DEVICE: u32 = 2;
const CU_MEMORYTYPE_ARRAY: u32 = 3;
const CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY: u32 = 1;
const GL_TEXTURE_2D: u32 = 0x0DE1;

struct Api {
    _lib: Library,
    push: unsafe extern "C" fn(CuContext) -> CuResult,
    pop: unsafe extern "C" fn(*mut CuContext) -> CuResult,
    register_image: unsafe extern "C" fn(*mut CuResource, u32, u32, u32) -> CuResult,
    unregister: unsafe extern "C" fn(CuResource) -> CuResult,
    map: unsafe extern "C" fn(u32, *mut CuResource, *mut c_void) -> CuResult,
    unmap: unsafe extern "C" fn(u32, *mut CuResource, *mut c_void) -> CuResult,
    mapped_array: unsafe extern "C" fn(*mut CuArray, CuResource, u32, u32) -> CuResult,
    memcpy2d: unsafe extern "C" fn(*const Memcpy2D) -> CuResult,
}

impl Api {
    fn load() -> Result<Self> {
        unsafe {
            let lib = Library::new("libcuda.so.1").context("load libcuda.so.1")?;
            macro_rules! sym {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).context($n)?
                };
            }
            Ok(Self {
                push: sym!("cuCtxPushCurrent_v2"),
                pop: sym!("cuCtxPopCurrent_v2"),
                register_image: sym!("cuGraphicsGLRegisterImage"),
                unregister: sym!("cuGraphicsUnregisterResource"),
                map: sym!("cuGraphicsMapResources"),
                unmap: sym!("cuGraphicsUnmapResources"),
                mapped_array: sym!("cuGraphicsSubResourceGetMappedArray"),
                memcpy2d: sym!("cuMemcpy2D_v2"),
                _lib: lib,
            })
        }
    }
}

fn check(r: CuResult, what: &str) -> Result<()> {
    if r != 0 { bail!("{what} failed (CUresult {r})") } else { Ok(()) }
}

pub struct Interop {
    api: Api,
    device: *mut ffi::AVBufferRef,
    ctx: CuContext,
    frames: *mut ffi::AVBufferRef,
    frames_size: (u32, u32),
    registered: Option<(CuResource, u32)>,
}

impl Interop {
    /// Must be called on the thread that owns the (current) GL context.
    pub fn new() -> Result<Self> {
        let api = Api::load()?;
        let mut device = ptr::null_mut();
        let r = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut device,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if r < 0 || device.is_null() {
            bail!("av_hwdevice_ctx_create(CUDA) failed: {}", ff::Error::from(r));
        }
        let ctx = unsafe {
            let hw = (*device).data as *mut ffi::AVHWDeviceContext;
            (*((*hw).hwctx as *mut ffi::AVCUDADeviceContext)).cuda_ctx as CuContext
        };
        info!("zero-copy: CUDA–GL interop enabled");
        Ok(Self { api, device, ctx, frames: ptr::null_mut(), frames_size: (0, 0), registered: None })
    }

    fn ensure_frames(&mut self, w: u32, h: u32) -> Result<()> {
        if !self.frames.is_null() && self.frames_size == (w, h) {
            return Ok(());
        }
        unsafe {
            ffi::av_buffer_unref(&mut self.frames);
            let frames = ffi::av_hwframe_ctx_alloc(self.device);
            if frames.is_null() {
                bail!("av_hwframe_ctx_alloc failed");
            }
            let fc = (*frames).data as *mut ffi::AVHWFramesContext;
            (*fc).format = ffi::AVPixelFormat::AV_PIX_FMT_CUDA;
            // The GL texture is RGBA8, so CUDA sees bytes in R,G,B,A order.
            (*fc).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_RGB0;
            (*fc).width = w as i32;
            (*fc).height = h as i32;
            let mut frames = frames;
            let r = ffi::av_hwframe_ctx_init(frames);
            if r < 0 {
                ffi::av_buffer_unref(&mut frames);
                bail!("av_hwframe_ctx_init failed: {}", ff::Error::from(r));
            }
            self.frames = frames;
        }
        self.frames_size = (w, h);
        Ok(())
    }

    /// Copies GL texture `tex` (RGBA8, `w`x`h`) into a fresh CUDA frame.
    pub fn copy_texture(&mut self, tex: u32, w: u32, h: u32) -> Result<ff::frame::Video> {
        self.ensure_frames(w, h)?;
        let api = &self.api;
        unsafe {
            check((api.push)(self.ctx), "cuCtxPushCurrent")?;
            let result = (|| -> Result<ff::frame::Video> {
                if self.registered.is_none_or(|(_, t)| t != tex) {
                    if let Some((res, _)) = self.registered.take() {
                        (api.unregister)(res);
                    }
                    let mut res = ptr::null_mut();
                    check(
                        (api.register_image)(&mut res, tex, GL_TEXTURE_2D, CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY),
                        "cuGraphicsGLRegisterImage",
                    )?;
                    self.registered = Some((res, tex));
                }
                let mut res = self.registered.unwrap().0;
                let mut frame = ff::frame::Video::empty();
                let r = ffi::av_hwframe_get_buffer(self.frames, frame.as_mut_ptr(), 0);
                if r < 0 {
                    bail!("av_hwframe_get_buffer failed: {}", ff::Error::from(r));
                }
                // Mapping waits for the GL blit that wrote the texture.
                check((api.map)(1, &mut res, ptr::null_mut()), "cuGraphicsMapResources")?;
                let mut array = ptr::null_mut();
                let copied = check((api.mapped_array)(&mut array, res, 0, 0), "cuGraphicsSubResourceGetMappedArray")
                    .and_then(|_| {
                        let f = frame.as_ptr();
                        let m = Memcpy2D {
                            src_x_in_bytes: 0,
                            src_y: 0,
                            src_memory_type: CU_MEMORYTYPE_ARRAY,
                            src_host: ptr::null(),
                            src_device: 0,
                            src_array: array,
                            src_pitch: 0,
                            dst_x_in_bytes: 0,
                            dst_y: 0,
                            dst_memory_type: CU_MEMORYTYPE_DEVICE,
                            dst_host: ptr::null_mut(),
                            dst_device: (*f).data[0] as u64,
                            dst_array: ptr::null_mut(),
                            dst_pitch: (*f).linesize[0] as usize,
                            width_in_bytes: w as usize * 4,
                            height: h as usize,
                        };
                        check((api.memcpy2d)(&m), "cuMemcpy2D")
                    });
                (api.unmap)(1, &mut res, ptr::null_mut());
                copied?;
                Ok(frame)
            })();
            let mut popped = ptr::null_mut();
            (api.pop)(&mut popped);
            result
        }
    }
}

impl Drop for Interop {
    fn drop(&mut self) {
        unsafe {
            if let Some((res, _)) = self.registered.take() {
                (self.api.push)(self.ctx);
                (self.api.unregister)(res);
                let mut p = ptr::null_mut();
                (self.api.pop)(&mut p);
            }
            ffi::av_buffer_unref(&mut self.frames);
            ffi::av_buffer_unref(&mut self.device);
        }
    }
}

/// The hw frames context of a CUDA frame, for configuring the encoder.
pub fn frames_ctx_of(frame: &ff::frame::Video) -> Result<*mut ffi::AVBufferRef> {
    let r = unsafe { (*frame.as_ptr()).hw_frames_ctx };
    if r.is_null() { Err(anyhow!("frame has no hw_frames_ctx")) } else { Ok(r) }
}
