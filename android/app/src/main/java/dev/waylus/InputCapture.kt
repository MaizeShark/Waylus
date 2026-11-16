// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.graphics.RectF
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.MotionEvent
import kotlin.math.atan
import kotlin.math.cos
import kotlin.math.roundToInt
import kotlin.math.sin
import kotlin.math.tan

/**
 * Turns MotionEvents into protocol messages. Pen samples include every
 * historical (batched) sample; coordinates are normalised to the video rect.
 * Fingers are suppressed while the pen is near the screen (palm rejection).
 */
class InputCapture(
    private val send: (ByteArray) -> Unit,
    private val videoRect: () -> RectF,
) {
    private val handler = Handler(Looper.getMainLooper())
    private var penState = Proto.PEN_OUT
    private var penLastSeenNs = 0L
    private var touchActive = false

    /** Android sends HOVER_EXIT right before ACTION_DOWN; delay "out of range" so taps don't toggle proximity. */
    private val penOut = Runnable {
        if (penState == Proto.PEN_HOVER) {
            penState = Proto.PEN_OUT
            send(Proto.pen(System.nanoTime(), Proto.PEN_OUT, 0, 0, 0, 0, 0, 0))
        }
    }

    fun onEvent(e: MotionEvent): Boolean {
        var handled = false
        var hasFinger = false
        for (i in 0 until e.pointerCount) {
            when (e.getToolType(i)) {
                MotionEvent.TOOL_TYPE_STYLUS, MotionEvent.TOOL_TYPE_ERASER -> {
                    pen(e, i)
                    handled = true
                }
                MotionEvent.TOOL_TYPE_FINGER, MotionEvent.TOOL_TYPE_UNKNOWN -> hasFinger = true
            }
        }
        if (hasFinger || touchActive) {
            touch(e)
            handled = true
        }
        return handled
    }

    private fun pen(e: MotionEvent, i: Int) {
        val action = e.actionMasked
        val eraser = e.getToolType(i) == MotionEvent.TOOL_TYPE_ERASER
        var buttons = 0
        if (e.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0) buttons = buttons or Proto.BTN_PRIMARY
        if (e.buttonState and MotionEvent.BUTTON_STYLUS_SECONDARY != 0) buttons = buttons or Proto.BTN_SECONDARY
        if (eraser) buttons = buttons or Proto.BTN_ERASER

        val state = when (action) {
            MotionEvent.ACTION_HOVER_ENTER, MotionEvent.ACTION_HOVER_MOVE -> Proto.PEN_HOVER
            MotionEvent.ACTION_HOVER_EXIT -> {
                // Keep "hover" for a moment; penOut fires unless the pen touches down.
                handler.removeCallbacks(penOut)
                handler.postDelayed(penOut, 60)
                return
            }
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_MOVE, MotionEvent.ACTION_POINTER_DOWN -> Proto.PEN_CONTACT
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP, MotionEvent.ACTION_CANCEL -> Proto.PEN_HOVER
            else -> return
        }
        handler.removeCallbacks(penOut)
        val r = videoRect()
        // Batched samples first, in order, then the current one.
        for (h in 0 until e.historySize) {
            val t = if (Build.VERSION.SDK_INT >= 34) e.getHistoricalEventTimeNanos(h) else e.getHistoricalEventTime(h) * 1_000_000
            emitPen(
                t, state, buttons, r,
                e.getHistoricalX(i, h), e.getHistoricalY(i, h),
                e.getHistoricalPressure(i, h),
                e.getHistoricalAxisValue(MotionEvent.AXIS_TILT, i, h),
                e.getHistoricalAxisValue(MotionEvent.AXIS_ORIENTATION, i, h),
            )
        }
        val t = if (Build.VERSION.SDK_INT >= 34) e.eventTimeNanos else e.eventTime * 1_000_000
        // On lift the pressure is meaningless; report hover with zero pressure.
        emitPen(
            t, state, buttons, r, e.getX(i), e.getY(i),
            if (state == Proto.PEN_CONTACT) e.getPressure(i) else 0f,
            e.getAxisValue(MotionEvent.AXIS_TILT, i),
            e.getAxisValue(MotionEvent.AXIS_ORIENTATION, i),
        )
    }

    private fun emitPen(tNs: Long, state: Int, buttons: Int, r: RectF, x: Float, y: Float, p: Float, tilt: Float, orient: Float) {
        // AXIS_TILT is the angle from the screen normal, AXIS_ORIENTATION the direction
        // the pen leans (0 = up, clockwise positive). Convert to evdev-style tilt X/Y.
        val t = tan(tilt.toDouble().coerceIn(0.0, 1.5))
        val tx = Math.toDegrees(atan(sin(orient.toDouble()) * t))
        val ty = Math.toDegrees(atan(-cos(orient.toDouble()) * t))
        penState = state
        penLastSeenNs = tNs
        val msg = Proto.pen(
            tNs, state, buttons,
            norm(x, r.left, r.width()), norm(y, r.top, r.height()),
            (p.coerceIn(0f, 1f) * 65535f).roundToInt(),
            (tx * 100).roundToInt().coerceIn(-9000, 9000),
            (ty * 100).roundToInt().coerceIn(-9000, 9000),
        )
        send(msg)
    }

    private val ids = IntArray(16)
    private val xs = IntArray(16)
    private val ys = IntArray(16)

    private fun touch(e: MotionEvent) {
        val action = e.actionMasked
        val t = if (Build.VERSION.SDK_INT >= 34) e.eventTimeNanos else e.eventTime * 1_000_000
        // Palm rejection: no fingers while the pen hovers or was just used. A hovering
        // pen reports continuously, so "seen recently" also covers missing HOVER_EXITs.
        val penNear = (System.nanoTime() - penLastSeenNs) < 300_000_000L
        var n = 0
        if (!penNear && action != MotionEvent.ACTION_CANCEL) {
            val r = videoRect()
            for (i in 0 until e.pointerCount) {
                if (n == ids.size) break
                val tool = e.getToolType(i)
                if (tool != MotionEvent.TOOL_TYPE_FINGER && tool != MotionEvent.TOOL_TYPE_UNKNOWN) continue
                val lifting = (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_POINTER_UP) && i == e.actionIndex
                if (lifting) continue
                ids[n] = e.getPointerId(i) and 0xff
                xs[n] = norm(e.getX(i), r.left, r.width())
                ys[n] = norm(e.getY(i), r.top, r.height())
                n++
            }
        }
        if (n == 0 && !touchActive) return
        touchActive = n > 0
        send(Proto.touch(t, n, ids, xs, ys))
    }

    /** Lift everything (e.g. focus lost). */
    fun reset() {
        handler.removeCallbacks(penOut)
        if (penState != Proto.PEN_OUT) {
            penState = Proto.PEN_OUT
            send(Proto.pen(System.nanoTime(), Proto.PEN_OUT, 0, 0, 0, 0, 0, 0))
        }
        if (touchActive) {
            touchActive = false
            send(Proto.touch(System.nanoTime(), 0, ids, xs, ys))
        }
    }

    private fun norm(v: Float, origin: Float, size: Float): Int {
        if (size <= 0f) return 0
        return (((v - origin) / size).coerceIn(0f, 1f) * 65535f).roundToInt()
    }
}
