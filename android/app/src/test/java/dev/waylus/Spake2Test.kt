// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import java.math.BigInteger

class Spake2Test {
    private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
    private fun n(s: String) = BigInteger(s, 16)

    /** RFC 9382 appendix B, first vector; same as host/src/wifi/spake2.rs. */
    @Test
    fun rfc9382Vector() {
        val w = n("2ee57912099d31560b3a44b1184b9b4866e904c49d12ac5042c97dca461b1a5f")
        val a = Spake2(true, w, n("43dd0fd7215bdcb482879fca3220c6a968e66d70b1356cac18bb26c84a78d729"), "server".toByteArray(), "client".toByteArray())
        val b = Spake2(false, w, n("dcb60106f276b02606d8ef0a328c02e4b629f84f89786af5befb0bc75b6e66be"), "server".toByteArray(), "client".toByteArray())
        assertEquals("04a56fa807caaa53a4d28dbb9853b9815c61a411118a6fe516a8798434751470f9010153ac33d0d5f2047ffdb1a3e42c9b4e6be662766e1eeb4116988ede5f912c", hex(a.msg))
        assertEquals("0406557e482bd03097ad0cbaa5df82115460d951e3451962f1eaf4367a420676d09857ccbc522686c83d1852abfa8ed6e4a1155cf8f1543ceca528afb591a1e0b7", hex(b.msg))
        val ka = a.finish(b.msg, ByteArray(0))
        val kb = b.finish(a.msg, ByteArray(0))
        assertEquals("0e0672dc86f8e45565d338b0540abe69", hex(ka.ke))
        assertEquals("58ad4aa88e0b60d5061eb6b5dd93e80d9c4f00d127c65b3b35b1b5281fee38f0", hex(ka.confA))
        assertEquals("d3e2e547f1ae04f2dbdbf0fc4b79f8ecff2dff314b5d32fe9fcef2fb26dc459b", hex(ka.confB))
        assertArrayEquals(ka.ke, kb.ke)
    }

    @Test
    fun wrongPinFails() {
        val id = "t".toByteArray(); val h = "h".toByteArray()
        val a = Spake2.newA(Spake2.wFromPin("123456", id, h), id, h)
        val b = Spake2(false, Spake2.wFromPin("123457", id, h), BigInteger.TEN, id, h)
        assertFalse(a.finish(b.msg, ByteArray(0)).confB.contentEquals(b.finish(a.msg, ByteArray(0)).confB))
    }

    @Test
    fun replayWindow() {
        val w = WifiCrypto.ReplayWindow()
        assert(w.accept(10)); assert(!w.accept(10)); assert(w.accept(8)); assert(w.accept(12))
        assert(!w.accept(8)); assert(w.accept(11)); assert(w.accept(200)); assert(!w.accept(12))
    }
}
