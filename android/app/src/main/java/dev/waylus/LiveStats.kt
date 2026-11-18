// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * Figures for the stats display, written where they arise and read once a second by the
 * display's ticker. Writers only do an atomic add or a field store, and only while the
 * display is on ([enabled]), so it costs nothing measurable on the frame path.
 */
object LiveStats {
    @Volatile var enabled = false

    // Decoder thread (VideoDecoder onShown).
    private val shown = AtomicInteger()
    private val decodeSumUs = AtomicLong()
    private val decodeN = AtomicInteger()

    fun frameShown(queuedNs: Long, decodedNs: Long) {
        if (!enabled) return
        shown.incrementAndGet()
        if (queuedNs > 0 && decodedNs >= queuedNs) {
            decodeSumUs.addAndGet((decodedNs - queuedNs) / 1000)
            decodeN.incrementAndGet()
        }
    }

    class Tablet(val fps: Float, val decodeMs: Float?)

    private var lastTakeNs = 0L

    /** Frames shown and the mean decode time since the last call. Ticker thread only. */
    fun takeTablet(nowNs: Long): Tablet {
        val n = shown.getAndSet(0)
        val sum = decodeSumUs.getAndSet(0)
        val dn = decodeN.getAndSet(0)
        val secs = if (lastTakeNs == 0L) 1.0 else (nowNs - lastTakeNs) / 1e9
        lastTakeNs = nowNs
        return Tablet((n / secs).toFloat(), if (dn > 0) sum / dn / 1000f else null)
    }

    /** Wi-Fi reception over the last second (WifiLink's 1 Hz report). */
    class Wifi(
        val freqMhz: Int, val rssi: Int, val shardsLost: Int, val shardsExpected: Int,
        val fecShards: Int, val framesLost: Int,
    )

    @Volatile var wifi: Wifi? = null

    fun wifiReport(stats: IntArray, freqMhz: Int, rssi: Int) {
        if (!enabled) return
        // VideoAssembler.takeStats order: shardsRx, shardsExpected, fecShards, fecFrames, framesOk, framesLost, ...
        wifi = Wifi(freqMhz, rssi, (stats[1] - stats[0]).coerceAtLeast(0), stats[1], stats[2], stats[5])
    }

    /** The host's rolling figures (Proto.STATS), about once a second. Null values: no data. */
    class Host(
        val shownP50: Float?, val shownP95: Float?, val penP50: Float?, val penP95: Float?,
        val heldMsPerS: Float?,
    )

    @Volatile var host: Host? = null
    @Volatile var hostFlags = 0
    @Volatile var hostAtNs = 0L

    fun reset() {
        shown.set(0); decodeSumUs.set(0); decodeN.set(0)
        wifi = null; host = null; hostFlags = 0; hostAtNs = 0L; lastTakeNs = 0L
    }
}
