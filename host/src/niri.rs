// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional niri integration: find the output being mirrored so we can print
//! the right `map-to-output` config and know its size in input-only mode.

use std::process::Command;

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Physical mode size.
    pub mode_w: u32,
    pub mode_h: u32,
}

pub fn is_niri() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
}

pub fn outputs() -> Vec<Output> {
    let Ok(out) = Command::new("niri").args(["msg", "-j", "outputs"]).output() else {
        return vec![];
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return vec![];
    };
    let Some(map) = v.as_object() else { return vec![] };
    map.iter()
        .filter_map(|(name, o)| {
            let l = o.get("logical")?;
            let mode = o
                .get("current_mode")
                .and_then(|i| i.as_u64())
                .and_then(|i| o.get("modes")?.get(i as usize).cloned());
            let get = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64());
            let width = get(l, "width")? as i32;
            let height = get(l, "height")? as i32;
            Some(Output {
                name: name.clone(),
                x: get(l, "x")? as i32,
                y: get(l, "y")? as i32,
                width,
                height,
                mode_w: mode.as_ref().and_then(|m| get(m, "width")).unwrap_or(width as i64) as u32,
                mode_h: mode.as_ref().and_then(|m| get(m, "height")).unwrap_or(height as i64) as u32,
            })
        })
        .collect()
}

pub fn by_name(name: &str) -> Option<Output> {
    outputs().into_iter().find(|o| o.name == name)
}

pub fn by_geometry(pos: (i32, i32), size: (i32, i32)) -> Option<Output> {
    outputs().into_iter().find(|o| (o.x, o.y) == pos && (o.width, o.height) == size)
}

pub fn config_hint(output: &str) -> String {
    format!(
        "input {{\n    tablet {{\n        map-to-output \"{output}\"\n    }}\n    touch {{\n        map-to-output \"{output}\"\n    }}\n}}"
    )
}
