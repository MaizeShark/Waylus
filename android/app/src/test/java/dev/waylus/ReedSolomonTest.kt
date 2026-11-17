// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class ReedSolomonTest {
    private fun hex(s: String) = Spake2.hex(s)

    /** Parity from the host's reed-solomon-erasure crate (video.rs rs_cross_vector). */
    @Test
    fun decodesHostParity() {
        val data = Array(4) { i -> ByteArray(8) { j -> (i * 31 + j * 7 + 1).toByte() } }
        assertArrayEquals(hex("01080f161d242b32"), data[0])
        val shards: Array<ByteArray?> = arrayOf(null, data[1], null, data[3], hex("f9248bd2813047e3"), hex("4523aae9a06801ef"))
        assertEquals(2, ReedSolomon.reconstructData(shards, 4, 2, 8))
        assertArrayEquals(data[0], shards[0])
        assertArrayEquals(data[2], shards[2])
    }
}
