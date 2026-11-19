// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import java.io.DataInputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Wire protocol, mirrors host/src/proto.rs. Frames: [u8 type][u32 len LE][payload]. */
object Proto {
    const val VERSION = 5
    const val DEFAULT_PORT = 27183

    const val HELLO = 1
    const val CONFIG = 2
    const val VIDEO = 3
    const val PEN = 4
    const val TOUCH = 5
    const val PING = 6
    const val PONG = 7
    const val FRAME_SHOWN = 8
    const val REQUEST_KEYFRAME = 9
    const val LAYOUT = 10
    /** USB accessory only: marks the start of a session in the byte stream. */
    const val SYNC = 11
    // Wi-Fi only (WifiLink.kt): pairing and authentication, before the session keys exist.
    const val PAIR_REQUEST = 12
    const val PAIR_READY = 13
    const val PAIR_A = 14
    const val PAIR_B = 15
    const val PAIR_CONFIRM = 16
    const val PAIR_DONE = 17
    const val AUTH_C = 18
    const val AUTH_H = 19
    const val AUTH_FIN = 20
    const val AUTH_FAIL = 21
    /** Wi-Fi only, sealed UDP: tells the host our UDP address (also a keepalive). */
    const val UDP_HELLO = 22
    /** Local only (WifiLink → Connection): frames were lost on the network. */
    const val FRAME_LOST = 23
    /** A received frame was dropped while the decoder waits for a keyframe. */
    const val FRAME_SKIPPED = 24
    /** Wi-Fi: frame numbers first..last could not be reassembled. */
    const val WIFI_FRAME_LOST = 25
    /** Wi-Fi: reception statistics, once per second. */
    const val WIFI_STATS = 26
    /**
     * Host → tablet, about once a second: the host's rolling figures for the stats display
     * (parseStats). Added without a version bump: older apps ignore unknown message types.
     */
    const val STATS = 27

    /** Host → tablet: the host ends the session ([u8 reason][UTF-8 text]). */
    const val BYE = 28
    const val BYE_STOPPED = 0
    const val BYE_ERROR = 1

    /** STATS flags. */
    const val STATS_HUD = 1
    /** The host reads the stylus directly (over adb): no pen→host figure. */
    const val STATS_RAW_PEN = 2

    /** AUTH_FAIL reasons (host/src/wifi/mod.rs `fail`). */
    const val FAIL_UNKNOWN_DEVICE = 1
    const val FAIL_BAD_MAC = 2
    const val FAIL_PAIRING_OFF = 3
    const val FAIL_WRONG_PIN = 4

    const val VIDEO_KEY = 1
    const val VIDEO_CONFIG = 2

    const val CODEC_H264 = 0
    const val CODEC_NONE = 255

    /** Opt-in, device-specific decoder tweaks (Config flags). Not for comparisons. */
    const val TWEAK_MTK_VPP_OFF = 1
    const val TWEAK_MTK_CPU_BOOST = 2

    const val PEN_OUT = 0
    const val PEN_HOVER = 1
    const val PEN_CONTACT = 2

    const val BTN_PRIMARY = 1
    const val BTN_SECONDARY = 2
    const val BTN_ERASER = 4

    private fun msg(type: Int, payloadLen: Int): ByteBuffer {
        val b = ByteBuffer.allocate(5 + payloadLen).order(ByteOrder.LITTLE_ENDIAN)
        b.put(type.toByte())
        b.putInt(payloadLen)
        return b
    }

    /** pxPerMm: physical pixel density of the panel (appended field; 0 = unknown). */
    fun hello(surfaceW: Int, surfaceH: Int, maxW: Int, maxH: Int, mbFrame: Int, mbSec: Int, refreshHz: Float, decoder: String, pxPerMm: Float): ByteArray {
        val name = decoder.toByteArray().take(255).toByteArray()
        val b = msg(HELLO, 2 + 2 + 2 + 2 + 2 + 4 + 4 + 2 + 1 + name.size + 2)
        b.putShort(VERSION.toShort())
        b.putShort(surfaceW.toShort()); b.putShort(surfaceH.toShort())
        b.putShort(maxW.toShort()); b.putShort(maxH.toShort())
        b.putInt(mbFrame); b.putInt(mbSec)
        b.putShort((refreshHz * 100).toInt().coerceIn(0, 65535).toShort())
        b.put(name.size.toByte()); b.put(name)
        b.putShort((pxPerMm * 100).toInt().coerceIn(0, 65535).toShort())
        return b.array()
    }

    fun pen(tNs: Long, state: Int, buttons: Int, x: Int, y: Int, pressure: Int, tiltX: Int, tiltY: Int): ByteArray {
        val b = msg(PEN, 8 + 1 + 1 + 2 * 5)
        b.putLong(tNs)
        b.put(state.toByte()); b.put(buttons.toByte())
        b.putShort(x.toShort()); b.putShort(y.toShort())
        b.putShort(pressure.toShort())
        b.putShort(tiltX.toShort()); b.putShort(tiltY.toShort())
        return b.array()
    }

    /** points: flat array of (id, x, y) triples. */
    fun touch(tNs: Long, count: Int, ids: IntArray, xs: IntArray, ys: IntArray): ByteArray {
        val b = msg(TOUCH, 8 + 1 + count * 5)
        b.putLong(tNs)
        b.put(count.toByte())
        for (i in 0 until count) {
            b.put(ids[i].toByte()); b.putShort(xs[i].toShort()); b.putShort(ys[i].toShort())
        }
        return b.array()
    }

    fun pong(hostNs: Long, devNs: Long): ByteArray {
        val b = msg(PONG, 16)
        b.putLong(hostNs); b.putLong(devNs)
        return b.array()
    }

    /** Timeline of one frame on the tablet: received, queued into the decoder, decoded, shown. */
    fun frameShown(captureNs: Long, recvNs: Long, renderNs: Long, queuedNs: Long, decodedNs: Long): ByteArray {
        val b = msg(FRAME_SHOWN, 40)
        b.putLong(captureNs); b.putLong(recvNs); b.putLong(renderNs)
        b.putLong(queuedNs); b.putLong(decodedNs)
        return b.array()
    }

    /** Display rotation, natural display size and the video rect (current orientation, px). */
    fun layout(rotation: Int, naturalW: Int, naturalH: Int, l: Int, t: Int, r: Int, b: Int): ByteArray {
        val m = msg(LAYOUT, 1 + 2 * 6)
        m.put(rotation.toByte())
        m.putShort(naturalW.toShort()); m.putShort(naturalH.toShort())
        m.putShort(l.toShort()); m.putShort(t.toShort()); m.putShort(r.toShort()); m.putShort(b.toShort())
        return m.array()
    }

    /** "WAYLUSYN" + nonce; the host echoes it back (host/src/proto.rs SYNC_MAGIC). */
    fun sync(nonce: Long): ByteArray {
        val m = msg(SYNC, 16)
        m.put("WAYLUSYN".toByteArray(Charsets.US_ASCII))
        m.putLong(nonce)
        return m.array()
    }

    /** A framed message with the given payload parts. */
    fun frame(type: Int, vararg parts: ByteArray): ByteArray {
        val b = msg(type, parts.sumOf { it.size })
        for (p in parts) b.put(p)
        return b.array()
    }

    fun frameSkipped(captureNs: Long): ByteArray = msg(FRAME_SKIPPED, 8).putLong(captureNs).array()

    fun wifiFrameLost(first: Long, last: Long): ByteArray = msg(WIFI_FRAME_LOST, 8).putInt(first.toInt()).putInt(last.toInt()).array()

    /** stats: VideoAssembler.takeStats() (9 values); see host proto::WifiStats for the order. */
    fun wifiStats(stats: IntArray, skipped: Int, freqMhz: Int, rssi: Int, linkMbps: Int): ByteArray {
        val b = msg(WIFI_STATS, 10 * 4 + 6)
        for (i in 0 until 6) b.putInt(stats[i])
        b.putInt(skipped)
        for (i in 6 until 9) b.putInt(stats[i])
        b.putShort(freqMhz.toShort()); b.putShort(rssi.toShort()); b.putShort(linkMbps.toShort())
        return b.array()
    }

    fun requestKeyframe(): ByteArray = msg(REQUEST_KEYFRAME, 0).array()

    class HostStats(val flags: Int, val stats: LiveStats.Host)

    /**
     * STATS payload: [u8 flags][u16 capture→shown p50][u16 p95][u16 pen→host p50][u16 p95]
     * [u16 held back ms/s], times in 0.1 ms, 0xFFFF = no data. Fields may be appended.
     */
    fun parseStats(p: ByteArray): HostStats? {
        if (p.isEmpty()) return null
        val b = le(p)
        val flags = b.get().toInt() and 0xff
        fun next(): Float? = if (b.remaining() >= 2) (b.getShort().toInt() and 0xffff).let { if (it == 0xffff) null else it / 10f } else null
        return HostStats(flags, LiveStats.Host(next(), next(), next(), next(), next()))
    }

    class Message(val type: Int, val payload: ByteArray)

    fun read(input: DataInputStream): Message {
        val type = input.readUnsignedByte()
        val len = Integer.reverseBytes(input.readInt())
        require(len in 0..(16 shl 20)) { "bad message length $len" }
        val payload = ByteArray(len)
        input.readFully(payload)
        return Message(type, payload)
    }

    fun le(payload: ByteArray): ByteBuffer = ByteBuffer.wrap(payload).order(ByteOrder.LITTLE_ENDIAN)
}
