// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.graphics.Color
import android.graphics.RectF
import android.graphics.drawable.GradientDrawable
import android.hardware.usb.UsbManager
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import android.view.Gravity
import android.view.InputDevice
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewConfiguration
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.compose.ui.platform.ComposeView
import androidx.compose.ui.platform.ViewCompositionStrategy
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import dev.waylus.ui.Discovered
import dev.waylus.ui.Hud
import dev.waylus.ui.HudState
import dev.waylus.ui.IdleScreen
import dev.waylus.ui.PairedHost
import dev.waylus.ui.Pairing
import dev.waylus.ui.Phase
import dev.waylus.ui.TransportKind
import dev.waylus.ui.UiActions
import dev.waylus.ui.UiModel
import dev.waylus.ui.WaylusTheme
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Owns the connection and decoder lifecycle. The screens are Compose (ui/), reading
 * [UiModel]; the video is a plain SurfaceView whose Surface goes straight to MediaCodec.
 *
 * While a stream runs, the window shows the SurfaceView and nothing else: the idle UI's
 * ComposeView is GONE and its content removed, and the stats display (off by default) is
 * a small non-interactive ComposeView updated once a second. Pen and touch go to [Root]'s
 * onTouchEvent/onGenericMotionEvent, unbuffered, as before.
 */
class MainActivity : ComponentActivity(), Connection.Listener, SurfaceHolder.Callback, UiActions {
    private lateinit var root: Root
    private lateinit var surfaceView: SurfaceView
    private lateinit var frame: View
    /** "input only" label (`waylus --no-video`); a plain view so it can't take input. */
    private lateinit var inputOnlyLabel: TextView
    private lateinit var idleView: ComposeView
    private lateinit var hudView: ComposeView
    private lateinit var conn: Connection
    private var port = Proto.DEFAULT_PORT
    private var usingAccessory = false
    private var usingWifi = false
    private var transport = TransportKind.WIFI
    private lateinit var wifiStore: WifiStore
    private lateinit var discovery: WifiDiscovery
    private lateinit var deviceInfo: DeviceInfoWatcher
    private lateinit var prefs: SharedPreferences
    private val pinQueue = LinkedBlockingQueue<String>()
    @Volatile private var pairCancelled = false
    private lateinit var decoder: VideoDecoder
    /** Fast patches above the video (experimental, docs/damage-path.md); idle unless the host sends them. */
    private val patches = PatchOverlay { c, r, s, l -> conn.send(Proto.patchShown(c, r, s, l)) }
    private lateinit var input: InputCapture
    private lateinit var boost: ClockBoost
    private val decoderInfo by lazy { VideoDecoder.pick() }
    private val main = Handler(Looper.getMainLooper())

    private val ui = UiModel()
    private val hud = HudState()
    /** The host asked for the stats display (--hud, in its STATS messages) this session. */
    private var hostHud = false
    /** The user hid the host-requested stats display; stays hidden this session. */
    private var hostHudDismissed = false
    private var resumed = false

    /** Where the video is drawn, in root-view coordinates. Read from the UI thread only. */
    private val videoRect = RectF()
    private var videoW = 16
    private var videoH = 9
    @Volatile private var firstFrameShown = false
    @Volatile private var loggedStartup = false
    /** Refresh rate of the display mode we asked for (the highest available). */
    @Volatile private var refreshHz = 60f

    /** Full-screen container that receives all pointer events, unbatched. */
    inner class Root(ctx: Context) : FrameLayout(ctx) {
        override fun onTouchEvent(e: MotionEvent): Boolean {
            if (letterboxTap(e)) return true
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
        installSplashScreen()
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(SystemBarStyle.dark(Color.TRANSPARENT), SystemBarStyle.dark(Color.TRANSPARENT))
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        // Generic Android API: ask for stable (not bursty) CPU/GPU clocks. Decode latency on
        // some SoCs follows the CPU clock, so frequency drops show up as decode spikes.
        val pm = getSystemService(android.os.PowerManager::class.java)
        if (pm.isSustainedPerformanceModeSupported) window.setSustainedPerformanceMode(true)
        Log.i(TAG, "sustained performance mode supported: ${pm.isSustainedPerformanceModeSupported}")
        window.attributes.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        port = intent.getIntExtra("port", Proto.DEFAULT_PORT)
        boost = ClockBoost(
            intent.getIntExtra("boost_secs", 20) * 1000L,
            intent.getIntExtra("boost_threads", 1).coerceIn(1, 4),
        )
        pickFastestMode()
        prefs = getSharedPreferences("settings", MODE_PRIVATE)

        root = Root(this).apply { setBackgroundColor(Color.BLACK) }
        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        root.addView(surfaceView, FrameLayout.LayoutParams(1, 1, Gravity.CENTER))
        frame = View(this).apply {
            background = GradientDrawable().apply { setStroke(2, 0xFF5A6A72.toInt()) }
            visibility = View.GONE
        }
        root.addView(frame, FrameLayout.LayoutParams(1, 1, Gravity.CENTER))
        inputOnlyLabel = TextView(this).apply {
            setTextColor(0xFFE0ECF0.toInt())
            textSize = 18f
            gravity = Gravity.CENTER
            visibility = View.GONE
        }
        root.addView(inputOnlyLabel, FrameLayout.LayoutParams(-1, -1))
        idleView = ComposeView(this).apply {
            setViewCompositionStrategy(ViewCompositionStrategy.DisposeOnViewTreeLifecycleDestroyed)
            setContent {
                WaylusTheme {
                    // Out of the composition while streaming, not just hidden.
                    if (ui.phase != Phase.STREAMING && ui.phase != Phase.INPUT_ONLY) IdleScreen(ui, this@MainActivity)
                }
            }
        }
        root.addView(idleView, FrameLayout.LayoutParams(-1, -1))
        hudView = ComposeView(this).apply {
            setViewCompositionStrategy(ViewCompositionStrategy.DisposeOnViewTreeLifecycleDestroyed)
            setContent { Hud(hud) }
            visibility = View.GONE
        }
        root.addView(hudView, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.START))
        setContentView(root)

        // Deliver every pen/touch sample as it arrives instead of batching to vsync.
        root.requestUnbufferedDispatch(InputDevice.SOURCE_STYLUS or InputDevice.SOURCE_TOUCHSCREEN)

        decoder = VideoDecoder(
            decoderInfo,
            onShown = { c, r, n, q, d ->
                conn.send(Proto.frameShown(c, r, n, q, d))
                patches.videoShown(c)
                LiveStats.frameShown(q, d)
                if (!firstFrameShown) {
                    firstFrameShown = true
                    if (!loggedStartup) {
                        loggedStartup = true
                        val ms = android.os.SystemClock.uptimeMillis() - android.os.Process.getStartUptimeMillis()
                        Log.i(TAG, "startup: first video frame $ms ms after process start")
                    }
                    runOnUiThread { enterStream(Phase.STREAMING) }
                }
            },
            onNeedKeyframe = { conn.send(Proto.requestKeyframe()) },
            onSkipped = { conn.send(Proto.frameSkipped(it)) },
        )
        input = InputCapture({ conn.send(it) }) { videoRect }
        Log.i(TAG, "decoder: $decoderInfo, port $port")
        wifiStore = WifiStore(this)
        discovery = WifiDiscovery(this) { onHostsChanged() }
        deviceInfo = DeviceInfoWatcher(this) { ui.device = it }
        WifiPower.init(this)
        loadSettings(intent)
        if (intent.getBooleanExtra("bench_crypto", false)) {
            Thread({
                WifiCrypto.bench()
                android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
                WifiCrypto.bench()
            }, "waylus-bench").start()
        }
        // Developer benchmark for docs/damage-path.md (DamageBench.kt).
        intent.getStringExtra("bench_damage")?.let { DamageBench(this, root, it, intent.getBooleanExtra("bench_boost", false)).start() }
        intent.getStringExtra("host")?.let { wifiStore.address = "$it:$port" }
        ui.port = port
        ui.paired = pairedHost()
        startConnection(pickTransport(intent))
        showIdle()
    }

    /** App settings; intent extras from the host (started over adb) win for this run. */
    private fun loadSettings(i: Intent) {
        ui.everConnected = prefs.getBoolean("ever_connected", false)
        ui.wifiLockFromHost = i.hasExtra("wifi_lock")
        ui.wifiLock = if (ui.wifiLockFromHost) i.getBooleanExtra("wifi_lock", true) else prefs.getBoolean("wifi_lock", true)
        WifiPower.enabled = ui.wifiLock
        ui.hudFromHost = i.hasExtra("hud")
        ui.hudEnabled = if (ui.hudFromHost) i.getBooleanExtra("hud", false) else prefs.getBoolean("hud", false)
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
        usingAccessory = t == Transport.USB
        usingWifi = t == Transport.WIFI || t == Transport.AUTO
        transport = when (t) {
            Transport.USB -> TransportKind.USB
            Transport.ADB -> TransportKind.ADB
            Transport.WIFI -> TransportKind.WIFI
            Transport.AUTO -> TransportKind.AUTO
        }
        Log.i(TAG, "transport: $t")
        val opener: () -> Connection.Link? = when (t) {
            Transport.USB -> ({ Accessory.open(applicationContext) })
            Transport.ADB -> Connection.tcp(port)
            Transport.WIFI -> ::openWifi
            // Wi-Fi; a watcher switches to USB once a host answers there.
            Transport.AUTO -> ::openWifi
        }
        if (t == Transport.AUTO) watchUsb()
        runOnUiThread {
            ui.transport = transport
            ui.detail = null
            if (ui.phase != Phase.STREAMING && ui.phase != Phase.INPUT_ONLY) ui.phase = waitingPhase()
        }
        conn = Connection(opener, this)
        conn.start()
    }

    private fun waitingPhase() = when (transport) {
        TransportKind.USB -> Phase.WAITING_USB
        TransportKind.ADB -> Phase.ADB_CONNECTING
        TransportKind.WIFI, TransportKind.AUTO -> Phase.WIFI_SEARCHING
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
        resumed = true
        WifiPower.foreground(true)
        if (idleView.visibility == View.VISIBLE) { deviceInfo.start(); deviceInfo.refresh() }
    }

    override fun onPause() {
        resumed = false
        WifiPower.foreground(false)
        boost.idle()
        deviceInfo.stop()
        super.onPause()
    }

    // --- idle screen / stream ---

    /** Back to the idle screen (start, or the stream ended). Main thread. */
    private fun showIdle() {
        stopHud()
        frame.visibility = View.GONE
        inputOnlyLabel.visibility = View.GONE
        ui.phase = waitingPhase()
        // Visible and opaque: covers the last decoded picture, which the SurfaceView keeps.
        idleView.visibility = View.VISIBLE
        discovery.start()
        if (resumed) deviceInfo.start()
        applySystemBars()
    }

    /** First frame shown (or an input-only session): nothing on screen but the video. */
    private fun enterStream(p: Phase) {
        if (!conn.connected) return
        ui.phase = p
        idleView.visibility = View.GONE
        deviceInfo.stop()
        applySystemBars()
        startHudIfWanted()
    }

    private fun streaming() = ui.phase == Phase.STREAMING || ui.phase == Phase.INPUT_ONLY

    private fun applySystemBars() {
        window.insetsController?.let {
            it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            if (streaming()) it.hide(WindowInsets.Type.systemBars()) else it.show(WindowInsets.Type.systemBars())
        }
    }

    // --- stats display ---

    private fun hudOn() = ui.hudEnabled || hostHud

    private val hudTick = object : Runnable {
        override fun run() {
            updateHud()
            main.postDelayed(this, 1000)
        }
    }

    private fun startHudIfWanted() {
        if (!streaming() || !hudOn() || hudView.visibility == View.VISIBLE) return
        LiveStats.reset()
        LiveStats.enabled = true
        hud.lines = listOf("stats: waiting for data…")
        hudView.visibility = View.VISIBLE
        main.removeCallbacks(hudTick)
        main.postDelayed(hudTick, 1000)
    }

    private fun stopHud() {
        LiveStats.enabled = false
        main.removeCallbacks(hudTick)
        hudView.visibility = View.GONE
        hud.lines = emptyList()
    }

    private fun updateHud() {
        val now = System.nanoTime()
        val t = LiveStats.takeTablet(now)
        val h = LiveStats.host?.takeIf { now - LiveStats.hostAtNs < 3_000_000_000L }
        val l = java.util.Locale.ROOT
        fun ms(v: Float?) = v?.let { "%.1f".format(l, it) } ?: "–"
        val pen = if (h != null && LiveStats.hostFlags and Proto.STATS_RAW_PEN != 0) "pen: raw stylus" else "pen ${ms(h?.penP50)} ms"
        val lines = ArrayList<String>(4)
        if (ui.phase == Phase.STREAMING) {
            lines += "%.0f fps · shown p50 %s / p95 %s ms".format(l, t.fps, ms(h?.shownP50), ms(h?.shownP95))
            lines += "decode ${ms(t.decodeMs)} ms · $pen" +
                (h?.heldMsPerS?.takeIf { it >= 1f }?.let { " · held %.0f ms/s".format(l, it) } ?: "")
        } else {
            lines += if (pen.startsWith("pen:")) "input only · $pen" else "input only · pen p50 ${ms(h?.penP50)} / p95 ${ms(h?.penP95)} ms"
        }
        val w = if (usingWifi && !usingAccessory) LiveStats.wifi else null
        lines += when {
            w != null -> "Wi-Fi ${w.freqMhz} MHz ${w.rssi} dBm"
            else -> ui.transport.label
        }
        if (w != null) {
            val loss = if (w.shardsExpected > 0) 100f * w.shardsLost / w.shardsExpected else 0f
            lines += "loss %.1f%% · FEC %d · lost %d".format(l, loss, w.fecShards, w.framesLost)
        }
        if (h == null && ui.phase == Phase.STREAMING) lines += "(host sends no stats: older waylus?)"
        hud.lines = lines
    }

    private fun setHudOn(on: Boolean) {
        ui.hudEnabled = on
        if (!on && hostHud) { hostHud = false; hostHudDismissed = true }
        prefs.edit().putBoolean("hud", on).apply()
        if (on) startHudIfWanted() else stopHud()
    }

    private var tapDownX = 0f
    private var tapDownY = 0f
    private var tapDownT = 0L
    private var tapActive = false

    /**
     * A finger tap on the black bars beside the video toggles the stats display. Gestures
     * that start there never reach the host; everything over the video does, unchanged.
     */
    private fun letterboxTap(e: MotionEvent): Boolean {
        if (ui.phase != Phase.STREAMING) return false
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (e.getToolType(0) != MotionEvent.TOOL_TYPE_FINGER || videoRect.contains(e.x, e.y)) return false
                tapActive = true
                tapDownX = e.x; tapDownY = e.y; tapDownT = e.eventTime
                return true
            }
            MotionEvent.ACTION_UP -> {
                if (!tapActive) return false
                tapActive = false
                val slop = ViewConfiguration.get(this).scaledTouchSlop
                if (e.eventTime - tapDownT < 400 && kotlin.math.hypot(e.x - tapDownX, e.y - tapDownY) < slop) setHudOn(!hudOn())
                return true
            }
            MotionEvent.ACTION_CANCEL -> { val was = tapActive; tapActive = false; return was }
            else -> return tapActive
        }
    }

    // --- Wi-Fi ---

    @Volatile private var destroyed = false

    /**
     * AUTO: the tablet is still in USB accessory mode (maybe from an earlier session) but
     * no host may serve USB. Writes to the accessory block while no host reads it and
     * can't be cancelled, so they never happen on the connection thread: this watcher
     * sends a SYNC and switches to USB only when a host echoes it.
     */
    private fun watchUsb() {
        Thread({
            while (!destroyed && !usingAccessory) {
                val acc = Accessory.open(applicationContext)
                if (acc == null) { Thread.sleep(1000); continue }
                try {
                    val sync = Proto.sync(kotlin.random.Random.nextLong())
                    acc.drain()
                    acc.output.write(sync) // blocks until a host reads the pipe
                    if (acc.awaitSync(sync, 1500)) {
                        Log.i(TAG, "a host answers on USB; switching from Wi-Fi to USB")
                        runOnUiThread {
                            if (!destroyed && !usingAccessory) { conn.stop(); startConnection(Transport.USB) }
                        }
                        return@Thread
                    }
                } catch (e: Exception) {
                    Thread.sleep(1000)
                }
            }
        }, "waylus-usb-watch").apply { isDaemon = true; start() }
    }

    private fun pairedHost(): PairedHost? = wifiStore.host()?.let { PairedHost(it.name, it.address) }

    private fun setDetail(msg: String?) = runOnUiThread { ui.detail = msg }

    /** Connection opener on the network thread: connect to the paired host, or wait for pairing. */
    private var lastWifiStatus = ""
    private fun openWifi(): Connection.Link? {
        val host = wifiStore.host()
        // Where mDNS last saw the paired host, else the stored address.
        val addr = host?.let { discovery.hosts[WifiStore.toHex(it.id)]?.address } ?: wifiStore.address
        if (host == null || addr.isNullOrBlank()) {
            setDetail(if (host != null) "The address of ${host.name} isn't known yet; it shows up once `waylus --wifi` runs there." else null)
            Thread.sleep(700)
            return null
        }
        return try {
            WifiLink.connect(wifiStore, host, WifiLink.parseAddress(addr, port))
        } catch (e: WifiLink.AuthFailed) {
            if (e.code == Proto.FAIL_UNKNOWN_DEVICE) {
                wifiStore.forgetPairing()
                runOnUiThread { ui.paired = null }
                setDetail("${host.name} no longer knows this tablet. Pair again: run `waylus --wifi --pair` on it.")
            } else {
                setDetail("${host.name} refused the connection: ${e.message}")
            }
            Thread.sleep(1000)
            null
        } catch (e: InterruptedException) {
            throw e
        } catch (e: Exception) {
            val why = when {
                e is java.net.ConnectException && e.message?.contains("ECONNREFUSED") == true ->
                    "Waylus isn't running there; start `waylus --wifi`."
                e is java.net.SocketTimeoutException || e.message?.contains("EHOSTUNREACH") == true ->
                    "no answer. Is the computer on, and on the same network?"
                else -> e.message ?: e.javaClass.simpleName
            }
            val msg = "${host.name} at $addr: $why"
            if (msg != lastWifiStatus) Log.i(TAG, msg)
            lastWifiStatus = msg
            setDetail(msg)
            Thread.sleep(700)
            null
        }
    }

    /** mDNS results changed: follow the paired host's address, list the hosts for pairing. */
    private fun onHostsChanged() {
        val paired = wifiStore.host()?.let { WifiStore.toHex(it.id) }
        // (ConcurrentHashMap rejects a null key: unpaired tablets crashed here.)
        paired?.let { discovery.hosts[it] }?.let { h ->
            if (h.address != wifiStore.address) {
                Log.i(TAG, "paired host ${h.name} is now at ${h.address}")
                wifiStore.address = h.address
            }
        }
        val list = discovery.hosts.values.sortedBy { it.name }.map { Discovered(it.id, it.name, it.address, it.id == paired) }
        runOnUiThread {
            ui.discovered = list
            ui.paired = pairedHost()
        }
    }

    override fun pair(address: String) {
        if (ui.pairing is Pairing.Asking || ui.pairing is Pairing.EnterPin) return
        ui.pairing = Pairing.Asking(address)
        ui.tab = dev.waylus.ui.Tab.WIFI
        pinQueue.clear()
        pairCancelled = false
        Thread({
            var wrongPin: String? = null
            var result: Pairing
            while (true) {
                result = try {
                    val name = WifiLink.pair(wifiStore, WifiLink.parseAddress(address, port)) { hostName ->
                        val err = wrongPin
                        runOnUiThread { ui.pairing = Pairing.EnterPin(hostName, err) }
                        val pin = pinQueue.poll(170, TimeUnit.SECONDS)
                        if (pin == CANCEL) null else pin
                    }
                    Pairing.Done("Paired with $name. Connecting…", ok = true)
                } catch (e: WifiLink.AuthFailed) {
                    when (e.code) {
                        Proto.FAIL_WRONG_PIN -> {
                            // The host shows a new PIN for the next attempt (and closes pairing
                            // after a few wrong ones, which ends up in PAIRING_OFF below).
                            wrongPin = "Wrong PIN. The computer now shows a new one."
                            continue
                        }
                        Proto.FAIL_PAIRING_OFF ->
                            Pairing.Done("The computer refused: ${e.message?.takeIf { it.isNotBlank() } ?: "pairing is off"}. Run `waylus --wifi --pair` there.", ok = false)
                        else -> Pairing.Done("Pairing failed: ${e.message}", ok = false)
                    }
                } catch (e: Exception) {
                    if (pairCancelled) Pairing.Idle
                    else Pairing.Done("Pairing failed: ${connectError(e)}", ok = false)
                }
                break
            }
            runOnUiThread {
                ui.pairing = result
                ui.paired = pairedHost()
            }
        }, "waylus-pair").start()
    }

    private fun connectError(e: Exception) = when {
        e is java.net.ConnectException -> "nothing answers there. Is `waylus --wifi --pair` running?"
        e is java.net.SocketTimeoutException || e.message?.contains("EHOSTUNREACH") == true -> "no answer from that address."
        e is java.net.UnknownHostException -> "unknown address."
        else -> e.message ?: e.javaClass.simpleName
    }

    override fun submitPin(pin: String) {
        pinQueue.offer(pin)
    }

    override fun cancelPairing() {
        pairCancelled = true
        pinQueue.offer(CANCEL)
        ui.pairing = Pairing.Idle
    }

    override fun dismissPairingResult() {
        ui.pairing = Pairing.Idle
    }

    override fun forgetHost() {
        wifiStore.forgetPairing()
        ui.paired = null
        onHostsChanged()
    }

    override fun setHud(on: Boolean) = setHudOn(on)

    override fun setWifiLock(on: Boolean) {
        ui.wifiLock = on
        WifiPower.enabled = on
        prefs.edit().putBoolean("wifi_lock", on).apply()
    }

    override fun openUrl(url: String) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, "No browser found. Open $url on another device.", Toast.LENGTH_LONG).show()
        }
    }

    override fun openDeveloperOptions() {
        val action = if (ui.device.devOptions == false) Settings.ACTION_DEVICE_INFO_SETTINGS else Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS
        try {
            startActivity(Intent(action))
        } catch (_: ActivityNotFoundException) {
            startActivity(Intent(Settings.ACTION_SETTINGS))
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
        if (hasFocus) applySystemBars() else input.reset()
    }

    override fun onDestroy() {
        destroyed = true
        discovery.stop()
        deviceInfo.stop()
        stopHud()
        boost.shutdown()
        conn.stop()
        decoder.shutdown()
        patches.shutdown()
        super.onDestroy()
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

    override fun onLinkOpened() {
        if (usingAccessory) runOnUiThread { if (!streaming()) ui.phase = Phase.USB_ATTACHED }
    }

    override fun onConnected() {
        discovery.stop()
        if (!ui.everConnected) prefs.edit().putBoolean("ever_connected", true).apply()
        runOnUiThread {
            hostHud = false
            hostHudDismissed = false
            ui.everConnected = true
            ui.lastDisconnect = null
            ui.detail = null
            ui.phase = Phase.WAITING_FIRST_FRAME
        }
    }

    override fun hello(): ByteArray {
        val bounds = windowManager.currentWindowMetrics.bounds
        val w = maxOf(bounds.width(), bounds.height())
        val h = minOf(bounds.width(), bounds.height())
        val d = decoderInfo
        val caps = if (PatchOverlay.supported) Proto.CAP_PATCHES else 0
        return Proto.hello(w, h, d.maxW, d.maxH, d.mbPerFrame, d.mbPerSec, refreshHz, d.name + if (d.lowLatency) " (low-latency)" else "", pxPerMm(), caps)
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

    override fun onDisconnected(reason: String) {
        firstFrameShown = false
        decoder.configure(0, 0)
        patches.configure(0, 0)
        runOnUiThread {
            input.reset()
            hostHud = false
            ui.lastDisconnect = if (destroyed) null else reason
            showIdle()
        }
    }

    override fun onConfig(codec: Int, width: Int, height: Int, flags: Int) {
        runOnUiThread {
            videoW = width.coerceAtLeast(1)
            videoH = height.coerceAtLeast(1)
            relayout()
            if (codec == Proto.CODEC_NONE) {
                frame.visibility = View.VISIBLE
                inputOnlyLabel.text = "Waylus — input only\n${width}×$height"
                inputOnlyLabel.visibility = View.VISIBLE
                enterStream(Phase.INPUT_ONLY)
            }
        }
        if (codec == Proto.CODEC_H264) {
            firstFrameShown = false
            decoder.configure(width, height, flags)
        }
        patches.configure(if (codec == Proto.CODEC_H264) width else 0, if (codec == Proto.CODEC_H264) height else 0)
    }

    override fun onPatch(payload: ByteArray, recvNs: Long) {
        patches.submit(payload, recvNs)
    }

    override fun onVideo(captureNs: Long, flags: Int, data: ByteArray, offset: Int, recvNs: Long) {
        decoder.submit(captureNs, flags, data, offset, recvNs)
    }

    override fun onVideoLost() {
        decoder.dropUntilKey()
    }

    override fun onHostStats(payload: ByteArray) {
        val s = Proto.parseStats(payload) ?: return
        LiveStats.host = s.stats
        LiveStats.hostFlags = s.flags
        LiveStats.hostAtNs = System.nanoTime()
        if (s.flags and Proto.STATS_HUD != 0) {
            runOnUiThread {
                if (hostHud || hostHudDismissed) return@runOnUiThread
                hostHud = true
                ui.hudFromHost = true
                startHudIfWanted()
            }
        }
    }

    // --- SurfaceHolder.Callback ---

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        // Tell the system our content rate so variable-refresh panels stay at full speed.
        runCatching { holder.surface.setFrameRate(refreshHz, android.view.Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE) }
        decoder.setSurface(holder.surface)
        if (PatchOverlay.supported) patches.setParent(surfaceView.surfaceControl, width, height)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        // The codec must let go of the surface before this returns.
        val done = CountDownLatch(2)
        decoder.setSurface(null)
        decoder.post { done.countDown() }
        patches.setParent(null, 0, 0)
        patches.post { done.countDown() }
        done.await(500, TimeUnit.MILLISECONDS)
    }

    companion object {
        private const val TAG = "waylus"
        private const val CANCEL = "\u0000cancel"
    }
}
