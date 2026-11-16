// SPDX-License-Identifier: GPL-3.0-or-later
//! DMA-BUF import via EGL + OpenGL.
//!
//! Compositors such as niri only hand out GPU buffers (DMA-BUF) for
//! screencasts. We import them as EGLImages, attach them to a framebuffer and
//! read them back into system memory for the encoder.
//! TODO: zero-copy path (CUDA/GL interop or Vulkan) straight into NVENC.

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
}

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
        Ok(Self { egl, display, _ctx: ctx, gl, image_target, query_modifiers, cache: HashMap::new(), staging: None })
    }

    fn open_display(egl: &Egl) -> Result<egl::Display> {
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

    /// Copies the DMA-BUF into `out` as tightly packed BGRA (BGRx) rows.
    pub fn read(&mut self, w: u32, h: u32, format: u32, modifier: u64, planes: &[Plane], out: &mut Vec<u8>) -> Result<()> {
        let p0 = planes.first().ok_or_else(|| anyhow!("no planes"))?;
        let key = (p0.fd, p0.offset, p0.stride, w << 16 | h);
        if !self.cache.contains_key(&key) {
            let imp = self.import(w, h, format, modifier, planes)?;
            self.cache.insert(key, imp);
        }
        let src_fbo = self.cache[&key].fbo;
        out.resize(w as usize * h as usize * 4, 0);
        unsafe {
            let gl = &self.gl;
            if self.staging.is_none_or(|s| (s.2, s.3) != (w, h)) {
                let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, w as i32, h as i32);
                let fbo = gl.create_framebuffer().map_err(|e| anyhow!(e))?;
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
                self.staging = Some((tex, fbo, w, h));
            }
            let (_, dst_fbo, _, _) = self.staging.unwrap();
            let t0 = std::time::Instant::now();
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(src_fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(dst_fbo));
            gl.blit_framebuffer(0, 0, w as i32, h as i32, 0, 0, w as i32, h as i32, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            gl.finish();
            let t1 = std::time::Instant::now();
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(dst_fbo));
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::BGRA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(out.as_mut_slice())),
            );
            tracing::trace!("blit+finish {:?}, readpixels {:?}", t1 - t0, t1.elapsed());
            let err = gl.get_error();
            if err != glow::NO_ERROR {
                bail!("glReadPixels error {err:#x}");
            }
        }
        Ok(())
    }

    /// Forget imported buffers (after renegotiation / buffer removal).
    pub fn clear(&mut self) {
        for (_, imp) in self.cache.drain() {
            unsafe {
                self.gl.delete_framebuffer(imp.fbo);
                self.gl.delete_texture(imp.tex);
            }
            let _ = self.egl.destroy_image(self.display, imp.image);
        }
    }
}

impl Drop for Importer {
    fn drop(&mut self) {
        self.clear();
    }
}
