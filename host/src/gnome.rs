// SPDX-License-Identifier: GPL-3.0-or-later
//! GNOME integration: map the virtual pen and touchscreen to the monitor shown on
//! the tablet. Mutter reads per-device GSettings
//! (`org.gnome.desktop.peripherals.{tablet,touchscreen}`, keyed by vendor:product)
//! whose `output` is the monitor's EDID `[vendor, product, serial]` — the same
//! thing Settings → Wacom Tablet writes. Monitor info comes from `gdctl show`.

use std::{process::Command, sync::Mutex, thread, time::Duration};

use tracing::{info, warn};

/// Connectors that existed before we asked for a virtual monitor.
static KNOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// uinput ids of our devices (see input.rs).
const PEN_ID: &str = "1209:5750";
const TOUCH_ID: &str = "1209:5751";

pub fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("GNOME")))
}

#[derive(Debug, Default, Clone)]
struct Monitor {
    connector: String,
    vendor: String,
    product: String,
    serial: String,
    mode: Option<(u32, u32)>,
    position: Option<(i32, i32)>,
}

/// Parses `gdctl show` (tree-formatted text).
fn monitors() -> Vec<Monitor> {
    let Ok(out) = Command::new("gdctl").arg("show").output() else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    let clean = |l: &str| l.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '(' && c != '-').trim().to_owned();
    let mut mons: Vec<Monitor> = Vec::new();
    let mut in_logical = false;
    let mut logical_pos: Option<(i32, i32)> = None;
    let mut expect_mode = false;
    for raw in text.lines() {
        let l = clean(raw);
        if l.starts_with("Logical monitors:") {
            in_logical = true;
            continue;
        }
        if !in_logical {
            if let Some(rest) = l.strip_prefix("Monitor ") {
                let connector = rest.split_whitespace().next().unwrap_or_default().to_owned();
                mons.push(Monitor { connector, ..Default::default() });
            } else if let Some(m) = mons.last_mut() {
                if let Some(v) = l.strip_prefix("Vendor:") {
                    m.vendor = v.trim().to_owned();
                } else if let Some(v) = l.strip_prefix("Product:") {
                    m.product = v.trim().to_owned();
                } else if let Some(v) = l.strip_prefix("Serial:") {
                    m.serial = v.trim().to_owned();
                } else if l.starts_with("Current mode") {
                    expect_mode = true;
                } else if expect_mode {
                    expect_mode = false;
                    let res = l.split('@').next().unwrap_or_default();
                    if let Some((w, h)) = res.split_once('x') {
                        m.mode = w.trim().parse().ok().zip(h.trim().parse().ok());
                    }
                }
            }
        } else if let Some(p) = l.strip_prefix("Position:") {
            let nums: Vec<i32> = p.split(|c: char| !c.is_ascii_digit() && c != '-').filter_map(|n| n.parse().ok()).collect();
            logical_pos = (nums.len() >= 2).then(|| (nums[0], nums[1]));
        } else if let Some(pos) = logical_pos {
            // Lines under "Monitors:" name the connectors of this logical monitor.
            if let Some(m) = mons.iter_mut().find(|m| !m.connector.is_empty() && l.starts_with(&m.connector)) {
                m.position = Some(pos);
            }
        }
    }
    mons
}

/// Connector of the monitor at logical position `pos` (mirroring).
pub fn output_at(pos: (i32, i32)) -> Option<String> {
    monitors().into_iter().find(|m| m.position == Some(pos)).map(|m| m.connector)
}

/// Connector of the virtual monitor Mutter created for us: the right size and not
/// among the monitors remembered by [`remember_connectors`].
pub fn virtual_output(size: (u32, u32)) -> Option<String> {
    let known = KNOWN.lock().unwrap().clone();
    monitors()
        .into_iter()
        .find(|m| m.mode == Some(size) && !known.contains(&m.connector))
        .map(|m| m.connector)
}

/// Call before creating a virtual monitor, so it can be told apart afterwards.
pub fn remember_connectors() {
    *KNOWN.lock().unwrap() = monitors().into_iter().map(|m| m.connector).collect();
}

fn set(schema: &str, kind: &str, id: &str, value: &str) -> bool {
    Command::new("gsettings")
        .args(["set", &format!("{schema}:/org/gnome/desktop/peripherals/{kind}/{id}/"), "output", value])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Resolves the target with `find` (retrying briefly: a new virtual monitor shows
/// up in Mutter's state with a delay) and maps the pen and touchscreen to it.
pub fn map_devices_with(find: impl Fn() -> Option<String> + Send + 'static) {
    thread::spawn(move || {
        for _ in 0..15 {
            if let Some(c) = find() {
                map_devices(&c);
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
        warn!("GNOME: couldn't tell which monitor is shown; map the Waylus pen/touch in Settings → Wacom Tablet");
    });
}

/// Maps the Waylus pen and touchscreen to the monitor on `connector`.
pub fn map_devices(connector: &str) {
    let Some(m) = monitors().into_iter().find(|m| m.connector == connector) else {
        warn!("GNOME: monitor {connector} not found; map the Waylus pen/touch in Settings → Wacom Tablet");
        return;
    };
    let q = |s: &str| format!("'{}'", s.replace('\'', "\\'"));
    let value = format!("[{}, {}, {}]", q(&m.vendor), q(&m.product), q(&m.serial));
    let ok = set("org.gnome.desktop.peripherals.tablet", "tablets", PEN_ID, &value)
        & set("org.gnome.desktop.peripherals.touchscreen", "touchscreens", TOUCH_ID, &value);
    if ok {
        info!("GNOME: mapped pen and touch to {connector} ({} {})", m.vendor, m.product);
    } else {
        warn!("GNOME: gsettings failed; map the Waylus pen/touch in Settings → Wacom Tablet");
    }
}
