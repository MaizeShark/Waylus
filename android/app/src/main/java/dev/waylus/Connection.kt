// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.util.Log
import java.io.BufferedInputStream
import java.io.DataInputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Connects to the host through the `adb reverse` tunnel (127.0.0.1:port on the
 * tablet ends up on the host), reconnecting forever. Outgoing messages go
 * through a queue so they can be posted from the UI thread.
 */
class Connection(private val port: Int, private val listener: Listener) {
    interface Listener {
        /** Called on the network thread right after connecting; return the hello message. */
        fun onConnected(): ByteArray
        fun onDisconnected()
        fun onConfig(codec: Int, width: Int, height: Int)
        fun onVideo(captureNs: Long, flags: Int, data: ByteArray, offset: Int, recvNs: Long)
    }

    private val out = LinkedBlockingQueue<ByteArray>()
    @Volatile private var running = true
    @Volatile private var socket: Socket? = null
    @Volatile var connected = false
        private set
    private val thread = Thread(::loop, "waylus-net")

    fun start() = thread.start()

    fun stop() {
        running = false
        try { socket?.close() } catch (_: Exception) {}
        thread.interrupt()
    }

    /** Thread-safe, never blocks. Dropped while disconnected. */
    fun send(msg: ByteArray) {
        if (connected) out.offer(msg)
    }

    private fun loop() {
        while (running) {
            try {
                val s = Socket()
                s.tcpNoDelay = true
                s.receiveBufferSize = 1 shl 20
                s.connect(InetSocketAddress("127.0.0.1", port), 1000)
                socket = s
                session(s)
            } catch (e: Exception) {
                if (connected) Log.i(TAG, "disconnected: $e")
            }
            if (connected) {
                connected = false
                listener.onDisconnected()
            }
            try { socket?.close() } catch (_: Exception) {}
            socket = null
            if (running) try { Thread.sleep(300) } catch (_: InterruptedException) {}
        }
    }

    private fun session(s: Socket) {
        val input = DataInputStream(BufferedInputStream(s.getInputStream(), 256 * 1024))
        val output = s.getOutputStream()
        out.clear()
        output.write(listener.onConnected())
        output.flush()
        connected = true
        Log.i(TAG, "connected to host")

        val writer = Thread({
            try {
                while (connected && running) {
                    val m = out.poll(200, TimeUnit.MILLISECONDS) ?: continue
                    output.write(m)
                    // Coalesce whatever else is already queued into the same flush.
                    while (true) output.write(out.poll() ?: break)
                    output.flush()
                }
            } catch (e: Exception) {
                Log.i(TAG, "writer: $e")
                try { s.close() } catch (_: Exception) {}
            }
        }, "waylus-writer")
        writer.start()

        try {
            while (running) {
                val m = Proto.read(input)
                val recvNs = System.nanoTime()
                when (m.type) {
                    Proto.PING -> {
                        val host = Proto.le(m.payload).getLong()
                        out.offer(Proto.pong(host, recvNs))
                    }
                    Proto.CONFIG -> {
                        val b = Proto.le(m.payload)
                        val codec = b.get().toInt() and 0xff
                        val w = b.getShort().toInt() and 0xffff
                        val h = b.getShort().toInt() and 0xffff
                        listener.onConfig(codec, w, h)
                    }
                    Proto.VIDEO -> {
                        val b = Proto.le(m.payload)
                        val captureNs = b.getLong()
                        val flags = b.get().toInt() and 0xff
                        listener.onVideo(captureNs, flags, m.payload, 9, recvNs)
                    }
                    else -> Log.d(TAG, "ignoring message ${m.type}")
                }
            }
        } finally {
            connected = false
            try { s.close() } catch (_: Exception) {}
            writer.join(500)
        }
    }

    companion object { private const val TAG = "waylus" }
}
