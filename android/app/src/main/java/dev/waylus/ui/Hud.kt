// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The stats display: a few lines of text in a corner. Its state changes about once a second
 * (MainActivity's ticker), never per frame. It has no pointer input, so touches pass through
 * to the root view (and the host) as before.
 */
@Composable
fun Hud(state: HudState) {
    val lines = state.lines
    if (lines.isEmpty()) return
    Column(
        Modifier
            .padding(8.dp)
            .background(Color(0xB3000000), RoundedCornerShape(10.dp))
            .padding(horizontal = 10.dp, vertical = 6.dp),
    ) {
        val style = TextStyle(color = Color(0xFFF2F7F9), fontFamily = FontFamily.Monospace, fontSize = 13.sp, lineHeight = 17.sp)
        for (l in lines) Text(l, style = style)
    }
}
