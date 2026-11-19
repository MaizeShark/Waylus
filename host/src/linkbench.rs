// SPDX-License-Identifier: GPL-3.0-or-later
//! Dev flag `WAYLUS_BENCH_LINK=1`: measures what the link to the tablet carries
//! (docs/damage-path.md, phase 1) instead of streaming, then ends the session.
//!
//! Needs nothing new in the app: the host sends messages of a type the app doesn't
//! know (it reads and drops them), each followed by a PING. The app answers pings
//! in order, so the PONG's receive time (tablet clock, mapped to ours through the
//! offset of the fastest plain pings) says when the whole message had arrived.

use std::io::{Read, Write};

use anyhow::{Result, bail};
use tracing::info;

use crate::proto::{self, DeviceMsg, now_ns};

/// A message type no app knows; it is read and ignored.
const FILLER: u8 = 250;

pub fn enabled() -> bool {
    std::env::var_os("WAYLUS_BENCH_LINK").is_some()
}

fn pong(mut reader: &mut dyn Read, buf: &mut Vec<u8>, host_ns: u64) -> Result<(u64, u64)> {
    loop {
        match proto::read_msg(&mut reader, buf)? {
            DeviceMsg::Pong { host_ns: h, dev_ns } if h == host_ns => return Ok((now_ns(), dev_ns)),
            _ => {}
        }
    }
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * q).round() as usize]
}

pub fn run(mut reader: &mut dyn Read, mut writer: &mut dyn Write, kind: &str) -> Result<()> {
    let mut buf = Vec::new();
    // Clock offset (tablet - host) from the fastest of a few plain pings.
    let (mut best_rtt, mut offset) = (u64::MAX, 0i64);
    for _ in 0..40 {
        let t0 = now_ns();
        proto::write_ping(&mut writer, t0)?;
        let (t1, dev) = pong(&mut reader, &mut buf, t0)?;
        if t1 - t0 < best_rtt {
            best_rtt = t1 - t0;
            offset = dev as i64 - (t0 + best_rtt / 2) as i64;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    info!("link bench over {kind}: best ping rtt {:.3} ms", best_rtt as f64 / 1e6);
    let arrived = |dev: u64, t0: u64| (dev as i64 - offset - t0 as i64) as f64 / 1e6;

    // Latency per message size: one message + ping, idle link in between.
    for size in [0usize, 4 << 10, 16 << 10, 64 << 10, 128 << 10, 256 << 10, 512 << 10, 1 << 20, 4 << 20] {
        let payload = vec![0x5au8; size];
        let (mut one_way, mut rtt) = (Vec::new(), Vec::new());
        for i in 0..40 {
            let t0 = now_ns();
            if size > 0 {
                proto::message(&mut writer, FILLER, &[&payload])?;
            }
            proto::write_ping(&mut writer, t0)?;
            let (t1, dev) = pong(&mut reader, &mut buf, t0)?;
            if i >= 5 {
                one_way.push(arrived(dev, t0));
                rtt.push((t1 - t0) as f64 / 1e6);
            }
            std::thread::sleep(std::time::Duration::from_millis(20 + (i % 7) as u64));
        }
        let (p50, p95) = (pct(&mut one_way, 0.5), pct(&mut one_way, 0.95));
        info!(
            "link bench {kind}: {:>5} KB message → arrived p50 {p50:.2} / p95 {p95:.2} ms ({:.1} MB/s at p50), rtt p50 {:.2} ms",
            size / 1024,
            if size > 0 { size as f64 / 1e3 / p50 } else { 0.0 },
            pct(&mut rtt, 0.5)
        );
    }

    // Throughput: 32 MB back to back in messages of one size, then a ping.
    for size in [16usize << 10, 64 << 10, 256 << 10, 1 << 20] {
        let payload = vec![0x5au8; size];
        let mut rates = Vec::new();
        for _ in 0..3 {
            let n = (32 << 20) / size;
            let t0 = now_ns();
            for _ in 0..n {
                proto::message(&mut writer, FILLER, &[&payload])?;
            }
            let tw = now_ns();
            proto::write_ping(&mut writer, t0)?;
            let (_, dev) = pong(&mut reader, &mut buf, t0)?;
            let ms = arrived(dev, t0);
            rates.push((n * size) as f64 / 1e3 / ms);
            info!(
                "link bench {kind}: 32 MB in {} KB messages: {:.1} MB/s (writes returned after {:.0} ms, arrived after {ms:.0} ms)",
                size / 1024,
                (n * size) as f64 / 1e3 / ms,
                (tw - t0) as f64 / 1e6
            );
        }
    }
    bail!("link bench finished")
}
