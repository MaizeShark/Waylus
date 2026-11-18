// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import dev.waylus.BuildConfig
import dev.waylus.Proto
import dev.waylus.R

const val PROJECT_URL = "https://github.com/MaizeShark/Waylus"
const val RELEASES_URL = "$PROJECT_URL/releases"

/** The screen while nothing is streaming: status, what to do next, Wi-Fi, device info, settings. */
@Composable
fun IdleScreen(ui: UiModel, act: UiActions) {
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Row(
            Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(horizontal = 24.dp, vertical = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(24.dp),
        ) {
            Column(
                Modifier.weight(0.42f).fillMaxHeight().verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                Brand()
                StatusCard(ui)
                if (!ui.everConnected && ui.paired == null && ui.tab != Tab.HOST) GetHostCard(act, compact = true)
            }
            Column(Modifier.weight(0.58f).fillMaxHeight()) {
                Tabs(ui)
                Spacer(Modifier.height(12.dp))
                Column(
                    Modifier.fillMaxWidth().weight(1f).verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    when (ui.tab) {
                        Tab.WIFI -> WifiTab(ui, act)
                        Tab.DEVICE -> DeviceTab(ui, act)
                        Tab.HOST -> GetHostCard(act, compact = false)
                        Tab.SETTINGS -> SettingsTab(ui, act)
                    }
                    Spacer(Modifier.height(8.dp))
                }
            }
        }
    }
    PairingDialog(ui, act)
}

@Composable
private fun Brand() {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(
            Modifier.size(56.dp).background(Color(0xFF0F2230), RoundedCornerShape(16.dp)),
            contentAlignment = Alignment.Center,
        ) {
            Image(painterResource(R.drawable.ic_launcher_foreground), contentDescription = null, Modifier.size(56.dp))
        }
        Spacer(Modifier.width(14.dp))
        Column {
            Text("Waylus", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold)
            Text("Pen display for your computer", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

private fun headline(ui: UiModel): String = when (ui.phase) {
    Phase.WAITING_USB -> "Waiting for the computer"
    Phase.USB_ATTACHED -> "USB connected"
    Phase.ADB_CONNECTING -> "Connecting through adb"
    Phase.WIFI_SEARCHING -> if (ui.paired != null) "Looking for ${ui.paired!!.name}" else "Not connected"
    Phase.WAITING_FIRST_FRAME -> "Connected"
    Phase.STREAMING, Phase.INPUT_ONLY -> "Streaming"
}

private fun hint(ui: UiModel): String = when (ui.phase) {
    Phase.WAITING_USB -> "Run `waylus` on your computer."
    Phase.USB_ATTACHED -> "Waiting for Waylus on the computer to answer. Run `waylus` there if it isn't running."
    Phase.ADB_CONNECTING -> "Waiting for `waylus --transport adb` on the computer (port ${ui.port})."
    Phase.WIFI_SEARCHING ->
        if (ui.paired != null) "Run `waylus --wifi` on ${ui.paired!!.name}, or plug in USB and run `waylus`."
        else "Plug in USB and run `waylus` on your computer, or pair over Wi-Fi: run `waylus --wifi --pair` there."
    Phase.WAITING_FIRST_FRAME -> "Starting the stream…"
    Phase.STREAMING, Phase.INPUT_ONLY -> ""
}

@Composable
private fun StatusCard(ui: UiModel) {
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
        shape = MaterialTheme.shapes.large,
        modifier = Modifier.fillMaxWidth().animateContentSize(),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Pill(ui.transport.label)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(headline(ui), style = MaterialTheme.typography.headlineSmall, modifier = Modifier.weight(1f))
                if (ui.phase == Phase.WAITING_FIRST_FRAME || ui.phase == Phase.USB_ATTACHED || ui.pairing is Pairing.Asking) {
                    CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 3.dp)
                }
            }
            Text(code(hint(ui)), style = MaterialTheme.typography.bodyLarge)
            ui.detail?.let { Text(code(it), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            AnimatedVisibility(ui.lastDisconnect != null) {
                Surface(color = MaterialTheme.colorScheme.tertiaryContainer, shape = MaterialTheme.shapes.small) {
                    Text(
                        code("Last session ended: ${ui.lastDisconnect}. Waylus reconnects by itself."),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onTertiaryContainer,
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun Tabs(ui: UiModel) {
    val tabs = Tab.entries
    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
        tabs.forEachIndexed { i, t ->
            SegmentedButton(
                selected = ui.tab == t,
                onClick = { ui.tab = t },
                shape = SegmentedButtonDefaults.itemShape(i, tabs.size),
                icon = {},
                label = { Text(t.label, maxLines = 1) },
            )
        }
    }
}

// --- Wi-Fi ---

@Composable
private fun ColumnScope.WifiTab(ui: UiModel, act: UiActions) {
    var confirmForget by remember { mutableStateOf(false) }
    Section("Paired computer") {
        val p = ui.paired
        if (p != null) {
            ListItem(
                headlineContent = { Text(p.name) },
                supportingContent = { Text(p.address.ifBlank { "address not known yet" }) },
                trailingContent = { OutlinedButton(onClick = { confirmForget = true }) { Text("Forget") } },
                colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            )
            Body("The tablet connects to it by itself whenever `waylus --wifi` runs there.")
        } else {
            Body("None yet. Run `waylus --wifi --pair` on your computer, then pick it below or enter its address.")
        }
    }
    (ui.pairing as? Pairing.Done)?.let { d ->
        Surface(
            color = if (d.ok) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.errorContainer,
            shape = MaterialTheme.shapes.medium,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Row(Modifier.padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    code(d.message), style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f),
                    color = if (d.ok) MaterialTheme.colorScheme.onPrimaryContainer else MaterialTheme.colorScheme.onErrorContainer,
                )
                TextButton(onClick = act::dismissPairingResult) { Text("OK") }
            }
        }
    }
    Section("On this network") {
        val others = ui.discovered
        if (others.isEmpty()) {
            Body("No Waylus computers found yet. They show up here while `waylus --wifi` runs on the same network.")
        } else {
            for (h in others) {
                ListItem(
                    headlineContent = { Text(h.name) },
                    supportingContent = { Text(h.address) },
                    trailingContent = {
                        if (h.paired) Text("paired", color = MaterialTheme.colorScheme.primary)
                        else FilledTonalButton(onClick = { act.pair(h.address) }, enabled = ui.pairing !is Pairing.Asking) { Text("Pair") }
                    },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                )
            }
        }
    }
    Section("Pair by address") {
        var addr by rememberSaveable { mutableStateOf("") }
        val go = { if (addr.isNotBlank()) act.pair(addr.trim()) }
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = addr,
                onValueChange = { addr = it.filter { c -> !c.isWhitespace() } },
                label = { Text("Computer's address, e.g. 192.168.1.20") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go),
                keyboardActions = KeyboardActions(onGo = { go() }),
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(12.dp))
            Button(onClick = go, enabled = addr.isNotBlank() && ui.pairing !is Pairing.Asking) { Text("Pair") }
        }
        Body("Waylus prints its addresses when it starts with `--wifi`.")
    }
    if (confirmForget) {
        AlertDialog(
            onDismissRequest = { confirmForget = false },
            title = { Text("Forget ${ui.paired?.name ?: "this computer"}?") },
            text = { Text(code("The tablet stops connecting to it over Wi-Fi. To use it again, pair again with `waylus --wifi --pair`.")) },
            confirmButton = { TextButton(onClick = { confirmForget = false; act.forgetHost() }) { Text("Forget") } },
            dismissButton = { TextButton(onClick = { confirmForget = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun PairingDialog(ui: UiModel, act: UiActions) {
    val p = ui.pairing as? Pairing.EnterPin ?: return
    var pin by remember(p) { mutableStateOf("") }
    val focus = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    val submit = { if (pin.length == PIN_DIGITS) { keyboard?.hide(); act.submitPin(pin) } }
    AlertDialog(
        onDismissRequest = {},
        title = { Text("Enter the PIN") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("${p.hostName} shows a $PIN_DIGITS-digit PIN in its terminal (and as a notification).")
                OutlinedTextField(
                    value = pin,
                    onValueChange = { v -> pin = v.filter(Char::isDigit).take(PIN_DIGITS) },
                    label = { Text("PIN") },
                    singleLine = true,
                    isError = p.error != null,
                    supportingText = p.error?.let { e -> { Text(e) } },
                    textStyle = MaterialTheme.typography.headlineSmall.copy(fontFamily = FontFamily.Monospace, letterSpacing = MaterialTheme.typography.headlineSmall.fontSize * 0.3f),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Done),
                    keyboardActions = KeyboardActions(onDone = { submit() }),
                    modifier = Modifier.focusRequester(focus),
                )
            }
        },
        confirmButton = { TextButton(onClick = submit, enabled = pin.length == PIN_DIGITS) { Text("Pair") } },
        dismissButton = { TextButton(onClick = act::cancelPairing) { Text("Cancel") } },
    )
    LaunchedEffect(p) { focus.requestFocus(); keyboard?.show() }
}

private const val PIN_DIGITS = 6

// --- This tablet ---

@Composable
private fun ColumnScope.DeviceTab(ui: UiModel, act: UiActions) {
    val d = ui.device
    Section("Wi-Fi") {
        if (!d.wifiConnected) {
            Body("Not connected to Wi-Fi.")
        } else {
            Fact("Address", d.addresses.ifEmpty { listOf("–") }.joinToString("\n"))
            val band = when (d.freqMhz) {
                in 2400..2500 -> "2.4 GHz"
                in 4900..5900 -> "5 GHz"
                in 5925..7125 -> "6 GHz"
                0 -> "–"
                else -> "?"
            }
            Fact("Band", if (d.freqMhz > 0) "$band (${d.freqMhz} MHz)" else band)
            Fact("Signal", if (d.rssi != 0) "${d.rssi} dBm (${signalWord(d.rssi)})" + if (d.linkMbps > 0) ", ${d.linkMbps} Mbit/s" else "" else "–")
            if (d.freqMhz in 2400..2500) {
                Warning("On 2.4 GHz the picture can freeze for 0.2–1 s at times. A 5 GHz network works much better.")
            } else if (d.rssi != 0 && d.rssi < -75) {
                Warning("The signal is weak; expect short stalls. Move closer to the access point if you can.")
            }
        }
    }
    Section("USB") {
        Fact("Waylus host", if (d.accessoryAttached) "attached (USB accessory mode)" else "not attached")
        Fact("Computer", when (d.usbConnected) { true -> "connected" + if (d.usbFunctions.isNotEmpty()) " (${d.usbFunctions})" else ""; false -> "not connected"; null -> "unknown" })
    }
    Section("USB debugging (adb)") {
        Fact("Developer options", onOff(d.devOptions))
        Fact("USB debugging", onOff(d.adbEnabled))
        d.wirelessAdb?.let { Fact("Wireless debugging", onOff(it)) }
        Body(
            "adb is optional: USB accessory mode and Wi-Fi work without it. It is needed for " +
                "`--transport adb` and for reading the stylus directly (full tilt on some tablets), " +
                "and lets `waylus` install and start this app by itself.",
        )
        if (d.devOptions == false) {
            Body("To turn on developer options: Settings › About tablet › tap Build number seven times.")
        }
        OutlinedButton(onClick = act::openDeveloperOptions) { Text(if (d.devOptions == false) "Open About tablet" else "Open developer options") }
    }
}

private fun signalWord(rssi: Int) = when {
    rssi >= -60 -> "good"
    rssi >= -70 -> "fair"
    else -> "weak"
}

private fun onOff(b: Boolean?) = when (b) { true -> "on"; false -> "off"; null -> "unknown" }

// --- Get the host ---

@Composable
private fun GetHostCard(act: UiActions, compact: Boolean) {
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer),
        shape = MaterialTheme.shapes.large,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(Modifier.padding(20.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(if (compact) "New here?" else "The computer side", style = MaterialTheme.typography.titleMedium)
                Body("Waylus needs its host program on your Linux (Wayland) or Windows computer. Get it here:")
                Text(PROJECT_URL, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.primary)
                if (!compact) Body("Releases: $RELEASES_URL")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilledTonalButton(onClick = { act.openUrl(PROJECT_URL) }) { Text("Open") }
                    if (!compact) OutlinedButton(onClick = { act.openUrl(RELEASES_URL) }) { Text("Releases") }
                }
            }
            Spacer(Modifier.width(16.dp))
            QrCode(PROJECT_URL, size = if (compact) 96.dp else 160.dp)
        }
    }
}

// --- Settings ---

@Composable
private fun ColumnScope.SettingsTab(ui: UiModel, act: UiActions) {
    Section("Display") {
        SwitchRow(
            "Stats display",
            "Frame rate and latency in a corner while streaming. Tap the black bar beside the picture to show or hide it." +
                if (ui.hudFromHost) " Turned on by the host (--hud)." else "",
            ui.hudEnabled,
            act::setHud,
        )
    }
    Section("Wi-Fi") {
        SwitchRow(
            "Low-latency Wi-Fi",
            "Keeps Wi-Fi power saving off while streaming over Wi-Fi. Costs some battery." +
                if (ui.wifiLockFromHost) " Set by the host for this session." else "",
            ui.wifiLock,
            act::setWifiLock,
        )
        if (ui.paired != null) {
            OutlinedButton(onClick = act::forgetHost) { Text("Forget ${ui.paired!!.name}") }
        }
    }
    Section("About") {
        Fact("App", "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})")
        Fact("Protocol", "${Proto.VERSION}")
        Body("Free software under the GNU GPL, version 3 or later. Source: $PROJECT_URL")
    }
}

// --- building blocks ---

@Composable
private fun Section(title: String, content: @Composable ColumnScope.() -> Unit) {
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer),
        shape = MaterialTheme.shapes.large,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(horizontal = 20.dp, vertical = 16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
            content()
        }
    }
}

@Composable
private fun Body(text: String) {
    Text(code(text), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun Fact(label: String, value: String) {
    Row(Modifier.fillMaxWidth()) {
        Text(label, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.width(170.dp))
        Text(value, style = MaterialTheme.typography.bodyLarge)
    }
}

@Composable
private fun Warning(text: String) {
    Surface(color = MaterialTheme.colorScheme.tertiaryContainer, shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth()) {
        Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onTertiaryContainer, modifier = Modifier.padding(12.dp))
    }
}

@Composable
private fun SwitchRow(title: String, text: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Spacer(Modifier.width(16.dp))
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

@Composable
private fun Pill(text: String) {
    Surface(color = MaterialTheme.colorScheme.primaryContainer, shape = RoundedCornerShape(50)) {
        Text(
            text, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onPrimaryContainer,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
        )
    }
}

/** Text with `code` spans in monospace. */
@Composable
private fun code(s: String): AnnotatedString {
    val bg = MaterialTheme.colorScheme.surfaceVariant
    return remember(s, bg) {
        buildAnnotatedString {
            val parts = s.split('`')
            parts.forEachIndexed { i, part ->
                if (i % 2 == 1) withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = bg)) { append(part) }
                else append(part)
            }
        }
    }
}
