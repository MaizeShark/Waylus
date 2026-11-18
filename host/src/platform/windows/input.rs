// SPDX-License-Identifier: GPL-3.0-or-later
//! Pen and touch for Windows 10 1809+ through synthetic pointer devices
//! (`CreateSyntheticPointerDevice` / `InjectSyntheticPointerInput`).
//!
//! The pen is a `PT_PEN` device: pressure 0–1024, tilt in degrees, barrel button and
//! eraser flags, hover (in range, no contact) and contact, so Windows Ink applications
//! (Krita, Photoshop, OneNote, ...) see a real pen. Fingers are a `PT_TOUCH` device with
//! up to 10 contacts, so scrolling and pinch-zoom work. Wire coordinates are normalised
//! to the mirrored monitor; they are mapped to that monitor's rectangle in
//! virtual-desktop pixels (the process is per-monitor DPI aware).

use std::sync::Mutex;

use anyhow::{Result, bail};
use windows::Win32::{
    Foundation::{POINT, RECT},
    UI::{
        Controls::{
            CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT,
            POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
        },
        Input::Pointer::{
            InjectSyntheticPointerInput, POINTER_FLAG_DOWN, POINTER_FLAG_FIFTHBUTTON, POINTER_FLAG_FIRSTBUTTON,
            POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_FLAG_NEW, POINTER_FLAG_SECONDBUTTON, POINTER_FLAG_UP,
            POINTER_FLAG_UPDATE, POINTER_FLAGS, POINTER_INFO, POINTER_PEN_INFO, POINTER_TOUCH_INFO,
        },
        WindowsAndMessaging::{
            PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_FLAG_NONE, PEN_MASK_PRESSURE, PEN_MASK_TILT_X,
            PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH, TOUCH_FLAG_NONE, TOUCH_MASK_CONTACTAREA,
            TOUCH_MASK_ORIENTATION, TOUCH_MASK_PRESSURE,
        },
    },
};

use super::monitors;
use crate::platform::InputTarget;
use crate::proto::{PenSample, TouchPoint, pen_buttons, pen_state};

const MAX_CONTACTS: usize = 10;
const WIRE_MAX: f64 = 65535.0;
/// Windows pen pressure range.
const PRESSURE_MAX: u32 = 1024;
/// Half the side of the contact rectangle reported for a finger, in pixels.
const CONTACT_HALF: i32 = 6;

/// Devices still alive, so the Ctrl-C handler can release them (the process exit does too).
static DEVICES: Mutex<Vec<isize>> = Mutex::new(Vec::new());

struct Device(HSYNTHETICPOINTERDEVICE);

// The handle is an opaque kernel object handle; injection is serialised by the callers'
// mutexes.
unsafe impl Send for Device {}

impl Device {
    fn new(kind: POINTER_INPUT_TYPE, max: u32) -> Result<Self> {
        let h = unsafe { CreateSyntheticPointerDevice(kind, max, POINTER_FEEDBACK_DEFAULT) }
            .map_err(|e| anyhow::anyhow!("CreateSyntheticPointerDevice: {e} (needs Windows 10 1809 or newer)"))?;
        DEVICES.lock().unwrap().push(h.0 as isize);
        Ok(Self(h))
    }

    fn inject(&self, frame: &[POINTER_TYPE_INFO]) -> Result<()> {
        unsafe { InjectSyntheticPointerInput(self.0, frame) }
            .map_err(|e| anyhow::anyhow!("InjectSyntheticPointerInput: {e}"))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        DEVICES.lock().unwrap().retain(|&h| h != self.0.0 as isize);
        unsafe { DestroySyntheticPointerDevice(self.0) };
    }
}

/// Releases every synthetic pointer device (Ctrl-C handler; safe to call more than once).
pub fn release_all() {
    let Ok(mut d) = DEVICES.try_lock() else { return };
    for h in d.drain(..) {
        unsafe { DestroySyntheticPointerDevice(HSYNTHETICPOINTERDEVICE(h as *mut _)) };
    }
}

/// The monitor rectangle the tablet maps to: the capture's, else `--output`, else primary.
fn target_rect(t: &InputTarget) -> Result<RECT> {
    if let Some(g) = t.geometry
        && let (Some((x, y)), Some((w, h))) = (g.position, g.size)
    {
        return Ok(RECT { left: x, top: y, right: x + w, bottom: y + h });
    }
    if let Some(n) = t.output_name {
        return match monitors::find(n) {
            Some(m) => Ok(m.rect),
            None => bail!("monitor {n:?} not found; see `waylus --list-outputs`"),
        };
    }
    let list = monitors::list();
    let remembered = std::fs::read_to_string(monitors::choice_file()).ok();
    let m = remembered
        .as_deref()
        .and_then(|n| list.iter().find(|m| m.name == n.trim()))
        .or_else(|| list.iter().find(|m| m.primary))
        .or(list.first());
    m.map(|m| m.rect).ok_or_else(|| anyhow::anyhow!("no monitors found"))
}

fn map(rect: &RECT, x: u16, y: u16) -> POINT {
    let w = (rect.right - rect.left - 1).max(0) as f64;
    let h = (rect.bottom - rect.top - 1).max(0) as f64;
    POINT { x: rect.left + (x as f64 / WIRE_MAX * w).round() as i32, y: rect.top + (y as f64 / WIRE_MAX * h).round() as i32 }
}

pub struct Pen {
    dev: Device,
    rect: RECT,
    in_prox: bool,
    eraser: bool,
    touching: bool,
    buttons: u8,
    /// Last position, for the "left range" frame.
    last: POINT,
}

impl Pen {
    pub fn new(target: &InputTarget) -> Result<Self> {
        Ok(Self {
            dev: Device::new(PT_PEN, 1)?,
            rect: target_rect(target)?,
            in_prox: false,
            eraser: false,
            touching: false,
            buttons: 0,
            last: POINT::default(),
        })
    }

    fn frame(&self, flags: POINTER_FLAGS, at: POINT, pressure: u32, tilt: (i32, i32)) -> POINTER_TYPE_INFO {
        let mut pen_flags = PEN_FLAG_NONE;
        if self.buttons & pen_buttons::PRIMARY != 0 {
            pen_flags |= PEN_FLAG_BARREL;
        }
        if self.eraser {
            pen_flags |= PEN_FLAG_INVERTED | PEN_FLAG_ERASER;
        }
        let pen = POINTER_PEN_INFO {
            pointerInfo: POINTER_INFO { pointerType: PT_PEN, pointerId: 0, pointerFlags: flags, ptPixelLocation: at, ..Default::default() },
            penFlags: pen_flags,
            penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
            pressure,
            rotation: 0,
            tiltX: tilt.0,
            tiltY: tilt.1,
        };
        POINTER_TYPE_INFO { r#type: PT_PEN, Anonymous: POINTER_TYPE_INFO_0 { penInfo: pen } }
    }

    /// Button bits of `pointerFlags` while the pen is in range.
    fn button_flags(&self) -> POINTER_FLAGS {
        let mut f = POINTER_FLAGS(0);
        if self.touching {
            f |= if self.eraser { POINTER_FLAG_FIFTHBUTTON } else { POINTER_FLAG_FIRSTBUTTON };
        }
        // The barrel button (and the pen's second button) reports as the second button.
        if self.buttons != 0 {
            f |= POINTER_FLAG_SECONDBUTTON;
        }
        f
    }

    fn leave(&mut self) -> Result<()> {
        if self.touching {
            self.touching = false;
            self.dev.inject(&[self.frame(POINTER_FLAG_UP | POINTER_FLAG_INRANGE, self.last, 0, (0, 0))])?;
        }
        self.buttons = 0;
        // Out of range: an update without INRANGE.
        self.dev.inject(&[self.frame(POINTER_FLAG_UPDATE, self.last, 0, (0, 0))])?;
        self.in_prox = false;
        Ok(())
    }

    pub fn handle(&mut self, s: &PenSample) -> Result<()> {
        if s.state == pen_state::OUT {
            if self.in_prox {
                self.leave()?;
            }
            return Ok(());
        }
        let eraser = s.buttons & pen_buttons::ERASER != 0;
        if self.in_prox && eraser != self.eraser {
            // Tool switch: leave proximity with the old tool first.
            self.leave()?;
        }
        self.eraser = eraser;
        let at = map(&self.rect, s.x, s.y);
        self.last = at;
        let touching = s.state == pen_state::CONTACT;
        let pressure = if touching { (s.pressure as u32 * PRESSURE_MAX + 32767) / 65535 } else { 0 };
        let tilt = ((s.tilt_x as i32 / 100).clamp(-90, 90), (s.tilt_y as i32 / 100).clamp(-90, 90));
        let barrel = s.buttons & (pen_buttons::PRIMARY | pen_buttons::SECONDARY);

        let mut frames: Vec<POINTER_TYPE_INFO> = Vec::with_capacity(2);
        if !self.in_prox {
            self.in_prox = true;
            self.buttons = barrel;
            frames.push(self.frame(POINTER_FLAG_NEW | POINTER_FLAG_INRANGE | POINTER_FLAG_UPDATE | self.button_flags(), at, 0, tilt));
        }
        self.buttons = barrel;
        let base = POINTER_FLAG_INRANGE;
        if touching && !self.touching {
            self.touching = true;
            frames.push(self.frame(POINTER_FLAG_DOWN | POINTER_FLAG_INCONTACT | base | self.button_flags(), at, pressure, tilt));
        } else if !touching && self.touching {
            self.touching = false;
            frames.push(self.frame(POINTER_FLAG_UP | base | self.button_flags(), at, 0, tilt));
        } else {
            let contact = if touching { POINTER_FLAG_INCONTACT } else { POINTER_FLAGS(0) };
            frames.push(self.frame(POINTER_FLAG_UPDATE | contact | base | self.button_flags(), at, pressure, tilt));
        }
        // One call per frame: Windows wants the frames of a single pointer in order.
        for f in &frames {
            self.dev.inject(std::slice::from_ref(f))?;
        }
        Ok(())
    }

    /// Lift everything, e.g. when the connection drops.
    pub fn reset(&mut self) -> Result<()> {
        if self.in_prox { self.leave() } else { Ok(()) }
    }
}

#[derive(Clone, Copy)]
struct Slot {
    /// Wire id of the contact occupying this slot.
    id: u8,
    x: u16,
    y: u16,
}

pub struct Touch {
    dev: Device,
    rect: RECT,
    slots: [Option<Slot>; MAX_CONTACTS],
}

impl Touch {
    pub fn new(target: &InputTarget) -> Result<Self> {
        Ok(Self { dev: Device::new(PT_TOUCH, MAX_CONTACTS as u32)?, rect: target_rect(target)?, slots: [None; MAX_CONTACTS] })
    }

    fn frame(&self, slot: usize, flags: POINTER_FLAGS, at: POINT) -> POINTER_TYPE_INFO {
        let contact_rect = RECT { left: at.x - CONTACT_HALF, top: at.y - CONTACT_HALF, right: at.x + CONTACT_HALF, bottom: at.y + CONTACT_HALF };
        let touch = POINTER_TOUCH_INFO {
            pointerInfo: POINTER_INFO { pointerType: PT_TOUCH, pointerId: slot as u32, pointerFlags: flags, ptPixelLocation: at, ..Default::default() },
            touchFlags: TOUCH_FLAG_NONE,
            touchMask: TOUCH_MASK_CONTACTAREA | TOUCH_MASK_ORIENTATION | TOUCH_MASK_PRESSURE,
            rcContact: contact_rect,
            rcContactRaw: contact_rect,
            orientation: 90,
            pressure: 32,
        };
        POINTER_TYPE_INFO { r#type: PT_TOUCH, Anonymous: POINTER_TYPE_INFO_0 { touchInfo: touch } }
    }

    pub fn handle(&mut self, points: &[TouchPoint]) -> Result<()> {
        let mut frame: Vec<POINTER_TYPE_INFO> = Vec::with_capacity(MAX_CONTACTS);
        let contact = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
        // Contacts that disappeared: UP, then the slot is free.
        for i in 0..MAX_CONTACTS {
            if let Some(s) = self.slots[i]
                && !points.iter().any(|p| p.id == s.id)
            {
                frame.push(self.frame(i, POINTER_FLAG_UP, map(&self.rect, s.x, s.y)));
                self.slots[i] = None;
            }
        }
        for p in points {
            let at = map(&self.rect, p.x, p.y);
            if let Some(i) = self.slots.iter().position(|s| s.is_some_and(|s| s.id == p.id)) {
                self.slots[i] = Some(Slot { id: p.id, x: p.x, y: p.y });
                frame.push(self.frame(i, POINTER_FLAG_UPDATE | contact, at));
            } else if let Some(i) = self.slots.iter().position(Option::is_none) {
                self.slots[i] = Some(Slot { id: p.id, x: p.x, y: p.y });
                frame.push(self.frame(i, POINTER_FLAG_DOWN | POINTER_FLAG_NEW | contact, at));
            }
        }
        // Slots that were already down and not in `points` are handled above; contacts that
        // stayed unchanged are in `points` too, so the frame lists every active contact.
        if !frame.is_empty() {
            self.dev.inject(&frame)?;
        }
        Ok(())
    }

    pub fn reset(&mut self) -> Result<()> {
        self.handle(&[])
    }
}

/// `waylus --test-input`: draws a few pen strokes and a two-finger pinch on the target
/// monitor, to check pressure, tilt, hover, buttons and touch with a pointer listener.
pub fn selftest(output: Option<&str>) -> Result<()> {
    use crate::proto::{PenSample, TouchPoint, pen_buttons, pen_state};
    let target = InputTarget { area: crate::platform::AreaMm { width: 100.0, height: 60.0 }, geometry: None, output_name: output };
    let mut pen = Pen::new(&target)?;
    let mut touch = Touch::new(&target)?;
    let sleep = |ms| std::thread::sleep(std::time::Duration::from_millis(ms));
    let sample = |state: u8, x: f64, y: f64, pressure: f64, tilt: i16, buttons: u8| PenSample {
        state,
        x: (x * 65535.0) as u16,
        y: (y * 65535.0) as u16,
        pressure: (pressure * 65535.0) as u16,
        tilt_x: tilt,
        tilt_y: -tilt / 2,
        buttons,
        t_ns: 0,
    };
    println!("hover");
    for i in 0..30u32 {
        pen.handle(&sample(pen_state::HOVER, 0.2 + i as f64 * 0.01, 0.3, 0.0, 0, 0))?;
        sleep(8);
    }
    println!("stroke with pressure ramp and tilt");
    for i in 0..120 {
        let t = i as f64 / 119.0;
        pen.handle(&sample(pen_state::CONTACT, 0.2 + 0.6 * t, 0.4, t, (t * 4500.0) as i16, 0))?;
        sleep(8);
    }
    println!("hover again, barrel button");
    for i in 0..30u32 {
        pen.handle(&sample(pen_state::HOVER, 0.8, 0.5, 0.0, 0, pen_buttons::PRIMARY))?;
        sleep(8);
    }
    println!("eraser stroke");
    for i in 0..60 {
        let t = i as f64 / 59.0;
        pen.handle(&sample(pen_state::CONTACT, 0.8 - 0.6 * t, 0.6, 0.5, 0, pen_buttons::ERASER))?;
        sleep(8);
    }
    pen.handle(&sample(pen_state::OUT, 0.2, 0.6, 0.0, 0, 0))?;
    println!("two-finger pinch");
    for i in 0..60 {
        let d = 0.05 + 0.2 * i as f64 / 59.0;
        let p = |id, x: f64, y: f64| TouchPoint { id, x: (x * 65535.0) as u16, y: (y * 65535.0) as u16, };
        touch.handle(&[p(7, 0.5 - d, 0.5), p(9, 0.5 + d, 0.5)])?;
        sleep(8);
    }
    touch.reset()?;
    println!("done");
    Ok(())
}
