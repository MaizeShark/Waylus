// SPDX-License-Identifier: GPL-3.0-or-later
//! "Raw pen": read the tablet's stylus evdev device directly over adb.
//!
//! Android's input pipeline can lose data (e.g. the TCL NXTPAPER drops the tilt
//! direction) and adds latency. The adb shell user may read `/dev/input/*`, so we
//! stream the raw `input_event`s with `adb exec-out cat` and map them ourselves,
//! using the display rotation and video rectangle the app reports (`Layout`).

use std::{
    io::Read,
    process::{Child, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};

use crate::{
    adb::Adb,
    proto::{Layout, PenSample, now_ns, pen_buttons, pen_state},
};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;
const ABS_TILT_X: u16 = 0x1a;
const ABS_TILT_Y: u16 = 0x1b;
const BTN_TOOL_PEN: u16 = 0x140;
const BTN_TOOL_RUBBER: u16 = 0x141;
const BTN_TOUCH: u16 = 0x14a;
const BTN_STYLUS: u16 = 0x14b;
const BTN_STYLUS2: u16 = 0x14c;

#[derive(Debug, Clone, Copy, Default)]
struct Range {
    min: i32,
    max: i32,
}

impl Range {
    fn norm(&self, v: i32) -> f64 {
        if self.max <= self.min { 0.0 } else { (v - self.min) as f64 / (self.max - self.min) as f64 }
    }
}

#[derive(Debug, Clone)]
pub struct PenDevice {
    pub path: String,
    pub name: String,
    x: Range,
    y: Range,
    pressure: Range,
    tilt_x: Option<Range>,
    tilt_y: Option<Range>,
}

/// Finds a direct stylus device (pen tool + pressure + X/Y) in `getevent -p` output.
pub fn detect(adb: &Adb) -> Result<PenDevice> {
    let out = adb.shell("getevent -p")?;
    let mut devs = Vec::new();
    let mut cur: Option<(String, String, Vec<u16>, Vec<(u16, Range)>, bool)> = None;
    let mut section = "";
    for line in out.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("add device") {
            if let Some(d) = cur.take() {
                devs.push(d);
            }
            let path = rest.split_whitespace().last().unwrap_or_default().to_owned();
            cur = Some((path, String::new(), vec![], vec![], false));
            continue;
        }
        let Some(d) = cur.as_mut() else { continue };
        if let Some(n) = t.strip_prefix("name:") {
            d.1 = n.trim().trim_matches('"').to_owned();
        } else if t.starts_with("input props:") {
            section = "props";
        } else if t.starts_with("INPUT_PROP_DIRECT") || (section == "props" && t == "00000001") {
            d.4 = true;
        }
        // Event sections: "KEY (0001): 0140 014a ..." and "ABS (0003): 0000 : value .., min .., max .."
        let body = if let Some(i) = t.find("):") {
            section = if t.starts_with("KEY") { "key" } else if t.starts_with("ABS") { "abs" } else { "other" };
            t[i + 2..].trim()
        } else {
            t
        };
        match section {
            "key" => d.2.extend(body.split_whitespace().filter_map(|c| u16::from_str_radix(c, 16).ok())),
            "abs" => {
                if let Some((code, rest)) = body.split_once(':')
                    && let Ok(code) = u16::from_str_radix(code.trim(), 16)
                {
                    let get = |k: &str| {
                        rest.split(',')
                            .find_map(|kv| kv.trim().strip_prefix(k).map(|v| v.trim().parse::<i32>().unwrap_or(0)))
                    };
                    if let (Some(min), Some(max)) = (get("min"), get("max")) {
                        d.3.push((code, Range { min, max }));
                    }
                }
            }
            _ => {}
        }
    }
    devs.extend(cur);
    for (path, name, keys, abs, direct) in devs {
        let a = |c: u16| abs.iter().find(|(k, _)| *k == c).map(|(_, r)| *r);
        if direct && keys.contains(&BTN_TOOL_PEN) && a(ABS_PRESSURE).is_some() && a(ABS_X).is_some() && a(ABS_Y).is_some() {
            return Ok(PenDevice {
                path,
                name,
                x: a(ABS_X).unwrap(),
                y: a(ABS_Y).unwrap(),
                pressure: a(ABS_PRESSURE).unwrap(),
                tilt_x: a(ABS_TILT_X),
                tilt_y: a(ABS_TILT_Y),
            });
        }
    }
    bail!("no direct stylus input device found")
}

#[derive(Default)]
struct State {
    x: i32,
    y: i32,
    pressure: i32,
    tilt_x: i32,
    tilt_y: i32,
    tool_pen: bool,
    tool_rubber: bool,
    touch: bool,
    stylus: bool,
    stylus2: bool,
    was_in: bool,
}

/// Maps a raw sample into the video rectangle using the app's layout.
fn map(dev: &PenDevice, s: &State, l: &Layout) -> PenSample {
    let (nw, nh) = (l.natural_w as f64, l.natural_h as f64);
    // Natural-orientation display pixels.
    let xn = dev.x.norm(s.x) * nw;
    let yn = dev.y.norm(s.y) * nh;
    // Rotate into the current display orientation (Android Surface.ROTATION_*).
    let (xc, yc) = match l.rotation & 3 {
        1 => (yn, nw - xn),
        2 => (nw - xn, nh - yn),
        3 => (nh - yn, xn),
        _ => (xn, yn),
    };
    let (tx, ty) = (s.tilt_x, s.tilt_y);
    let (tx, ty) = match l.rotation & 3 {
        1 => (ty, -tx),
        2 => (-tx, -ty),
        3 => (-ty, tx),
        _ => (tx, ty),
    };
    let (rl, rt, rr, rb) = (l.left as f64, l.top as f64, l.right as f64, l.bottom as f64);
    let nx = ((xc - rl) / (rr - rl).max(1.0)).clamp(0.0, 1.0);
    let ny = ((yc - rt) / (rb - rt).max(1.0)).clamp(0.0, 1.0);
    // Raw tilt is in degrees: Android's InputReader assumes exactly that (M_PI/180)
    // whatever range the driver advertises (the NXTPAPER claims ±9000 but sends
    // ±60-ish). The wire format wants centidegrees.
    let scale_tilt = |v: i32, r: Option<Range>| -> i16 {
        match r {
            Some(_) => (v.clamp(-90, 90) * 100) as i16,
            None => 0,
        }
    };
    let mut buttons = 0;
    if s.stylus {
        buttons |= pen_buttons::PRIMARY;
    }
    if s.stylus2 {
        buttons |= pen_buttons::SECONDARY;
    }
    if s.tool_rubber {
        buttons |= pen_buttons::ERASER;
    }
    let state = if !(s.tool_pen || s.tool_rubber) {
        pen_state::OUT
    } else if s.touch && s.pressure > dev.pressure.min {
        pen_state::CONTACT
    } else {
        pen_state::HOVER
    };
    PenSample {
        t_ns: 0,
        state,
        buttons,
        x: (nx * 65535.0).round() as u16,
        y: (ny * 65535.0).round() as u16,
        pressure: (dev.pressure.norm(s.pressure).clamp(0.0, 1.0) * 65535.0).round() as u16,
        tilt_x: scale_tilt(tx, dev.tilt_x),
        tilt_y: scale_tilt(ty, dev.tilt_y),
    }
}

/// Shared between the raw reader and the session: current layout (None = no
/// tablet session) and where to deliver samples.
pub type Sink = Arc<Mutex<Option<(Layout, Box<dyn FnMut(PenSample) + Send>)>>>;

pub struct RawPen {
    child: Child,
    adb: Adb,
    pub device: String,
}

impl Drop for RawPen {
    fn drop(&mut self) {
        let _ = self.child.kill();
        self.adb.kill_remote_cat(&self.device);
    }
}

/// Starts streaming the stylus device; samples go to whatever `sink` holds.
pub fn start(adb: &Adb, sink: Sink) -> Result<RawPen> {
    let device = detect(adb)?;
    info!(
        "raw pen: {} ({}), x {}..{}, y {}..{}, pressure {}..{}, tilt {}",
        device.name,
        device.path,
        device.x.min,
        device.x.max,
        device.y.min,
        device.y.max,
        device.pressure.min,
        device.pressure.max,
        if device.tilt_x.is_some() { "x+y" } else { "none" }
    );
    adb.kill_remote_cat(&device.path); // leftovers from crashed or killed sessions
    let mut child = adb
        .command()
        .args(["exec-out", "cat", &device.path])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn adb exec-out cat")?;
    let mut out = child.stdout.take().unwrap();
    let dev = device.clone();
    thread::Builder::new().name("rawpen".into()).spawn(move || {
        // arm64/x86_64 userspace: struct input_event = timeval(16) + type(2) + code(2) + value(4).
        let mut buf = [0u8; 24 * 64];
        let mut have = 0usize;
        let mut s = State::default();
        loop {
            let n = match out.read(&mut buf[have..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            have += n;
            let whole = have / 24 * 24;
            for ev in buf[..whole].chunks_exact(24) {
                let ty = u16::from_le_bytes([ev[16], ev[17]]);
                let code = u16::from_le_bytes([ev[18], ev[19]]);
                let value = i32::from_le_bytes([ev[20], ev[21], ev[22], ev[23]]);
                match (ty, code) {
                    (EV_ABS, ABS_X) => s.x = value,
                    (EV_ABS, ABS_Y) => s.y = value,
                    (EV_ABS, ABS_PRESSURE) => s.pressure = value,
                    (EV_ABS, ABS_TILT_X) => s.tilt_x = value,
                    (EV_ABS, ABS_TILT_Y) => s.tilt_y = value,
                    (EV_KEY, BTN_TOOL_PEN) => s.tool_pen = value != 0,
                    (EV_KEY, BTN_TOOL_RUBBER) => s.tool_rubber = value != 0,
                    (EV_KEY, BTN_TOUCH) => s.touch = value != 0,
                    (EV_KEY, BTN_STYLUS) => s.stylus = value != 0,
                    (EV_KEY, BTN_STYLUS2) => s.stylus2 = value != 0,
                    (EV_SYN, 0) => {
                        let in_range = s.tool_pen || s.tool_rubber;
                        if in_range || s.was_in {
                            let mut g = sink.lock().unwrap();
                            if let Some((layout, deliver)) = g.as_mut() {
                                let mut sample = map(&dev, &s, layout);
                                sample.t_ns = now_ns(); // host clock: arrival time
                                deliver(sample);
                            }
                        }
                        s.was_in = in_range;
                    }
                    _ => {}
                }
            }
            buf.copy_within(whole..have, 0);
            have -= whole;
        }
        warn!("raw pen stream ended");
    })?;
    debug!("raw pen streaming from {}", device.path);
    Ok(RawPen { child, adb: adb.clone(), device: device.path.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nxt() -> PenDevice {
        PenDevice {
            path: "/dev/input/event4".into(),
            name: "fts_ts,pen".into(),
            x: Range { min: 0, max: 16009 },
            y: Range { min: 0, max: 24009 },
            pressure: Range { min: 0, max: 4096 },
            tilt_x: Some(Range { min: -9000, max: 9000 }),
            tilt_y: Some(Range { min: -9000, max: 9000 }),
        }
    }

    #[test]
    fn rotation_maps_corners() {
        let d = nxt();
        // Landscape (ROTATION_90): 2400x1600 screen, 16:9 video letterboxed 125 px top/bottom.
        let l = Layout { rotation: 1, natural_w: 1600, natural_h: 2400, left: 0, top: 125, right: 2400, bottom: 1475 };
        // Natural top-right corner (x=max, y=0) is the top-left in ROTATION_90.
        let s = State { x: 16009, y: 0, tool_pen: true, ..Default::default() };
        let p = map(&d, &s, &l);
        assert_eq!((p.x, p.y), (0, 0));
        // Natural bottom-left (x=0, y=max) is the bottom-right.
        let s = State { x: 0, y: 24009, tool_pen: true, ..Default::default() };
        let p = map(&d, &s, &l);
        assert_eq!((p.x, p.y), (65535, 65535));
    }

    #[test]
    fn tilt_degrees_and_rotation() {
        let d = nxt();
        let l = Layout { rotation: 1, natural_w: 1600, natural_h: 2400, left: 0, top: 125, right: 2400, bottom: 1475 };
        // 30° toward natural +X becomes 30° toward screen -Y (up) in ROTATION_90.
        let s = State { tilt_x: 30, tool_pen: true, ..Default::default() };
        let p = map(&d, &s, &l);
        assert_eq!((p.tilt_x, p.tilt_y), (0, -3000));
    }
}
