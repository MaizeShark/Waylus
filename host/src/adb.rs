// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal adb driver: install/upgrade the client APK, set up the USB tunnel
//! (`adb reverse`) and launch the activity.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use tracing::{debug, info};

pub const PACKAGE: &str = "dev.waylus";
pub const ACTIVITY: &str = "dev.waylus/.MainActivity";

#[derive(Clone)]
pub struct Adb {
    serial: Option<String>,
}

impl Adb {
    pub fn new(serial: Option<String>) -> Self {
        Self { serial }
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new("adb");
        if let Some(s) = &self.serial {
            c.args(["-s", s]);
        }
        c
    }

    pub fn run(&self, args: &[&str]) -> Result<String> {
        debug!("adb {}", args.join(" "));
        let out = self.command().args(args).output().context("failed to run adb (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "adb {} failed: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn shell(&self, cmd: &str) -> Result<String> {
        self.run(&["shell", cmd])
    }

    pub fn wait_for_device(&self) -> Result<()> {
        let state = self.run(&["get-state"]).unwrap_or_default();
        if state.trim() != "device" {
            info!("waiting for tablet (adb)...");
            self.run(&["wait-for-device"])?;
        }
        let model = self.shell("getprop ro.product.model").unwrap_or_default();
        info!("tablet: {}", model.trim());
        Ok(())
    }

    /// Installs `apk` unless the installed copy is byte-identical.
    pub fn ensure_installed(&self, apk: &Path) -> Result<()> {
        let local = sha256_local(apk)?;
        let remote = self
            .shell(&format!("p=$(pm path {PACKAGE} | head -n1 | cut -d: -f2); [ -n \"$p\" ] && sha256sum \"$p\""))
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_owned));
        if remote.as_deref() == Some(local.as_str()) {
            debug!("client APK up to date");
            return Ok(());
        }
        info!("installing client APK {}", apk.display());
        let apk = apk.to_str().context("non-utf8 APK path")?;
        self.run(&["install", "-r", "-g", apk])?;
        Ok(())
    }

    pub fn is_installed(&self) -> bool {
        self.shell(&format!("pm path {PACKAGE}")).is_ok_and(|s| s.contains("package:"))
    }

    pub fn reverse(&self, port: u16) -> Result<()> {
        let spec = format!("tcp:{port}");
        self.run(&["reverse", &spec, &spec])?;
        Ok(())
    }

    pub fn remove_reverse(&self, port: u16) {
        let _ = self.run(&["reverse", "--remove", &format!("tcp:{port}")]);
    }

    pub fn start_app(&self, port: u16) -> Result<()> {
        self.shell(&format!("am start -S -n {ACTIVITY} --ei port {port}"))?;
        Ok(())
    }

    pub fn stop_app(&self) {
        let _ = self.shell(&format!("am force-stop {PACKAGE}"));
    }
}

fn sha256_local(p: &Path) -> Result<String> {
    let out = Command::new("sha256sum").arg(p).output().context("sha256sum")?;
    if !out.status.success() {
        bail!("cannot hash {}", p.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or_default().to_owned())
}

/// Looks for the client APK next to the executable or in the source tree.
pub fn find_apk() -> Option<PathBuf> {
    let rel = [
        "waylus.apk",
        "android/app/build/outputs/apk/release/app-release.apk",
        "android/app/build/outputs/apk/debug/app-debug.apk",
    ];
    let mut roots = vec![];
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).take(4).map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    roots.iter().flat_map(|r| rel.iter().map(move |p| r.join(p))).find(|p| p.is_file())
}
