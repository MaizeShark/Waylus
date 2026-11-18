// SPDX-License-Identifier: GPL-3.0-or-later
//! KDE Plasma integration: map the virtual pen and touchscreen to the output
//! being shown on the tablet, through KWin's per-device D-Bus settings
//! (`org.kde.KWin.InputDevice.outputName`). Equivalent to picking the screen in
//! System Settings → Drawing Tablet / Touchscreen, but automatic.

use std::{process::Command, thread, time::Duration};

use tracing::{debug, info, warn};

pub fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("KDE")))
}

#[derive(Debug, Clone)]
struct Output {
    name: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn outputs() -> Vec<Output> {
    let Ok(out) = Command::new("kscreen-doctor").arg("-j").output() else { return vec![] };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return vec![] };
    let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    v.get("outputs")
        .and_then(|o| o.as_array())
        .into_iter()
        .flatten()
        .filter(|o| o.get("connected").and_then(|c| c.as_bool()).unwrap_or(false))
        .filter_map(|o| {
            let pos = o.get("pos")?;
            let size = o.get("size")?;
            let scale = o.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0).max(0.1);
            Some(Output {
                name: o.get("name")?.as_str()?.to_owned(),
                x: num(pos, "x") as i32,
                y: num(pos, "y") as i32,
                // Logical size, as the portal reports it.
                w: (num(size, "width") / scale).round() as i32,
                h: (num(size, "height") / scale).round() as i32,
            })
        })
        .collect()
}

/// The output a mirrored portal stream shows, matched by its logical position.
pub fn output_at(pos: (i32, i32), size: Option<(i32, i32)>) -> Option<String> {
    let outs = outputs();
    outs.iter()
        .find(|o| (o.x, o.y) == pos && size.is_none_or(|s| (o.w, o.h) == s))
        .or_else(|| outs.iter().find(|o| (o.x, o.y) == pos))
        .map(|o| o.name.clone())
}

/// The virtual output KWin created for us (`Virtual-…`), matched by size.
pub fn virtual_output(size: (u32, u32)) -> Option<String> {
    outputs()
        .into_iter()
        .find(|o| o.name.starts_with("Virtual-") && (o.w as u32, o.h as u32) == size)
        .map(|o| o.name)
}

fn waylus_event_nodes() -> Vec<String> {
    let Ok(s) = std::fs::read_to_string("/proc/bus/input/devices") else { return vec![] };
    s.split("\n\n")
        .filter(|b| b.contains("N: Name=\"Waylus Pen\"") || b.contains("N: Name=\"Waylus Touch\""))
        .filter_map(|b| b.split_whitespace().find(|w| w.starts_with("event")).map(str::to_owned))
        .collect()
}

fn set_output(event: &str, output: &str) -> bool {
    Command::new("busctl")
        .args([
            "--user",
            "set-property",
            "org.kde.KWin",
            &format!("/org/kde/KWin/InputDevice/{event}"),
            "org.kde.KWin.InputDevice",
            "outputName",
            "s",
            output,
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Maps the Waylus pen and touchscreen to `output`. Runs in the background and
/// retries for a few seconds, since KWin registers freshly created devices asynchronously.
pub fn map_devices(output: String) {
    thread::spawn(move || {
        for _ in 0..30 {
            let nodes = waylus_event_nodes();
            if !nodes.is_empty() && nodes.iter().all(|n| set_output(n, &output)) {
                info!("KDE: mapped pen and touch to {output}");
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
        warn!("KDE: could not map pen/touch to {output}; set it in System Settings → Drawing Tablet");
    });
    debug!("KDE: mapping input devices");
}
