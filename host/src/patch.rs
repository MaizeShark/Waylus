// SPDX-License-Identifier: GPL-3.0-or-later
//! Fast patches (`--fast-patches`, experimental; docs/damage-path.md, design A).
//!
//! While the pen is near the screen, the capture backend reads a window of the scaled
//! frame around the pen back to the CPU (the same pixels the encoder gets), finds the
//! 16×16 tiles that changed since it last looked, and sends them right away as raw
//! LZ4-compressed RGBA rectangles (`PATCH`), before the frame goes to the encoder. The
//! app draws them on a layer above the video until the H.264 frame with the same
//! capture time (or a newer one) is on screen. H.264 is unchanged and stays the ground
//! truth, so a dropped or late patch costs nothing.

// The Windows capture doesn't make patches yet (it would need a D3D11 readback of the window).
#![cfg_attr(windows, allow(dead_code))]

use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use crate::damage::{Damage, Rect, TILE, tiles};

/// `Hello.caps` bit: the app can draw patches.
pub const CAP_PATCHES: u32 = 1;

/// Side of the square looked at around the pen, in frame pixels (a multiple of [`TILE`]).
pub const WINDOW: u32 = 512;
/// Patches are only made while the pen was seen this recently.
const PEN_RECENT_NS: u64 = 500_000_000;
/// A frame whose patches are larger than this (LZ4) goes through H.264 only.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// One frame's patches: rectangles of the `frame_w`×`frame_h` stream picture, each as
/// LZ4 (block format) of its RGBA pixels, rows packed.
pub struct PatchMsg {
    pub capture_ns: u64,
    pub frame_w: u16,
    pub frame_h: u16,
    pub rects: Vec<(Rect, Vec<u8>)>,
    /// Uncompressed size, for statistics.
    pub raw_bytes: usize,
}

impl PatchMsg {
    pub fn bytes(&self) -> usize {
        self.rects.iter().map(|r| r.1.len()).sum()
    }
}

/// Shared between the session (pen position, on/off, where patches go) and the capture
/// backend (which makes them).
#[derive(Default)]
pub struct PatchCtl {
    /// Set by a capture backend that can make patches.
    pub supported: AtomicBool,
    enabled: AtomicBool,
    /// Pen position in 1/65536 of the frame (`x << 16 | y`) and when it was seen (host ns).
    pen_xy: AtomicU32,
    pen_ns: AtomicU64,
    pub max_bytes: AtomicU32,
    tx: Mutex<Option<crossbeam_channel::Sender<PatchMsg>>>,
    /// Statistics (capture side): frames looked at, frames with patches sent, dropped
    /// because too large, dropped because the sender was busy.
    pub looked: AtomicU64,
    pub made: AtomicU64,
    pub too_large: AtomicU64,
    pub busy: AtomicU64,
    /// Readback + diff + compression time, ns (sum).
    pub work_ns: AtomicU64,
}

impl PatchCtl {
    /// Pen sample in the video rectangle (0..=65535 on both axes).
    pub fn pen(&self, x: u16, y: u16, now: u64) {
        self.pen_xy.store((x as u32) << 16 | y as u32, Ordering::Relaxed);
        self.pen_ns.store(now, Ordering::Relaxed);
    }

    /// Where the pen is, if patches are on and it was seen recently.
    pub fn pen_at(&self, now: u64) -> Option<(u16, u16)> {
        if !self.enabled.load(Ordering::Relaxed) || now.saturating_sub(self.pen_ns.load(Ordering::Relaxed)) > PEN_RECENT_NS {
            return None;
        }
        let v = self.pen_xy.load(Ordering::Relaxed);
        Some(((v >> 16) as u16, v as u16))
    }

    pub fn start(&self, tx: crossbeam_channel::Sender<PatchMsg>, max_bytes: usize) {
        self.max_bytes.store(max_bytes as u32, Ordering::Relaxed);
        *self.tx.lock().unwrap() = Some(tx);
        self.enabled.store(true, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.enabled.store(false, Ordering::SeqCst);
        self.tx.lock().unwrap().take();
    }

    /// Hands patches to the session's sender; drops them if it is still busy (never queues).
    pub fn send(&self, m: PatchMsg) {
        let g = self.tx.lock().unwrap();
        match g.as_ref().map(|tx| tx.try_send(m)) {
            Some(Ok(())) => {
                self.made.fetch_add(1, Ordering::Relaxed);
            }
            Some(Err(_)) => {
                self.busy.fetch_add(1, Ordering::Relaxed);
            }
            None => {}
        }
    }
}

/// Finds what changed in the window around the pen and packs it (capture thread).
#[derive(Default)]
pub struct Patcher {
    /// What the window looked like when last read, for the whole frame (RGBA), and
    /// which tiles of it are known.
    shadow: Vec<u8>,
    known: Vec<bool>,
    size: (u32, u32),
    /// The window's pixels as read (RGBA, rows packed).
    pub window: Vec<u8>,
    scratch: Vec<u8>,
    /// The last `diff` returned None because the patches were too large.
    pub last_too_large: bool,
}

impl Patcher {
    /// The window around the pen, tile-aligned and inside the `fw`×`fh` frame.
    pub fn window_at(fw: u32, fh: u32, pen: (u16, u16)) -> Rect {
        let side = |len: u32| WINDOW.min(len / TILE * TILE).max(TILE.min(len));
        let (ww, wh) = (side(fw), side(fh));
        let cx = (pen.0 as u64 * fw as u64 / 65536) as u32;
        let cy = (pen.1 as u64 * fh as u64 / 65536) as u32;
        let place = |c: u32, len: u32, w: u32| (c.saturating_sub(w / 2) / TILE * TILE).min(len.saturating_sub(w) / TILE * TILE);
        Rect { x: place(cx, fw, ww), y: place(cy, fh, wh), w: ww, h: wh }
    }

    /// `self.window` holds the pixels of `win` (4 bytes per pixel, rows packed; `bgra`:
    /// B,G,R,x order, else R,G,B,x). Returns the changed tiles as patches, or None if
    /// nothing changed or the patches would be larger than `max_bytes`.
    pub fn diff(&mut self, capture_ns: u64, fw: u32, fh: u32, win: Rect, bgra: bool, max_bytes: usize) -> Option<PatchMsg> {
        if self.size != (fw, fh) {
            self.size = (fw, fh);
            self.shadow = vec![0; fw as usize * fh as usize * 4];
            self.known = vec![false; (fw.div_ceil(TILE) * fh.div_ceil(TILE)) as usize];
        }
        let tw = fw.div_ceil(TILE) as usize;
        let row = win.w as usize * 4;
        let stride = fw as usize * 4;
        self.last_too_large = false;
        if self.window.len() < row * win.h as usize {
            return None;
        }
        let mut d = Damage { frames: 1, ..Default::default() };
        let mut ty = win.y;
        while ty < win.y + win.h {
            let th = TILE.min(win.y + win.h - ty);
            let mut tx = win.x;
            while tx < win.x + win.w {
                let tww = TILE.min(win.x + win.w - tx) as usize * 4;
                let ti = (ty / TILE) as usize * tw + (tx / TILE) as usize;
                // A tile seen for the first time (the window moved onto it) only seeds the
                // comparison: what is there now is most likely what the video already shows,
                // and sending it would add a full strip of tiles at every move.
                let known = self.known[ti];
                let mut changed = false;
                for y in ty..ty + th {
                    let src = (y - win.y) as usize * row + (tx - win.x) as usize * 4;
                    let dst = y as usize * stride + tx as usize * 4;
                    let (a, b) = (&self.window[src..src + tww], &mut self.shadow[dst..dst + tww]);
                    if a != b {
                        changed = known;
                        b.copy_from_slice(a);
                    }
                }
                self.known[ti] = true;
                if changed {
                    d.add(Rect { x: tx, y: ty, w: tww as u32 / 4, h: th });
                }
                tx += TILE;
            }
            ty += TILE;
        }
        if d.rects.is_empty() {
            return None;
        }
        let mut rects = Vec::new();
        let (mut total, mut raw) = (0usize, 0usize);
        let merged = tiles(&d, fw, fh, TILE);
        if merged.len() > 255 {
            self.last_too_large = true;
            return None;
        }
        for r in merged {
            self.scratch.clear();
            for y in r.y..r.y + r.h {
                let o = (y - win.y) as usize * row + (r.x - win.x) as usize * 4;
                self.scratch.extend_from_slice(&self.window[o..o + r.w as usize * 4]);
            }
            // RGBA with an opaque alpha: the app's layer is transparent where it has no patch.
            for px in self.scratch.chunks_exact_mut(4) {
                if bgra {
                    px.swap(0, 2);
                }
                px[3] = 255;
            }
            raw += self.scratch.len();
            let packed = lz4_flex::block::compress(&self.scratch);
            total += packed.len();
            if total > max_bytes {
                self.last_too_large = true;
                return None;
            }
            rects.push((r, packed));
        }
        Some(PatchMsg { capture_ns, frame_w: fw as u16, frame_h: fh as u16, rects, raw_bytes: raw })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_stays_inside() {
        assert_eq!(Patcher::window_at(1920, 1080, (0, 0)), Rect { x: 0, y: 0, w: 512, h: 512 });
        assert_eq!(Patcher::window_at(1920, 1080, (65535, 65535)), Rect { x: 1408, y: 560, w: 512, h: 512 });
        let r = Patcher::window_at(1920, 1080, (32768, 32768));
        assert_eq!((r.x, r.y), (704, 272));
        // Smaller than the window: the whole (tile-cropped) frame.
        assert_eq!(Patcher::window_at(300, 200, (32768, 32768)), Rect { x: 0, y: 0, w: 288, h: 192 });
    }

    #[test]
    fn sends_only_changes() {
        let (fw, fh) = (640u32, 480u32);
        let win = Patcher::window_at(fw, fh, (32768, 32768));
        let mut p = Patcher { window: vec![10; (win.w * win.h * 4) as usize], ..Default::default() };
        // First look: nothing to compare with, nothing sent.
        assert!(p.diff(1, fw, fh, win, false, usize::MAX).is_none());
        assert!(p.diff(2, fw, fh, win, false, usize::MAX).is_none());
        // One pixel changes: one tile, opaque, decodable.
        let o = (20 * win.w as usize + 40) * 4;
        p.window[o..o + 4].copy_from_slice(&[1, 2, 3, 0]);
        let m = p.diff(3, fw, fh, win, true, usize::MAX).unwrap();
        assert_eq!(m.rects.len(), 1);
        let (r, data) = &m.rects[0];
        assert_eq!((r.x - win.x, r.y - win.y, r.w, r.h), (32, 16, 16, 16));
        let px = lz4_flex::block::decompress(data, 16 * 16 * 4).unwrap();
        let i = ((20 - 16) * 16 + (40 - 32)) * 4;
        assert_eq!(&px[i..i + 4], &[3, 2, 1, 255]);
        // Too large: nothing.
        p.window.iter_mut().for_each(|b| *b = b.wrapping_add(7));
        assert!(p.diff(4, fw, fh, win, false, 100).is_none());
    }
}
