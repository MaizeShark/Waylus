// SPDX-License-Identifier: GPL-3.0-or-later
//! USB transport without adb: Android Open Accessory (AOA).
//!
//! The host switches the tablet into accessory mode with a few vendor control
//! requests; it then re-enumerates as 18d1:2d00 (2d01 with adb) and exposes a bulk
//! IN/OUT pair that the app opens as a file (UsbManager.openAccessory). The data path
//! skips adbd and the TCP stack on both ends.
//!
//! Unlike a TCP connection, the bulk pipe has no connection boundaries: it stays up
//! while the app or the host restarts, and the tablet can still hold bytes from a
//! previous session. So every session starts with a SYNC message carrying a random
//! nonce from the app; the host skips everything before it and echoes it back, and the
//! app does the same in the other direction.

use std::{
    io::{self, BufRead, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use nusb::{
    DeviceInfo, Interface, MaybeFuture,
    io::{EndpointRead, EndpointWrite},
    transfer::{Bulk, ControlIn, ControlOut, ControlType, Direction, In, Out, Recipient},
    descriptors::TransferType,
};
use tracing::{debug, info, warn};

use crate::{link::Link, proto};

const GOOGLE: u16 = 0x18d1;
/// Accessory, and accessory + adb.
const ACCESSORY_PIDS: [u16; 2] = [0x2d00, 0x2d01];

const GET_PROTOCOL: u8 = 51;
const SEND_STRING: u8 = 52;
const START: u8 = 53;

/// Identification strings; the app's res/xml/accessory_filter.xml matches the first two.
const STRINGS: [&str; 6] = [
    "Waylus",                                  // manufacturer
    "Waylus",                                  // model
    "Waylus pen display",                      // description
    env!("CARGO_PKG_VERSION"),                 // version
    "https://github.com/MaizeShark/Waylus",    // URI (shown if the app isn't installed)
    "waylus",                                  // serial
];

pub struct Aoa {
    /// USB serial number of the tablet (same as its adb serial), if known.
    serial: Option<String>,
    iface: Option<(Interface, u8, u8)>,
}

impl Aoa {
    pub fn new(serial: Option<String>) -> Self {
        Self { serial, iface: None }
    }

    /// Puts the tablet into accessory mode (if it isn't already) and claims the
    /// accessory interface. Waits for a tablet to be plugged in, or with `give_up`,
    /// fails after that long or as soon as the tablet turns out not to support AOA.
    pub fn connect(&mut self, give_up: Option<Duration>) -> Result<()> {
        let start = Instant::now();
        let mut said_waiting = false;
        let mut switched_at: Option<Instant> = None;
        loop {
            let devices: Vec<DeviceInfo> = nusb::list_devices().wait().context("list USB devices")?.collect();
            if let Some(d) = self.pick_accessory(&devices) {
                match claim(&d) {
                    Ok(i) => {
                        info!("USB accessory link up ({:04x}:{:04x}, bus {} addr {})", d.vendor_id(), d.product_id(), d.busnum(), d.device_address());
                        self.iface = Some(i);
                        return Ok(());
                    }
                    Err(e) => debug!("accessory not ready yet: {e:#}"),
                }
            } else if switched_at.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
                if let Some(d) = self.pick_android(&devices) {
                    match switch(&d) {
                        Ok(v) => {
                            info!("switching {} into accessory mode (AOA v{v})", describe(&d));
                            switched_at = Some(Instant::now());
                        }
                        Err(e) if give_up.is_some() => bail!("{}: no AOA ({e:#})", describe(&d)),
                        Err(e) => debug!("{}: no AOA ({e:#})", describe(&d)),
                    }
                } else if !said_waiting {
                    info!("waiting for the tablet on USB...");
                    said_waiting = true;
                }
            }
            if give_up.is_some_and(|t| start.elapsed() > t) {
                bail!("no tablet in accessory mode after {:?}", start.elapsed());
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn is_connected(&self) -> bool {
        self.iface.is_some()
    }

    fn serial_matches(&self, d: &DeviceInfo) -> bool {
        self.serial.as_deref().is_none_or(|s| d.serial_number() == Some(s))
    }

    fn pick_accessory(&self, devices: &[DeviceInfo]) -> Option<DeviceInfo> {
        let acc = |d: &&DeviceInfo| d.vendor_id() == GOOGLE && ACCESSORY_PIDS.contains(&d.product_id());
        devices
            .iter()
            .filter(acc)
            .find(|d| self.serial_matches(d))
            // Fall back to the only accessory if its serial changed with the mode switch.
            .or_else(|| {
                let mut it = devices.iter().filter(acc);
                it.next().filter(|_| it.next().is_none())
            })
            .cloned()
    }

    /// An Android device to switch: the given serial, else the first one exposing adb or MTP.
    fn pick_android(&self, devices: &[DeviceInfo]) -> Option<DeviceInfo> {
        devices
            .iter()
            .filter(|d| self.serial_matches(d))
            .find(|d| {
                self.serial.is_some()
                    || d.interfaces().any(|i| {
                        (i.class(), i.subclass(), i.protocol()) == (0xff, 0x42, 0x01) // adb
                            || (i.class(), i.subclass(), i.protocol()) == (0x06, 0x01, 0x01) // MTP / PTP
                    })
            })
            .cloned()
    }

    /// Waits for the app to start a session and returns the link. On a USB error
    /// (unplugged), reconnects first.
    pub fn next_link(&mut self) -> Result<Link> {
        loop {
            if self.iface.is_none() {
                self.connect(None)?;
            }
            let (iface, ep_in, ep_out) = self.iface.clone().unwrap();
            match open_link(&iface, ep_in, ep_out) {
                Ok(l) => return Ok(l),
                Err(e) => {
                    warn!("USB link lost: {e:#}");
                    self.iface = None;
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }
}

fn describe(d: &DeviceInfo) -> String {
    format!(
        "{} {} ({:04x}:{:04x})",
        d.manufacturer_string().unwrap_or("?"),
        d.product_string().unwrap_or("?"),
        d.vendor_id(),
        d.product_id()
    )
}

/// Sends the AOA handshake. Returns the device's AOA protocol version.
fn switch(d: &DeviceInfo) -> Result<u16> {
    let dev = d.open().wait().context("open USB device")?;
    let t = Duration::from_secs(1);
    let v = dev
        .control_in(
            ControlIn { control_type: ControlType::Vendor, recipient: Recipient::Device, request: GET_PROTOCOL, value: 0, index: 0, length: 2 },
            t,
        )
        .wait()
        .context("AOA get protocol")?;
    let version = u16::from_le_bytes([*v.first().unwrap_or(&0), *v.get(1).unwrap_or(&0)]);
    if version == 0 {
        bail!("device does not support accessory mode");
    }
    for (i, s) in STRINGS.iter().enumerate() {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        dev.control_out(
            ControlOut { control_type: ControlType::Vendor, recipient: Recipient::Device, request: SEND_STRING, value: 0, index: i as u16, data: &data },
            t,
        )
        .wait()
        .context("AOA send string")?;
    }
    dev.control_out(
        ControlOut { control_type: ControlType::Vendor, recipient: Recipient::Device, request: START, value: 0, index: 0, data: &[] },
        t,
    )
    .wait()
    .context("AOA start")?;
    Ok(version)
}

/// Claims the accessory interface (the vendor one with a bulk IN/OUT pair, not adb).
fn claim(d: &DeviceInfo) -> Result<(Interface, u8, u8)> {
    let dev = d.open().wait().context("open accessory")?;
    let cfg = dev.active_configuration().context("accessory configuration")?;
    for group in cfg.interfaces() {
        let alt = group.first_alt_setting();
        if alt.class() != 0xff || alt.subclass() == 0x42 {
            continue;
        }
        let bulk = |dir| alt.endpoints().find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == dir).map(|e| e.address());
        if let (Some(i), Some(o)) = (bulk(Direction::In), bulk(Direction::Out)) {
            let iface = dev.claim_interface(group.interface_number()).wait().context("claim accessory interface")?;
            return Ok((iface, i, o));
        }
    }
    bail!("no accessory interface")
}

fn open_link(iface: &Interface, ep_in: u8, ep_out: u8) -> Result<Link> {
    // One max-size packet per transfer: every packet completes a read, so a write from
    // the tablet that happens to end on a packet boundary (no short packet) can't sit
    // in a half-full transfer.
    let mut reader = iface
        .endpoint::<Bulk, In>(ep_in)?
        .reader(512)
        .with_num_transfers(32)
        .with_read_timeout(Duration::from_millis(100));
    let writer =
        iface.endpoint::<Bulk, Out>(ep_out)?.writer(64 * 1024).with_num_transfers(4).with_write_timeout(Duration::from_secs(2));

    debug!("waiting for the app's sync");
    let nonce = wait_sync(&mut reader)?;
    debug!("sync {nonce:016x}");
    let mut writer = UsbWriter(writer);
    proto::write_sync(&mut writer, nonce)?;

    let stop = Arc::new(AtomicBool::new(false));
    Ok(Link {
        reader: Box::new(UsbRead { inner: reader, stop: stop.clone(), last: Instant::now() }),
        writer: Box::new(writer),
        close: Arc::new(move || stop.store(true, Ordering::SeqCst)),
        sync: Some(nonce),
    })
}

/// Skips the stream up to the next SYNC message and returns its nonce.
fn wait_sync(r: &mut impl BufRead) -> Result<u64> {
    let pat = proto::SYNC_PREFIX;
    let mut matched = 0;
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e).context("read"),
        };
        let mut used = 0;
        let mut found = false;
        for &b in buf {
            used += 1;
            matched = if b == pat[matched] { matched + 1 } else if b == pat[0] { 1 } else { 0 };
            if matched == pat.len() {
                found = true;
                break;
            }
        }
        r.consume(used);
        if found {
            let mut nonce = [0u8; 8];
            read_exact_retry(r, &mut nonce)?;
            return Ok(u64::from_le_bytes(nonce));
        }
    }
}

fn read_exact_retry(r: &mut impl Read, mut buf: &mut [u8]) -> Result<()> {
    while !buf.is_empty() {
        match r.read(buf) {
            Ok(0) => bail!("USB stream ended"),
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e).context("read"),
        }
    }
    Ok(())
}

/// Blocking reader that rides out the short poll timeouts, fails after
/// [crate::link::IDLE_TIMEOUT] of silence, and stops when the link is closed.
struct UsbRead {
    inner: EndpointRead<Bulk>,
    stop: Arc<AtomicBool>,
    last: Instant,
}

impl Read for UsbRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::NotConnected.into());
            }
            match self.inner.read(buf) {
                Ok(n) => {
                    self.last = Instant::now();
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    if self.last.elapsed() > crate::link::IDLE_TIMEOUT {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// `flush` ends the transfer with a short (or zero-length) packet: the tablet's
/// accessory driver completes a read only on a short packet or a full buffer.
struct UsbWriter(EndpointWrite<Bulk>);

impl Write for UsbWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.submit_end();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sync_after_stale_bytes() {
        let mut msg = Vec::new();
        proto::write_sync(&mut msg, 0x1122_3344_5566_7788).unwrap();
        // Stale data, including a partial sync prefix, before the real one.
        let mut stream = vec![3, 0, 0, 1, 0, proto::ty::SYNC, 16, 0, 0, 0, b'W', b'A', 9, 9];
        stream.extend_from_slice(&msg);
        stream.extend_from_slice(&[1, 2, 3]);
        let mut r = io::Cursor::new(stream);
        assert_eq!(wait_sync(&mut r).unwrap(), 0x1122_3344_5566_7788);
        // The stream continues right after the sync message.
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, [1, 2, 3]);
    }
}
