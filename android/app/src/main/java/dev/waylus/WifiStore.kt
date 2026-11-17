// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * This tablet's Wi-Fi identity and its paired host, in app-private preferences.
 * The pairing key is stored encrypted with a non-exportable Android Keystore key.
 */
class WifiStore(ctx: Context) {
    class Host(val id: ByteArray, val name: String, val key: ByteArray, val address: String)

    private val prefs = ctx.getSharedPreferences("wifi", Context.MODE_PRIVATE)

    val deviceId: ByteArray = prefs.getString("device_id", null)?.let { Spake2.hex(it) } ?: ByteArray(16).also {
        SecureRandom().nextBytes(it)
        prefs.edit().putString("device_id", toHex(it)).apply()
    }

    val deviceName: String = "${Build.MANUFACTURER} ${Build.MODEL}".trim()

    /** Last address used or given by the host (adb), even before pairing. */
    var address: String?
        get() = prefs.getString("address", null)
        set(v) { prefs.edit().putString("address", v).apply() }

    fun host(): Host? {
        val id = prefs.getString("host_id", null) ?: return null
        val wrapped = prefs.getString("host_key", null) ?: return null
        val key = runCatching { unwrap(Base64.decode(wrapped, Base64.NO_WRAP)) }.getOrNull() ?: return null
        return Host(Spake2.hex(id), prefs.getString("host_name", "") ?: "", key, prefs.getString("address", "") ?: "")
    }

    fun savePairing(hostId: ByteArray, name: String, key: ByteArray, address: String) {
        prefs.edit()
            .putString("host_id", toHex(hostId))
            .putString("host_name", name)
            .putString("host_key", Base64.encodeToString(wrap(key), Base64.NO_WRAP))
            .putString("address", address)
            .apply()
    }

    fun forgetPairing() {
        prefs.edit().remove("host_id").remove("host_name").remove("host_key").apply()
    }

    private fun wrapKey(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val g = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        g.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return g.generateKey()
    }

    private fun wrap(plain: ByteArray): ByteArray {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.ENCRYPT_MODE, wrapKey())
        return c.iv + c.doFinal(plain)
    }

    private fun unwrap(blob: ByteArray): ByteArray {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.DECRYPT_MODE, wrapKey(), GCMParameterSpec(128, blob.copyOfRange(0, 12)))
        return c.doFinal(blob, 12, blob.size - 12)
    }

    companion object {
        private const val ALIAS = "waylus-pairing"
        fun toHex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
    }
}
