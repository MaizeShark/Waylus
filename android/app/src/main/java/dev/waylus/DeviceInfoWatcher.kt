// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.database.ContentObserver
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiInfo
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import dev.waylus.ui.DeviceInfo
import java.net.Inet4Address
import java.net.Inet6Address

/**
 * Read-only device facts for the idle screen: Wi-Fi (addresses, band, signal), USB and adb.
 * Needs no permissions beyond normal ones (ACCESS_NETWORK_STATE, ACCESS_WIFI_STATE); no
 * location permission, so no SSID. Updated by callbacks while [start]ed (the idle screen is
 * visible), never polled; stopped while streaming.
 */
class DeviceInfoWatcher(ctx: Context, private val onChange: (DeviceInfo) -> Unit) {
    private val app = ctx.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private val cm = app.getSystemService(ConnectivityManager::class.java)
    private val wm = app.getSystemService(WifiManager::class.java)
    private var running = false

    // Latest values (main thread).
    private var wifiNet: Network? = null
    private var wifiInfo: WifiInfo? = null
    private var link: LinkProperties? = null
    private var usbConnected: Boolean? = null
    private var usbFunctions = ""

    private val netCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
            wifiNet = network
            wifiInfo = caps.transportInfo as? WifiInfo
            publish()
        }
        override fun onLinkPropertiesChanged(network: Network, lp: LinkProperties) {
            wifiNet = network
            link = lp
            publish()
        }
        override fun onLost(network: Network) {
            if (network == wifiNet) { wifiNet = null; wifiInfo = null; link = null; publish() }
        }
    }

    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(c: Context, i: Intent) {
            usbConnected = i.getBooleanExtra("connected", false)
            usbFunctions = listOf("mtp", "ptp", "rndis", "midi", "accessory", "adb")
                .filter { i.getBooleanExtra(it, false) }.joinToString(", ")
            publish()
        }
    }

    private val settingsObserver = object : ContentObserver(main) {
        override fun onChange(selfChange: Boolean) = publish()
    }

    fun start() {
        if (running) return
        running = true
        runCatching {
            cm.registerNetworkCallback(NetworkRequest.Builder().addTransportType(NetworkCapabilities.TRANSPORT_WIFI).build(), netCallback, main)
        }
        // Sticky: delivers the current state right away.
        runCatching { app.registerReceiver(usbReceiver, IntentFilter("android.hardware.usb.action.USB_STATE"), Context.RECEIVER_EXPORTED) }
        for (k in listOf(Settings.Global.ADB_ENABLED, Settings.Global.DEVELOPMENT_SETTINGS_ENABLED, ADB_WIFI)) {
            runCatching { app.contentResolver.registerContentObserver(Settings.Global.getUriFor(k), false, settingsObserver) }
        }
        publish()
    }

    fun stop() {
        if (!running) return
        running = false
        runCatching { cm.unregisterNetworkCallback(netCallback) }
        runCatching { app.unregisterReceiver(usbReceiver) }
        runCatching { app.contentResolver.unregisterContentObserver(settingsObserver) }
    }

    /** Re-reads what has no callback (onResume). */
    fun refresh() { if (running) publish() }

    private fun global(key: String): Boolean? =
        runCatching { Settings.Global.getInt(app.contentResolver, key) != 0 }.getOrNull()

    private class Adb(val usb: Boolean?, val wireless: Boolean?, val devOptions: Boolean?, val hidden: Boolean)

    /**
     * USB/wireless debugging and developer options. Android 17 hides these settings from
     * apps (they read as 0 whatever they are), so the system properties of the adb daemon
     * come first: `sys.usb.config` lists adb while USB debugging is on, and
     * `service.adb.tls.port` is set while wireless debugging runs. They are read with the
     * `getprop` tool, which works from the app sandbox. A 0 from the settings on Android 17
     * and later is reported as "can't tell" rather than "off". Worker thread.
     */
    private fun readAdb(): Adb {
        val props = runCatching {
            val p = ProcessBuilder("getprop").redirectErrorStream(true).start()
            val out = p.inputStream.bufferedReader().readText()
            p.waitFor()
            PROP.findAll(out).associate { it.groupValues[1] to it.groupValues[2] }
        }.getOrDefault(emptyMap())
        val hidden = android.os.Build.VERSION.SDK_INT >= 37
        fun merge(fromProps: Boolean, setting: Boolean?): Boolean? = when {
            fromProps || setting == true -> true
            hidden -> null
            else -> setting
        }
        val usb = merge(props["sys.usb.config"].orEmpty().split(',').contains("adb"), global(Settings.Global.ADB_ENABLED))
        val wireless = merge(props["service.adb.tls.port"].orEmpty().let { it.isNotEmpty() && it != "0" }, global(ADB_WIFI))
        // Debugging can only be on with developer options on.
        val dev = merge(usb == true || wireless == true, global(Settings.Global.DEVELOPMENT_SETTINGS_ENABLED))
        return Adb(usb, wireless, dev, hidden && (usb == null || wireless == null || dev == null))
    }

    private val worker = java.util.concurrent.Executors.newSingleThreadExecutor { r -> Thread(r, "waylus-devinfo").apply { isDaemon = true } }

    @Suppress("DEPRECATION")
    private fun publish() {
        if (!running) return
        // The callback's WifiInfo has frequency and RSSI without location permission; the
        // older WifiManager call is the fallback.
        val info = wifiInfo ?: if (wifiNet != null) runCatching { wm.connectionInfo }.getOrNull() else null
        val addrs = link?.linkAddresses.orEmpty().map { it.address }
            .filter { it is Inet4Address || (it is Inet6Address && !it.isLinkLocalAddress) }
            .sortedBy { if (it is Inet4Address) 0 else 1 }
            .mapNotNull { it.hostAddress }
        val base = DeviceInfo(
            wifiConnected = wifiNet != null,
            addresses = addrs,
            freqMhz = info?.frequency?.takeIf { it > 0 } ?: 0,
            rssi = info?.rssi?.takeIf { it in -126..-1 } ?: 0,
            linkMbps = info?.linkSpeed?.takeIf { it > 0 } ?: 0,
            usbConnected = usbConnected,
            usbFunctions = usbFunctions,
            accessoryAttached = Accessory.find(app) != null,
        )
        worker.execute {
            val a = readAdb()
            main.post {
                if (running) onChange(base.copy(adbEnabled = a.usb, wirelessAdb = a.wireless, devOptions = a.devOptions, adbHidden = a.hidden))
            }
        }
    }

    companion object {
        /** Settings.Global.ADB_WIFI_ENABLED (hidden); unreadable on some versions. */
        private const val ADB_WIFI = "adb_wifi_enabled"
        /** One line of `getprop` output: [key]: [value]. */
        private val PROP = Regex("""\[([^\]]+)\]: \[([^\]]*)\]""")
    }
}
