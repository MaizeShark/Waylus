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
        /** [reason]: why the session ended, in words for the idle screen. Reconnecting follows. */
        fun onDisconnected(reason: String)
        /** The opener returned a link; the session handshake (hello, SYNC) follows. */
        fun onLinkOpened() {}
        /** Host STATS message (Proto.parseStats). */
        fun onHostStats(payload: ByteArray) {}
        /** flags: Proto.TWEAK_* bits, opt-in vendor-specific decoder tweaks. */
        fun onConfig(codec: Int, width: Int, height: Int, flags: Int)
        fun onVideo(captureNs: Long, flags: Int, data: ByteArray, offset: Int, recvNs: Long)
        /** Wi-Fi: frames were lost; drop frames until the next keyframe. */
        fun onVideoLost() {}
        /** Proto.PATCH (fast patches, experimental). */
        fun onPatch(payload: ByteArray, recvNs: Long) {}
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

    /** The host said goodbye (Proto.BYE). */
    class HostClosed(val reason: Int, text: String) : java.io.IOException(text)

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
        var lastError = ""
        while (running) {
            var l: Link? = null
            var hadSession = false
            var reason = "the connection closed"
            try {
                l = opener()
                if (l != null) {
                    link = l
                    hadSession = true
                    listener.onLinkOpened()
                    session(l)
                }
            } catch (e: InterruptedException) {
                break
            } catch (e: Exception) {
                // Log each new kind of failure once (not every retry).
                val kind = "${e.javaClass.simpleName}: ${e.message?.replace(Regex("\\d+"), "#")}"
                if (hadSession || kind != lastError) Log.i(TAG, if (hadSession) "disconnected: $e" else "connect failed: $e")
                lastError = kind
                reason = describe(e)
            }
            // session() clears `connected` itself; report every session that started.
            if (hadSession || connected) {
                connected = false
                listener.onDisconnected(reason)
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
                    Proto.FRAME_LOST -> listener.onVideoLost()
                    Proto.PATCH -> listener.onPatch(m.payload, recvNs)
                    Proto.STATS -> listener.onHostStats(m.payload)
                    Proto.BYE -> throw HostClosed(
                        if (m.payload.isNotEmpty()) m.payload[0].toInt() and 0xff else Proto.BYE_STOPPED,
                        String(m.payload, minOf(1, m.payload.size), maxOf(0, m.payload.size - 1)),
                    )
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

        /** A disconnect, in words. */
        fun describe(e: Exception): String = when {
            e is HostClosed && e.reason == Proto.BYE_STOPPED -> "Waylus was stopped on the computer"
            e is HostClosed -> "the computer ended the session: ${e.message}"
            e is java.io.EOFException -> "the computer closed the connection"
            e is java.net.SocketTimeoutException -> "the computer stopped answering"
            e.message?.contains("ECONNRESET") == true || e.message?.contains("reset") == true -> "the connection was reset"
            e.message?.contains("EPIPE") == true || e.message?.contains("Broken pipe") == true -> "the computer closed the connection"
            else -> e.message ?: e.javaClass.simpleName
        }
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
