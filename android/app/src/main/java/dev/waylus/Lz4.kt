// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

/**
 * LZ4 block format (no frame header), as written by the host's `lz4_flex::block`.
 * Used for raw screen patches (docs/damage-path.md); the compressor is only for the
 * benchmark and tests.
 */
object Lz4 {
    /**
     * Decompresses [src] (offset [srcOff], [srcLen] bytes) into [dst] at [dstOff]; returns the
     * number of bytes written. Throws on malformed input instead of reading or writing out of bounds.
     */
    fun decompress(src: ByteArray, srcOff: Int, srcLen: Int, dst: ByteArray, dstOff: Int, dstLen: Int): Int {
        var ip = srcOff
        val iend = srcOff + srcLen
        var op = dstOff
        val oend = dstOff + dstLen
        while (ip < iend) {
            val token = src[ip++].toInt() and 0xff
            var lit = token ushr 4
            if (lit == 15) {
                var b: Int
                do {
                    if (ip >= iend) throw IllegalArgumentException("lz4: truncated literal length")
                    b = src[ip++].toInt() and 0xff
                    lit += b
                } while (b == 255)
            }
            if (lit > iend - ip || lit > oend - op) throw IllegalArgumentException("lz4: literals out of bounds")
            System.arraycopy(src, ip, dst, op, lit)
            ip += lit
            op += lit
            if (ip >= iend) break // the last sequence has literals only
            if (iend - ip < 2) throw IllegalArgumentException("lz4: truncated offset")
            val offset = (src[ip].toInt() and 0xff) or ((src[ip + 1].toInt() and 0xff) shl 8)
            ip += 2
            var len = token and 15
            if (len == 15) {
                var b: Int
                do {
                    if (ip >= iend) throw IllegalArgumentException("lz4: truncated match length")
                    b = src[ip++].toInt() and 0xff
                    len += b
                } while (b == 255)
            }
            len += 4
            var from = op - offset
            if (offset == 0 || from < dstOff || len > oend - op) throw IllegalArgumentException("lz4: match out of bounds")
            if (offset >= len) {
                System.arraycopy(dst, from, dst, op, len)
                op += len
            } else {
                // Overlapping (run-length style): copy in growing chunks.
                var left = len
                while (left > 0) {
                    val n = minOf(left, op - from)
                    System.arraycopy(dst, from, dst, op, n)
                    op += n
                    left -= n
                }
            }
        }
        return op - dstOff
    }

    /** Greedy LZ4 block compressor (test data for the benchmark; the host has its own). */
    fun compress(src: ByteArray, off: Int = 0, len: Int = src.size): ByteArray {
        val out = java.io.ByteArrayOutputStream(len / 2 + 16)
        val end = off + len
        val table = IntArray(1 shl 16) { -1 }
        fun int(i: Int) = (src[i].toInt() and 0xff) or ((src[i + 1].toInt() and 0xff) shl 8) or
            ((src[i + 2].toInt() and 0xff) shl 16) or ((src[i + 3].toInt() and 0xff) shl 24)
        fun length(v: Int) {
            var n = v
            while (n >= 255) { out.write(255); n -= 255 }
            out.write(n)
        }
        fun sequence(litFrom: Int, litLen: Int, offset: Int, matchLen: Int) {
            val ml = matchLen - 4
            out.write((minOf(litLen, 15) shl 4) or (if (matchLen > 0) minOf(ml, 15) else 0))
            if (litLen >= 15) length(litLen - 15)
            out.write(src, litFrom, litLen)
            if (matchLen > 0) {
                out.write(offset and 0xff)
                out.write(offset ushr 8)
                if (ml >= 15) length(ml - 15)
            }
        }
        var anchor = off
        var ip = off
        val limit = end - 12 // a match must start at least 12 bytes before the end
        while (ip < limit) {
            val seq = int(ip)
            val h = (seq * -1640531535) ushr 16
            val ref = table[h]
            table[h] = ip
            if (ref >= 0 && ip - ref < 65536 && int(ref) == seq) {
                var m = 4
                while (ip + m < end - 5 && src[ref + m] == src[ip + m]) m++
                sequence(anchor, ip - anchor, ip - ref, m)
                ip += m
                anchor = ip
            } else {
                ip++
            }
        }
        sequence(anchor, end - anchor, 0, 0)
        return out.toByteArray()
    }
}
