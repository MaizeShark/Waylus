// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus.ui

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

/*
 * The app's theme, all in one place.
 *
 * Material 3 Expressive is not used yet: in the newest stable Compose BOM (2026.09.00,
 * Material 3 1.4.0) MaterialExpressiveTheme, the expressive MotionScheme and the components
 * (ButtonGroup, LoadingIndicator, ...) are all internal; they are public only in the 1.5.0
 * pre-releases. So this is plain Material 3 with Expressive-style rounder shapes. To switch
 * once 1.5.0 is stable: raise the BOM in app/build.gradle.kts and call
 * MaterialExpressiveTheme(colorScheme, shapes = ...) below instead of MaterialTheme.
 *
 * Colours: Waylus's own dark scheme (the logo's teal and amber), no dynamic colour. Text is
 * near-white on dark surfaces: the NXTPAPER's matte panel makes mid greys hard to read.
 */

val WaylusTeal = Color(0xFF43D6C4)
val WaylusAmber = Color(0xFFFFC46B)

private val Colors = darkColorScheme(
    primary = WaylusTeal,
    onPrimary = Color(0xFF00382F),
    primaryContainer = Color(0xFF005047),
    onPrimaryContainer = Color(0xFFA6FAEB),
    inversePrimary = Color(0xFF006B5F),
    secondary = Color(0xFFB4CAD6),
    onSecondary = Color(0xFF1F333D),
    secondaryContainer = Color(0xFF2A4A5C),
    onSecondaryContainer = Color(0xFFDCEFF7),
    // Amber: warnings and notes (2.4 GHz, weak signal, last disconnect).
    tertiary = WaylusAmber,
    onTertiary = Color(0xFF432C00),
    tertiaryContainer = Color(0xFF5A3E00),
    onTertiaryContainer = Color(0xFFFFE1B8),
    background = Color(0xFF0B1A24),
    onBackground = Color(0xFFEDF6F9),
    surface = Color(0xFF0B1A24),
    onSurface = Color(0xFFEDF6F9),
    surfaceVariant = Color(0xFF1E3A4C),
    onSurfaceVariant = Color(0xFFCCDCE3),
    surfaceTint = WaylusTeal,
    inverseSurface = Color(0xFFEDF6F9),
    inverseOnSurface = Color(0xFF132838),
    error = Color(0xFFFFB4AB),
    onError = Color(0xFF690005),
    errorContainer = Color(0xFF93000A),
    onErrorContainer = Color(0xFFFFDAD6),
    outline = Color(0xFF94ABB5),
    outlineVariant = Color(0xFF3A5566),
    scrim = Color.Black,
    surfaceBright = Color(0xFF243F52),
    surfaceDim = Color(0xFF0B1A24),
    surfaceContainerLowest = Color(0xFF06111A),
    surfaceContainerLow = Color(0xFF0F2230),
    surfaceContainer = Color(0xFF132838),
    surfaceContainerHigh = Color(0xFF183044),
    surfaceContainerHighest = Color(0xFF1E3A50),
)

/** Rounder than the Material 3 defaults, closer to the Expressive shape scale. */
private val WaylusShapes = Shapes(
    extraSmall = RoundedCornerShape(8.dp),
    small = RoundedCornerShape(12.dp),
    medium = RoundedCornerShape(20.dp),
    large = RoundedCornerShape(28.dp),
    extraLarge = RoundedCornerShape(36.dp),
)

@Composable
fun WaylusTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = Colors, shapes = WaylusShapes, content = content)
}
