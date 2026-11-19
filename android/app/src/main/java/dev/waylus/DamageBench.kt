// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.PixelFormat
import android.hardware.HardwareBuffer
import android.media.ImageReader
import android.opengl.EGL14
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.os.Process
import android.util.Log
import android.view.SurfaceControl
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.FrameLayout
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Developer benchmark for docs/damage-path.md, phase 1: what it costs the tablet to put a
 * raw screen patch on the glass without the video decoder. Started with
 * `am start -n de.hut9.waylus/dev.waylus.MainActivity --es bench_damage LABEL`; results go
 * to logcat (tag waylus, lines starting with "dbench"). LABEL names the run (say "idle" or
 * "input"); with `--ez bench_boost true` the app's own clock boost ([ClockBoost]) runs.
 *
 * Measures, per patch size (64², 256², 512², 1920×1080):
 *  1. LZ4 decompression of screen-like content (strokes on white, text, noise);
 *  2. glTexSubImage2D into a 1920×1080 RGBA texture, call alone and with glFinish;
 *  3. drawing that texture over a full 1920×1080 buffer (glFinish);
 *  4. the whole path as a prototype would run it: decompress, upload, draw into a buffer
 *     from an ImageReader, hand it to SurfaceFlinger on a child SurfaceControl above a
 *     SurfaceView, and wait for SurfaceFlinger's latch time and the present fence.
 */
class DamageBench(private val activity: Activity, private val root: FrameLayout, private val label: String, private val boost: Boolean) {
    private val w = 1920
    private val h = 1080

    fun start() {
        val sv = SurfaceView(activity)
        sv.setZOrderOnTop(true)
        sv.holder.setFormat(PixelFormat.TRANSPARENT)
        sv.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                Thread({
                    Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_DISPLAY)
                    val clock = if (boost) ClockBoost(120_000, 1).also { it.poke() } else null
                    try {
                        Thread.sleep(1500) // let the launch boost and the boost governor settle
                        run(sv)
                    } catch (e: Throwable) {
                        Log.e(TAG, "dbench failed", e)
                    } finally {
                        clock?.shutdown()
                        activity.runOnUiThread { root.removeView(sv) }
                        Log.i(TAG, "dbench done")
                    }
                }, "waylus-dbench").start()
            }
            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}
            override fun surfaceDestroyed(holder: SurfaceHolder) {}
        })
        root.addView(sv, FrameLayout.LayoutParams(-1, -1))
    }

    private fun log(s: String) = Log.i(TAG, "dbench [$label${if (boost) "+boost" else ""}] $s")

    private class Stat(val name: String) {
        val v = ArrayList<Double>()
        fun add(ns: Long) { v.add(ns / 1e6) }
        fun p(q: Double): Double { val s = v.sorted(); return if (s.isEmpty()) Double.NaN else s[((s.size - 1) * q).toInt()] }
        override fun toString() = String.format(java.util.Locale.ROOT, "%s p50 %.3f / p95 %.3f ms (n=%d)", name, p(0.5), p(0.95), v.size)
    }

    // --- test content -------------------------------------------------------------------

    /** A 1920×1080 RGBA picture: dark anti-aliased strokes on white, like a drawing app. */
    private fun strokes(): ByteArray {
        val bm = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        val c = Canvas(bm)
        c.drawColor(Color.WHITE)
        val p = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeCap = Paint.Cap.ROUND }
        val r = java.util.Random(7)
        repeat(60) {
            p.strokeWidth = 2f + r.nextInt(30)
            p.color = Color.argb(255, r.nextInt(80), r.nextInt(80), r.nextInt(80))
            val path = android.graphics.Path()
            path.moveTo(r.nextFloat() * w, r.nextFloat() * h)
            repeat(4) { path.quadTo(r.nextFloat() * w, r.nextFloat() * h, r.nextFloat() * w, r.nextFloat() * h) }
            c.drawPath(path, p)
        }
        return pixels(bm)
    }

    /** A page of text, like a browser or terminal. */
    private fun text(): ByteArray {
        val bm = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        val c = Canvas(bm)
        c.drawColor(Color.WHITE)
        val p = Paint(Paint.ANTI_ALIAS_FLAG).apply { textSize = 18f; color = Color.rgb(30, 30, 30) }
        val words = "the quick brown fox jumps over a lazy dog latency decoder pen display tablet frame".split(' ')
        val r = java.util.Random(3)
        var y = 24f
        while (y < h) {
            val sb = StringBuilder()
            while (sb.length < 160) sb.append(words[r.nextInt(words.size)]).append(' ')
            c.drawText(sb.toString(), 20f, y, p)
            y += 26f
        }
        return pixels(bm)
    }

    private fun noise(): ByteArray = ByteArray(w * h * 4).also { java.util.Random(5).nextBytes(it) }

    private fun pixels(bm: Bitmap): ByteArray {
        val b = ByteBuffer.allocate(bm.byteCount)
        bm.copyPixelsToBuffer(b)
        bm.recycle()
        return b.array()
    }

    /** The `s`×`s` patch (or the whole frame for s = 0) at the centre of `img`, rows packed. */
    private fun crop(img: ByteArray, s: Int): ByteArray {
        if (s == 0) return img
        val out = ByteArray(s * s * 4)
        val x0 = (w - s) / 2
        val y0 = (h - s) / 2
        for (y in 0 until s) System.arraycopy(img, ((y0 + y) * w + x0) * 4, out, y * s * 4, s * 4)
        return out
    }

    private val sizes = intArrayOf(64, 256, 512, 0)
    private fun dims(s: Int) = if (s == 0) w to h else s to s
    private fun sizeName(s: Int) = if (s == 0) "${w}x$h" else "${s}x$s"

    // --- the benchmark ------------------------------------------------------------------

    private fun run(sv: SurfaceView) {
        log("start: ${android.os.Build.MODEL}, ${android.os.Build.SOC_MODEL}, SDK ${android.os.Build.VERSION.SDK_INT}")
        val content = mapOf("strokes" to strokes(), "text" to text(), "noise" to noise())

        // 1. LZ4
        for ((name, img) in content) {
            for (s in sizes) {
                val raw = crop(img, s)
                val c = Lz4.compress(raw)
                val out = ByteArray(raw.size)
                val st = Stat("lz4 $name ${sizeName(s)}")
                val n = if (s == 0) 30 else 200
                repeat(n / 5) { Lz4.decompress(c, 0, c.size, out, 0, out.size) }
                repeat(n) {
                    val t = System.nanoTime()
                    Lz4.decompress(c, 0, c.size, out, 0, out.size)
                    st.add(System.nanoTime() - t)
                }
                log(String.format(java.util.Locale.ROOT, "%s, %d KB → %.1f KB (%.1f:1), %.0f MB/s", st, raw.size / 1024, c.size / 1024.0, raw.size.toDouble() / c.size, raw.size / 1e3 / st.p(0.5)))
            }
        }

        // 2.–4. GL
        val gl = SimpleGl()
        try {
            gl.init()
            log("GL: ${GLES20.glGetString(GLES20.GL_RENDERER)}, ${GLES20.glGetString(GLES20.GL_VERSION)}, BGRA upload ${gl.bgra}")
            val img = content.getValue("strokes")
            val tex = gl.texture(w, h)
            for (s in sizes) {
                val (pw, ph) = dims(s)
                val buf = ByteBuffer.allocateDirect(pw * ph * 4).order(ByteOrder.nativeOrder())
                buf.put(crop(img, s)).flip()
                val call = Stat("upload ${sizeName(s)} call")
                val fin = Stat("upload ${sizeName(s)} +glFinish")
                repeat(120) { i ->
                    val t = System.nanoTime()
                    GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, tex)
                    GLES20.glTexSubImage2D(GLES20.GL_TEXTURE_2D, 0, (w - pw) / 2, (h - ph) / 2, pw, ph, GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, buf)
                    val t1 = System.nanoTime()
                    GLES20.glFinish()
                    if (i >= 20) { call.add(t1 - t); fin.add(System.nanoTime() - t) }
                }
                log("$call; $fin")
            }
            // Draw the texture over a full buffer, as the overlay layer would.
            val fbo = gl.framebuffer(w, h)
            val draw = Stat("draw full-screen quad +glFinish")
            repeat(120) { i ->
                val t = System.nanoTime()
                GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, fbo)
                gl.drawTexture(tex, w, h)
                GLES20.glFinish()
                if (i >= 20) draw.add(System.nanoTime() - t)
            }
            log("$draw")
            GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)

            present(sv, gl, tex, img)
        } finally {
            gl.release()
        }
    }

    /** 4.: decompress + upload + draw + swap → SurfaceFlinger latch → present fence. */
    private fun present(sv: SurfaceView, gl: SimpleGl, tex: Int, img: ByteArray) {
        val ht = HandlerThread("dbench-reader", Process.THREAD_PRIORITY_URGENT_DISPLAY).apply { start() }
        val handler = Handler(ht.looper)
        val reader = ImageReader.newInstance(
            w, h, PixelFormat.RGBA_8888, 4,
            HardwareBuffer.USAGE_GPU_COLOR_OUTPUT or HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE or HardwareBuffer.USAGE_COMPOSER_OVERLAY,
        )
        val sc = SurfaceControl.Builder().setName("waylus-dbench").setParent(sv.surfaceControl)
            .setBufferSize(w, h).setOpaque(false).setHidden(false).build()
        SurfaceControl.Transaction().setLayer(sc, 1).setVisibility(sc, true).apply()
        val win = gl.windowSurface(reader.surface)
        try {
            for (s in sizes) {
                val (pw, ph) = dims(s)
                val raw = crop(img, s)
                val packed = Lz4.compress(raw)
                val out = ByteArray(raw.size)
                val buf = ByteBuffer.allocateDirect(raw.size).order(ByteOrder.nativeOrder())
                val work = Stat("${sizeName(s)} decompress+upload+draw+swap")
                val latch = Stat("swap→latch")
                val toLatch = Stat("start→latch")
                val shown = Stat("latch→present")
                val total = Stat("start→present")
                repeat(70) { i ->
                    // Random phase against vsync, as patches arrive whenever the host sends them.
                    Thread.sleep(25L + (i * 7919) % 17)
                    val t0 = System.nanoTime()
                    Lz4.decompress(packed, 0, packed.size, out, 0, out.size)
                    buf.clear(); buf.put(out).flip()
                    GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, tex)
                    GLES20.glTexSubImage2D(GLES20.GL_TEXTURE_2D, 0, (w - pw) / 2, (h - ph) / 2, pw, ph, GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, buf)
                    gl.drawTexture(tex, w, h)
                    val done = CountDownLatch(1)
                    var latchNs = 0L
                    var presentNs = 0L
                    reader.setOnImageAvailableListener({ r ->
                        val image = r.acquireNextImage() ?: return@setOnImageAvailableListener
                        val hb = image.hardwareBuffer!!
                        SurfaceControl.Transaction()
                            .setBuffer(sc, hb, image.fence) { f -> f.awaitForever(); hb.close(); image.close() }
                            .addTransactionCompletedListener(Runnable::run) { st ->
                                latchNs = st.latchTimeNanos
                                val pf = st.presentFence
                                if (pf.isValid && pf.awaitForever()) presentNs = pf.signalTime
                                pf.close()
                                done.countDown()
                            }
                            .apply()
                    }, handler)
                    val tSwap = System.nanoTime()
                    EGL14.eglSwapBuffers(gl.display, win)
                    val tAfter = System.nanoTime()
                    if (!done.await(500, TimeUnit.MILLISECONDS)) { log("frame $i: no completion"); return@repeat }
                    if (i < 10) return@repeat
                    work.add(tAfter - t0)
                    if (latchNs > 0) { latch.add(latchNs - tSwap); toLatch.add(latchNs - t0) }
                    if (presentNs > 0 && latchNs > 0) { shown.add(presentNs - latchNs); total.add(presentNs - t0) }
                }
                log("$work; $latch; $toLatch; $shown; $total")
            }
        } finally {
            gl.releaseSurface(win)
            SurfaceControl.Transaction().reparent(sc, null).apply()
            sc.release()
            reader.close()
            ht.quitSafely()
        }
    }

    companion object {
        private const val TAG = "waylus"
    }
}
