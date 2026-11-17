// SPDX-License-Identifier: GPL-3.0-or-later
//! DMA-BUF import via EGL + OpenGL.
//!
//! Compositors such as niri only hand out GPU buffers (DMA-BUF) for
//! screencasts. We import them as EGLImages and blit (and scale) them into our own
//! texture, then either read that back into system memory, hand it to NVENC through
//! CUDA, or convert it to NV12 straight into VAAPI encoder surfaces.

use std::{collections::HashMap, ffi::c_void, os::fd::RawFd};

use anyhow::{Context as _, Result, anyhow, bail};
use glow::HasContext;
use khronos_egl as egl;
use tracing::{debug, info};

type Egl = egl::DynamicInstance<egl::EGL1_5>;

const PLATFORM_DEVICE_EXT: egl::Enum = 0x313F;
const DRM_RENDER_NODE_FILE_EXT: egl::Int = 0x3377;
const LINUX_DMA_BUF_EXT: egl::Enum = 0x3270;
const LINUX_DRM_FOURCC_EXT: usize = 0x3271;
const PLANE_ATTRS: [[usize; 5]; 4] = [
    // fd, offset, pitch, modifier lo, modifier hi
    [0x3272, 0x3273, 0x3274, 0x3443, 0x3444],
    [0x3275, 0x3276, 0x3277, 0x3445, 0x3446],
    [0x3278, 0x3279, 0x327A, 0x3447, 0x3448],
    [0x3440, 0x3441, 0x3442, 0x3449, 0x344A],
];
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}

#[derive(Debug, Clone, Copy)]
pub struct Plane {
    pub fd: RawFd,
    pub offset: u32,
    pub stride: u32,
}

type QueryDevices = unsafe extern "system" fn(egl::Int, *mut *mut c_void, *mut egl::Int) -> egl::Boolean;
type QueryDeviceString = unsafe extern "system" fn(*mut c_void, egl::Int) -> *const std::ffi::c_char;
type QueryModifiers =
    unsafe extern "system" fn(*mut c_void, egl::Int, egl::Int, *mut u64, *mut egl::Boolean, *mut egl::Int) -> egl::Boolean;
type ImageTargetTexture = unsafe extern "system" fn(u32, *mut c_void);

struct Imported {
    image: egl::Image,
    tex: glow::Texture,
    fbo: glow::Framebuffer,
}

pub struct Importer {
    egl: Egl,
    display: egl::Display,
    _ctx: egl::Context,
    gl: glow::Context,
    image_target: ImageTargetTexture,
    query_modifiers: Option<QueryModifiers>,
    cache: HashMap<(RawFd, u32, u32, u32), Imported>,
    /// Our own render target the imported buffer is blitted into before readback.
    staging: Option<(glow::Texture, glow::Framebuffer, u32, u32)>,
    /// GPU-side pixel buffer for the zero-copy path (buffer, byte size).
    pbo: Option<(glow::Buffer, usize)>,
    /// Bumped whenever `pbo` is recreated. GL reuses object names, so the name
    /// alone can't tell CUDA that its registration now points at freed memory.
    pbo_generation: u64,
    /// VAAPI path: render targets per surface (VASurfaceID → luma, chroma), for
    /// pool generation `.0`.
    nv12_targets: (u64, HashMap<usize, (Imported, Imported)>),
    nv12_prog: Option<(glow::Program, glow::VertexArray)>,
}

/// GPU encoder the zero-copy path feeds.
pub enum Interop {
    Cuda(crate::cuda::Interop),
    Vaapi(crate::vaapi::Interop),
}

const NV12_VS: &str = "#version 330 core
void main() {
    // Full-screen triangle.
    vec2 p = vec2(float((gl_VertexID & 1) << 2), float((gl_VertexID & 2) << 1)) - 1.0;
    gl_Position = vec4(p, 0.0, 1.0);
}";

// BT.709, limited range (what the stream signals). Texture row 0 is the top image row,
// and so is row 0 of each target: no flips.
const NV12_FS: &str = "#version 330 core
uniform sampler2D src;
uniform int chroma;
out vec4 o;
void main() {
    if (chroma == 0) {
        vec3 c = texelFetch(src, ivec2(gl_FragCoord.xy), 0).rgb;
        o = vec4(16.0 / 255.0 + dot(c, vec3(0.2126, 0.7152, 0.0722)) * (219.0 / 255.0), 0.0, 0.0, 1.0);
    } else {
        // Bilinear sample at the centre of the 2x2 block = its average.
        vec2 size = vec2(textureSize(src, 0));
        vec3 c = texture(src, floor(gl_FragCoord.xy) * 2.0 / size + 1.0 / size).rgb;
        float u = 128.0 / 255.0 + dot(c, vec3(-0.1146, -0.3854, 0.5)) * (224.0 / 255.0);
        float v = 128.0 / 255.0 + dot(c, vec3(0.5, -0.4542, -0.0458)) * (224.0 / 255.0);
        o = vec4(u, v, 0.0, 1.0);
    }
}";

impl Importer {
    /// Creates a surfaceless EGL/OpenGL context on the first DRM GPU.
    pub fn new() -> Result<Self> {
        let egl = unsafe { Egl::load_required() }.map_err(|e| anyhow!("load libEGL: {e}"))?;
        let display = Self::open_display(&egl)?;
        egl.initialize(display).context("eglInitialize")?;
        let exts = egl.query_string(Some(display), egl::EXTENSIONS)?.to_string_lossy().into_owned();
        if !exts.contains("EGL_EXT_image_dma_buf_import") {
            bail!("EGL lacks EGL_EXT_image_dma_buf_import");
        }
        egl.bind_api(egl::OPENGL_API)?;
        let config = egl
            .choose_first_config(display, &[egl::SURFACE_TYPE, 0, egl::RENDERABLE_TYPE, egl::OPENGL_BIT, egl::NONE])?
            .ok_or_else(|| anyhow!("no EGL config"))?;
        let ctx = egl.create_context(display, config, None, &[egl::NONE])?;
        egl.make_current(display, None, None, Some(ctx)).context("eglMakeCurrent (surfaceless)")?;
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s).map_or(std::ptr::null(), |p| p as *const c_void)
            })
        };
        let image_target: ImageTargetTexture = unsafe {
            std::mem::transmute(
                egl.get_proc_address("glEGLImageTargetTexture2DOES")
                    .ok_or_else(|| anyhow!("no glEGLImageTargetTexture2DOES"))?,
            )
        };
        let query_modifiers = exts
            .contains("EGL_EXT_image_dma_buf_import_modifiers")
            .then(|| egl.get_proc_address("eglQueryDmaBufModifiersEXT"))
            .flatten()
            .map(|p| unsafe { std::mem::transmute::<extern "system" fn(), QueryModifiers>(p) });
        info!("EGL importer: {}", unsafe { gl.get_parameter_string(glow::RENDERER) });
        Ok(Self { egl, display, _ctx: ctx, gl, image_target, query_modifiers, cache: HashMap::new(), staging: None, pbo: None, pbo_generation: 0, nv12_targets: (0, HashMap::new()), nv12_prog: None })
    }

    fn open_display(egl: &Egl) -> Result<egl::Display> {
        // GBM on the render node first (what compositors use), then the EGL device platform.
        match Self::open_gbm_display(egl) {
            Ok(d) => return Ok(d),
            Err(e) => debug!("GBM EGL display unavailable: {e:#}"),
        }
        // Prefer an explicit DRM device (works without any window system).
        if let (Some(qd), Some(qs)) =
            (egl.get_proc_address("eglQueryDevicesEXT"), egl.get_proc_address("eglQueryDeviceStringEXT"))
        {
            let qd: QueryDevices = unsafe { std::mem::transmute(qd) };
            let qs: QueryDeviceString = unsafe { std::mem::transmute(qs) };
            let mut devs = [std::ptr::null_mut(); 16];
            let mut n = 0;
            if unsafe { qd(devs.len() as _, devs.as_mut_ptr(), &mut n) } == egl::TRUE {
                for &dev in &devs[..n as usize] {
                    let node = unsafe { qs(dev, DRM_RENDER_NODE_FILE_EXT) };
                    if node.is_null() {
                        continue;
                    }
                    let name = unsafe { std::ffi::CStr::from_ptr(node) }.to_string_lossy().into_owned();
                    if let Ok(d) = unsafe { egl.get_platform_display(PLATFORM_DEVICE_EXT, dev, &[egl::ATTRIB_NONE]) } {
                        debug!("EGL device {name}");
                        return Ok(d);
                    }
                }
            }
        }
        unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| anyhow!("no EGL display"))
    }

    fn open_gbm_display(egl: &Egl) -> Result<egl::Display> {
        const PLATFORM_GBM_KHR: egl::Enum = 0x31D7;
        let node = std::fs::read_dir("/dev/dri")?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD")))
            .min()
            .ok_or_else(|| anyhow!("no render node"))?;
        let file = std::fs::OpenOptions::new().read(true).write(true).open(&node)?;
        // Leaked on purpose: the GBM device and its fd must outlive the EGL display.
        let lib = Box::leak(Box::new(unsafe { libloading::Library::new("libgbm.so.1")? }));
        let create: libloading::Symbol<unsafe extern "C" fn(i32) -> *mut c_void> =
            unsafe { lib.get(b"gbm_create_device\0")? };
        let fd = std::os::fd::IntoRawFd::into_raw_fd(file);
        let dev = unsafe { create(fd) };
        if dev.is_null() {
            bail!("gbm_create_device failed");
        }
        let d = unsafe { egl.get_platform_display(PLATFORM_GBM_KHR, dev, &[egl::ATTRIB_NONE]) }
            .map_err(|e| anyhow!("eglGetPlatformDisplay(GBM): {e}"))?;
        debug!("EGL on GBM ({})", node.display());
        Ok(d)
    }

    /// Modifiers the GPU can import for `format` as a regular 2D texture.
    pub fn modifiers(&self, format: u32) -> Vec<u64> {
        let Some(q) = self.query_modifiers else { return vec![] };
        let dpy = self.display.as_ptr();
        let mut n = 0;
        unsafe {
            if q(dpy, format as _, 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut n) != egl::TRUE || n <= 0 {
                return vec![];
            }
            let mut mods = vec![0u64; n as usize];
            let mut ext = vec![0 as egl::Boolean; n as usize];
            q(dpy, format as _, n, mods.as_mut_ptr(), ext.as_mut_ptr(), &mut n);
            mods.truncate(n as usize);
            mods.into_iter().zip(ext).filter(|(_, e)| *e == egl::FALSE).map(|(m, _)| m).collect()
        }
    }

    fn import(&mut self, w: u32, h: u32, format: u32, modifier: u64, planes: &[Plane]) -> Result<Imported> {
        let mut attrs: Vec<usize> =
            vec![egl::WIDTH as usize, w as usize, egl::HEIGHT as usize, h as usize, LINUX_DRM_FOURCC_EXT, format as usize];
        for (p, a) in planes.iter().zip(PLANE_ATTRS) {
            attrs.extend([a[0], p.fd as usize, a[1], p.offset as usize, a[2], p.stride as usize]);
            if modifier != DRM_FORMAT_MOD_INVALID {
                attrs.extend([a[3], (modifier & 0xffff_ffff) as usize, a[4], (modifier >> 32) as usize]);
            }
        }
        attrs.push(egl::ATTRIB_NONE);
        let image = self
            .egl
            .create_image(
                self.display,
                unsafe { egl::Context::from_ptr(egl::NO_CONTEXT) },
                LINUX_DMA_BUF_EXT,
                unsafe { egl::ClientBuffer::from_ptr(std::ptr::null_mut()) },
                &attrs,
            )
            .context("eglCreateImage(dmabuf)")?;
        unsafe {
            let gl = &self.gl;
            let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            (self.image_target)(glow::TEXTURE_2D, image.as_ptr());
            let fbo = gl.create_framebuffer().map_err(|e| anyhow!(e))?;
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
            let status = gl.check_framebuffer_status(glow::READ_FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                bail!("framebuffer incomplete: {status:#x}");
            }
            Ok(Imported { image, tex, fbo })
        }
    }

    /// Blits the `w`x`h` DMA-BUF into our own RGBA8 texture of size `ow`x`oh`
    /// (bilinear when scaling). Returns that texture's GL name.
    #[allow(clippy::too_many_arguments)]
    fn blit(&mut self, w: u32, h: u32, ow: u32, oh: u32, format: u32, modifier: u64, planes: &[Plane]) -> Result<u32> {
        let p0 = planes.first().ok_or_else(|| anyhow!("no planes"))?;
        let key = (p0.fd, p0.offset, p0.stride, w << 16 | h);
        if !self.cache.contains_key(&key) {
            let imp = self.import(w, h, format, modifier, planes)?;
            self.cache.insert(key, imp);
        }
        let src_fbo = self.cache[&key].fbo;
        unsafe {
            let gl = &self.gl;
            if self.staging.is_none_or(|s| (s.2, s.3) != (ow, oh)) {
                if let Some((tex, fbo, _, _)) = self.staging.take() {
                    gl.delete_framebuffer(fbo);
                    gl.delete_texture(tex);
                }
                let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, ow as i32, oh as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                let fbo = gl.create_framebuffer().map_err(|e| anyhow!(e))?;
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
                self.staging = Some((tex, fbo, ow, oh));
            }
            let (tex, dst_fbo, _, _) = self.staging.unwrap();
            // Blitting first matters on NVIDIA: reading the imported (tiled) buffer
            // directly with glReadPixels takes a pathological ~800 ms path.
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(src_fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(dst_fbo));
            let filter = if (ow, oh) == (w, h) { glow::NEAREST } else { glow::LINEAR };
            gl.blit_framebuffer(0, 0, w as i32, h as i32, 0, 0, ow as i32, oh as i32, glow::COLOR_BUFFER_BIT, filter);
            gl.flush();
            Ok(tex.0.get())
        }
    }

    /// Copies the DMA-BUF into `out` as tightly packed BGRA (BGRx) rows, scaled to `ow`x`oh`.
    #[allow(clippy::too_many_arguments)]
    pub fn read(
        &mut self,
        w: u32,
        h: u32,
        ow: u32,
        oh: u32,
        format: u32,
        modifier: u64,
        planes: &[Plane],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        self.blit(w, h, ow, oh, format, modifier, planes)?;
        let (_, dst_fbo, _, _) = self.staging.unwrap();
        out.resize(ow as usize * oh as usize * 4, 0);
        unsafe {
            let gl = &self.gl;
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(dst_fbo));
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.read_pixels(
                0,
                0,
                ow as i32,
                oh as i32,
                glow::BGRA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(out.as_mut_slice())),
            );
            let err = gl.get_error();
            if err != glow::NO_ERROR {
                bail!("glReadPixels error {err:#x}");
            }
        }
        Ok(())
    }

    /// Zero-copy variant: the scaled frame stays on the GPU as an FFmpeg hardware
    /// frame (CUDA RGB0 or VAAPI NV12) for the encoder.
    #[allow(clippy::too_many_arguments)]
    pub fn read_gpu(
        &mut self,
        w: u32,
        h: u32,
        ow: u32,
        oh: u32,
        format: u32,
        modifier: u64,
        planes: &[Plane],
        interop: &mut Interop,
    ) -> Result<ffmpeg_next::frame::Video> {
        match interop {
            Interop::Cuda(i) => self.read_cuda(w, h, ow, oh, format, modifier, planes, i),
            Interop::Vaapi(i) => self.read_vaapi(w, h, ow, oh, format, modifier, planes, i),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn read_vaapi(
        &mut self,
        w: u32,
        h: u32,
        ow: u32,
        oh: u32,
        format: u32,
        modifier: u64,
        planes: &[Plane],
        interop: &mut crate::vaapi::Interop,
    ) -> Result<ffmpeg_next::frame::Video> {
        let src = self.blit(w, h, ow, oh, format, modifier, planes)?;
        let (frame, surface) = interop.get(ow, oh)?;
        if self.nv12_targets.0 != interop.generation {
            for (_, (y, uv)) in std::mem::take(&mut self.nv12_targets.1) {
                self.release(y);
                self.release(uv);
            }
            self.nv12_targets.0 = interop.generation;
        }
        if !self.nv12_targets.1.contains_key(&surface) {
            let exp = interop.export(&frame)?;
            let (l0, l1) = (&exp.layers[0], &exp.layers[1]);
            let y = self.import(ow, oh, l0.drm_format, l0.modifier, &[l0.plane]).context("import luma surface")?;
            let uv = self.import(ow.div_ceil(2), oh.div_ceil(2), l1.drm_format, l1.modifier, &[l1.plane]).context("import chroma surface")?;
            debug!("VAAPI surface {surface:#x}: luma {:#x}/{:#x}, chroma {:#x}", l0.drm_format, l0.modifier, l1.drm_format);
            self.nv12_targets.1.insert(surface, (y, uv));
        }
        let (prog, vao) = match self.nv12_prog {
            Some(p) => p,
            None => {
                let p = self.nv12_program()?;
                self.nv12_prog = Some(p);
                p
            }
        };
        let (y_fbo, uv_fbo) = {
            let (y, uv) = &self.nv12_targets.1[&surface];
            (y.fbo, uv.fbo)
        };
        unsafe {
            let gl = &self.gl;
            gl.use_program(Some(prog));
            gl.bind_vertex_array(Some(vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(glow::NativeTexture(std::num::NonZeroU32::new(src).unwrap())));
            gl.uniform_1_i32(gl.get_uniform_location(prog, "src").as_ref(), 0);
            let chroma = gl.get_uniform_location(prog, "chroma");
            for (fbo, vw, vh, c) in [(y_fbo, ow, oh, 0), (uv_fbo, ow.div_ceil(2), oh.div_ceil(2), 1)] {
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbo));
                gl.viewport(0, 0, vw as i32, vh as i32);
                gl.uniform_1_i32(chroma.as_ref(), c);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
            // The encoder reads the surface right away: wait until it's written.
            gl.finish();
            let err = gl.get_error();
            if err != glow::NO_ERROR {
                bail!("GL error {err:#x} converting to NV12");
            }
        }
        Ok(frame)
    }

    fn nv12_program(&self) -> Result<(glow::Program, glow::VertexArray)> {
        unsafe {
            let gl = &self.gl;
            let prog = gl.create_program().map_err(|e| anyhow!(e))?;
            for (kind, src) in [(glow::VERTEX_SHADER, NV12_VS), (glow::FRAGMENT_SHADER, NV12_FS)] {
                let sh = gl.create_shader(kind).map_err(|e| anyhow!(e))?;
                gl.shader_source(sh, src);
                gl.compile_shader(sh);
                if !gl.get_shader_compile_status(sh) {
                    bail!("NV12 shader: {}", gl.get_shader_info_log(sh));
                }
                gl.attach_shader(prog, sh);
            }
            gl.link_program(prog);
            if !gl.get_program_link_status(prog) {
                bail!("NV12 program: {}", gl.get_program_info_log(prog));
            }
            let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
            Ok((prog, vao))
        }
    }

    fn release(&self, imp: Imported) {
        unsafe {
            self.gl.delete_framebuffer(imp.fbo);
            self.gl.delete_texture(imp.tex);
        }
        let _ = self.egl.destroy_image(self.display, imp.image);
    }

    #[allow(clippy::too_many_arguments)]
    fn read_cuda(
        &mut self,
        w: u32,
        h: u32,
        ow: u32,
        oh: u32,
        format: u32,
        modifier: u64,
        planes: &[Plane],
        interop: &mut crate::cuda::Interop,
    ) -> Result<ffmpeg_next::frame::Video> {
        self.blit(w, h, ow, oh, format, modifier, planes)?;
        let (_, fbo, _, _) = self.staging.unwrap();
        let bytes = ow as usize * oh as usize * 4;
        let pbo = unsafe {
            let gl = &self.gl;
            if self.pbo.is_none_or(|(_, n)| n != bytes) {
                if let Some((b, _)) = self.pbo.take() {
                    gl.delete_buffer(b);
                }
                let b = gl.create_buffer().map_err(|e| anyhow!(e))?;
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(b));
                // Immutable storage: the driver can't swap the memory behind CUDA's back.
                gl.buffer_storage(glow::PIXEL_PACK_BUFFER, bytes as i32, None, 0);
                self.pbo = Some((b, bytes));
                self.pbo_generation += 1;
            }
            let (b, _) = self.pbo.unwrap();
            // GPU-to-GPU: read the scaled frame into the pixel buffer, no CPU copy.
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(b));
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.read_pixels(0, 0, ow as i32, oh as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::BufferOffset(0));
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.finish();
            b.0.get()
        };
        interop.copy_buffer(pbo, self.pbo_generation, ow, oh)
    }

    /// Forget imported buffers (after renegotiation / buffer removal).
    pub fn clear(&mut self) {
        let cache: Vec<_> = self.cache.drain().map(|(_, i)| i).collect();
        for imp in cache {
            self.release(imp);
        }
    }
}

impl Drop for Importer {
    fn drop(&mut self) {
        self.clear();
    }
}
