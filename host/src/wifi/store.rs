// SPDX-License-Identifier: GPL-3.0-or-later
//! Host identity and paired tablets: `$XDG_STATE_HOME/waylus/wifi.json` (mode 0600) on
//! Linux, `%LOCALAPPDATA%\waylus\wifi.json` on Windows (the directory's inherited ACL
//! already limits it to the user, SYSTEM and administrators; Waylus doesn't widen it).

use std::{
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Device {
    /// Random id chosen by the tablet at pairing, hex.
    pub id: String,
    pub name: String,
    /// 32-byte pairing key, hex. Secret.
    pub key: String,
    /// Unix time of pairing.
    pub paired: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Store {
    pub host_id: String,
    #[serde(default)]
    pub devices: Vec<Device>,
}

fn path() -> PathBuf {
    crate::platform::state_dir().join("wifi.json")
}

/// Opens the temp file for the new store; on Unix with mode 0600 from the start.
fn create_private(p: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(p)
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

impl Store {
    /// Loads the store, creating a fresh host id on first use.
    pub fn load() -> Result<Self> {
        let p = path();
        match std::fs::read(&p) {
            Ok(b) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let meta = std::fs::metadata(&p)?;
                    if meta.permissions().mode() & 0o077 != 0 {
                        tracing::warn!("{} is readable by other users; fixing permissions", p.display());
                        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
                    }
                }
                serde_json::from_slice(&b).with_context(|| format!("parse {}", p.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut id = [0u8; 16];
                OsRng.fill_bytes(&mut id);
                let s = Self { host_id: to_hex(&id), devices: Vec::new() };
                s.save()?;
                Ok(s)
            }
            Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("json.tmp");
        let mut f = create_private(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self)?)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &p).with_context(|| format!("write {}", p.display()))
    }

    pub fn host_id(&self) -> [u8; 16] {
        from_hex(&self.host_id).and_then(|v| v.try_into().ok()).expect("valid host id in wifi.json")
    }

    pub fn key_for(&self, device_id: &[u8]) -> Option<(String, [u8; 32])> {
        let id = to_hex(device_id);
        self.devices
            .iter()
            .find(|d| d.id == id)
            .and_then(|d| Some((d.name.clone(), from_hex(&d.key)?.try_into().ok()?)))
    }

    pub fn add(&mut self, device_id: &[u8], name: &str, key: &[u8; 32]) -> Result<()> {
        let id = to_hex(device_id);
        self.devices.retain(|d| d.id != id);
        let paired = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.devices.push(Device { id, name: name.to_owned(), key: to_hex(key), paired });
        self.save()
    }

    /// Removes devices whose name or id matches (or all); returns how many.
    pub fn remove(&mut self, which: &str) -> Result<usize> {
        let before = self.devices.len();
        self.devices.retain(|d| !(which == "all" || d.name == which || d.id.starts_with(which)));
        self.save()?;
        Ok(before - self.devices.len())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_roundtrip() {
        assert_eq!(super::from_hex(&super::to_hex(&[0, 1, 0xab, 0xff])).unwrap(), vec![0, 1, 0xab, 0xff]);
        assert!(super::from_hex("abc").is_none());
        assert!(super::from_hex("zz").is_none());
    }
}
