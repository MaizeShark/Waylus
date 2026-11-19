// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** How the app talks to the host, or is trying to. */
enum class TransportKind(val label: String) {
    USB("USB accessory"),
    ADB("USB through adb"),
    WIFI("Wi-Fi"),
    /** Wi-Fi, switching to USB once a host answers on the accessory link. */
    AUTO("Wi-Fi, or USB once the computer answers"),
}

enum class Phase {
    /** USB accessory mode, but the accessory isn't open yet (no cable, or not switched). */
    WAITING_USB,
    /** The accessory is open; waiting for the host to answer on it. */
    USB_ATTACHED,
    /** Connecting through the `adb reverse` tunnel. */
    ADB_CONNECTING,
    /** Looking for the paired host, or for hosts to pair with. */
    WIFI_SEARCHING,
    /** A session is up; the first frame hasn't been shown yet. */
    WAITING_FIRST_FRAME,
    /** Video is on screen: the idle UI is gone. */
    STREAMING,
    /** Session without video (`waylus --no-video`). */
    INPUT_ONLY,
}

class Discovered(val id: String, val name: String, val address: String, val paired: Boolean)

class PairedHost(val name: String, val address: String)

sealed interface Pairing {
    data object Idle : Pairing
    data class Asking(val address: String) : Pairing
    /** The host shows a PIN. [error] is set after a wrong PIN (the host then shows a new one). */
    data class EnterPin(val hostName: String, val error: String?) : Pairing
    data class Done(val message: String, val ok: Boolean) : Pairing
}

data class DeviceInfo(
    val wifiConnected: Boolean = false,
    val addresses: List<String> = emptyList(),
    val freqMhz: Int = 0,
    val rssi: Int = 0,
    val linkMbps: Int = 0,
    /** Cable connected to a computer (USB_STATE "connected"); null = unknown. */
    val usbConnected: Boolean? = null,
    val usbFunctions: String = "",
    val accessoryAttached: Boolean = false,
    val adbEnabled: Boolean? = null,
    val devOptions: Boolean? = null,
    val wirelessAdb: Boolean? = null,
    /** Android 17+ hides these settings from apps and nothing else showed them on. */
    val adbHidden: Boolean = false,
)

/**
 * What the screens show. Written on the main thread only (MainActivity posts there), read by
 * Compose. Nothing here changes per video frame or per input event: the stats display has
 * its own state ([HudState]) that changes once a second.
 */
class UiModel {
    var phase by mutableStateOf(Phase.WIFI_SEARCHING)
    var transport by mutableStateOf(TransportKind.WIFI)
    /** Specific to the current attempt, e.g. "Waylus isn't running at 192.168.1.20". */
    var detail by mutableStateOf<String?>(null)
    /** The adb port, for the hints. */
    var port by mutableStateOf(0)
    /** Why the last session ended; cleared when the next one starts. */
    var lastDisconnect by mutableStateOf<String?>(null)
    /** A host has connected before (USB or Wi-Fi): the user has the PC side already. */
    var everConnected by mutableStateOf(false)

    var paired by mutableStateOf<PairedHost?>(null)
    var discovered by mutableStateOf<List<Discovered>>(emptyList())
    var pairing by mutableStateOf<Pairing>(Pairing.Idle)

    var device by mutableStateOf(DeviceInfo())

    var hudEnabled by mutableStateOf(false)
    var wifiLock by mutableStateOf(true)
    /** Set by intent extras from the host; the switch then shows the host's choice. */
    var wifiLockFromHost by mutableStateOf(false)
    var hudFromHost by mutableStateOf(false)

    var tab by mutableStateOf(Tab.WIFI)
}

enum class Tab(val label: String) { WIFI("Wi-Fi"), DEVICE("This tablet"), HOST("Get Waylus"), SETTINGS("Settings") }

/** User actions from the screens; implemented by MainActivity. */
interface UiActions {
    fun pair(address: String)
    fun submitPin(pin: String)
    fun cancelPairing()
    fun dismissPairingResult()
    fun forgetHost()
    fun setHud(on: Boolean)
    fun setWifiLock(on: Boolean)
    fun openUrl(url: String)
    fun openDeveloperOptions()
}

/** The stats display's lines, replaced about once a second while it is shown. */
class HudState {
    var lines by mutableStateOf<List<String>>(emptyList())
}
