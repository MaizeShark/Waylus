// SPDX-License-Identifier: GPL-3.0-or-later
//! Direct3D 11 side of the Windows pipeline: pick the GPU that drives a monitor, convert
//! and scale captured BGRA frames to NV12 with the D3D11 video processor (colour matrix
//! BT.709, limited range, as the stream signals), and hand the result to FFmpeg as
//! `AV_PIX_FMT_D3D11` frames so NVENC / AMF / QSV encode it without a copy to system memory.

use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result, anyhow, bail};
use ffmpeg_next::{self as ff, ffi};
use windows::{
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
    core::{BOOL, Interface},
};

pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_AMD: u32 = 0x1002;
pub const VENDOR_INTEL: u32 = 0x8086;

/// A D3D11 device on one GPU.
#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub vendor: u32,
    pub name: String,
}

fn adapters() -> Result<Vec<IDXGIAdapter1>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("CreateDXGIFactory1")?;
    let mut v = Vec::new();
    for i in 0.. {
        match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => v.push(a),
            Err(_) => break,
        }
    }
    Ok(v)
}

fn adapter_info(a: &IDXGIAdapter1) -> (u32, String) {
    match unsafe { a.GetDesc1() } {
        Ok(d) => {
            let n = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
            (d.VendorId, String::from_utf16_lossy(&d.Description[..n]))
        }
        Err(_) => (0, "?".into()),
    }
}

/// The adapter whose output is `monitor` (an `HMONITOR`).
fn adapter_of_monitor(monitor: isize) -> Result<IDXGIAdapter1> {
    for a in adapters()? {
        for j in 0.. {
            let Ok(out) = (unsafe { a.EnumOutputs(j) }) else { break };
            if let Ok(d) = unsafe { out.GetDesc() }
                && d.Monitor.0 as isize == monitor
            {
                return Ok(a);
            }
        }
    }
    bail!("no GPU found for the monitor")
}

/// D3D11 device on the GPU that drives `monitor`.
pub fn gpu_for_monitor(monitor: isize) -> Result<Gpu> {
    let adapter = adapter_of_monitor(monitor)?;
    let (vendor, name) = adapter_info(&adapter);
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .context("D3D11CreateDevice")?;
    let device = device.ok_or_else(|| anyhow!("no D3D11 device"))?;
    let context = context.ok_or_else(|| anyhow!("no D3D11 context"))?;
    // The capture thread and FFmpeg's encoder both use this device.
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
    }
    Ok(Gpu { device, context, vendor, name })
}

/// `--encoder auto`: by the vendor of the first GPU (the one Windows lists first drives the
/// primary display on desktops; on hybrid laptops pass --encoder).
pub fn auto_encoder() -> &'static str {
    let vendor = adapters().ok().and_then(|a| a.first().map(|a| adapter_info(a).0)).unwrap_or(VENDOR_NVIDIA);
    match vendor {
        VENDOR_AMD => "h264_amf",
        VENDOR_INTEL => "h264_qsv",
        _ => "h264_nvenc",
    }
}

/// BGRA → NV12 conversion and scaling on the GPU.
pub struct Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// Our copy of the captured frame (the capture pool's surface is released at once).
    pub src: ID3D11Texture2D,
    src_view: ID3D11VideoProcessorInputView,
    pub src_size: (u32, u32),
    pub dst_size: (u32, u32),
    query: ID3D11Query,
    context: ID3D11DeviceContext,
}

impl Converter {
    pub fn new(gpu: &Gpu, src_size: (u32, u32), dst_size: (u32, u32)) -> Result<Self> {
        unsafe {
            let video_device: ID3D11VideoDevice = gpu.device.cast().context("video device")?;
            let video_context: ID3D11VideoContext = gpu.context.cast().context("video context")?;
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: src_size.0,
                InputHeight: src_size.1,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: dst_size.0,
                OutputHeight: dst_size.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = video_device.CreateVideoProcessorEnumerator(&desc).context("video processor enumerator")?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0).context("video processor")?;

            let tex_desc = D3D11_TEXTURE2D_DESC {
                Width: src_size.0,
                Height: src_size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut src = None;
            gpu.device.CreateTexture2D(&tex_desc, None, Some(&mut src)).context("create source texture")?;
            let src = src.ok_or_else(|| anyhow!("no source texture"))?;
            let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut src_view = None;
            video_device
                .CreateVideoProcessorInputView(&src, &enumerator, &view_desc, Some(&mut src_view))
                .context("video processor input view")?;
            let src_view = src_view.ok_or_else(|| anyhow!("no input view"))?;

            // Stream setup: whole source onto the whole destination, no auto enhancement.
            let sr = RECT { left: 0, top: 0, right: src_size.0 as i32, bottom: src_size.1 as i32 };
            let dr = RECT { left: 0, top: 0, right: dst_size.0 as i32, bottom: dst_size.1 as i32 };
            video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&sr));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dr));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dr));
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamOutputRate(&processor, 0, D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL, true, None);
            if let Ok(vc1) = video_context.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                vc1.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            }

            let mut query = None;
            gpu.device
                .CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut query))
                .context("create query")?;
            Ok(Self {
                video_device,
                video_context,
                enumerator,
                processor,
                src,
                src_view,
                src_size,
                dst_size,
                query: query.ok_or_else(|| anyhow!("no query"))?,
                context: gpu.context.clone(),
            })
        }
    }

    /// Converts `self.src` into the NV12 texture `dst`, and waits
    /// until the GPU has finished (the encoder reads the texture outside D3D11's tracking).
    pub fn convert(&self, dst: &ID3D11Texture2D) -> Result<()> {
        unsafe {
            let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut out = None;
            self.video_device
                .CreateVideoProcessorOutputView(dst, &self.enumerator, &view_desc, Some(&mut out))
                .context("video processor output view")?;
            let out = out.ok_or_else(|| anyhow!("no output view"))?;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(self.src_view.clone())),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let r = self.video_context.VideoProcessorBlt(&self.processor, &out, 0, std::slice::from_ref(&stream));
            drop(std::mem::ManuallyDrop::into_inner(stream.pInputSurface));
            r.context("VideoProcessorBlt")?;
            self.context.End(&self.query);
            self.context.Flush();
            let mut done = BOOL(0);
            let mut spins = 0u32;
            loop {
                let hr = self.context.GetData(&self.query, Some(&mut done as *mut BOOL as *mut c_void), std::mem::size_of::<BOOL>() as u32, 0);
                if hr.is_ok() && done.as_bool() {
                    break;
                }
                spins += 1;
                if spins > 200_000 {
                    bail!("GPU did not finish the colour conversion");
                }
                std::hint::spin_loop();
            }
        }
        Ok(())
    }
}

/// Free NV12 textures of one size, handed out wrapped in FFmpeg frames and taken back when
/// FFmpeg releases the last reference to a frame.
struct TexturePool {
    free: Mutex<Vec<ID3D11Texture2D>>,
    total: AtomicUsize,
}

/// FFmpeg hardware frames: NV12 textures of one size on our D3D11 device.
///
/// FFmpeg's own D3D11 pool allocates one texture *array*, and NV12 render-target arrays are
/// not supported by NVIDIA's driver; the video processor needs a render target. So the
/// frames context is only the description FFmpeg's encoders look at, and the frames are
/// wrapped around single textures we create.
pub struct HwFrames {
    frames: *mut ffi::AVBufferRef,
    gpu: Gpu,
    pool: Arc<TexturePool>,
    pub size: (u32, u32),
}

// The buffer references are reference counted by FFmpeg and used from the capture thread
// and the encoder thread.
unsafe impl Send for HwFrames {}
unsafe impl Sync for HwFrames {}

const MAX_TEXTURES: usize = 12;

fn nv12_desc(size: (u32, u32)) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    }
}

fn new_nv12(gpu: &Gpu, size: (u32, u32)) -> Result<ID3D11Texture2D> {
    let mut t = None;
    unsafe { gpu.device.CreateTexture2D(&nv12_desc(size), None, Some(&mut t)) }.context("create NV12 texture")?;
    t.ok_or_else(|| anyhow!("no NV12 texture"))
}

unsafe extern "C" fn release_texture(opaque: *mut c_void, _data: *mut u8) {
    let b = unsafe { Box::from_raw(opaque as *mut (Arc<TexturePool>, ID3D11Texture2D)) };
    let (pool, tex) = *b;
    pool.free.lock().unwrap().push(tex);
}

impl HwFrames {
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Result<Self> {
        unsafe {
            let mut dev = ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            if dev.is_null() {
                bail!("av_hwdevice_ctx_alloc failed");
            }
            let dctx = (*dev).data as *mut ffi::AVHWDeviceContext;
            let hwctx = (*dctx).hwctx as *mut ffi::AVD3D11VADeviceContext;
            // FFmpeg releases this reference when the device context is freed.
            (*hwctx).device = gpu.device.clone().into_raw() as *mut _;
            let r = ffi::av_hwdevice_ctx_init(dev);
            if r < 0 {
                ffi::av_buffer_unref(&mut dev);
                bail!("D3D11VA device context: {}", ff::Error::from(r));
            }
            let mut frames = ffi::av_hwframe_ctx_alloc(dev);
            ffi::av_buffer_unref(&mut dev);
            if frames.is_null() {
                bail!("av_hwframe_ctx_alloc failed");
            }
            let fc = (*frames).data as *mut ffi::AVHWFramesContext;
            (*fc).format = ffi::AVPixelFormat::AV_PIX_FMT_D3D11;
            (*fc).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
            (*fc).width = size.0 as i32;
            (*fc).height = size.1 as i32;
            (*fc).initial_pool_size = 1;
            let d3d = (*fc).hwctx as *mut ffi::AVD3D11VAFramesContext;
            (*d3d).BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
            // A texture of our own, so FFmpeg doesn't try to create a render-target array.
            let first = new_nv12(gpu, size)?;
            (*d3d).texture = first.as_raw() as *mut _;
            let r = ffi::av_hwframe_ctx_init(frames);
            if r < 0 {
                ffi::av_buffer_unref(&mut frames);
                diagnose(gpu, size);
                bail!("D3D11 frames context: {}", ff::Error::from(r));
            }
            let pool = Arc::new(TexturePool { free: Mutex::new(vec![first]), total: AtomicUsize::new(1) });
            Ok(Self { frames, gpu: gpu.clone(), pool, size })
        }
    }

    /// A free NV12 frame and the texture behind it.
    pub fn get(&self) -> Result<(ff::frame::Video, ID3D11Texture2D)> {
        let tex = match self.pool.free.lock().unwrap().pop() {
            Some(t) => t,
            None => {
                if self.pool.total.load(Ordering::Relaxed) >= MAX_TEXTURES {
                    bail!("all {MAX_TEXTURES} GPU frames are in use (the encoder is behind)");
                }
                self.pool.total.fetch_add(1, Ordering::Relaxed);
                new_nv12(&self.gpu, self.size)?
            }
        };
        unsafe {
            let f = ffi::av_frame_alloc();
            if f.is_null() {
                bail!("av_frame_alloc failed");
            }
            let opaque = Box::into_raw(Box::new((self.pool.clone(), tex.clone()))) as *mut c_void;
            let buf = ffi::av_buffer_create(tex.as_raw() as *mut u8, 0, Some(release_texture), opaque, 0);
            if buf.is_null() {
                drop(Box::from_raw(opaque as *mut (Arc<TexturePool>, ID3D11Texture2D)));
                let mut f = f;
                ffi::av_frame_free(&mut f);
                bail!("av_buffer_create failed");
            }
            (*f).format = ffi::AVPixelFormat::AV_PIX_FMT_D3D11 as i32;
            (*f).width = self.size.0 as i32;
            (*f).height = self.size.1 as i32;
            (*f).data[0] = tex.as_raw() as *mut u8;
            (*f).data[1] = std::ptr::null_mut();
            (*f).buf[0] = buf;
            (*f).hw_frames_ctx = ffi::av_buffer_ref(self.frames);
            Ok((ff::frame::Video::wrap(f), tex))
        }
    }
}

impl Drop for HwFrames {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.frames) };
    }
}

/// Logs which NV12 texture flavours this GPU accepts (for a failed frames context).
fn diagnose(gpu: &Gpu, size: (u32, u32)) {
    unsafe {
        match gpu.device.CheckFormatSupport(DXGI_FORMAT_NV12) {
            Ok(f) => tracing::warn!("NV12 format support flags {f:#x}"),
            Err(e) => tracing::warn!("CheckFormatSupport(NV12): {e}"),
        }
        for (name, bind, array, misc) in [
            ("RT", D3D11_BIND_RENDER_TARGET.0 as u32, 1u32, 0u32),
            ("RT|SRV", (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32, 1, 0),
            ("SRV", D3D11_BIND_SHADER_RESOURCE.0 as u32, 1, 0),
            ("none", 0, 1, 0),
            ("RT array8", D3D11_BIND_RENDER_TARGET.0 as u32, 8, 0),
            ("RT shared", D3D11_BIND_RENDER_TARGET.0 as u32, 1, D3D11_RESOURCE_MISC_SHARED.0 as u32),
        ] {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: array,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: bind,
                CPUAccessFlags: 0,
                MiscFlags: misc,
            };
            let mut t = None;
            let r = gpu.device.CreateTexture2D(&desc, None, Some(&mut t));
            tracing::warn!("NV12 {}x{} {name}: {:?}", size.0, size.1, r.map(|_| ()));
        }
    }
}
