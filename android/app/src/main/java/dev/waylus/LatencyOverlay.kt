// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.PixelFormat
import android.os.IBinder
import android.util.Log
import android.view.Choreographer
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import java.io.DataInputStream
import java.net.InetSocketAddress
import java.net.Socket

/**
 * Measurement tool (tools/latency): an always-on-top barcode showing, for every
 * frame, the time at which Android expects to present it, converted to the
 * host's wall clock. A screenshot then contains both this and the mirrored host
 * barcode, and the difference is the end-to-end latency, independent of
 * which mirroring app (Waylus or a browser) is showing the host screen.
 *
 * Start: adb shell appops set dev.waylus SYSTEM_ALERT_WINDOW allow
 *        adb shell am start-foreground-service -n dev.waylus/.LatencyOverlay --ei port 27184
 */
class LatencyOverlay : Service() {
    @Volatile private var offsetNs = Long.MIN_VALUE // hostRealtime - deviceMonotonic
    @Volatile private var running = true
    private var view: View? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (view != null) return START_NOT_STICKY
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel("probe", "Latency probe", NotificationManager.IMPORTANCE_LOW))
        val n = Notification.Builder(this, "probe").setContentTitle("Waylus latency probe")
            .setSmallIcon(android.R.drawable.ic_menu_recent_history).build()
        startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)

        val port = intent?.getIntExtra("port", 27184) ?: 27184
        Thread({ syncLoop(port) }, "probe-sync").start()

        val v = Bar()
        val lp = WindowManager.LayoutParams(
            CELL_W * 8, CELL_H * 4,
            WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN or WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS,
            PixelFormat.OPAQUE,
        ).apply { gravity = Gravity.BOTTOM or Gravity.END }
        getSystemService(WindowManager::class.java).addView(v, lp)
        view = v
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        view?.let { getSystemService(WindowManager::class.java).removeView(it) }
        super.onDestroy()
    }

    /** NTP-style sync against tools/latency/probe.py: keep the lowest-RTT estimate. */
    private fun syncLoop(port: Int) {
        while (running) {
            try {
                Socket().use { s ->
                    s.tcpNoDelay = true
                    s.connect(InetSocketAddress("127.0.0.1", port), 1000)
                    val inp = DataInputStream(s.getInputStream())
                    val out = s.getOutputStream()
                    var bestRtt = Long.MAX_VALUE
                    var round = 0
                    while (running) {
                        val t0 = System.nanoTime()
                        out.write(1); out.flush()
                        val host = java.lang.Long.reverseBytes(inp.readLong())
                        val t1 = System.nanoTime()
                        val rtt = t1 - t0
                        // Re-anchor every 64 samples so slow drift is followed.
                        if (rtt <= bestRtt || ++round % 64 == 0) {
                            bestRtt = rtt
                            offsetNs = host - (t0 + t1) / 2
                        }
                        Thread.sleep(50)
                    }
                }
            } catch (e: Exception) {
                Log.i("waylus", "probe sync: $e")
                Thread.sleep(500)
            }
        }
    }

    private inner class Bar : View(this@LatencyOverlay) {
        private val on = Paint().apply { color = Color.WHITE }
        private val off = Paint().apply { color = Color.rgb(40, 40, 40) }
        private var bits = 0

        init { Choreographer.getInstance().postVsyncCallback { onVsync(it) } }

        private fun onVsync(data: Choreographer.FrameData) {
            if (!running) return
            val off = offsetNs
            if (off != Long.MIN_VALUE) {
                // When this frame will actually be on screen, on the host's wall clock.
                val presentNs = data.preferredFrameTimeline.expectedPresentationTimeNanos
                val ms = (((presentNs + off) / 1_000_000L) and 0xffffff).toInt()
                val sum = (((ms and 0xff) + ((ms shr 8) and 0xff) + ((ms shr 16) and 0xff)) and 0xff) xor 0xa5
                bits = ms or (sum shl 24)
                invalidate()
            }
            Choreographer.getInstance().postVsyncCallback { onVsync(it) }
        }

        override fun onDraw(c: Canvas) {
            for (i in 0 until 32) {
                val x = (i % 8) * CELL_W
                val y = (i / 8) * CELL_H
                c.drawRect(x.toFloat(), y.toFloat(), (x + CELL_W).toFloat(), (y + CELL_H).toFloat(),
                    if ((bits ushr i) and 1 == 1) on else off)
            }
        }
    }

    companion object {
        const val CELL_W = 40
        const val CELL_H = 24
    }
}
