// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Handler
import android.os.HandlerThread
import android.os.Process
import android.util.Log
import android.view.Surface

/**
 * Hardware H.264 decoding straight into a SurfaceView surface. Runs MediaCodec
 * in async mode on its own high-priority thread; every decoded frame is
 * released for display immediately (no A/V-sync style scheduling).
 */
class VideoDecoder(
    private val info: Info,
    private val onShown: (captureNs: Long, recvNs: Long, renderNs: Long, queuedNs: Long, decodedNs: Long) -> Unit,
    private val onNeedKeyframe: () -> Unit,
    /** A frame was dropped while waiting for a keyframe (releases the host's back-pressure). */
    private val onSkipped: (captureNs: Long) -> Unit = {},
) {
    data class Info(
        val name: String,
        val maxW: Int,
        val maxH: Int,
        val mbPerFrame: Int,
        val mbPerSec: Int,
        val lowLatency: Boolean,
    )

    private class Pkt(val data: ByteArray, val off: Int, val ptsUs: Long, val flags: Int, val captureNs: Long, val recvNs: Long)

    private val thread = HandlerThread("waylus-decoder", Process.THREAD_PRIORITY_URGENT_DISPLAY).apply { start() }
    private val handler = Handler(thread.looper)

    // All fields below are only touched on the decoder thread.
    private var codec: MediaCodec? = null
    private var surface: Surface? = null
    private var width = 0
    private var height = 0
    private var csd: ByteArray? = null
    private var waitingKey = true
    private var lastKeyRequestNs = 0L
    private val pending = ArrayDeque<Pkt>()
    private val freeInputs = ArrayDeque<Int>()
    private val inflight = HashMap<Long, LongArray>()
    private var loggedVendorParams = false
    private var tweaks = 0
    // Stuck-decoder watchdog: frames fed in since the last decoded frame came out.
    private var inputsSinceOutput = 0
    private var firstInputSinceOutputNs = 0L
    private var lastOutputNs = 0L

    fun setSurface(s: Surface?) = handler.post {
        if (s === surface && codec != null) return@post
        surface = s
        restart()
    }

    /** Runs [r] on the decoder thread after everything queued before it. */
    fun post(r: () -> Unit) = handler.post(r)

    fun configure(w: Int, h: Int, flags: Int = 0) = handler.post {
        width = w
        height = h
        tweaks = flags
        csd = null
        release()
    }

    fun submit(captureNs: Long, flags: Int, data: ByteArray, off: Int, recvNs: Long) = handler.post {
        if (flags and Proto.VIDEO_CONFIG != 0) {
            csd = data.copyOfRange(off, data.size)
            restart()
            return@post
        }
        val c = codec ?: return@post
        val key = flags and Proto.VIDEO_KEY != 0
        if (waitingKey && !key) {
            onSkipped(captureNs)
            // The keyframe request (or the keyframe) may have been lost.
            if (System.nanoTime() - lastKeyRequestNs > KEY_RETRY_NS) requestKeyframe()
            return@post
        }
        waitingKey = false
        if (stuck()) return@post
        val bufFlags = if (key) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
        pending.addLast(Pkt(data, off, captureNs / 1000, bufFlags, captureNs, recvNs))
        drain(c)
    }

    /** Frames were lost on the network: show nothing new until a keyframe arrives. */
    fun dropUntilKey() = handler.post {
        if (!waitingKey) {
            waitingKey = true
            lastKeyRequestNs = System.nanoTime() // the host already forces one on the loss report
        }
    }

    /**
     * MediaTek's decoder (MT8781) can stop producing output for good while still taking
     * input: the video daemon logs "un-support type of NAL (12)" for every frame from then
     * on. Seen on a nearly static screen at idle clocks, with and without --fast-patches.
     * A new codec instance and a keyframe recover it; without this the picture stays frozen.
     */
    private fun stuck(): Boolean {
        if (inputsSinceOutput < STUCK_FRAMES) return false
        val now = System.nanoTime()
        if (now - firstInputSinceOutputNs < STUCK_NS) return false
        Log.w(TAG, "decoder stuck: no output for ${(now - lastOutputNs) / 1_000_000} ms after $inputsSinceOutput frames; restarting it")
        restart()
        return true
    }

    private fun requestKeyframe() {
        lastKeyRequestNs = System.nanoTime()
        onNeedKeyframe()
    }

    fun release() {
        val c = codec ?: return
        codec = null
        try { c.stop() } catch (_: Exception) {}
        try { c.release() } catch (_: Exception) {}
        pending.clear()
        freeInputs.clear()
        inflight.clear()
        inputsSinceOutput = 0
    }

    fun shutdown() {
        handler.post { release() }
        thread.quitSafely()
    }

    private fun restart() {
        release()
        val s = surface ?: return
        val config = csd ?: return
        if (width == 0 || height == 0 || !s.isValid) return
        try {
            val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            format.setInteger(MediaFormat.KEY_PRIORITY, 0) // realtime
            format.setInteger(MediaFormat.KEY_OPERATING_RATE, 120)
            format.setInteger(MediaFormat.KEY_MAX_WIDTH, width)
            format.setInteger(MediaFormat.KEY_MAX_HEIGHT, height)
            val c = MediaCodec.createByCodecName(info.name)
            c.setCallback(callback, handler)
            c.setOnFrameRenderedListener({ _, ptsUs, nanoTime -> frameRendered(ptsUs, nanoTime) }, handler)
            if (tweaks != 0) applyVendorTweaks(format)
            applyDebugParams(format)
            c.configure(format, s, null, 0)
            if (android.os.Build.VERSION.SDK_INT >= 31 && !loggedVendorParams) {
                loggedVendorParams = true
                runCatching { c.supportedVendorParameters }.onSuccess { Log.i(TAG, "decoder vendor parameters: $it") }
            }
            c.start()
            codec = c
            pending.addLast(Pkt(config, 0, 0, MediaCodec.BUFFER_FLAG_CODEC_CONFIG, 0, 0))
            waitingKey = true
            Log.i(TAG, "decoder ${info.name} started ${width}x$height (low-latency feature: ${info.lowLatency})")
            requestKeyframe()
        } catch (e: Exception) {
            Log.e(TAG, "decoder start failed", e)
            release()
        }
    }

    /** Opt-in MediaTek knobs (Proto.TWEAK_*). Unknown keys are ignored by other decoders. */
    private fun applyVendorTweaks(format: MediaFormat) {
        if (tweaks and Proto.TWEAK_MTK_VPP_OFF != 0) {
            // Skip MediaTek's video post-processing (scaler, quality tuner, motion interpolation).
            format.setInteger("vendor.mtk.ext.vdec.vpp.disabled.value", 1)
        }
        if (tweaks and Proto.TWEAK_MTK_CPU_BOOST != 0) {
            format.setInteger("vendor.mtk.vdec.cpu.boost.mode.value", 1)
        }
        Log.i(TAG, "vendor tweaks enabled: $tweaks")
    }

    /**
     * Developer hook for decoder experiments, not a feature:
     * `adb shell setprop debug.waylus.vdec "key=int;key=int"` sets those integer keys on the
     * next decoder start. Empty by default.
     */
    private fun applyDebugParams(format: MediaFormat) {
        val v = debugProp("debug.waylus.vdec")
        for (kv in v.split(';').map { it.trim() }.filter { it.isNotEmpty() }) {
            val (k, num) = kv.split('=', limit = 2).let { it[0].trim() to it.getOrNull(1)?.trim()?.toIntOrNull() }
            if (num == null) { Log.w(TAG, "debug.waylus.vdec: bad entry '$kv'"); continue }
            format.setInteger(k, num)
            Log.i(TAG, "debug decoder param $k=$num")
        }
    }

    private fun drain(c: MediaCodec) {
        while (pending.isNotEmpty() && freeInputs.isNotEmpty()) {
            val p = pending.removeFirst()
            val idx = freeInputs.removeFirst()
            val buf = c.getInputBuffer(idx) ?: continue
            val len = p.data.size - p.off
            buf.clear()
            buf.put(p.data, p.off, len)
            if (p.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                // capture, received, queued into the codec, decoded (filled in later)
                val now = System.nanoTime()
                inflight[p.ptsUs] = longArrayOf(p.captureNs, p.recvNs, now, 0)
                if (inputsSinceOutput++ == 0) firstInputSinceOutputNs = now
            }
            c.queueInputBuffer(idx, 0, len, p.ptsUs, p.flags)
        }
    }

    private fun frameRendered(ptsUs: Long, nanoTime: Long) {
        val v = inflight.remove(ptsUs) ?: return
        // Some vendor codecs (MediaTek c2) report pts*1000 instead of a render time;
        // fall back to the callback time, which follows the actual latch closely.
        val now = System.nanoTime()
        val renderNs = if (kotlin.math.abs(now - nanoTime) < 1_000_000_000L) nanoTime else now
        // Forget anything older (frames the decoder dropped).
        if (inflight.size > 8) inflight.keys.removeAll { it < ptsUs }
        onShown(v[0], v[1], renderNs, v[2], v[3])
    }

    private val callback = object : MediaCodec.Callback() {
        override fun onInputBufferAvailable(c: MediaCodec, index: Int) {
            if (c !== codec) return
            freeInputs.addLast(index)
            drain(c)
        }

        override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            if (c !== codec) return
            lastOutputNs = System.nanoTime()
            inputsSinceOutput = 0
            inflight[info.presentationTimeUs]?.let { it[3] = lastOutputNs }
            c.releaseOutputBuffer(index, true)
        }

        override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {
            if (c !== codec) return
            Log.e(TAG, "decoder error", e)
            restart()
        }

        override fun onOutputFormatChanged(c: MediaCodec, format: MediaFormat) {
            Log.i(TAG, "decoder output format $format")
        }
    }

    companion object {
        private const val TAG = "waylus"

        @android.annotation.SuppressLint("PrivateApi")
        private fun debugProp(name: String): String = runCatching {
            Class.forName("android.os.SystemProperties").getMethod("get", String::class.java)
                .invoke(null, name) as String
        }.getOrDefault("")
        private const val KEY_RETRY_NS = 300_000_000L
        private const val STUCK_NS = 500_000_000L
        private const val STUCK_FRAMES = 5

        /** Picks the best hardware AVC decoder and reports its limits. */
        fun pick(): Info {
            val mime = MediaFormat.MIMETYPE_VIDEO_AVC
            val all = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.filter {
                !it.isEncoder && it.supportedTypes.any { t -> t.equals(mime, true) }
            }
            val best = all.firstOrNull { it.isHardwareAccelerated && !it.name.contains("secure") }
                ?: all.first()
            val caps = best.getCapabilitiesForType(mime)
            val v = checkNotNull(caps.videoCapabilities) { "${best.name} has no video capabilities" }
            val maxW = v.supportedWidths.upper
            val maxHForW = runCatching { v.getSupportedHeightsFor(maxW).upper }.getOrDefault(v.supportedHeights.upper)
            val mbFrame = (maxW / 16) * (maxHForW / 16)
            // Macroblock throughput: the best of what the codec claims at 1080p and at its
            // largest size (rounded, so 60.0 fps * 14400 MB doesn't become 863999).
            val fps1080 = runCatching { v.getSupportedFrameRatesFor(1920, 1080).upper }.getOrDefault(60.0)
            val fpsMax = runCatching { v.getSupportedFrameRatesFor(maxW, maxHForW).upper }.getOrDefault(0.0)
            val mbSec = maxOf(Math.round(fps1080 * 120 * 68), Math.round(fpsMax * mbFrame)).toInt()
            val ll = caps.isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency)
            return Info(best.name, maxW, maxHForW, mbFrame, mbSec, ll)
        }
    }
}
