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

    /// A device is attached and authorized.
    pub fn is_available(&self) -> bool {
        self.run(&["get-state"]).is_ok_and(|s| s.trim() == "device")
    }

    pub fn serial_no(&self) -> Option<String> {
        self.run(&["get-serialno"]).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
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

    /// `boost_secs`: keep the tablet's CPU clocks up this long after pen/touch input (0 = off).
    /// `aoa`: connect over the USB accessory instead of the `adb reverse` port.
    pub fn start_app(&self, port: u16, aoa: bool, boost_secs: u32, boost_threads: u32) -> Result<()> {
        let transport = if aoa { "aoa" } else { "tcp" };
        self.shell(&format!(
            "am start -S -n {ACTIVITY} --es transport {transport} --ei port {port} --ei boost_secs {boost_secs} --ei boost_threads {boost_threads}"
        ))?;
        Ok(())
    }

    pub fn stop_app(&self) {
        let _ = self.shell(&format!("am force-stop {PACKAGE}"));
    }

    /// Kills raw-pen readers (`cat <device>`, see rawpen.rs) on the tablet. Killing the local
    /// `adb exec-out` does not end the remote `cat`, which stays blocked in read() until the
    /// pen next produces an event, so every session would otherwise leave one behind.
    pub fn kill_remote_cat(&self, device: &str) {
        let _ = self.shell(&format!("pkill -x -f 'cat {device}'"));
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
