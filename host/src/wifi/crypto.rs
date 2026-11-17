// SPDX-License-Identifier: GPL-3.0-or-later
//! Session crypto for the Wi-Fi transport (mirrored in android/.../WifiCrypto.kt).
//!
//! AES-256-GCM everywhere. The nonce is `[stream, 0, 0, 0, counter u64 LE]`, where the
//! stream byte separates TCP records, UDP datagrams and video frames that share one
//! direction's key. Each direction has its own key, so counters never collide.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::AeadInPlace,
};
use anyhow::{Result, anyhow};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub const TAG_LEN: usize = 16;

pub mod stream {
    pub const TCP: u8 = b'T';
    pub const UDP: u8 = b'U';
    pub const VIDEO: u8 = b'V';
}

fn nonce(stream: u8, counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = stream;
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

#[derive(Clone)]
pub struct Aead(Aes256Gcm);

impl Aead {
    pub fn new(key: &[u8; 32]) -> Self {
        Self(Aes256Gcm::new(key.into()))
    }

    /// Encrypts `buf[start..]` in place and appends the tag.
    pub fn seal(&self, stream: u8, counter: u64, aad: &[u8], buf: &mut Vec<u8>, start: usize) {
        let n = nonce(stream, counter);
        let tag = self.0.encrypt_in_place_detached(&Nonce::from(n), aad, &mut buf[start..]).expect("AES-GCM seal");
        buf.extend_from_slice(&tag);
    }

    /// Decrypts `buf` (ciphertext + tag) in place and drops the tag.
    pub fn open(&self, stream: u8, counter: u64, aad: &[u8], buf: &mut Vec<u8>) -> Result<()> {
        if buf.len() < TAG_LEN {
            return Err(anyhow!("short ciphertext"));
        }
        let n = nonce(stream, counter);
        let split = buf.len() - TAG_LEN;
        let tag: [u8; TAG_LEN] = buf[split..].try_into().unwrap();
        self.0
            .decrypt_in_place_detached(&Nonce::from(n), aad, &mut buf[..split], (&tag).into())
            .map_err(|_| anyhow!("authentication failed"))?;
        buf.truncate(split);
        Ok(())
    }
}

pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC key");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn hkdf(salt: Option<&[u8]>, ikm: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(salt, ikm).expand(info, out).expect("HKDF length");
}

/// Per-session keys: host→client and client→host.
pub struct SessionKeys {
    pub h2c: [u8; 32],
    pub c2h: [u8; 32],
}

/// Session keys from the ephemeral ECDH secret and the long-term pairing key, bound to
/// the handshake transcript (forward secrecy from the ephemeral keys).
pub fn session_keys(ecdh: &[u8], pair_key: &[u8], transcript: &[u8; 32]) -> SessionKeys {
    let mut okm = [0u8; 64];
    hkdf(Some(transcript), &[ecdh, pair_key].concat(), b"waylus session v1", &mut okm);
    SessionKeys { h2c: okm[..32].try_into().unwrap(), c2h: okm[32..].try_into().unwrap() }
}

/// Sliding replay window over 64-bit sequence numbers.
#[derive(Default)]
pub struct ReplayWindow {
    highest: Option<u64>,
    bits: u64,
}

impl ReplayWindow {
    /// True if `seq` is new (and records it). Call only after authentication.
    pub fn accept(&mut self, seq: u64) -> bool {
        match self.highest {
            None => {
                self.highest = Some(seq);
                self.bits = 1;
                true
            }
            Some(h) if seq > h => {
                let shift = seq - h;
                self.bits = if shift >= 64 { 0 } else { self.bits << shift };
                self.bits |= 1;
                self.highest = Some(seq);
                true
            }
            Some(h) => {
                let back = h - seq;
                if back >= 64 || self.bits & (1 << back) != 0 {
                    return false;
                }
                self.bits |= 1 << back;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let a = Aead::new(&[7u8; 32]);
        let mut buf = b"hdr-hello".to_vec();
        a.seal(stream::UDP, 5, b"aad", &mut buf, 4);
        assert_eq!(&buf[..4], b"hdr-");
        let mut body = buf[4..].to_vec();
        assert!(a.open(stream::UDP, 6, b"aad", &mut body.clone()).is_err());
        assert!(a.open(stream::TCP, 5, b"aad", &mut body.clone()).is_err());
        a.open(stream::UDP, 5, b"aad", &mut body).unwrap();
        assert_eq!(body, b"hello");
    }

    #[test]
    fn replay_window() {
        let mut w = ReplayWindow::default();
        assert!(w.accept(10));
        assert!(!w.accept(10));
        assert!(w.accept(8));
        assert!(w.accept(12));
        assert!(!w.accept(8));
        assert!(w.accept(11));
        assert!(w.accept(200));
        assert!(!w.accept(12));
    }
}
