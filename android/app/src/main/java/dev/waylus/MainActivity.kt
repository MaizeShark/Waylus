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
import android.text.InputType
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

class MainActivity : Activity(), Connection.Listener, SurfaceHolder.Callback {
    private lateinit var root: Root
    private lateinit var surfaceView: SurfaceView
    private lateinit var frame: View
    private lateinit var status: TextView
    private lateinit var conn: Connection
    private var port = Proto.DEFAULT_PORT
    private var usingAccessory = false
    private var usingWifi = false
    private lateinit var wifiStore: WifiStore
    private lateinit var wifiPanel: LinearLayout
    private lateinit var wifiMsg: TextView
    private lateinit var wifiAddr: EditText
    private lateinit var pairButton: Button
    private lateinit var pinRow: LinearLayout
    private lateinit var hostsRow: LinearLayout
    private lateinit var discovery: WifiDiscovery
    private lateinit var pinEdit: EditText
    private val pinQueue = LinkedBlockingQueue<String>()
    /** Keeps the pairing result readable for a while instead of the connection status. */
    @Volatile private var pairNoteUntil = 0L
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
            onSkipped = { conn.send(Proto.frameSkipped(it)) },
        )
        input = InputCapture({ conn.send(it) }) { videoRect }
        Log.i(TAG, "decoder: $decoderInfo, port $port")
        wifiStore = WifiStore(this)
        discovery = WifiDiscovery(this) { onHostsChanged() }
        WifiPower.init(this)
        WifiPower.enabled = intent.getBooleanExtra("wifi_lock", true)
        buildWifiPanel()
        if (intent.getBooleanExtra("bench_crypto", false)) {
            Thread({
                WifiCrypto.bench()
                android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
                WifiCrypto.bench()
            }, "waylus-bench").start()
        }
        intent.getStringExtra("host")?.let { wifiStore.address = "$it:$port" }
        startConnection(pickTransport(intent))
    }

    private enum class Transport { USB, ADB, WIFI, AUTO }

    /**
     * What the host asked for (started over adb), else the USB accessory when we were
     * opened by it or it is attached (USB wins whenever a cable is connected), else Wi-Fi.
     */
    private fun pickTransport(i: Intent): Transport = when (i.getStringExtra("transport")) {
        "aoa" -> Transport.USB
        "tcp" -> Transport.ADB
        "wifi" -> Transport.WIFI
        else -> when {
            i.action == UsbManager.ACTION_USB_ACCESSORY_ATTACHED -> Transport.USB
            // Still in accessory mode, but maybe from an earlier session: try USB, then Wi-Fi.
            Accessory.find(this) != null -> Transport.AUTO
            else -> Transport.WIFI
        }
    }

    private fun startConnection(t: Transport) {
        usingAccessory = t == Transport.USB || t == Transport.AUTO
        usingWifi = t == Transport.WIFI || t == Transport.AUTO
        Log.i(TAG, "transport: $t")
        var usbTurn = true
        val opener: () -> Connection.Link? = when (t) {
            Transport.USB -> ({ Accessory.open(applicationContext) })
            Transport.ADB -> Connection.tcp(port)
            Transport.WIFI -> ::openWifi
            // USB first when a host reads it (a blocked probe switches over later), else Wi-Fi.
            Transport.AUTO -> ({
                usbTurn = !usbTurn
                val usb = if (usbTurn) null else Accessory.open(applicationContext)?.takeIf {
                    Accessory.probe(1500) { runOnUiThread { conn.stop(); startConnection(Transport.USB) } }
                }
                usb ?: openWifi()
            })
        }
        if (usingWifi) discovery.start() else {
            discovery.stop()
            runOnUiThread { wifiPanel.visibility = View.GONE }
        }
        conn = Connection(opener, this, syncGiveUpMs = if (t == Transport.AUTO) 3000 else 0)
        conn.start()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // The host switched the tablet into accessory mode while we were on TCP or Wi-Fi.
        if (intent.action == UsbManager.ACTION_USB_ACCESSORY_ATTACHED && !usingAccessory) {
            conn.stop()
            startConnection(Transport.USB)
        }
    }

    override fun onResume() {
        super.onResume()
        WifiPower.foreground(true)
    }

    // --- Wi-Fi ---

    /** Connection opener on the network thread: connect to the paired host, or show the pairing panel. */
    private var lastWifiStatus = ""
    private fun openWifi(): Connection.Link? {
        val host = wifiStore.host()
        // Where mDNS last saw the paired host, else the stored address.
        val addr = host?.let { discovery.hosts[WifiStore.toHex(it.id)]?.address } ?: wifiStore.address
        if (host == null || addr.isNullOrBlank()) {
            showWifiPanel(null)
            Thread.sleep(700)
            return null
        }
        return try {
            WifiLink.connect(wifiStore, host, WifiLink.parseAddress(addr, port))
        } catch (e: WifiLink.AuthFailed) {
            if (e.code == Proto.FAIL_UNKNOWN_DEVICE) {
                wifiStore.forgetPairing()
                showWifiPanel("${host.name} no longer knows this tablet. Pair again: run `waylus --wifi --pair` on it.")
            } else {
                showWifiPanel("${host.name}: ${e.message}")
            }
            Thread.sleep(1000)
            null
        } catch (e: InterruptedException) {
            throw e
        } catch (e: Exception) {
            val msg = "connecting to ${host.name} at $addr… (${e.message ?: e.javaClass.simpleName})"
            if (msg != lastWifiStatus) Log.i(TAG, msg)
            lastWifiStatus = msg
            showWifiPanel(msg)
            Thread.sleep(700)
            null
        }
    }

    /** mDNS results changed: follow the paired host's address, list the others for pairing. */
    private fun onHostsChanged() {
        val paired = wifiStore.host()?.let { WifiStore.toHex(it.id) }
        discovery.hosts[paired]?.let { h ->
            if (h.address != wifiStore.address) {
                Log.i(TAG, "paired host ${h.name} is now at ${h.address}")
                wifiStore.address = h.address
            }
        }
        runOnUiThread {
            hostsRow.removeAllViews()
            for (h in discovery.hosts.values.sortedBy { it.name }) {
                if (h.id == paired) continue
                hostsRow.addView(Button(this).apply {
                    text = "Pair with ${h.name}"
                    isAllCaps = false
                    setOnClickListener { wifiAddr.setText(h.address); startPairing() }
                })
            }
        }
    }

    private fun showWifiPanel(msg: String?) = runOnUiThread {
        if (!usingWifi || conn.connected) return@runOnUiThread
        val host = wifiStore.host()
        if (wifiAddr.text.isNullOrBlank()) wifiAddr.setText(wifiStore.address ?: "")
        if (pinRow.visibility != View.VISIBLE && System.nanoTime() > pairNoteUntil) {
            wifiMsg.text = msg ?: if (host != null) "Wi-Fi: paired with ${host.name}" else
                "Wi-Fi: not paired. On the computer run `waylus --wifi --pair`, then enter its address here."
        }
        wifiPanel.visibility = View.VISIBLE
    }

    private fun buildWifiPanel() {
        val dp = resources.displayMetrics.density
        wifiMsg = TextView(this).apply { setTextColor(0xFFB0B0B0.toInt()); textSize = 15f; gravity = Gravity.CENTER }
        wifiAddr = EditText(this).apply {
            hint = "host address, e.g. 192.168.1.20"
            setSingleLine()
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
            imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_ACTION_DONE
            setTextColor(Color.WHITE); setHintTextColor(0xFF707070.toInt())
            minWidth = (320 * dp).toInt()
        }
        pairButton = Button(this).apply { text = "Pair with this host"; setOnClickListener { startPairing() } }
        pinEdit = EditText(this).apply {
            hint = "PIN"
            setSingleLine()
            inputType = InputType.TYPE_CLASS_NUMBER
            imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_ACTION_DONE
            setTextColor(Color.WHITE); setHintTextColor(0xFF707070.toInt())
            minWidth = (160 * dp).toInt()
            setOnEditorActionListener { _, _, _ -> submitPin(); true }
        }
        pinRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            addView(pinEdit)
            addView(Button(this@MainActivity).apply { text = "OK"; setOnClickListener { submitPin() } })
            visibility = View.GONE
        }
        wifiPanel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding((16 * dp).toInt(), (8 * dp).toInt(), (16 * dp).toInt(), (8 * dp).toInt())
            hostsRow = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.HORIZONTAL }
            addView(wifiMsg); addView(hostsRow); addView(wifiAddr); addView(pairButton); addView(pinRow)
            visibility = View.GONE
        }
        root.addView(wifiPanel, FrameLayout.LayoutParams(-2, -2, Gravity.CENTER_HORIZONTAL or Gravity.BOTTOM).apply {
            bottomMargin = (24 * dp).toInt()
        })
    }

    private fun submitPin() {
        pinQueue.offer(pinEdit.text.toString())
        pinEdit.setText("")
        getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(pinEdit.windowToken, 0)
    }

    private fun startPairing() {
        val addr = wifiAddr.text.toString().trim()
        if (addr.isEmpty()) return
        wifiStore.address = addr
        pairButton.isEnabled = false
        wifiMsg.text = "asking $addr to pair…"
        pinQueue.clear()
        Thread({
            val result = try {
                val name = WifiLink.pair(wifiStore, WifiLink.parseAddress(addr, port)) { hostName ->
                    runOnUiThread {
                        wifiMsg.text = "Enter the PIN shown on $hostName"
                        pinRow.visibility = View.VISIBLE
                        pinEdit.requestFocus()
                        getSystemService(InputMethodManager::class.java).showSoftInput(pinEdit, 0)
                    }
                    pinQueue.poll(170, TimeUnit.SECONDS)
                }
                "Paired with $name. Connecting…"
            } catch (e: WifiLink.AuthFailed) {
                when (e.code) {
                    Proto.FAIL_PAIRING_OFF -> "Pairing is off on the host. Run `waylus --wifi --pair` there."
                    Proto.FAIL_WRONG_PIN -> "Wrong PIN. Try again; the host shows a new PIN."
                    else -> "Pairing failed: ${e.message}"
                }
            } catch (e: Exception) {
                "Pairing failed: ${e.message ?: e.javaClass.simpleName}"
            }
            pairNoteUntil = System.nanoTime() + 15_000_000_000L
            runOnUiThread {
                wifiMsg.text = result
                pinRow.visibility = View.GONE
                pairButton.isEnabled = true
            }
        }, "waylus-pair").start()
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
        WifiPower.foreground(false)
        boost.idle()
        super.onPause()
    }

    override fun onDestroy() {
        discovery.stop()
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
        if (usingWifi) discovery.stop()
        runOnUiThread {
            wifiPanel.visibility = View.GONE
            setStatus("Waylus\nconnected" + if (usingWifi) " (Wi-Fi)" else "")
        }
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
        if (usingWifi) discovery.start()
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

    override fun onVideoLost() {
        decoder.dropUntilKey()
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
