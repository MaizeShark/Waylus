// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional niri integration: find the output being mirrored, know its size in
//! input-only mode, and map the pen/touch to it while Waylus runs.
//!
//! The mapping lives in its own file, `config.d/90-waylus.kdl` next to the niri
//! config, written on start and removed on exit (niri reloads it either way). The
//! user's config includes it once with `include optional=true`, so a missing file
//! is fine; `waylus --niri-setup` adds that line. Waylus never edits the main
//! config on its own.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};

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

/// Relative to the niri config directory, as written in the include line.
pub const DROP_IN: &str = "config.d/90-waylus.kdl";
const HEADER: &str = "// Written by Waylus while it runs and removed when it exits; edits here are lost.\n";

/// The drop-in currently written, for removal on exit.
static ACTIVE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("niri"))
}

fn main_config() -> Option<PathBuf> {
    std::env::var_os("NIRI_CONFIG").map(PathBuf::from).or_else(|| Some(config_dir()?.join("config.kdl")))
}

pub fn include_line() -> String {
    format!("include optional=true \"{DROP_IN}\"")
}

/// Whether some config file in the niri config directory includes the drop-in.
pub fn is_included() -> bool {
    fn walk(dir: &std::path::Path, depth: u32) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else { return false };
        rd.flatten().any(|e| {
            let p = e.path();
            if p.is_dir() {
                depth < 3 && walk(&p, depth + 1)
            } else {
                p.extension().is_some_and(|x| x == "kdl")
                    && !p.ends_with(DROP_IN)
                    && std::fs::read_to_string(&p).is_ok_and(|t| {
                        t.lines().any(|l| !l.trim_start().starts_with("//") && l.contains("90-waylus.kdl"))
                    })
            }
        })
    }
    let main = main_config();
    main.as_ref().and_then(|m| std::fs::read_to_string(m).ok()).is_some_and(|t| {
        t.lines().any(|l| !l.trim_start().starts_with("//") && l.contains("90-waylus.kdl"))
    }) || config_dir().is_some_and(|d| walk(&d, 0))
}

/// Writes the pen/touch mapping for `output`. Refuses to overwrite a file Waylus didn't write.
pub fn write_mapping(output: &str) -> Result<PathBuf> {
    let path = config_dir().context("no config directory ($HOME unset)")?.join(DROP_IN);
    if let Ok(old) = std::fs::read_to_string(&path)
        && !old.starts_with(HEADER)
    {
        bail!("{} exists and wasn't written by Waylus; leaving it alone", path.display());
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("kdl.tmp");
    std::fs::write(&tmp, format!("{HEADER}{}\n", config_hint(output)))?;
    std::fs::rename(&tmp, &path)?;
    *ACTIVE.lock().unwrap() = Some(path.clone());
    Ok(path)
}

/// Removes the drop-in written by `write_mapping`, if any. Safe to call from the Ctrl-C handler.
pub fn remove_mapping() {
    let Ok(mut a) = ACTIVE.try_lock() else { return };
    if let Some(p) = a.take() {
        let _ = std::fs::remove_file(p);
    }
}

/// `--niri-setup`: appends the include line to the main config (through a symlink, if it is one).
pub fn setup() -> Result<()> {
    let main = main_config().context("no niri config path ($HOME unset)")?;
    if is_included() {
        println!("{} already includes {DROP_IN}; nothing to do", main.display());
        return Ok(());
    }
    let target = std::fs::canonicalize(&main).with_context(|| format!("{} not found", main.display()))?;
    let mut text = std::fs::read_to_string(&target)?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    // Last, so the mapping overrides the config's own tablet/touch settings while Waylus runs.
    text.push_str(&format!(
        "\n// Waylus: maps the pen/touch to the mirrored output while it runs (file is removed on exit).\n{}\n",
        include_line()
    ));
    std::fs::write(&target, text)?;
    println!("added `{}` to {}", include_line(), target.display());
    Ok(())
}
