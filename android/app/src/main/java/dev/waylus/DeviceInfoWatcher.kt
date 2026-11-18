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
        onChange(
            DeviceInfo(
                wifiConnected = wifiNet != null,
                addresses = addrs,
                freqMhz = info?.frequency?.takeIf { it > 0 } ?: 0,
                rssi = info?.rssi?.takeIf { it in -126..-1 } ?: 0,
                linkMbps = info?.linkSpeed?.takeIf { it > 0 } ?: 0,
                usbConnected = usbConnected,
                usbFunctions = usbFunctions,
                accessoryAttached = Accessory.find(app) != null,
                adbEnabled = global(Settings.Global.ADB_ENABLED),
                devOptions = global(Settings.Global.DEVELOPMENT_SETTINGS_ENABLED),
                wirelessAdb = global(ADB_WIFI),
            ),
        )
    }

    companion object {
        /** Settings.Global.ADB_WIFI_ENABLED (hidden); unreadable on some versions. */
        private const val ADB_WIFI = "adb_wifi_enabled"
    }
}
