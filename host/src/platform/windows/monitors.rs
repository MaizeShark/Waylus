// SPDX-License-Identifier: GPL-3.0-or-later
//! Monitor listing and choice on Windows (`EnumDisplayMonitors`). The process is made
//! per-monitor DPI aware first (see `mod.rs`), so every rectangle here is in physical
//! virtual-desktop pixels, which is what both the capture and the pointer injection use.

use std::path::PathBuf;

use anyhow::{Result, bail};
use windows::Win32::{
    Foundation::{LPARAM, RECT},
    Graphics::Gdi::{
        CDS_TYPE, ChangeDisplaySettingsExW, DEVMODE_FIELD_FLAGS, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICEW,
        ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayDevicesW, EnumDisplayMonitors, EnumDisplaySettingsW,
        GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    },
};
use windows::core::{BOOL, PCWSTR};

use crate::platform::Output;

const MONITORINFOF_PRIMARY: u32 = 1;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub handle: isize,
    /// GDI device name, e.g. `\\.\DISPLAY2`.
    pub name: String,
    pub rect: RECT,
    pub primary: bool,
    pub refresh_hz: u32,
    /// Name of the display adapter ("NVIDIA GeForce ...", "Virtual Display Driver").
    pub adapter: String,
}

impl Monitor {
    pub fn output(&self) -> Output {
        let (w, h) = (self.rect.right - self.rect.left, self.rect.bottom - self.rect.top);
        Output { name: self.name.clone(), x: self.rect.left, y: self.rect.top, width: w, height: h, mode_w: w as u32, mode_h: h as u32 }
    }
}

unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let list = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(h, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
        let n = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        let name = String::from_utf16_lossy(&info.szDevice[..n]);
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        let hz = if unsafe { EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
            dm.dmDisplayFrequency
        } else {
            0
        };
        list.push(Monitor {
            handle: h.0 as isize,
            name,
            rect: info.monitorInfo.rcMonitor,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            refresh_hz: hz,
            adapter: String::new(),
        });
    }
    true.into()
}

/// All attached monitors, in the order of their device numbers (`\\.\DISPLAYn`). The
/// numbers `--output` takes index this list, so they must not move when the main display
/// or the desktop layout changes.
pub fn list() -> Vec<Monitor> {
    let mut v: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut v as *mut Vec<Monitor> as isize));
    }
    let names = adapter_names();
    for m in &mut v {
        m.adapter = names.get(&m.name).cloned().unwrap_or_default();
    }
    v.sort_by_key(|m| (device_number(&m.name), m.name.clone()));
    v
}

/// `\\.\DISPLAY12` → 12 (names without a number sort last).
fn device_number(name: &str) -> u32 {
    let digits = name.trim_start_matches(|c: char| !c.is_ascii_digit());
    digits.parse().unwrap_or(u32::MAX)
}

fn wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
}

/// `\.\DISPLAYn` → the name of the adapter it belongs to.
fn adapter_names() -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for i in 0.. {
        let mut dd = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) }.as_bool() {
            break;
        }
        m.insert(wide(&dd.DeviceName), wide(&dd.DeviceString));
    }
    m
}

/// Display adapters of Indirect Display drivers (virtual monitors).
const VIRTUAL_ADAPTERS: [&str; 5] = ["virtual display", "indirect display", "iddsample", "usbmmidd", "spacedesk"];

impl Monitor {
    pub fn is_virtual(&self) -> bool {
        let a = self.adapter.to_ascii_lowercase();
        VIRTUAL_ADAPTERS.iter().any(|v| a.contains(v)) && !a.contains("parsec")
    }
}

/// Modes (width, height, Hz) the monitor's driver offers.
pub fn modes(device: &str) -> Vec<(u32, u32, u32)> {
    let name: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
    let mut v = Vec::new();
    for i in 0.. {
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }.as_bool() {
            break;
        }
        let m = (dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency);
        if dm.dmBitsPerPel == 32 && !v.contains(&m) {
            v.push(m);
        }
    }
    v
}

/// Switches the monitor to `mode` (not saved: it is gone after a restart).
pub fn set_mode(device: &str, mode: (u32, u32, u32)) -> Result<()> {
    let name: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    if !unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
        bail!("cannot read the current mode of {device}");
    }
    dm.dmPelsWidth = mode.0;
    dm.dmPelsHeight = mode.1;
    dm.dmDisplayFrequency = mode.2;
    dm.dmFields = DEVMODE_FIELD_FLAGS(0x80000 | 0x100000 | 0x400000); // width, height, frequency
    let r = unsafe { ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm), None, CDS_TYPE(0), None) };
    if r != DISP_CHANGE_SUCCESSFUL {
        bail!("Windows refused {}x{} @ {} Hz on {device} (code {})", mode.0, mode.1, mode.2, r.0);
    }
    Ok(())
}

/// The mode of a virtual monitor that best fits a `want`-sized stream at `fps`: the same
/// aspect ratio, the largest area not above the request, the closest refresh rate.
pub fn best_mode(modes: &[(u32, u32, u32)], want: (u32, u32), fps: u32) -> Option<(u32, u32, u32)> {
    let aspect = want.0 as f64 / want.1 as f64;
    let score = |m: &&(u32, u32, u32)| {
        let a = m.0 as f64 / m.1 as f64;
        let aspect_off = ((a / aspect) - 1.0).abs();
        let over = (m.0 * m.1) as f64 > (want.0 * want.1) as f64 * 1.001;
        let area = -((m.0 * m.1) as f64);
        (aspect_off > 0.02, over, area as i64, (m.2 as i64 - fps as i64).abs())
    };
    modes.iter().min_by_key(score).copied()
}

/// `--output`: a 1-based index into [`list`] (as printed by `--list-outputs`), a GDI name
/// (`\\.\DISPLAY2` or just `DISPLAY2`), or a prefix of one.
pub fn find(spec: &str) -> Option<Monitor> {
    let all = list();
    if let Ok(i) = spec.parse::<usize>() {
        return all.get(i.checked_sub(1)?).cloned();
    }
    let norm = |s: &str| s.trim_start_matches('\\').trim_start_matches('.').to_ascii_lowercase();
    let want = norm(spec);
    all.into_iter().find(|m| norm(&m.name) == want)
}

pub fn choice_file() -> PathBuf {
    super::state_dir().join("output.txt")
}

/// The monitor to mirror: `--output`, else the remembered one (unless `choose_again`),
/// else an interactive pick among the non-primary monitors (or the only monitor).
pub fn choose(spec: Option<&str>, choose_again: bool) -> Result<Monitor> {
    if let Some(s) = spec {
        return find(s).ok_or_else(|| anyhow::anyhow!("monitor {s:?} not found; see `waylus --list-outputs`"));
    }
    let all = list();
    if all.is_empty() {
        bail!("no monitors found");
    }
    if !choose_again
        && let Ok(name) = std::fs::read_to_string(choice_file())
        && let Some(m) = all.iter().find(|m| m.name == name.trim())
    {
        return Ok(m.clone());
    }
    let pick = if all.len() == 1 {
        all[0].clone()
    } else {
        println!("Which monitor should the tablet show?");
        print_list(&all);
        loop {
            print!("number: ");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                bail!("no monitor chosen (pass --output)");
            }
            if let Some(m) = line.trim().parse::<usize>().ok().and_then(|i| all.get(i.checked_sub(1)?)) {
                break m.clone();
            }
        }
    };
    if let Some(dir) = choice_file().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(choice_file(), &pick.name);
    Ok(pick)
}

fn print_list(all: &[Monitor]) {
    for (i, m) in all.iter().enumerate() {
        let (w, h) = (m.rect.right - m.rect.left, m.rect.bottom - m.rect.top);
        println!(
            "  {}: {}  {w}x{h} @ {} Hz at ({}, {}){}  [{}]",
            i + 1,
            m.name,
            m.refresh_hz,
            m.rect.left,
            m.rect.top,
            if m.primary { "  (primary)" } else { "" },
            m.adapter
        );
    }
}

/// `waylus --list-outputs`.
pub fn print() {
    print_list(&list());
}
