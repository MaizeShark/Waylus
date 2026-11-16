// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import java.io.DataInputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Wire protocol, mirrors host/src/proto.rs. Frames: [u8 type][u32 len LE][payload]. */
object Proto {
    const val VERSION = 4
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

    fun hello(surfaceW: Int, surfaceH: Int, maxW: Int, maxH: Int, mbFrame: Int, mbSec: Int, refreshHz: Float, decoder: String): ByteArray {
        val name = decoder.toByteArray().take(255).toByteArray()
        val b = msg(HELLO, 2 + 2 + 2 + 2 + 2 + 4 + 4 + 2 + 1 + name.size)
        b.putShort(VERSION.toShort())
        b.putShort(surfaceW.toShort()); b.putShort(surfaceH.toShort())
        b.putShort(maxW.toShort()); b.putShort(maxH.toShort())
        b.putInt(mbFrame); b.putInt(mbSec)
        b.putShort((refreshHz * 100).toInt().coerceIn(0, 65535).toShort())
        b.put(name.size.toByte()); b.put(name)
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

    fun requestKeyframe(): ByteArray = msg(REQUEST_KEYFRAME, 0).array()

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
