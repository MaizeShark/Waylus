// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ProtoTest {
    /** The payload host/src/proto.rs `stats_encoding` writes. */
    @Test
    fun parsesHostStats() {
        val payload = byteArrayOf(1, 0x7f, 0x01, 0xd6.toByte(), 0x01, -1, -1, 0xfe.toByte(), -1, 0, 0)
        val s = Proto.parseStats(payload)!!
        assertEquals(Proto.STATS_HUD, s.flags)
        assertEquals(38.3f, s.stats.shownP50!!, 0.001f)
        assertEquals(47.0f, s.stats.shownP95!!, 0.001f)
        assertNull(s.stats.penP50)
        assertEquals(6553.4f, s.stats.penP95!!, 0.001f)
        assertEquals(0f, s.stats.heldMsPerS!!, 0.001f)
    }

    /** A shorter payload (an older or newer host) leaves the missing fields empty. */
    @Test
    fun shortStats() {
        val s = Proto.parseStats(byteArrayOf(0, 100, 0))!!
        assertEquals(10f, s.stats.shownP50!!, 0.001f)
        assertNull(s.stats.shownP95)
        assertNull(s.stats.heldMsPerS)
    }
}
