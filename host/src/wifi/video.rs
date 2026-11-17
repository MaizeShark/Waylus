// SPDX-License-Identifier: GPL-3.0-or-later
//! Video over UDP: one AES-GCM seal per frame, then Reed-Solomon FEC over the
//! ciphertext (docs/wifi.md §2.5). Reassembly on the tablet: VideoAssembler.kt.
//!
//! Datagram: 16-byte header + one shard of `SHARD` bytes.
//! ```text
//! u8 kind=2 | u8 flags | u16 shard index in block | u32 frame number |
//! u8 block | u8 blocks | u8 data shards | u8 parity shards | u32 ciphertext length
//! ```
//! The ciphertext is split into `ceil(len / SHARD)` data shards (the last one zero
//! padded), spread evenly over `ceil(shards / MAX_DATA)` blocks; block `b` gets
//! `D / blocks` shards, plus one if `b < D % blocks`.

use std::{
    collections::{HashMap, VecDeque},
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::Instant,
};

use reed_solomon_erasure::galois_8::ReedSolomon;
use tracing::{debug, info};

use super::crypto::{Aead, stream};

pub const KIND_VIDEO: u8 = 2;
pub const HDR: usize = 16;
/// Shard payload: 16-aligned; datagram 1424 B → 1452 B IPv4 / 1472 B IPv6 packets.
pub const SHARD: usize = 1408;
pub const MAX_DATA: usize = 200;
const MIN_PARITY: usize = 2;

pub mod flags {
    pub const KEY: u8 = 1;
}

/// Frame numbers → capture timestamps of recent frames, to translate the tablet's loss
/// reports (it can't know the timestamp of a frame it never received).
pub type FrameMap = Arc<Mutex<VecDeque<(u32, u64)>>>;

pub struct Sender {
    pub fec_percent: usize,
    frame: u32,
    rs: HashMap<(usize, usize), ReedSolomon>,
    frames: FrameMap,
    buf: Vec<u8>,
    parity: Vec<Vec<u8>>,
    dgram: Vec<u8>,
    // Stats since the last report.
    window: Instant,
    n: u64,
    keys: u64,
    shards: u64,
    parity_sent: u64,
    prep_ns: u64,
    send_ns: u64,
    max_shards: usize,
    /// Test only: drop this many shards per 1000 (env WAYLUS_TEST_DROP=percent).
    drop_permille: u64,
    rng: u64,
}

/// xorshift64*, good enough for loss injection.
fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s >> 12;
    *s ^= *s << 25;
    *s ^= *s >> 27;
    s.wrapping_mul(0x2545f4914f6cdd1d)
}

/// (data, parity) shards for each block of a frame with `len` bytes of ciphertext.
pub fn layout(len: usize, fec_percent: usize) -> Vec<(usize, usize)> {
    let d = len.div_ceil(SHARD).max(1);
    let blocks = d.div_ceil(MAX_DATA);
    (0..blocks)
        .map(|b| {
            let db = d / blocks + usize::from(b < d % blocks);
            let p = (db * fec_percent).div_ceil(100).max(if fec_percent > 0 { MIN_PARITY } else { 0 }).min(255 - db);
            (db, p)
        })
        .collect()
}

impl Sender {
    pub fn new(fec_percent: usize, frames: FrameMap) -> Self {
        Self {
            fec_percent,
            frame: 0,
            rs: HashMap::new(),
            frames,
            buf: Vec::new(),
            parity: Vec::new(),
            dgram: Vec::with_capacity(HDR + SHARD),
            window: Instant::now(),
            n: 0,
            keys: 0,
            shards: 0,
            parity_sent: 0,
            prep_ns: 0,
            send_ns: 0,
            max_shards: 0,
            drop_permille: std::env::var("WAYLUS_TEST_DROP")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .map(|p| {
                    tracing::warn!("WAYLUS_TEST_DROP: dropping {p}% of video shards (test only)");
                    (p * 10.0) as u64
                })
                .unwrap_or(0),
            rng: 0x9e3779b97f4a7c15,
        }
    }


    /// Sends one frame. `payload` = the VIDEO message payload ([u64 capture_ns][u8 flags][data]).
    pub fn send(&mut self, sock: &UdpSocket, peer: SocketAddr, aead: &Aead, payload: &[u8]) {
        let t0 = Instant::now();
        self.frame = self.frame.wrapping_add(1);
        let frame = self.frame;
        let capture_ns = u64::from_le_bytes(payload[..8].try_into().unwrap());
        let key = payload[8] & crate::proto::video_flags::KEY != 0;
        {
            let mut m = self.frames.lock().unwrap();
            m.push_back((frame, capture_ns));
            while m.len() > 512 {
                m.pop_front();
            }
        }
        self.buf.clear();
        self.buf.extend_from_slice(payload);
        aead.seal(stream::VIDEO, frame as u64, &[], &mut self.buf, 0);
        let len = self.buf.len();
        let blocks = layout(len, self.fec_percent);
        let total_data: usize = blocks.iter().map(|b| b.0).sum();
        self.buf.resize(total_data * SHARD, 0);

        let fl = if key { flags::KEY } else { 0 };
        let mut off = 0usize;
        let mut sent = 0usize;
        let mut t_send = 0u64;
        for (bi, &(d, p)) in blocks.iter().enumerate() {
            let data = &self.buf[off * SHARD..(off + d) * SHARD];
            if self.parity.len() < p {
                self.parity.resize_with(p, || vec![0u8; SHARD]);
            }
            if p > 0 {
                let rs = self.rs.entry((d, p)).or_insert_with(|| ReedSolomon::new(d, p).expect("RS parameters"));
                let shards: Vec<&[u8]> = data.chunks(SHARD).collect();
                let mut par: Vec<&mut [u8]> = self.parity[..p].iter_mut().map(|v| v.as_mut_slice()).collect();
                rs.encode_sep(&shards, &mut par).expect("RS encode");
            }
            let ts = Instant::now();
            for i in 0..d + p {
                let shard: &[u8] = if i < d { &data[i * SHARD..(i + 1) * SHARD] } else { &self.parity[i - d] };
                // Test-only loss injection (WAYLUS_TEST_DROP).
                if self.drop_permille > 0 && xorshift(&mut self.rng) % 1000 < self.drop_permille {
                    sent += 1;
                    continue;
                }
                let g = &mut self.dgram;
                g.clear();
                g.extend_from_slice(&[KIND_VIDEO, fl]);
                g.extend_from_slice(&(i as u16).to_le_bytes());
                g.extend_from_slice(&frame.to_le_bytes());
                g.extend_from_slice(&[bi as u8, blocks.len() as u8, d as u8, p as u8]);
                g.extend_from_slice(&(len as u32).to_le_bytes());
                g.extend_from_slice(shard);
                if let Err(e) = sock.send_to(g, peer) {
                    debug!("video send: {e}");
                }
                sent += 1;
            }
            t_send += ts.elapsed().as_nanos() as u64;
            off += d;
            self.parity_sent += p as u64;
        }
        let total = t0.elapsed().as_nanos() as u64;
        self.prep_ns += total - t_send;
        self.send_ns += t_send;
        self.n += 1;
        self.keys += key as u64;
        self.shards += sent as u64;
        self.max_shards = self.max_shards.max(sent);
        if self.rs.len() > 256 {
            self.rs.clear();
        }
        if self.window.elapsed().as_secs() >= 5 {
            let n = self.n.max(1);
            info!(
                "wifi video: {} frames ({} key), {:.1} shards/frame (max {}), parity {:.0}%, seal+FEC {:.2} ms/frame, send {:.2} ms/frame",
                self.n,
                self.keys,
                self.shards as f64 / n as f64,
                self.max_shards,
                self.parity_sent as f64 * 100.0 / (self.shards - self.parity_sent).max(1) as f64,
                self.prep_ns as f64 / n as f64 / 1e6,
                self.send_ns as f64 / n as f64 / 1e6,
            );
            self.window = Instant::now();
            (self.n, self.keys, self.shards, self.parity_sent, self.prep_ns, self.send_ns, self.max_shards) = (0, 0, 0, 0, 0, 0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_blocks() {
        assert_eq!(layout(100, 20), vec![(1, 2)]);
        assert_eq!(layout(SHARD * 30, 20), vec![(30, 6)]);
        assert_eq!(layout(SHARD * 30 + 1, 20), vec![(31, 7)]);
        let big = layout(SHARD * 450, 20);
        assert_eq!(big.len(), 3);
        assert_eq!(big.iter().map(|b| b.0).sum::<usize>(), 450);
        assert!(big.iter().all(|&(d, p)| d + p <= 255 && p >= d / 5));
        assert_eq!(layout(SHARD * 2, 0), vec![(2, 0)]);
    }

    /// Fixed vector shared with the tablet's decoder test (ReedSolomonTest.kt).
    #[test]
    fn rs_cross_vector() {
        let rs = ReedSolomon::new(4, 2).unwrap();
        let data: Vec<Vec<u8>> = (0..4u8).map(|i| (0..8u8).map(|j| i.wrapping_mul(31).wrapping_add(j.wrapping_mul(7)).wrapping_add(1)).collect()).collect();
        let mut parity = vec![vec![0u8; 8]; 2];
        rs.encode_sep(&data, &mut parity).unwrap();
        let hex = |v: &Vec<u8>| v.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(&data[0]), "01080f161d242b32");
        assert_eq!(hex(&parity[0]), "f9248bd2813047e3");
        assert_eq!(hex(&parity[1]), "4523aae9a06801ef");
    }
}
