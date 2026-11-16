// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual input devices (uinput) that replay the tablet's pen and fingers.
//!
//! The pen shows up in libinput as a tablet tool (Wayland tablet-v2, pressure
//! and tilt for apps), fingers as a direct multitouch screen (`wl_touch`).
//! Coordinates are normalised to the mirrored output; the compositor maps the
//! devices to that output (niri: `input { tablet/touch { map-to-output } }`).

use anyhow::{Context, Result};
use evdev::{
    AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    PropType, UinputAbsSetup, uinput::VirtualDevice,
};

use crate::proto::{PenSample, TouchPoint, pen_buttons, pen_state};

/// Coordinate range used for both devices (matches the u16 wire format).
const AXIS_MAX: i32 = 65535;
const PRESSURE_MAX: i32 = 4095;
/// Tilt is sent in centidegrees; 9000 cd = 90° = π/2 rad.
const TILT_MAX: i32 = 9000;
const TILT_RES_PER_RAD: i32 = 5730; // 9000 / (π/2)
const MAX_SLOTS: usize = 10;

/// Physical size of the area the video occupies on the tablet, used to set
/// the axis resolution (units/mm). libinput wants a resolution on tablets.
#[derive(Debug, Clone, Copy)]
pub struct AreaMm {
    pub width: f32,
    pub height: f32,
}

fn abs(code: Abs, min: i32, max: i32, res: i32) -> UinputAbsSetup {
    UinputAbsSetup::new(code, AbsInfo::new(0, min, max, 0, 0, res))
}

fn ev(ty: EventType, code: u16, value: i32) -> InputEvent {
    InputEvent::new(ty.0, code, value)
}
fn key(code: KeyCode, down: bool) -> InputEvent {
    ev(EventType::KEY, code.0, down as i32)
}
fn absv(code: Abs, value: i32) -> InputEvent {
    ev(EventType::ABSOLUTE, code.0, value)
}

pub struct Pen {
    dev: VirtualDevice,
    in_prox: bool,
    eraser: bool,
    touching: bool,
    buttons: u8,
}

impl Pen {
    pub fn new(area: AreaMm) -> Result<Self> {
        let rx = ((AXIS_MAX as f32 / area.width).round() as i32).max(1);
        let ry = ((AXIS_MAX as f32 / area.height).round() as i32).max(1);
        let keys = AttributeSet::from_iter([
            KeyCode::BTN_TOOL_PEN,
            KeyCode::BTN_TOOL_RUBBER,
            KeyCode::BTN_TOUCH,
            KeyCode::BTN_STYLUS,
            KeyCode::BTN_STYLUS2,
        ]);
        let dev = VirtualDevice::builder()
            .context("open /dev/uinput (are you in the uinput group?)")?
            .name("Waylus Pen")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x5750, 1))
            .with_keys(&keys)?
            .with_absolute_axis(&abs(Abs::ABS_X, 0, AXIS_MAX, rx))?
            .with_absolute_axis(&abs(Abs::ABS_Y, 0, AXIS_MAX, ry))?
            .with_absolute_axis(&abs(Abs::ABS_PRESSURE, 0, PRESSURE_MAX, 0))?
            .with_absolute_axis(&abs(Abs::ABS_TILT_X, -TILT_MAX, TILT_MAX, TILT_RES_PER_RAD))?
            .with_absolute_axis(&abs(Abs::ABS_TILT_Y, -TILT_MAX, TILT_MAX, TILT_RES_PER_RAD))?
            .with_properties(&AttributeSet::from_iter([PropType::DIRECT]))?
            .build()
            .context("create uinput pen")?;
        Ok(Self { dev, in_prox: false, eraser: false, touching: false, buttons: 0 })
    }

    fn tool(&self) -> KeyCode {
        if self.eraser { KeyCode::BTN_TOOL_RUBBER } else { KeyCode::BTN_TOOL_PEN }
    }

    fn leave(&mut self, evs: &mut Vec<InputEvent>) {
        if self.touching {
            evs.push(key(KeyCode::BTN_TOUCH, false));
            evs.push(absv(Abs::ABS_PRESSURE, 0));
            self.touching = false;
        }
        if self.buttons & pen_buttons::PRIMARY != 0 {
            evs.push(key(KeyCode::BTN_STYLUS, false));
        }
        if self.buttons & pen_buttons::SECONDARY != 0 {
            evs.push(key(KeyCode::BTN_STYLUS2, false));
        }
        self.buttons = 0;
        evs.push(key(self.tool(), false));
        self.in_prox = false;
    }

    pub fn handle(&mut self, s: &PenSample) -> Result<()> {
        let mut evs = Vec::with_capacity(12);
        let eraser = s.buttons & pen_buttons::ERASER != 0;

        if s.state == pen_state::OUT {
            if self.in_prox {
                self.leave(&mut evs);
                self.dev.emit(&evs)?;
            }
            return Ok(());
        }
        if self.in_prox && eraser != self.eraser {
            // Tool switch: leave proximity with the old tool first.
            self.leave(&mut evs);
            self.dev.emit(&evs)?;
            evs.clear();
        }
        self.eraser = eraser;

        let touching = s.state == pen_state::CONTACT;
        let pressure = if touching { (s.pressure as i32 * PRESSURE_MAX + 32767) / 65535 } else { 0 };
        // Axes first so the proximity-in frame already carries a valid position.
        evs.push(absv(Abs::ABS_X, s.x as i32));
        evs.push(absv(Abs::ABS_Y, s.y as i32));
        evs.push(absv(Abs::ABS_PRESSURE, pressure));
        evs.push(absv(Abs::ABS_TILT_X, (s.tilt_x as i32).clamp(-TILT_MAX, TILT_MAX)));
        evs.push(absv(Abs::ABS_TILT_Y, (s.tilt_y as i32).clamp(-TILT_MAX, TILT_MAX)));
        if !self.in_prox {
            evs.push(key(self.tool(), true));
            self.in_prox = true;
        }
        let changed = s.buttons ^ self.buttons;
        if changed & pen_buttons::PRIMARY != 0 {
            evs.push(key(KeyCode::BTN_STYLUS, s.buttons & pen_buttons::PRIMARY != 0));
        }
        if changed & pen_buttons::SECONDARY != 0 {
            evs.push(key(KeyCode::BTN_STYLUS2, s.buttons & pen_buttons::SECONDARY != 0));
        }
        self.buttons = s.buttons & (pen_buttons::PRIMARY | pen_buttons::SECONDARY);
        if touching != self.touching {
            evs.push(key(KeyCode::BTN_TOUCH, touching));
            self.touching = touching;
        }
        self.dev.emit(&evs)?;
        Ok(())
    }

    /// Lift everything, e.g. when the connection drops.
    pub fn reset(&mut self) -> Result<()> {
        if self.in_prox {
            let mut evs = Vec::new();
            self.leave(&mut evs);
            self.dev.emit(&evs)?;
        }
        Ok(())
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
    dev: VirtualDevice,
    slots: [Option<Slot>; MAX_SLOTS],
    next_tracking_id: i32,
}

impl Touch {
    pub fn new(area: AreaMm) -> Result<Self> {
        let rx = ((AXIS_MAX as f32 / area.width).round() as i32).max(1);
        let ry = ((AXIS_MAX as f32 / area.height).round() as i32).max(1);
        let dev = VirtualDevice::builder()
            .context("open /dev/uinput")?
            .name("Waylus Touch")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0x5751, 1))
            .with_keys(&AttributeSet::from_iter([KeyCode::BTN_TOUCH]))?
            .with_absolute_axis(&abs(Abs::ABS_X, 0, AXIS_MAX, rx))?
            .with_absolute_axis(&abs(Abs::ABS_Y, 0, AXIS_MAX, ry))?
            .with_absolute_axis(&abs(Abs::ABS_MT_SLOT, 0, MAX_SLOTS as i32 - 1, 0))?
            .with_absolute_axis(&abs(Abs::ABS_MT_TRACKING_ID, 0, 65535, 0))?
            .with_absolute_axis(&abs(Abs::ABS_MT_POSITION_X, 0, AXIS_MAX, rx))?
            .with_absolute_axis(&abs(Abs::ABS_MT_POSITION_Y, 0, AXIS_MAX, ry))?
            .with_properties(&AttributeSet::from_iter([PropType::DIRECT]))?
            .build()
            .context("create uinput touchscreen")?;
        Ok(Self { dev, slots: [None; MAX_SLOTS], next_tracking_id: 0 })
    }

    pub fn handle(&mut self, points: &[TouchPoint]) -> Result<()> {
        let mut evs = Vec::with_capacity(4 + points.len() * 4);
        let was_down = self.slots.iter().any(Option::is_some);

        // Lift contacts that disappeared.
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let Some(s) = slot
                && !points.iter().any(|p| p.id == s.id)
            {
                evs.push(absv(Abs::ABS_MT_SLOT, i as i32));
                evs.push(absv(Abs::ABS_MT_TRACKING_ID, -1));
                *slot = None;
            }
        }
        // Move existing contacts, place new ones in free slots.
        for p in points {
            if let Some(i) = self.slots.iter().position(|s| s.is_some_and(|s| s.id == p.id)) {
                let s = self.slots[i].as_mut().unwrap();
                if (s.x, s.y) != (p.x, p.y) {
                    evs.push(absv(Abs::ABS_MT_SLOT, i as i32));
                    if s.x != p.x {
                        evs.push(absv(Abs::ABS_MT_POSITION_X, p.x as i32));
                    }
                    if s.y != p.y {
                        evs.push(absv(Abs::ABS_MT_POSITION_Y, p.y as i32));
                    }
                    s.x = p.x;
                    s.y = p.y;
                }
            } else if let Some(i) = self.slots.iter().position(Option::is_none) {
                evs.push(absv(Abs::ABS_MT_SLOT, i as i32));
                evs.push(absv(Abs::ABS_MT_TRACKING_ID, self.next_tracking_id));
                self.next_tracking_id = (self.next_tracking_id + 1) & 0xffff;
                evs.push(absv(Abs::ABS_MT_POSITION_X, p.x as i32));
                evs.push(absv(Abs::ABS_MT_POSITION_Y, p.y as i32));
                self.slots[i] = Some(Slot { id: p.id, x: p.x, y: p.y });
            }
        }
        let first = self.slots.iter().flatten().next().copied();
        let is_down = first.is_some();
        if let Some(f) = first {
            evs.push(absv(Abs::ABS_X, f.x as i32));
            evs.push(absv(Abs::ABS_Y, f.y as i32));
        }
        if is_down != was_down {
            evs.push(key(KeyCode::BTN_TOUCH, is_down));
        }
        if !evs.is_empty() {
            self.dev.emit(&evs)?;
        }
        Ok(())
    }

    pub fn reset(&mut self) -> Result<()> {
        self.handle(&[])
    }
}
