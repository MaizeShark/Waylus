// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.util.Log
import java.io.BufferedInputStream
import java.io.DataInputStream
import java.io.InputStream
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.random.Random

/**
 * Talks to the host over a [Link] (TCP through `adb reverse`, or the USB accessory),
 * reconnecting forever. Outgoing messages go through a queue so they can be posted
 * from the UI thread.
 */
class Connection(private val opener: () -> Link?, private val listener: Listener) {
    interface Listener {
        /** Called on the network thread when a session starts; return the hello message. */
        fun hello(): ByteArray
        fun onConnected()
        fun onDisconnected()
        /** flags: Proto.TWEAK_* bits, opt-in vendor-specific decoder tweaks. */
        fun onConfig(codec: Int, width: Int, height: Int, flags: Int)
        fun onVideo(captureNs: Long, flags: Int, data: ByteArray, offset: Int, recvNs: Long)
    }

    /** A byte stream to the host. */
    interface Link {
        val input: InputStream
        /** Each write() call must carry whole messages. */
        val output: OutputStream
        /**
         * The stream outlives sessions (USB accessory): sessions start with a SYNC
         * handshake (see host/src/aoa.rs) instead of a fresh connection.
         */
        val needsSync: Boolean
        fun close()
        /** Drops data received before this session. */
        fun drain() {}
        /** Skips input up to [pattern]; false if it didn't arrive within the timeout. */
        fun awaitSync(pattern: ByteArray, timeoutMs: Long): Boolean = true
    }

    private class TcpLink(val socket: Socket) : Link {
        override val input = BufferedInputStream(socket.getInputStream(), 256 * 1024)
        override val output: OutputStream = socket.getOutputStream()
        override val needsSync = false
        override fun close() { try { socket.close() } catch (_: Exception) {} }
    }

    private val out = LinkedBlockingQueue<ByteArray>()
    @Volatile private var running = true
    @Volatile private var link: Link? = null
    @Volatile var connected = false
        private set
    private val thread = Thread(::loop, "waylus-net")

    fun start() = thread.start()

    fun stop() {
        running = false
        link?.close()
        thread.interrupt()
    }

    /** Thread-safe, never blocks. Dropped while disconnected. */
    fun send(msg: ByteArray) {
        if (connected) out.offer(msg)
    }

    private fun loop() {
        // Video frames arrive here; at normal priority the thread can sit on a slow
        // core or wait behind other work, which showed up as 30-40 ms transfer spikes.
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
        while (running) {
            var l: Link? = null
            try {
                l = opener()
                if (l != null) {
                    link = l
                    session(l)
                }
            } catch (e: InterruptedException) {
                break
            } catch (e: Exception) {
                if (connected) Log.i(TAG, "disconnected: $e")
            }
            if (connected) {
                connected = false
                listener.onDisconnected()
            }
            if (l != null && !l.needsSync) l.close()
            link = null
            if (running) try { Thread.sleep(300) } catch (_: InterruptedException) {}
        }
    }

    private fun session(l: Link) {
        val input = DataInputStream(l.input)
        val output = l.output
        out.clear()
        val hello = listener.hello()
        if (l.needsSync) {
            l.drain()
            val sync = Proto.sync(Random.nextLong())
            // Repeat until the host answers: it may not be running yet, or still be
            // finishing an old session.
            while (running) {
                output.write(sync + hello)
                if (l.awaitSync(sync, 1000)) break
            }
        } else {
            output.write(hello)
            output.flush()
        }
        connected = true
        listener.onConnected()
        Log.i(TAG, "connected to host")

        val writer = Thread({
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
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
                if (!l.needsSync) l.close()
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
                        val flags = if (b.hasRemaining()) b.get().toInt() and 0xff else 0
                        listener.onConfig(codec, w, h, flags)
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
            if (!l.needsSync) l.close()
            writer.join(500)
        }
    }

    companion object {
        private const val TAG = "waylus"
        /** The host pings at 2 Hz; this much silence means it's gone. Matches host/src/link.rs. */
        const val IDLE_TIMEOUT_MS = 5000L

        fun tcp(port: Int): () -> Link = {
            val s = Socket()
            s.tcpNoDelay = true
            s.receiveBufferSize = 1 shl 20
            s.soTimeout = IDLE_TIMEOUT_MS.toInt()
            s.connect(InetSocketAddress("127.0.0.1", port), 1000)
            TcpLink(s)
        }
    }
}
