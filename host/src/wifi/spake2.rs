// SPDX-License-Identifier: GPL-3.0-or-later
//! SPAKE2 (RFC 9382), ciphersuite P256-SHA256-HKDF-HMAC, for pairing with a PIN.
//!
//! The tablet is party A, the host party B. Mirrored in
//! `android/app/src/main/java/dev/waylus/Spake2.kt`; both are tested against the RFC's
//! test vectors.

use anyhow::{Result, bail};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use p256::{
    AffinePoint, EncodedPoint, FieldBytes, NonZeroScalar, ProjectivePoint, PublicKey, Scalar, U256,
    elliptic_curve::{ops::Reduce, sec1::ToEncodedPoint},
};
use sha2::{Digest, Sha256};

/// RFC 9382 section 6, compressed SEC1.
const M: [u8; 33] = hex33("02886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f");
const N: [u8; 33] = hex33("03d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49");

const fn hex33(s: &str) -> [u8; 33] {
    let b = s.as_bytes();
    let mut out = [0u8; 33];
    let mut i = 0;
    while i < 33 {
        out[i] = (nib(b[2 * i]) << 4) | nib(b[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nib(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("bad hex"),
    }
}

fn point(bytes: &[u8]) -> Result<ProjectivePoint> {
    // Rejects the identity and points not on the curve.
    match PublicKey::from_sec1_bytes(bytes) {
        Ok(p) => Ok(p.to_projective()),
        Err(_) => bail!("invalid SPAKE2 point"),
    }
}

fn encode(p: &ProjectivePoint) -> [u8; 65] {
    let e: EncodedPoint = AffinePoint::from(*p).to_encoded_point(false);
    e.as_bytes().try_into().expect("uncompressed point")
}

/// w for a PIN: SHA-256 over a label, both identities and the PIN, reduced mod n.
/// (No memory-hard function: the PIN is single-use and SPAKE2 allows only online guesses.)
pub fn w_from_pin(pin: &str, a: &[u8], b: &[u8]) -> Scalar {
    let mut h = Sha256::new();
    h.update(b"waylus spake2 w v1");
    for s in [a, b, pin.as_bytes()] {
        h.update((s.len() as u64).to_le_bytes());
        h.update(s);
    }
    let digest: FieldBytes = h.finalize();
    <Scalar as Reduce<U256>>::reduce_bytes(&digest)
}

/// Keys from a completed exchange.
pub struct Keys {
    /// The shared secret (Ke).
    pub ke: [u8; 16],
    /// Confirmation MACs of A (tablet) and B (host).
    pub conf_a: [u8; 32],
    pub conf_b: [u8; 32],
}

pub struct Party {
    is_a: bool,
    a: Vec<u8>,
    b: Vec<u8>,
    w: Scalar,
    secret: Scalar,
    /// Our message (pA or pB).
    pub msg: [u8; 65],
}

impl Party {
    /// Host side (party B) with a fresh random scalar.
    pub fn new_b(w: Scalar, a: &[u8], b: &[u8]) -> Result<Self> {
        let y = *NonZeroScalar::random(&mut rand_core::OsRng);
        Self::with_secret(false, w, y, a, b)
    }

    pub fn with_secret(is_a: bool, w: Scalar, secret: Scalar, a: &[u8], b: &[u8]) -> Result<Self> {
        let blind = point(if is_a { &M } else { &N })?;
        let msg = encode(&(ProjectivePoint::GENERATOR * secret + blind * w));
        Ok(Self { is_a, a: a.to_vec(), b: b.to_vec(), w, secret, msg })
    }

    /// Takes the peer's message; `aad` goes into the confirmation keys.
    pub fn finish(&self, peer: &[u8], aad: &[u8]) -> Result<Keys> {
        let peer_pt = point(peer)?;
        let unblind = point(if self.is_a { &N } else { &M })?;
        // Cofactor h = 1 for P-256.
        let k = (peer_pt - unblind * self.w) * self.secret;
        if k == ProjectivePoint::IDENTITY {
            bail!("SPAKE2: K is the identity");
        }
        let (pa, pb) = if self.is_a { (&self.msg[..], peer) } else { (peer, &self.msg[..]) };
        let mut tt = Vec::with_capacity(300);
        let w_bytes: FieldBytes = self.w.into();
        for s in [&self.a[..], &self.b[..], pa, pb, &encode(&k)[..], &w_bytes[..]] {
            tt.extend_from_slice(&(s.len() as u64).to_le_bytes());
            tt.extend_from_slice(s);
        }
        let h = Sha256::digest(&tt);
        let (ke, ka) = h.split_at(16);
        let mut info = b"ConfirmationKeys".to_vec();
        info.extend_from_slice(aad);
        let mut kc = [0u8; 32];
        Hkdf::<Sha256>::new(None, ka).expand(&info, &mut kc).expect("HKDF length");
        let mac = |key: &[u8]| -> [u8; 32] {
            let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC key");
            m.update(&tt);
            m.finalize().into_bytes().into()
        };
        Ok(Keys { ke: ke.try_into().unwrap(), conf_a: mac(&kc[..16]), conf_b: mac(&kc[16..]) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::elliptic_curve::PrimeField;

    fn scalar(h: &str) -> Scalar {
        let b: [u8; 32] = hex::decode(h).unwrap().try_into().unwrap();
        Scalar::from_repr(b.into()).unwrap()
    }

    /// RFC 9382 appendix B, first vector (A='server', B='client', no AAD).
    #[test]
    fn rfc9382_vector() {
        let w = scalar("2ee57912099d31560b3a44b1184b9b4866e904c49d12ac5042c97dca461b1a5f");
        let x = scalar("43dd0fd7215bdcb482879fca3220c6a968e66d70b1356cac18bb26c84a78d729");
        let y = scalar("dcb60106f276b02606d8ef0a328c02e4b629f84f89786af5befb0bc75b6e66be");
        let a = Party::with_secret(true, w, x, b"server", b"client").unwrap();
        let b = Party::with_secret(false, w, y, b"server", b"client").unwrap();
        assert_eq!(
            hex::encode(a.msg),
            "04a56fa807caaa53a4d28dbb9853b9815c61a411118a6fe516a8798434751470f9010153ac33d0d5f2047ffdb1a3e42c9b4e6be662766e1eeb4116988ede5f912c"
        );
        assert_eq!(
            hex::encode(b.msg),
            "0406557e482bd03097ad0cbaa5df82115460d951e3451962f1eaf4367a420676d09857ccbc522686c83d1852abfa8ed6e4a1155cf8f1543ceca528afb591a1e0b7"
        );
        let ka = a.finish(&b.msg, b"").unwrap();
        let kb = b.finish(&a.msg, b"").unwrap();
        assert_eq!(hex::encode(ka.ke), "0e0672dc86f8e45565d338b0540abe69");
        assert_eq!(ka.ke, kb.ke);
        assert_eq!(hex::encode(ka.conf_a), "58ad4aa88e0b60d5061eb6b5dd93e80d9c4f00d127c65b3b35b1b5281fee38f0");
        assert_eq!(hex::encode(ka.conf_b), "d3e2e547f1ae04f2dbdbf0fc4b79f8ecff2dff314b5d32fe9fcef2fb26dc459b");
        assert_eq!(ka.conf_a, kb.conf_a);
        assert_eq!(ka.conf_b, kb.conf_b);
    }

    #[test]
    fn wrong_pin_fails_confirmation() {
        let a = Party::with_secret(true, w_from_pin("123456", b"t", b"h"), scalar(&"11".repeat(32)), b"t", b"h").unwrap();
        let b = Party::new_b(w_from_pin("123457", b"t", b"h"), b"t", b"h").unwrap();
        assert_ne!(a.finish(&b.msg, b"x").unwrap().conf_b, b.finish(&a.msg, b"x").unwrap().conf_b);
    }
}
