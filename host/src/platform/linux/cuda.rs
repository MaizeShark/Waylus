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
const CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY: u32 = 1;

struct Api {
    _lib: Library,
    push: unsafe extern "C" fn(CuContext) -> CuResult,
    pop: unsafe extern "C" fn(*mut CuContext) -> CuResult,
    register_buffer: unsafe extern "C" fn(*mut CuResource, u32, u32) -> CuResult,
    mapped_pointer: unsafe extern "C" fn(*mut u64, *mut usize, CuResource) -> CuResult,
    unregister: unsafe extern "C" fn(CuResource) -> CuResult,
    map: unsafe extern "C" fn(u32, *mut CuResource, *mut c_void) -> CuResult,
    unmap: unsafe extern "C" fn(u32, *mut CuResource, *mut c_void) -> CuResult,
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
                register_buffer: sym!("cuGraphicsGLRegisterBuffer"),
                mapped_pointer: sym!("cuGraphicsResourceGetMappedPointer_v2"),
                unregister: sym!("cuGraphicsUnregisterResource"),
                map: sym!("cuGraphicsMapResources"),
                unmap: sym!("cuGraphicsUnmapResources"),
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
    /// Registered GL buffer: (resource, GL name, generation).
    registered: Option<(CuResource, u32, u64)>,
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

    /// Copies GL pixel buffer `pbo` (tightly packed RGBA rows, `w`x`h`) into a fresh
    /// CUDA frame. `generation` changes whenever the buffer was recreated: GL reuses
    /// object names, and a stale registration silently maps the freed old buffer
    /// (that bug froze the stream after the first resize).
    pub fn copy_buffer(&mut self, pbo: u32, generation: u64, w: u32, h: u32) -> Result<ff::frame::Video> {
        self.ensure_frames(w, h)?;
        let api = &self.api;
        unsafe {
            check((api.push)(self.ctx), "cuCtxPushCurrent")?;
            let result = (|| -> Result<ff::frame::Video> {
                if self.registered.is_none_or(|(_, name, g)| (name, g) != (pbo, generation)) {
                    if let Some((res, _, _)) = self.registered.take() {
                        (api.unregister)(res);
                    }
                    let mut res = ptr::null_mut();
                    check(
                        (api.register_buffer)(&mut res, pbo, CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY),
                        "cuGraphicsGLRegisterBuffer",
                    )?;
                    self.registered = Some((res, pbo, generation));
                }
                let mut res = self.registered.unwrap().0;
                let mut frame = ff::frame::Video::empty();
                let r = ffi::av_hwframe_get_buffer(self.frames, frame.as_mut_ptr(), 0);
                if r < 0 {
                    bail!("av_hwframe_get_buffer failed: {}", ff::Error::from(r));
                }
                check((api.map)(1, &mut res, ptr::null_mut()), "cuGraphicsMapResources")?;
                let mut src = 0u64;
                let mut size = 0usize;
                let copied = check((api.mapped_pointer)(&mut src, &mut size, res), "cuGraphicsResourceGetMappedPointer")
                    .and_then(|_| {
                        if size < w as usize * 4 * h as usize {
                            bail!("pixel buffer too small");
                        }
                        let f = frame.as_ptr();
                        let m = Memcpy2D {
                            src_x_in_bytes: 0,
                            src_y: 0,
                            src_memory_type: CU_MEMORYTYPE_DEVICE,
                            src_host: ptr::null(),
                            src_device: src,
                            src_array: ptr::null_mut(),
                            src_pitch: w as usize * 4,
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
                        check((api.memcpy2d)(&m), "cuMemcpy2D")?;
                        Ok(())
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
            if let Some((res, _, _)) = self.registered.take() {
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
