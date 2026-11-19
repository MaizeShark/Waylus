// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.graphics.PixelFormat
import android.opengl.EGL14
import android.opengl.EGLSurface
import android.opengl.GLES20
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Process
import android.util.Log
import android.view.Choreographer
import android.view.Surface
import android.view.SurfaceControl
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Fast patches (experimental, docs/damage-path.md, design A): raw pieces of the screen
 * that the host sends next to the video while the pen moves (Proto.PATCH). They are drawn
 * on a transparent layer above the video SurfaceView, a child SurfaceControl of it, and
 * so reach the screen without waiting for the hardware decoder. Once the decoder has
 * shown a frame at least as new as a patch, the patch is removed again: the video stays
 * the ground truth, and a lost or late patch costs nothing.
 *
 * Nothing is set up until the first patch arrives. The layer keeps a canvas texture the
 * size of the video frame (transparent where there is no patch) and the capture time
 * of the patch in each 16×16 tile. GL draws the canvas straight into the layer's surface
 * (the same buffer path a SurfaceView has), scaled to the size the video has on screen:
 * a layer the compositor has to scale went to GPU composition on the NXTPAPER 14, which
 * cost a frame. Half a second after the last patch is gone the layer is hidden, so the
 * video is composited as without patches.
 *
 * Needs Android 13.
 */
class PatchOverlay(
    /** A patch was drawn and handed to the compositor (latchNs: 0, not known here). Overlay thread. */
    private val onShown: (captureNs: Long, recvNs: Long, swapNs: Long, latchNs: Long) -> Unit,
) {
    private val thread = HandlerThread("waylus-patches", Process.THREAD_PRIORITY_URGENT_DISPLAY).apply { start() }
    private val handler = Handler(thread.looper)

    // Overlay thread only.
    private var parent: SurfaceControl? = null
    private var viewW = 0
    private var viewH = 0
    private var bufW = 0
    private var bufH = 0
    private var frameW = 0
    private var frameH = 0
    private var gl: SimpleGl? = null
    private var layer: SurfaceControl? = null
    private var surface: Surface? = null
    private var win: EGLSurface? = null
    private var canvas = 0
    private var canvasFbo = 0
    private var stamps = LongArray(0)
    private var tilesW = 0
    private var visible = false
    /** Some tile holds a patch. */
    private var active = false
    /** Bumped on every patch, so a pending hide knows it is stale. */
    private var generation = 0
    private var lastVideoNs = 0L
    private var pixels = ByteArray(0)
    private var upload: ByteBuffer = ByteBuffer.allocateDirect(0)

    /** The video SurfaceView's SurfaceControl and its size on screen (null: surface gone). */
    fun setParent(sc: SurfaceControl?, w: Int, h: Int) = handler.post {
        if (sc === parent && w == viewW && h == viewH) return@post
        parent = sc
        viewW = w
        viewH = h
        release() // set up again on the next patch
    }

    /** The video's frame size (from CONFIG); 0 stops the overlay. */
    fun configure(w: Int, h: Int) = handler.post {
        if (w == frameW && h == frameH) return@post
        frameW = w
        frameH = h
        lastVideoNs = 0
        release()
    }

    /** Runs [r] on the overlay thread after everything queued before it. */
    fun post(r: () -> Unit) = handler.post(r)

    fun shutdown() {
        handler.post { release() }
        thread.quitSafely()
    }

    /** A PATCH message. */
    fun submit(payload: ByteArray, recvNs: Long) = handler.post { draw(payload, recvNs) }

    /**
     * The decoder showed the frame captured at [captureNs]: patches up to that age go away.
     * Two vsyncs later, so the video frame is surely latched before its patch disappears
     * (on some decoders the "shown" callback comes when the frame is queued, not latched).
     */
    fun videoShown(captureNs: Long) = handler.post {
        if (captureNs <= lastVideoNs) return@post
        lastVideoNs = captureNs
        if (!active) return@post
        val c = Choreographer.getInstance()
        c.postFrameCallback { c.postFrameCallback { clearUpTo(captureNs) } }
    }

    private fun rebuild() {
        release()
        val p = parent ?: return
        if (frameW <= 0 || frameH <= 0 || viewW <= 0 || viewH <= 0) return
        try {
            // Buffers at the size the video has on screen. With the frame size and a scale
            // instead, the NXTPAPER 14 composited the layer on the GPU (a frame later); at the
            // screen size it goes on a hardware plane like the video. (Pre-rotating to the
            // panel isn't possible here: buffers queued through a Surface get their
            // producer's transform, none for EGL, whatever the SurfaceControl was given.)
            bufW = viewW
            bufH = viewH
            val sc = SurfaceControl.Builder().setName("waylus-patches").setParent(p)
                .setBufferSize(bufW, bufH).setFormat(PixelFormat.RGBA_8888).setOpaque(false).setHidden(true).build()
            layer = sc
            SurfaceControl.Transaction()
                .setLayer(sc, 1)
                .setPosition(sc, 0f, 0f)
                .apply()
            val s = Surface(sc)
            surface = s
            val g = SimpleGl().also { it.init() }
            gl = g
            win = g.windowSurface(s)
            EGL14.eglSwapInterval(g.display, 0)
            canvas = g.texture(frameW, frameH)
            // Scaled up like the video (bilinear), not in blocks.
            GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
            GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
            canvasFbo = g.framebufferFor(canvas)
            GLES20.glClearColor(0f, 0f, 0f, 0f)
            GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)
            GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
            tilesW = (frameW + TILE - 1) / TILE
            stamps = LongArray(tilesW * ((frameH + TILE - 1) / TILE))
            visible = false
            active = false
            Log.i(TAG, "patch overlay ${frameW}x$frameH on a ${viewW}x$viewH video")
        } catch (e: Exception) {
            Log.e(TAG, "patch overlay failed", e)
            release()
        }
    }

    private fun release() {
        val g = gl
        if (g != null) {
            win?.let { g.releaseSurface(it) }
            if (canvasFbo != 0) g.deleteFramebuffer(canvasFbo)
            if (canvas != 0) g.deleteTexture(canvas)
            g.release()
        }
        surface?.release()
        layer?.let { SurfaceControl.Transaction().reparent(it, null).apply(); it.release() }
        gl = null; win = null; layer = null; surface = null
        canvas = 0; canvasFbo = 0
        visible = false
        active = false
    }

    private fun prop(name: String) = runCatching {
        Class.forName("android.os.SystemProperties").getMethod("get", String::class.java).invoke(null, name) as String
    }.getOrDefault("")

    /** `adb shell setprop debug.waylus.patchlog 1`: per-patch timings in logcat. */
    private val debugLog = prop("debug.waylus.patchlog") == "1"
    /** `adb shell setprop debug.waylus.patchtint 1`: patches drawn reddish, to see where they are. */
    private val tint = if (prop("debug.waylus.patchtint") == "1") floatArrayOf(1f, 0.4f, 0.4f, 1f) else SimpleGl.NO_TINT

    private fun draw(payload: ByteArray, recvNs: Long) {
        val tStart = System.nanoTime()
        // Nothing exists until the first patch: without --fast-patches the app does as before.
        if (gl == null) rebuild()
        val g = gl ?: return
        val b = Proto.le(payload)
        val captureNs = b.getLong()
        val w = b.getShort().toInt() and 0xffff
        val h = b.getShort().toInt() and 0xffff
        val flags = b.get().toInt() and 0xff
        val n = b.get().toInt() and 0xff
        // Older than what the video already shows, or for another frame size: useless.
        if (captureNs <= lastVideoNs || w != frameW || h != frameH) return
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, canvas)
        GLES20.glPixelStorei(GLES20.GL_UNPACK_ALIGNMENT, 4)
        try {
            repeat(n) {
                val x = b.getShort().toInt() and 0xffff
                val y = b.getShort().toInt() and 0xffff
                val rw = b.getShort().toInt() and 0xffff
                val rh = b.getShort().toInt() and 0xffff
                val len = b.getInt()
                val off = b.position()
                b.position(off + len)
                if (x + rw > frameW || y + rh > frameH || rw == 0 || rh == 0) return@repeat
                val size = rw * rh * 4
                if (pixels.size < size) pixels = ByteArray(size)
                if (flags and 1 != 0) {
                    if (Lz4.decompress(payload, off, len, pixels, 0, size) != size) return@repeat
                } else {
                    if (len != size) return@repeat
                    System.arraycopy(payload, off, pixels, 0, size)
                }
                if (upload.capacity() < size) upload = ByteBuffer.allocateDirect(size).order(ByteOrder.nativeOrder())
                upload.clear()
                upload.put(pixels, 0, size).flip()
                GLES20.glTexSubImage2D(GLES20.GL_TEXTURE_2D, 0, x, y, rw, rh, GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, upload)
                for (ty in y / TILE until (y + rh + TILE - 1) / TILE) {
                    for (tx in x / TILE until (x + rw + TILE - 1) / TILE) {
                        val i = ty * tilesW + tx
                        if (stamps[i] < captureNs) stamps[i] = captureNs
                    }
                }
            }
        } catch (e: RuntimeException) {
            Log.w(TAG, "bad patch: $e")
            return
        }
        if (prop("debug.waylus.patchtest") == "1") testPattern()
        active = true
        generation++
        val sc = layer
        if (!visible && sc != null) {
            // Hidden only half a second after it went empty, so the buffer it shows meanwhile is empty.
            SurfaceControl.Transaction().setVisibility(sc, true).apply()
            visible = true
        }
        val tUp = System.nanoTime()
        present(g)
        val tSwap = System.nanoTime()
        if (debugLog) Log.i(TAG, String.format(java.util.Locale.ROOT, "patchlog queue %.2f upload %.2f draw+swap %.2f ms, %d rects",
            (tStart - recvNs) / 1e6, (tUp - tStart) / 1e6, (tSwap - tUp) / 1e6, n))
        onShown(captureNs, recvNs, tSwap, 0)
    }

    /** Debug (`setprop debug.waylus.patchtest 1`): a red block at the top left of the frame,
     *  a blue one at the bottom right, that never go away; for checking the orientation. */
    private fun testPattern() {
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, canvasFbo)
        GLES20.glEnable(GLES20.GL_SCISSOR_TEST)
        GLES20.glScissor(64, 64, 256, 128)
        GLES20.glClearColor(1f, 0f, 0f, 1f)
        GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)
        GLES20.glScissor(frameW - 64 - 256, frameH - 64 - 128, 256, 128)
        GLES20.glClearColor(0f, 0f, 1f, 1f)
        GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)
        GLES20.glDisable(GLES20.GL_SCISSOR_TEST)
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
        for (i in stamps.indices) if (stamps[i] == 0L) stamps[i] = Long.MAX_VALUE
    }

    /** Removes the patches of frames up to [captureNs] (the video shows them now). */
    private fun clearUpTo(captureNs: Long) {
        val g = gl ?: return
        var cleared = false
        var any = false
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, canvasFbo)
        GLES20.glEnable(GLES20.GL_SCISSOR_TEST)
        GLES20.glClearColor(0f, 0f, 0f, 0f)
        val rows = stamps.size / tilesW
        for (ty in 0 until rows) {
            var tx = 0
            while (tx < tilesW) {
                val s = stamps[ty * tilesW + tx]
                if (s == 0L || s > captureNs) {
                    if (s != 0L) any = true
                    tx++
                    continue
                }
                val x0 = tx
                while (tx < tilesW && stamps[ty * tilesW + tx].let { it != 0L && it <= captureNs }) {
                    stamps[ty * tilesW + tx] = 0
                    tx++
                }
                GLES20.glScissor(x0 * TILE, ty * TILE, (tx - x0) * TILE, TILE)
                GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)
                cleared = true
            }
        }
        GLES20.glDisable(GLES20.GL_SCISSOR_TEST)
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
        active = any
        if (!cleared) return
        present(g)
        if (!active) {
            val gen = generation
            handler.postDelayed({ if (gen == generation && !active && visible) hide() }, HIDE_AFTER_MS)
        }
    }

    private fun hide() {
        val sc = layer ?: return
        SurfaceControl.Transaction().setVisibility(sc, false).apply()
        visible = false
    }

    /**
     * Draws the canvas into the layer's next buffer and queues it. The surface is in async
     * mode (swap interval 0): a buffer queued before the compositor took the previous one
     * replaces it, and swapping never waits. The canvas always holds the newest state, so
     * the newest buffer is the right one.
     */
    private fun present(g: SimpleGl) {
        val s = win ?: return
        // Only the tiles with a patch need drawing (the rest of the canvas is transparent):
        // clear the buffer, draw their bounding box. Much less work for the GPU than the
        // whole screen, which slowed the video decoder down measurably.
        var x0 = Int.MAX_VALUE; var y0 = Int.MAX_VALUE; var x1 = -1; var y1 = -1
        for (i in stamps.indices) {
            if (stamps[i] == 0L) continue
            val tx = i % tilesW; val ty = i / tilesW
            if (tx < x0) x0 = tx; if (tx > x1) x1 = tx
            if (ty < y0) y0 = ty; if (ty > y1) y1 = ty
        }
        if (x1 < 0) {
            g.drawTexture(canvas, bufW, bufH, tint = tint, scissor = intArrayOf(0, 0, 0, 0))
        } else {
            // Frame tiles → buffer pixels, a pixel of margin for the bilinear scaling.
            val sx = bufW.toFloat() / frameW; val sy = bufH.toFloat() / frameH
            val l = maxOf(0, (x0 * TILE * sx).toInt() - 2)
            val r = minOf(bufW, ((x1 + 1) * TILE * sx).toInt() + 2)
            val t = maxOf(0, (y0 * TILE * sy).toInt() - 2)
            val b = minOf(bufH, ((y1 + 1) * TILE * sy).toInt() + 2)
            g.drawTexture(canvas, bufW, bufH, tint = tint, scissor = intArrayOf(l, bufH - b, r - l, b - t))
        }
        EGL14.eglSwapBuffers(g.display, s)
    }

    companion object {
        private const val TAG = "waylus"
        private const val TILE = 16
        private const val HIDE_AFTER_MS = 500L

        /** Whether this device can show patches (Proto.CAP_PATCHES). */
        val supported get() = Build.VERSION.SDK_INT >= 33
    }
}
