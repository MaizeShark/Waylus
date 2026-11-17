// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbManager
import android.graphics.Color
import android.graphics.RectF
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.util.Log
import android.view.Gravity
import android.view.InputDevice
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class MainActivity : Activity(), Connection.Listener, SurfaceHolder.Callback {
    private lateinit var root: Root
    private lateinit var surfaceView: SurfaceView
    private lateinit var frame: View
    private lateinit var status: TextView
    private lateinit var conn: Connection
    private var port = Proto.DEFAULT_PORT
    private var usingAccessory = false
    private lateinit var decoder: VideoDecoder
    private lateinit var input: InputCapture
    private lateinit var boost: ClockBoost
    private val decoderInfo by lazy { VideoDecoder.pick() }

    /** Where the video is drawn, in root-view coordinates. Read from the UI thread only. */
    private val videoRect = RectF()
    private var videoW = 16
    private var videoH = 9
    @Volatile private var firstFrameShown = false
    /** Refresh rate of the display mode we asked for (the highest available). */
    @Volatile private var refreshHz = 60f

    /** Full-screen container that receives all pointer events, unbatched. */
    inner class Root(ctx: Context) : FrameLayout(ctx) {
        override fun onTouchEvent(e: MotionEvent): Boolean {
            boost.poke()
            return input.onEvent(e) || super.onTouchEvent(e)
        }
        override fun onGenericMotionEvent(e: MotionEvent): Boolean {
            boost.poke()
            return input.onEvent(e) || super.onGenericMotionEvent(e)
        }
        override fun onLayout(changed: Boolean, l: Int, t: Int, r: Int, b: Int) {
            super.onLayout(changed, l, t, r, b)
            if (changed) post { relayout() }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        // Generic Android API: ask for stable (not bursty) CPU/GPU clocks. Decode latency on
        // some SoCs follows the CPU clock, so frequency drops show up as decode spikes.
        val pm = getSystemService(android.os.PowerManager::class.java)
        if (pm.isSustainedPerformanceModeSupported) window.setSustainedPerformanceMode(true)
        android.util.Log.i("waylus", "sustained performance mode supported: ${pm.isSustainedPerformanceModeSupported}")
        window.attributes.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        port = intent.getIntExtra("port", Proto.DEFAULT_PORT)
        boost = ClockBoost(
            intent.getIntExtra("boost_secs", 20) * 1000L,
            intent.getIntExtra("boost_threads", 1).coerceIn(1, 4),
        )
        pickFastestMode()

        root = Root(this).apply { setBackgroundColor(Color.BLACK) }
        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        root.addView(surfaceView, FrameLayout.LayoutParams(1, 1, Gravity.CENTER))
        frame = View(this).apply {
            background = GradientDrawable().apply { setStroke(2, 0xFF3A3A3A.toInt()) }
        }
        root.addView(frame, FrameLayout.LayoutParams(1, 1, Gravity.CENTER))
        status = TextView(this).apply {
            setTextColor(0xFFB0B0B0.toInt())
            textSize = 18f
            gravity = Gravity.CENTER
        }
        root.addView(status, FrameLayout.LayoutParams(-1, -1))
        setContentView(root)
        setStatus("Waylus\nwaiting for host…\n\nrun `waylus` on your computer")

        // Deliver every pen/touch sample as it arrives instead of batching to vsync.
        root.requestUnbufferedDispatch(InputDevice.SOURCE_STYLUS or InputDevice.SOURCE_TOUCHSCREEN)

        decoder = VideoDecoder(
            decoderInfo,
            onShown = { c, r, n, q, d ->
                conn.send(Proto.frameShown(c, r, n, q, d))
                if (!firstFrameShown) {
                    firstFrameShown = true
                    runOnUiThread { status.visibility = View.GONE; frame.visibility = View.GONE }
                }
            },
            onNeedKeyframe = { conn.send(Proto.requestKeyframe()) },
        )
        input = InputCapture({ conn.send(it) }) { videoRect }
        Log.i(TAG, "decoder: $decoderInfo, port $port")
        startConnection(wantsAccessory(intent))
    }

    /**
     * USB accessory (AOA) when the host asked for it, or when we were opened by the
     * accessory / a Waylus accessory is attached; otherwise TCP through `adb reverse`.
     */
    private fun wantsAccessory(i: Intent): Boolean = when (i.getStringExtra("transport")) {
        "aoa" -> true
        "tcp" -> false
        else -> i.action == UsbManager.ACTION_USB_ACCESSORY_ATTACHED || Accessory.find(this) != null
    }

    private fun startConnection(accessory: Boolean) {
        usingAccessory = accessory
        Log.i(TAG, "transport: ${if (accessory) "USB accessory" else "TCP port $port"}")
        val opener: () -> Connection.Link? = if (accessory) ({ Accessory.open(applicationContext) }) else Connection.tcp(port)
        conn = Connection(opener, this)
        conn.start()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // The host switched the tablet into accessory mode while we were on TCP.
        if (intent.action == UsbManager.ACTION_USB_ACCESSORY_ATTACHED && !usingAccessory) {
            conn.stop()
            startConnection(true)
        }
    }

    /** Ask for the highest refresh rate at the current resolution (e.g. 120/144 Hz tablets). */
    private fun pickFastestMode() {
        val d = display ?: return
        val cur = d.mode
        val best = d.supportedModes
            .filter { it.physicalWidth == cur.physicalWidth && it.physicalHeight == cur.physicalHeight }
            .maxByOrNull { it.refreshRate } ?: cur
        refreshHz = best.refreshRate
        if (best.modeId != cur.modeId) {
            window.attributes = window.attributes.apply { preferredDisplayModeId = best.modeId }
        }
        Log.i(TAG, "display mode ${best.physicalWidth}x${best.physicalHeight} @ ${best.refreshRate} Hz")
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) {
            window.insetsController?.let {
                it.hide(WindowInsets.Type.systemBars())
                it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            }
        } else {
            input.reset()
        }
    }

    override fun onPause() {
        boost.idle()
        super.onPause()
    }

    override fun onDestroy() {
        boost.shutdown()
        conn.stop()
        decoder.shutdown()
        super.onDestroy()
    }

    private fun setStatus(text: String) {
        status.text = text
        status.visibility = View.VISIBLE
    }

    private fun relayout() {
        val sw = root.width.toFloat()
        val sh = root.height.toFloat()
        if (sw <= 0 || sh <= 0) return
        val s = minOf(sw / videoW, sh / videoH)
        val w = videoW * s
        val h = videoH * s
        videoRect.set((sw - w) / 2, (sh - h) / 2, (sw + w) / 2, (sh + h) / 2)
        for (v in listOf(surfaceView, frame)) {
            v.layoutParams = FrameLayout.LayoutParams(w.toInt(), h.toInt(), Gravity.CENTER)
        }
        sendLayout()
    }

    /** Lets the host map raw stylus coordinates (read over adb) into the video rect. */
    private fun sendLayout() {
        val d = display ?: return
        val loc = IntArray(2)
        root.getLocationOnScreen(loc)
        val r = videoRect
        conn.send(Proto.layout(
            d.rotation, d.mode.physicalWidth, d.mode.physicalHeight,
            (r.left + loc[0]).toInt(), (r.top + loc[1]).toInt(), (r.right + loc[0]).toInt(), (r.bottom + loc[1]).toInt(),
        ))
    }

    // --- Connection.Listener (network thread) ---

    override fun onConnected() {
        runOnUiThread { setStatus("Waylus\nconnected") }
    }

    override fun hello(): ByteArray {
        val bounds = windowManager.currentWindowMetrics.bounds
        val w = maxOf(bounds.width(), bounds.height())
        val h = minOf(bounds.width(), bounds.height())
        val d = decoderInfo
        return Proto.hello(w, h, d.maxW, d.maxH, d.mbPerFrame, d.mbPerSec, refreshHz, d.name + if (d.lowLatency) " (low-latency)" else "", pxPerMm())
    }

    /**
     * Physical panel density in px/mm, or 0 if unknown. Vendors sometimes report nonsense
     * (the NXTPAPER 14 claims 134.5 x 303.6 dpi for a ~202 dpi panel), so only trust
     * xdpi/ydpi when they roughly agree: real panels have square pixels. densityDpi is a
     * UI scale bucket, not a physical value, so it's no fallback.
     */
    private fun pxPerMm(): Float {
        val m = resources.displayMetrics
        val square = m.xdpi > 0 && m.ydpi > 0 && maxOf(m.xdpi, m.ydpi) / minOf(m.xdpi, m.ydpi) < 1.15f
        val dpi = (m.xdpi + m.ydpi) / 2
        return if (square && dpi in 80f..1000f) dpi / 25.4f else 0f
    }

    override fun onDisconnected() {
        firstFrameShown = false
        decoder.configure(0, 0)
        runOnUiThread {
            input.reset()
            frame.visibility = View.VISIBLE
            setStatus("Waylus\nwaiting for host…")
        }
    }

    override fun onConfig(codec: Int, width: Int, height: Int, flags: Int) {
        runOnUiThread {
            videoW = width.coerceAtLeast(1)
            videoH = height.coerceAtLeast(1)
            relayout()
            if (codec == Proto.CODEC_NONE) {
                frame.visibility = View.VISIBLE
                setStatus("Waylus — input only\n${width}×$height")
            }
        }
        if (codec == Proto.CODEC_H264) {
            firstFrameShown = false
            decoder.configure(width, height, flags)
        }
    }

    override fun onVideo(captureNs: Long, flags: Int, data: ByteArray, offset: Int, recvNs: Long) {
        decoder.submit(captureNs, flags, data, offset, recvNs)
    }

    // --- SurfaceHolder.Callback ---

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        // Tell the system our content rate so variable-refresh panels stay at full speed.
        runCatching { holder.surface.setFrameRate(refreshHz, android.view.Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE) }
        decoder.setSurface(holder.surface)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        // The codec must let go of the surface before this returns.
        val done = CountDownLatch(1)
        decoder.setSurface(null)
        decoder.post { done.countDown() }
        done.await(500, TimeUnit.MILLISECONDS)
    }

    companion object { private const val TAG = "waylus" }
}
