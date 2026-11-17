// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Reassembles video frames from UDP shards (format: host/src/wifi/video.rs), recovers
 * missing shards with Reed-Solomon and decrypts the frame. Frames are delivered in
 * order. A frame that can't be completed is reported lost as soon as a later frame is
 * complete, shards of a frame two ahead arrive, or after [LOST_AFTER_NS]; no waiting
 * for retransmissions (after Moonlight's RtpVideoQueue).
 *
 * Not thread-safe except for [takeStats]; called from the UDP receive thread.
 */
class VideoAssembler(
    private val aead: WifiCrypto.Aead,
    /** The VIDEO message payload ([u64 capture_ns][u8 flags][data]) and when it was complete. */
    private val deliver: (ByteArray, Long) -> Unit,
    /** Frames first..last are lost. */
    private val lost: (Long, Long) -> Unit,
) {
    private class Block(val d: Int, val p: Int) {
        val shards = arrayOfNulls<ByteArray>(d + p)
        var have = 0
        var done = false
    }

    private class Frame(val no: Long, val len: Int, val blocks: Array<Block?>, val firstNs: Long) {
        var lastNs = firstNs
        var doneBlocks = 0
        var payload: ByteArray? = null
        var failed = false
    }

    private val frames = HashMap<Long, Frame>()
    private var next = -1L
    private var newest = -1L

    // Stats since the last takeStats().
    private var shardsRx = 0
    private var shardsExpected = 0
    private var fecShards = 0
    private var fecFrames = 0
    private var framesOk = 0
    private var framesLost = 0
    private var spreadSumNs = 0L
    private var spreadMaxNs = 0L
    private var assembleSumNs = 0L
    private var decryptSumNs = 0L
    private var decryptBytes = 0L
    private var decryptFrames = 0

    @Synchronized
    fun add(buf: ByteArray, n: Int, nowNs: Long) {
        if (n != HDR + SHARD) return
        val b = ByteBuffer.wrap(buf, 0, n).order(ByteOrder.LITTLE_ENDIAN)
        val idx = b.getShort(2).toInt() and 0xffff
        val no = b.getInt(4).toLong() and 0xffffffffL
        val bi = buf[8].toInt() and 0xff
        val nb = buf[9].toInt() and 0xff
        val d = buf[10].toInt() and 0xff
        val p = buf[11].toInt() and 0xff
        val len = b.getInt(12)
        if (nb == 0 || bi >= nb || d == 0 || idx >= d + p || len <= 0 || len > nb * 255 * SHARD) return
        if (next < 0) next = no
        if (no < next) {
            // Typically parity of a frame that was already complete without it.
            if (next - no <= 8) shardsRx++
            return
        }
        if (no - next > 64) {
            // Far ahead (a long outage): give up everything in between.
            resolveLost(next, no - 1)
            next = no
        }
        shardsRx++
        val f = frames.getOrPut(no) { Frame(no, len, arrayOfNulls(nb), nowNs) }
        if (f.len != len || f.blocks.size != nb) return
        f.lastNs = nowNs
        val blk = f.blocks[bi] ?: Block(d, p).also { f.blocks[bi] = it }
        if (blk.d != d || blk.p != p || blk.done || blk.shards[idx] != null) return
        blk.shards[idx] = buf.copyOfRange(HDR, HDR + SHARD)
        blk.have++
        if (blk.have >= d) {
            val t0 = System.nanoTime()
            val rec = try { ReedSolomon.reconstructData(blk.shards, d, p, SHARD) } catch (_: Exception) { -1 }
            assembleSumNs += System.nanoTime() - t0
            if (rec < 0) f.failed = true
            if (rec > 0) { fecShards += rec; fecFrames++ }
            blk.done = true
            f.doneBlocks++
            if (f.doneBlocks == nb && !f.failed) finish(f)
        }
        if (no > newest) newest = no
        pump(nowNs)
    }

    /** Call periodically (e.g. on receive timeouts) so a stalled frame is reported lost. */
    @Synchronized
    fun tick(nowNs: Long) = pump(nowNs)

    private fun finish(f: Frame) {
        val t0 = System.nanoTime()
        val ct = ByteArray(f.len)
        var off = 0
        for (blk in f.blocks) {
            for (i in 0 until blk!!.d) {
                val n = minOf(SHARD, f.len - off)
                if (n <= 0) break
                System.arraycopy(blk.shards[i]!!, 0, ct, off, n)
                off += n
            }
        }
        val t1 = System.nanoTime()
        try {
            f.payload = aead.open(WifiCrypto.STREAM_VIDEO, f.no, EMPTY, ct)
        } catch (_: Exception) {
            f.failed = true // forged or corrupted: treat as lost
        }
        val t2 = System.nanoTime()
        assembleSumNs += t2 - t0
        decryptSumNs += t2 - t1
        decryptBytes += f.len
        if (++decryptFrames == 300) {
            android.util.Log.i("waylus", "video decrypt: %.3f ms/frame (%.0f KB avg, %.0f MB/s)".format(
                decryptSumNs / 300 / 1e6, decryptBytes / 300 / 1024.0, decryptBytes * 1e3 / decryptSumNs))
            decryptSumNs = 0; decryptBytes = 0; decryptFrames = 0
        }
    }

    private fun pump(nowNs: Long) {
        var lostFirst = -1L
        var lostLast = -1L
        while (next >= 0 && newest >= next) {
            val f = frames[next]
            val payload = f?.payload
            if (payload != null) {
                if (lostFirst >= 0) { report(lostFirst, lostLast); lostFirst = -1 }
                frames.remove(next)
                framesOk++
                shardsExpected += f.blocks.sumOf { (it?.d ?: 0) + (it?.p ?: 0) }
                val spread = f.lastNs - f.firstNs
                spreadSumNs += spread
                if (spread > spreadMaxNs) spreadMaxNs = spread
                deliver(payload, f.lastNs)
                next++
                continue
            }
            val after = frames[next + 1]
            val gone = f?.failed == true ||
                newest >= next + 2 ||
                after?.payload != null ||
                (f != null && nowNs - f.firstNs > LOST_AFTER_NS) ||
                (f == null && after != null && nowNs - after.firstNs > LOST_AFTER_NS)
            if (!gone) break
            if (f != null) shardsExpected += f.blocks.sumOf { (it?.d ?: 0) + (it?.p ?: 0) }
            frames.remove(next)
            framesLost++
            if (lostFirst < 0) lostFirst = next
            lostLast = next
            next++
        }
        if (lostFirst >= 0) report(lostFirst, lostLast)
    }

    private fun resolveLost(first: Long, last: Long) {
        for (i in first..last) frames.remove(i)
        framesLost += (last - first + 1).toInt()
        report(first, last)
    }

    private fun report(first: Long, last: Long) = lost(first, last)

    /** WIFI_STATS payload fields since the last call (see host proto::WifiStats). */
    @Synchronized
    fun takeStats(): IntArray {
        val ok = maxOf(framesOk, 1)
        val r = intArrayOf(
            shardsRx, shardsExpected, fecShards, fecFrames, framesOk, framesLost,
            (spreadSumNs / ok / 1000).toInt(), (spreadMaxNs / 1000).toInt(), (assembleSumNs / ok / 1000).toInt(),
        )
        shardsRx = 0; shardsExpected = 0; fecShards = 0; fecFrames = 0; framesOk = 0; framesLost = 0
        spreadSumNs = 0; spreadMaxNs = 0; assembleSumNs = 0
        return r
    }

    companion object {
        const val KIND_VIDEO: Byte = 2
        const val HDR = 16
        const val SHARD = 1408
        private const val LOST_AFTER_NS = 100_000_000L
        private val EMPTY = ByteArray(0)
    }
}
