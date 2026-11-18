// SPDX-License-Identifier: GPL-3.0-or-later
//! Monitor listing and choice on Windows (`EnumDisplayMonitors`). The process is made
//! per-monitor DPI aware first (see `mod.rs`), so every rectangle here is in physical
//! virtual-desktop pixels, which is what both the capture and the pointer injection use.

use std::path::PathBuf;

use anyhow::{Result, bail};
use windows::Win32::{
    Foundation::{LPARAM, RECT},
    Graphics::Gdi::{
        DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR,
        MONITORINFO, MONITORINFOEXW,
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
        });
    }
    true.into()
}

/// All attached monitors, primary first, then left to right.
pub fn list() -> Vec<Monitor> {
    let mut v: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut v as *mut Vec<Monitor> as isize));
    }
    v.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    v
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
            "  {}: {}  {w}x{h} @ {} Hz at ({}, {}){}",
            i + 1,
            m.name,
            m.refresh_hz,
            m.rect.left,
            m.rect.top,
            if m.primary { "  (primary)" } else { "" }
        );
    }
}

/// `waylus --list-outputs`.
pub fn print() {
    print_list(&list());
}
