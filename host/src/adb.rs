// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal adb driver: install/upgrade the client APK, set up the USB tunnel
//! (`adb reverse`) and launch the activity.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use tracing::{debug, info};

pub const PACKAGE: &str = "de.hut9.waylus";
pub const ACTIVITY: &str = "de.hut9.waylus/dev.waylus.MainActivity";
/// The app's id before version 0.2.0. Removed when the new app is installed: both declare
/// the Waylus USB accessory, and with two such apps Android asks which one to open instead
/// of starting the app when the host switches the tablet to accessory mode.
const OLD_PACKAGE: &str = "dev.waylus";

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
        } else {
            info!("installing client APK {}", apk.display());
            let apk = apk.to_str().context("non-utf8 APK path")?;
            self.run(&["install", "-r", "-g", apk])?;
        }
        if self.shell(&format!("pm path {OLD_PACKAGE}")).is_ok_and(|s| s.contains("package:")) {
            info!("removing the old app ({OLD_PACKAGE}); its Wi-Fi pairing does not carry over, pair again with --wifi --pair");
            let _ = self.run(&["uninstall", OLD_PACKAGE]);
        }
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
    /// `transport`: "aoa" (USB accessory), "tcp" (the `adb reverse` port) or "wifi"
    /// (connect to `host` on the LAN).
    pub fn start_app(&self, port: u16, transport: &str, host: Option<&str>, boost_secs: u32, boost_threads: u32) -> Result<()> {
        let host = host.map(|h| format!(" --es host {h}")).unwrap_or_default();
        self.shell(&format!(
            "am start -S -n {ACTIVITY} --es transport {transport}{host} --ei port {port} --ei boost_secs {boost_secs} --ei boost_threads {boost_threads}"
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

/// Hex SHA-256 of a local file, as `sha256sum` prints it (Windows has no `sha256sum`).
fn sha256_local(p: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(p).with_context(|| format!("cannot read {}", p.display()))?;
    Ok(Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect())
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

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_local_is_lowercase_hex() {
        let p = std::env::temp_dir().join(format!("waylus-sha256-test-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let hash = super::sha256_local(&p);
        std::fs::remove_file(&p).ok();
        assert_eq!(hash.unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
