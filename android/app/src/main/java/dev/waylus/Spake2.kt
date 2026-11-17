// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import org.bouncycastle.asn1.nist.NISTNamedCurves
import org.bouncycastle.math.ec.ECPoint
import java.math.BigInteger
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.security.SecureRandom

/**
 * SPAKE2 (RFC 9382), P256-SHA256-HKDF-HMAC, mirroring host/src/wifi/spake2.rs.
 * The tablet is party A. BouncyCastle provides the point arithmetic the JCA lacks.
 */
class Spake2(private val isA: Boolean, private val w: BigInteger, private val secret: BigInteger, private val a: ByteArray, private val b: ByteArray) {
    class Keys(val ke: ByteArray, val confA: ByteArray, val confB: ByteArray)

    /** Our message (pA or pB), uncompressed SEC1. */
    val msg: ByteArray = G.multiply(secret).add((if (isA) M else N).multiply(w)).normalize().getEncoded(false)

    fun finish(peer: ByteArray, aad: ByteArray): Keys {
        val peerPt = decode(peer)
        val k = peerPt.subtract((if (isA) N else M).multiply(w)).multiply(secret).normalize()
        require(!k.isInfinity) { "SPAKE2: K is the identity" }
        val (pA, pB) = if (isA) msg to peer else peer to msg
        val tt = java.io.ByteArrayOutputStream()
        for (s in listOf(a, b, pA, pB, k.getEncoded(false), scalarBytes(w))) {
            tt.write(le64(s.size.toLong()))
            tt.write(s)
        }
        val ttb = tt.toByteArray()
        val h = MessageDigest.getInstance("SHA-256").digest(ttb)
        val kc = WifiCrypto.hkdf(null, h.copyOfRange(16, 32), "ConfirmationKeys".toByteArray() + aad, 32)
        return Keys(
            h.copyOfRange(0, 16),
            WifiCrypto.hmac(kc.copyOfRange(0, 16), ttb),
            WifiCrypto.hmac(kc.copyOfRange(16, 32), ttb),
        )
    }

    companion object {
        private val P256 = NISTNamedCurves.getByName("P-256")
        private val G: ECPoint = P256.g
        val ORDER: BigInteger = P256.n
        private val M = decode(hex("02886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f"))
        private val N = decode(hex("03d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49"))

        private fun decode(b: ByteArray): ECPoint {
            val p = P256.curve.decodePoint(b) // checks the point is on the curve
            require(!p.isInfinity && p.isValid) { "invalid SPAKE2 point" }
            return p
        }

        fun hex(s: String): ByteArray = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

        private fun le64(v: Long): ByteArray = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(v).array()

        /** Big-endian, padded to 32 bytes. */
        fun scalarBytes(v: BigInteger): ByteArray {
            val raw = v.toByteArray()
            val out = ByteArray(32)
            val n = minOf(raw.size, 32)
            System.arraycopy(raw, raw.size - n, out, 32 - n, n)
            return out
        }

        /** Same as w_from_pin in spake2.rs. */
        fun wFromPin(pin: String, a: ByteArray, b: ByteArray): BigInteger {
            val d = MessageDigest.getInstance("SHA-256")
            d.update("waylus spake2 w v1".toByteArray())
            for (s in listOf(a, b, pin.toByteArray())) {
                d.update(le64(s.size.toLong()))
                d.update(s)
            }
            return BigInteger(1, d.digest()).mod(ORDER)
        }

        fun newA(w: BigInteger, a: ByteArray, b: ByteArray): Spake2 {
            var x: BigInteger
            val rnd = SecureRandom()
            do { x = BigInteger(256, rnd) } while (x.signum() == 0 || x >= ORDER)
            return Spake2(true, w, x, a, b)
        }
    }
}
