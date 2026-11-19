// SPDX-License-Identifier: GPL-3.0-or-later
//! Damage: which parts of the screen changed since the previous frame, as reported by
//! the compositor (PipeWire `SPA_META_VideoDamage`, Windows `DirtyRegion`).
//!
//! Experimental (docs/damage-path.md). Nothing here runs unless the damage log
//! (`WAYLUS_DAMAGE_LOG`) or `--fast-patches` asks for it.

use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    /// Clipped to a `w`×`h` frame; None if nothing is left.
    pub fn clip(self, w: u32, h: u32) -> Option<Rect> {
        let x1 = (self.x + self.w).min(w);
        let y1 = (self.y + self.h).min(h);
        (self.x < x1 && self.y < y1).then(|| Rect { x: self.x, y: self.y, w: x1 - self.x, h: y1 - self.y })
    }

    fn union(self, o: Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Rect { x, y, w: x1 - x, h: y1 - y }
    }

    /// Scaled from a `src`-sized frame to `dst`, rounded outwards.
    pub fn scale(self, src: (u32, u32), dst: (u32, u32)) -> Rect {
        if src == dst || src.0 == 0 || src.1 == 0 {
            return self;
        }
        let sx = |v: u32, up: bool| {
            let f = v as u64 * dst.0 as u64;
            (if up { f.div_ceil(src.0 as u64) } else { f / src.0 as u64 }) as u32
        };
        let sy = |v: u32, up: bool| {
            let f = v as u64 * dst.1 as u64;
            (if up { f.div_ceil(src.1 as u64) } else { f / src.1 as u64 }) as u32
        };
        let (x0, y0) = (sx(self.x, false), sy(self.y, false));
        let (x1, y1) = (sx(self.x + self.w, true), sy(self.y + self.h, true));
        Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }
}

/// Damage collected over one or more compositor frames, until a frame is passed on.
/// Frames that capture skips (pacing, draining to the newest buffer) still count.
#[derive(Debug, Clone, Default)]
pub struct Damage {
    /// Everything changed, or the compositor didn't say what.
    pub full: bool,
    pub rects: Vec<Rect>,
    /// Compositor frames merged into this one.
    pub frames: u32,
}

/// More rectangles than this are merged into their bounding box.
const MAX_RECTS: usize = 64;

impl Damage {
    pub fn full() -> Self {
        Damage { full: true, rects: Vec::new(), frames: 1 }
    }

    pub fn add(&mut self, r: Rect) {
        if self.full || r.w == 0 || r.h == 0 {
            return;
        }
        self.rects.push(r);
        if self.rects.len() > MAX_RECTS {
            let b = self.rects.iter().copied().reduce(Rect::union).unwrap();
            self.rects.clear();
            self.rects.push(b);
        }
    }

    pub fn set_full(&mut self) {
        self.full = true;
        self.rects.clear();
    }

    pub fn merge(&mut self, o: Damage) {
        self.frames += o.frames;
        if o.full {
            self.set_full();
        }
        for r in o.rects {
            self.add(r);
        }
    }

    pub fn bbox(&self) -> Option<Rect> {
        self.rects.iter().copied().reduce(Rect::union)
    }

    /// Raw area of the rectangles (overlaps counted twice).
    pub fn area(&self) -> u64 {
        self.rects.iter().map(Rect::area).sum()
    }
}

/// The damaged tiles of a `w`×`h` frame as few rectangles, each a multiple of `tile`
/// (clipped at the frame edge): runs of tiles per tile row, merged with identical runs
/// in the rows below.
pub fn tiles(d: &Damage, w: u32, h: u32, tile: u32) -> Vec<Rect> {
    if d.full {
        return vec![Rect { x: 0, y: 0, w, h }];
    }
    let (tw, th) = (w.div_ceil(tile) as usize, h.div_ceil(tile) as usize);
    let mut grid = vec![false; tw * th];
    for r in &d.rects {
        let Some(r) = r.clip(w, h) else { continue };
        for ty in (r.y / tile) as usize..=((r.y + r.h - 1) / tile) as usize {
            for tx in (r.x / tile) as usize..=((r.x + r.w - 1) / tile) as usize {
                grid[ty * tw + tx] = true;
            }
        }
    }
    // Runs per row: (x0, x1) in tiles.
    let mut out: Vec<(usize, usize, usize, usize)> = Vec::new(); // x0, x1, y0, y1 (exclusive)
    let mut open: Vec<usize> = Vec::new(); // indices into out that end at the previous row
    for ty in 0..th {
        let mut runs = Vec::new();
        let mut tx = 0;
        while tx < tw {
            if grid[ty * tw + tx] {
                let x0 = tx;
                while tx < tw && grid[ty * tw + tx] {
                    tx += 1;
                }
                runs.push((x0, tx));
            } else {
                tx += 1;
            }
        }
        let mut next_open = Vec::new();
        for (x0, x1) in runs {
            if let Some(&i) = open.iter().find(|&&i| out[i].0 == x0 && out[i].1 == x1) {
                out[i].3 = ty + 1;
                next_open.push(i);
            } else {
                out.push((x0, x1, ty, ty + 1));
                next_open.push(out.len() - 1);
            }
        }
        open = next_open;
    }
    let t = tile as usize;
    out.into_iter()
        .filter_map(|(x0, x1, y0, y1)| {
            Rect { x: (x0 * t) as u32, y: (y0 * t) as u32, w: ((x1 - x0) * t) as u32, h: ((y1 - y0) * t) as u32 }.clip(w, h)
        })
        .collect()
}

/// Copies the pixels of `r` out of a 4-byte-per-pixel image into `out` (rows packed).
pub fn extract(data: &[u8], stride: usize, r: Rect, out: &mut Vec<u8>) {
    let row = r.w as usize * 4;
    for y in r.y..r.y + r.h {
        let o = y as usize * stride + r.x as usize * 4;
        out.extend_from_slice(&data[o..o + row]);
    }
}

/// The 16×16 tiles in which `cur` differs from `prev` (both `w`×`h`, 4 bytes per
/// pixel, `stride` bytes per row) as damage. `prev` is then updated to `cur`.
pub fn diff_tiles(prev: &mut Vec<u8>, cur: &[u8], stride: usize, w: u32, h: u32) -> Damage {
    let need = stride * h as usize;
    if prev.len() != need || cur.len() < need {
        prev.clear();
        prev.extend_from_slice(&cur[..need.min(cur.len())]);
        return Damage::full();
    }
    let mut d = Damage { frames: 1, ..Default::default() };
    let tw = w.div_ceil(TILE);
    let mut row_dirty = vec![false; tw as usize];
    for ty in 0..h.div_ceil(TILE) {
        row_dirty.iter_mut().for_each(|v| *v = false);
        let y0 = ty * TILE;
        let y1 = (y0 + TILE).min(h);
        for y in y0..y1 {
            let o = y as usize * stride;
            let (a, b) = (&prev[o..o + w as usize * 4], &cur[o..o + w as usize * 4]);
            if a == b {
                continue;
            }
            for (tx, dirty) in row_dirty.iter_mut().enumerate() {
                if *dirty {
                    continue;
                }
                let x0 = tx * TILE as usize * 4;
                let x1 = (x0 + TILE as usize * 4).min(a.len());
                if a[x0..x1] != b[x0..x1] {
                    *dirty = true;
                }
            }
        }
        for (tx, &dirty) in row_dirty.iter().enumerate() {
            if dirty {
                d.rects.push(Rect { x: tx as u32 * TILE, y: y0, w: TILE, h: TILE });
            }
        }
    }
    prev[..need].copy_from_slice(&cur[..need]);
    d
}

/// LZ4 (block format) size of the pixels of `patches`, compressed one rectangle at a time.
fn lz4_size(data: &[u8], stride: usize, patches: &[Rect], scratch: &mut Vec<u8>, out: &mut Vec<u8>) -> usize {
    let mut total = 0;
    for r in patches {
        scratch.clear();
        extract(data, stride, *r, scratch);
        out.resize(lz4_flex::block::get_maximum_output_size(scratch.len()), 0);
        total += lz4_flex::block::compress_into(scratch, out).unwrap_or(scratch.len());
    }
    total
}

/// Dev flag `WAYLUS_DAMAGE_LOG=<file>`: one CSV line per frame passed on by capture, for
/// docs/damage-path.md phase 1 (`tools/damage/analyze.py` summarises it). Two damage
/// sources: what the compositor reported (`meta`, or full if it reports nothing), and,
/// when the CPU has the pixels (readback or shared memory), the 16×16 tiles that really
/// changed since the previous frame passed on (`diff_*`).
pub struct DamageLog {
    out: std::io::BufWriter<std::fs::File>,
    t0: u64,
    scratch: Vec<u8>,
    lz4: Vec<u8>,
    prev: Vec<u8>,
}

pub const TILE: u32 = 16;

impl DamageLog {
    pub fn from_env() -> Option<DamageLog> {
        let path = std::env::var_os("WAYLUS_DAMAGE_LOG")?;
        let f = match std::fs::File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("WAYLUS_DAMAGE_LOG {}: {e}", path.to_string_lossy());
                return None;
            }
        };
        tracing::info!("logging damage per frame to {}", path.to_string_lossy());
        let mut out = std::io::BufWriter::new(f);
        let _ = writeln!(
            out,
            "t_ms,frame_w,frame_h,merged,meta,full,rects,area,bbox_x,bbox_y,bbox_w,bbox_h,patches,patch_px,bgra_bytes,lz4_bytes,\
             diff_tiles,diff_patches,diff_px,diff_bbox_x,diff_bbox_y,diff_bbox_w,diff_bbox_h,diff_lz4_bytes,lz4_us,diff_us"
        );
        Some(DamageLog { out, t0: crate::proto::now_ns(), scratch: Vec::new(), lz4: Vec::new(), prev: Vec::new() })
    }

    /// `d` in the coordinates of the `w`×`h` frame; `pixels`: the frame as packed
    /// 4-byte pixels when the CPU has them (readback/SHM).
    pub fn frame(&mut self, now: u64, w: u32, h: u32, d: &Damage, meta: bool, pixels: Option<(&[u8], usize)>) {
        let patches = tiles(d, w, h, TILE);
        let patch_px: u64 = patches.iter().map(Rect::area).sum();
        let b = if d.full { Rect { x: 0, y: 0, w, h } } else { d.bbox().unwrap_or_default() };
        let (rects, area) = if d.full { (1, w as u64 * h as u64) } else { (d.rects.len(), d.area()) };
        let _ = write!(
            self.out,
            "{:.3},{w},{h},{},{},{},{rects},{area},{},{},{},{},{},{patch_px},{},",
            (now.saturating_sub(self.t0)) as f64 / 1e6,
            d.frames,
            meta as u8,
            d.full as u8,
            b.x,
            b.y,
            b.w,
            b.h,
            patches.len(),
            patch_px * 4,
        );
        let Some((data, stride)) = pixels.filter(|(data, stride)| data.len() >= stride * h as usize) else {
            let _ = writeln!(self.out, ",,,,,,,,,,");
            return;
        };
        let t = crate::proto::now_ns();
        let lz4 = if d.full { String::new() } else { lz4_size(data, stride, &patches, &mut self.scratch, &mut self.lz4).to_string() };
        let t1 = crate::proto::now_ns();
        let _ = t;
        let diff = diff_tiles(&mut self.prev, data, stride, w, h);
        let dp = tiles(&diff, w, h, TILE);
        let t2 = crate::proto::now_ns();
        let dpx: u64 = dp.iter().map(Rect::area).sum();
        let db = if diff.full { Rect { x: 0, y: 0, w, h } } else { diff.bbox().unwrap_or_default() };
        let dlz4 = lz4_size(data, stride, &dp, &mut self.scratch, &mut self.lz4);
        let t3 = crate::proto::now_ns();
        let _ = writeln!(
            self.out,
            "{lz4},{},{},{dpx},{},{},{},{},{dlz4},{},{}",
            if diff.full { (w.div_ceil(TILE) * h.div_ceil(TILE)) as usize } else { diff.rects.len() },
            dp.len(),
            db.x,
            db.y,
            db.w,
            db.h,
            (t3 - t2) / 1000,
            (t2 - t1) / 1000,
        );
    }
}

impl Drop for DamageLog {
    fn drop(&mut self) {
        let _ = self.out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_finds_changed_tiles() {
        let (w, h) = (64u32, 40u32);
        let stride = w as usize * 4;
        let a = vec![0u8; stride * h as usize];
        let mut prev = Vec::new();
        assert!(diff_tiles(&mut prev, &a, stride, w, h).full);
        let mut b = a.clone();
        b[(35 * stride) + 20 * 4] = 1; // pixel (20, 35): tile (1, 2)
        let d = diff_tiles(&mut prev, &b, stride, w, h);
        assert_eq!(d.rects, vec![Rect { x: 16, y: 32, w: 16, h: 16 }]);
        assert!(diff_tiles(&mut prev, &b, stride, w, h).rects.is_empty());
    }

    #[test]
    fn tiles_merge_runs() {
        let mut d = Damage::default();
        d.add(Rect { x: 5, y: 5, w: 20, h: 40 }); // tiles x 0..2, y 0..3
        d.add(Rect { x: 100, y: 0, w: 1, h: 1 });
        let t = tiles(&d, 200, 100, 16);
        assert_eq!(t, vec![Rect { x: 0, y: 0, w: 32, h: 48 }, Rect { x: 96, y: 0, w: 16, h: 16 }]);
    }

    #[test]
    fn tiles_clip_at_edge() {
        let mut d = Damage::default();
        d.add(Rect { x: 1910, y: 1075, w: 50, h: 50 });
        assert_eq!(tiles(&d, 1920, 1080, 16), vec![Rect { x: 1904, y: 1072, w: 16, h: 8 }]);
        assert_eq!(tiles(&Damage::full(), 10, 10, 16), vec![Rect { x: 0, y: 0, w: 10, h: 10 }]);
    }

    #[test]
    fn scale_rounds_outwards() {
        let r = Rect { x: 1, y: 1, w: 1, h: 1 }.scale((2560, 1440), (1920, 1080));
        assert_eq!(r, Rect { x: 0, y: 0, w: 2, h: 2 });
        let r = Rect { x: 0, y: 0, w: 2560, h: 1440 }.scale((2560, 1440), (1920, 1080));
        assert_eq!(r, Rect { x: 0, y: 0, w: 1920, h: 1080 });
    }

    #[test]
    fn many_rects_collapse_to_bbox() {
        let mut d = Damage::default();
        for i in 0..100 {
            d.add(Rect { x: i * 2, y: 0, w: 1, h: 1 });
        }
        assert!(d.rects.len() <= MAX_RECTS);
        assert_eq!(d.bbox(), Some(Rect { x: 0, y: 0, w: 199, h: 1 }));
    }
}
