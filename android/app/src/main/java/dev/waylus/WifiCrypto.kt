// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import java.math.BigInteger
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.MessageDigest
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Session crypto for the Wi-Fi transport; mirrors host/src/wifi/crypto.rs. Platform JCA only. */
object WifiCrypto {
    const val TAG_LEN = 16
    const val STREAM_TCP = 'T'.code
    const val STREAM_UDP = 'U'.code
    const val STREAM_VIDEO = 'V'.code

    fun nonce(stream: Int, counter: Long): ByteArray {
        val n = ByteBuffer.allocate(12).order(ByteOrder.LITTLE_ENDIAN)
        n.put(stream.toByte()); n.put(0); n.put(0); n.put(0)
        n.putLong(counter)
        return n.array()
    }

    /** AES-256-GCM with one key. Not thread-safe: one instance per thread. */
    class Aead(key: ByteArray) {
        private val k = SecretKeySpec(key, "AES")
        private val c = Cipher.getInstance("AES/GCM/NoPadding")

        /** Returns ciphertext + tag of `data[off, off+len)`. */
        fun seal(stream: Int, counter: Long, aad: ByteArray, data: ByteArray, off: Int = 0, len: Int = data.size - off): ByteArray {
            c.init(Cipher.ENCRYPT_MODE, k, GCMParameterSpec(128, nonce(stream, counter)))
            c.updateAAD(aad)
            return c.doFinal(data, off, len)
        }

        /** Decrypts ciphertext + tag; throws on authentication failure. */
        fun open(stream: Int, counter: Long, aad: ByteArray, data: ByteArray, off: Int = 0, len: Int = data.size - off): ByteArray {
            c.init(Cipher.DECRYPT_MODE, k, GCMParameterSpec(128, nonce(stream, counter)))
            c.updateAAD(aad)
            return c.doFinal(data, off, len)
        }
    }

    fun hmac(key: ByteArray, vararg parts: ByteArray): ByteArray {
        val m = Mac.getInstance("HmacSHA256")
        m.init(SecretKeySpec(key, "HmacSHA256"))
        for (p in parts) m.update(p)
        return m.doFinal()
    }

    fun sha256(vararg parts: ByteArray): ByteArray {
        val d = MessageDigest.getInstance("SHA-256")
        for (p in parts) d.update(p)
        return d.digest()
    }

    /** RFC 5869 HKDF-SHA256. A null salt means 32 zero bytes. */
    fun hkdf(salt: ByteArray?, ikm: ByteArray, info: ByteArray, len: Int): ByteArray {
        val prk = hmac(salt ?: ByteArray(32), ikm)
        val out = java.io.ByteArrayOutputStream()
        var t = ByteArray(0)
        var i = 1
        while (out.size() < len) {
            t = hmac(prk, t, info, byteArrayOf(i.toByte()))
            out.write(t)
            i++
        }
        return out.toByteArray().copyOf(len)
    }

    class SessionKeys(val h2c: ByteArray, val c2h: ByteArray)

    fun sessionKeys(ecdh: ByteArray, pairKey: ByteArray, transcript: ByteArray): SessionKeys {
        val okm = hkdf(transcript, ecdh + pairKey, "waylus session v1".toByteArray(), 64)
        return SessionKeys(okm.copyOfRange(0, 32), okm.copyOfRange(32, 64))
    }

    fun ephemeral(): KeyPair = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }.generateKeyPair()

    /** Uncompressed SEC1 encoding of an EC public key. */
    fun encodePublic(k: KeyPair): ByteArray {
        val w = (k.public as ECPublicKey).w
        return byteArrayOf(4) + Spake2.scalarBytes(w.affineX) + Spake2.scalarBytes(w.affineY)
    }

    fun ecdh(own: KeyPair, peer: ByteArray): ByteArray {
        require(peer.size == 65 && peer[0] == 4.toByte()) { "bad public key" }
        val params = (own.public as ECPublicKey).params
        val pt = ECPoint(BigInteger(1, peer.copyOfRange(1, 33)), BigInteger(1, peer.copyOfRange(33, 65)))
        val pub = KeyFactory.getInstance("EC").generatePublic(ECPublicKeySpec(pt, params))
        val ka = KeyAgreement.getInstance("ECDH")
        ka.init(own.private)
        ka.doPhase(pub, true)
        return ka.generateSecret()
    }

    /** Sliding replay window over 64-bit sequence numbers (see crypto.rs). */
    class ReplayWindow {
        private var highest = -1L
        private var bits = 0L

        fun accept(seq: Long): Boolean {
            if (highest < 0) { highest = seq; bits = 1; return true }
            if (seq > highest) {
                val shift = seq - highest
                bits = if (shift >= 64) 0 else bits shl shift.toInt()
                bits = bits or 1
                highest = seq
                return true
            }
            val back = highest - seq
            if (back >= 64 || bits and (1L shl back.toInt()) != 0L) return false
            bits = bits or (1L shl back.toInt())
            return true
        }
    }

    /**
     * Developer benchmark (`am start ... --ez bench_crypto true`): AES-GCM open of a typical
     * frame vs AES-CTR (no authentication) vs a plain copy, logged to logcat.
     */
    fun bench() {
        val key = SecretKeySpec(ByteArray(32) { it.toByte() }, "AES")
        val frame = ByteArray(41 * 1024) { (it * 7).toByte() }
        val gcm = Cipher.getInstance("AES/GCM/NoPadding")
        gcm.init(Cipher.ENCRYPT_MODE, key, GCMParameterSpec(128, ByteArray(12)))
        val sealed = gcm.doFinal(frame)
        val ctr = Cipher.getInstance("AES/CTR/NoPadding")
        fun time(name: String, block: () -> Unit) {
            repeat(50) { block() }
            val n = 300
            val t0 = System.nanoTime()
            repeat(n) { block() }
            val per = (System.nanoTime() - t0) / n
            android.util.Log.i("waylus", "bench $name: %.3f ms/frame (%.0f MB/s)".format(per / 1e6, frame.size * 1e3 / per))
        }
        android.util.Log.i("waylus", "bench provider: ${gcm.provider.name} ${gcm.provider.version}, priority ${android.os.Process.getThreadPriority(android.os.Process.myTid())}")
        time("AES-GCM open") { gcm.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, ByteArray(12))); gcm.doFinal(sealed) }
        time("AES-CTR") { ctr.init(Cipher.DECRYPT_MODE, key, javax.crypto.spec.IvParameterSpec(ByteArray(16))); ctr.doFinal(frame) }
        runCatching {
            val ck = SecretKeySpec(ByteArray(32) { it.toByte() }, "ChaCha20")
            val cc = Cipher.getInstance("ChaCha20/Poly1305/NoPadding")
            cc.init(Cipher.ENCRYPT_MODE, ck, javax.crypto.spec.IvParameterSpec(ByteArray(12)))
            val cs = cc.doFinal(frame)
            time("ChaCha20-Poly1305 open") { cc.init(Cipher.DECRYPT_MODE, ck, javax.crypto.spec.IvParameterSpec(ByteArray(12))); cc.doFinal(cs) }
        }.onFailure { android.util.Log.i("waylus", "bench ChaCha20-Poly1305: $it") }
        time("copy") { frame.copyOf() }
    }
}
