// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.content.Context
import android.net.wifi.WifiManager
import android.util.Log
import java.io.BufferedInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Wi-Fi transport (host/src/wifi/mod.rs, docs/wifi.md). Reliable messages go over TCP
 * in AES-GCM records, input and frame reports over sealed UDP datagrams. Pen/touch
 * input is sent with redundancy (recent samples repeated in every datagram, the final
 * state repeated shortly after) instead of waiting for retransmissions.
 */
class WifiLink private constructor(
    private val socket: Socket,
    private val udp: DatagramSocket,
    keys: WifiCrypto.SessionKeys,
) : Connection.Link {
    class AuthFailed(val code: Int, msg: String) : IOException(msg)

    private val chunks = LinkedBlockingQueue<ByteArray>()
    private val assembler: VideoAssembler
    private val framesSkipped = java.util.concurrent.atomic.AtomicInteger()
    @Volatile private var closed = false
    private val tcpOut = socket.getOutputStream()
    private val txTcp = WifiCrypto.Aead(keys.c2h)
    private val txUdp = WifiCrypto.Aead(keys.c2h)
    private var tcpCtr = 0L
    private var udpSeq = 0L
    private val udpLock = Any()
    private val timer = Executors.newSingleThreadScheduledExecutor { r -> Thread(r, "waylus-wifi-timer").apply { isDaemon = true } }
    private var repeats = ArrayList<ScheduledFuture<*>>()
    /** Last pen/touch messages sent, oldest first. */
    private val recentInput = ArrayDeque<ByteArray>()

    init {
        val rxTcp = WifiCrypto.Aead(keys.h2c)
        val rxUdp = WifiCrypto.Aead(keys.h2c)
        assembler = VideoAssembler(
            WifiCrypto.Aead(keys.h2c),
            deliver = { payload, _ -> chunks.put(Proto.frame(Proto.VIDEO, payload)) },
            lost = { first, last ->
                sendUdp(listOf(Proto.wifiFrameLost(first, last)))
                chunks.put(Proto.frame(Proto.FRAME_LOST))
            },
        )
        Thread({ tcpLoop(rxTcp) }, "waylus-wifi-tcp").apply { isDaemon = true; start() }
        Thread({ udpLoop(rxUdp) }, "waylus-wifi-udp").apply { isDaemon = true; start() }
        // Tell the host where we are; repeated so a lost one doesn't matter, then as keepalive.
        val hello = Proto.frame(Proto.UDP_HELLO)
        for (ms in longArrayOf(0, 50, 200)) timer.schedule({ sendUdp(listOf(hello)) }, ms, TimeUnit.MILLISECONDS)
        timer.scheduleAtFixedRate({
            val (freq, rssi, link) = WifiPower.info()
            val skipped = framesSkipped.getAndSet(0)
            sendUdp(listOf(hello, Proto.wifiStats(assembler.takeStats(), skipped, freq, rssi, link)))
        }, 1000, 1000, TimeUnit.MILLISECONDS)
        WifiPower.streaming(true)
    }

    override val needsSync = false
    override val input: InputStream = ChunkInput(chunks)
    override val output: OutputStream = object : OutputStream() {
        private val pending = ByteArrayOutputStream()
        override fun write(b: Int) = pending.write(b)
        override fun write(b: ByteArray, off: Int, len: Int) = pending.write(b, off, len)
        override fun flush() {
            val all = pending.toByteArray()
            pending.reset()
            route(all)
        }
    }

    override fun close() {
        if (closed) return
        closed = true
        WifiPower.streaming(false)
        timer.shutdownNow()
        try { socket.close() } catch (_: Exception) {}
        udp.close()
        chunks.put(EOF)
    }

    private fun route(all: ByteArray) {
        val tcp = ByteArrayOutputStream()
        val udpMsgs = ArrayList<ByteArray>()
        var i = 0
        while (i + 5 <= all.size) {
            val t = all[i].toInt() and 0xff
            val len = ByteBuffer.wrap(all, i + 1, 4).order(ByteOrder.LITTLE_ENDIAN).getInt()
            val end = i + 5 + len
            if (len < 0 || end > all.size) break
            if (t == Proto.FRAME_SKIPPED) framesSkipped.incrementAndGet()
            if (t in UDP_TYPES) udpMsgs.add(all.copyOfRange(i, end)) else tcp.write(all, i, end - i)
            i = end
        }
        if (tcp.size() > 0) sendTcp(tcp.toByteArray())
        if (udpMsgs.isNotEmpty()) sendInput(udpMsgs)
    }

    private fun sendTcp(plain: ByteArray) {
        synchronized(tcpOut) {
            val len = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(plain.size + WifiCrypto.TAG_LEN).array()
            val ct = txTcp.seal(WifiCrypto.STREAM_TCP, tcpCtr++, len, plain)
            tcpOut.write(len + ct) // one write per record
            tcpOut.flush()
        }
    }

    private fun isInput(m: ByteArray) = m[0].toInt() == Proto.PEN || m[0].toInt() == Proto.TOUCH

    private var lastPenNs = 0L
    private var lastTouchNs = 0L

    /**
     * The host drops input that is not newer than the last sample it applied (that is how
     * the repeated copies are discarded), so timestamps must strictly increase per type.
     * Before Android 14 event times have millisecond resolution; nudge equal ones by 1 ns.
     */
    private fun makeMonotonic(m: ByteArray) {
        val b = ByteBuffer.wrap(m, 5, 8).order(ByteOrder.LITTLE_ENDIAN)
        var t = b.getLong(5)
        val last = if (m[0].toInt() == Proto.PEN) lastPenNs else lastTouchNs
        if (t <= last) { t = last + 1; b.putLong(5, t) }
        if (m[0].toInt() == Proto.PEN) lastPenNs = t else lastTouchNs = t
    }

    private fun sendInput(msgs: List<ByteArray>) {
        val hasInput = msgs.any(::isInput)
        synchronized(udpLock) {
            for (m in msgs) if (isInput(m) && m.size >= 13) makeMonotonic(m)
            // Recent input first (oldest first), so the host's newest-sample check keeps order.
            val dgram = ArrayList<ByteArray>()
            if (hasInput) dgram.addAll(recentInput.takeLast(REDUNDANCY))
            dgram.addAll(msgs)
            sendUdp(dgram)
            if (hasInput) {
                for (m in msgs) if (isInput(m)) recentInput.addLast(m)
                while (recentInput.size > REDUNDANCY + 1) recentInput.removeFirst()
                // Repeat the latest state shortly after, in case this was the last datagram
                // of a stroke (pen lift) and gets lost. Newer input reschedules this.
                for (f in repeats) f.cancel(false)
                repeats = ArrayList()
                val snapshot = recentInput.toList()
                for (ms in REPEAT_MS) {
                    runCatching { repeats.add(timer.schedule({ sendUdp(snapshot) }, ms, TimeUnit.MILLISECONDS)) }
                }
            }
        }
    }

    private fun sendUdp(msgs: List<ByteArray>) {
        if (closed) return
        synchronized(udpLock) {
            val body = ByteArrayOutputStream()
            for (m in msgs) body.write(m)
            val hdr = ByteBuffer.allocate(UDP_HDR).order(ByteOrder.LITTLE_ENDIAN)
            hdr.put(KIND_CONTROL); hdr.put(0); hdr.putShort(0); hdr.putLong(udpSeq)
            val h = hdr.array()
            val ct = txUdp.seal(WifiCrypto.STREAM_UDP, udpSeq++, h, body.toByteArray())
            try {
                udp.send(DatagramPacket(h + ct, h.size + ct.size))
            } catch (e: IOException) {
                if (!closed) Log.d(TAG, "UDP send: $e")
            }
        }
    }

    private fun tcpLoop(aead: WifiCrypto.Aead) {
        val inp = DataInputStream(BufferedInputStream(socket.getInputStream(), 256 * 1024))
        var ctr = 0L
        try {
            while (true) {
                val lenB = ByteArray(4)
                inp.readFully(lenB)
                val n = ByteBuffer.wrap(lenB).order(ByteOrder.LITTLE_ENDIAN).getInt()
                if (n < WifiCrypto.TAG_LEN || n > MAX_RECORD) throw IOException("bad record length $n")
                val ct = ByteArray(n)
                inp.readFully(ct)
                chunks.put(aead.open(WifiCrypto.STREAM_TCP, ctr++, lenB, ct))
            }
        } catch (e: Exception) {
            if (!closed) Log.i(TAG, "Wi-Fi TCP: $e")
        }
        chunks.put(EOF)
    }

    private fun udpLoop(aead: WifiCrypto.Aead) {
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
        val replay = WifiCrypto.ReplayWindow()
        val buf = ByteArray(65536)
        val p = DatagramPacket(buf, buf.size)
        var rejected = 0
        while (!closed) {
            try {
                p.setLength(buf.size)
                udp.receive(p)
            } catch (_: SocketTimeoutException) {
                assembler.tick(System.nanoTime())
                continue
            } catch (e: IOException) {
                if (!closed) Log.i(TAG, "Wi-Fi UDP: $e")
                break
            }
            val n = p.length
            if (buf[0] == VideoAssembler.KIND_VIDEO) {
                assembler.add(buf, n, System.nanoTime())
                continue
            }
            if (n < UDP_HDR + WifiCrypto.TAG_LEN || buf[0] != KIND_CONTROL) { rejected++; continue }
            val seq = ByteBuffer.wrap(buf, 4, 8).order(ByteOrder.LITTLE_ENDIAN).getLong()
            val plain = try {
                aead.open(WifiCrypto.STREAM_UDP, seq, buf.copyOfRange(0, UDP_HDR), buf, UDP_HDR, n - UDP_HDR)
            } catch (_: Exception) { rejected++; continue }
            if (!replay.accept(seq)) { rejected++; continue }
            chunks.put(plain)
        }
        if (rejected > 0) Log.i(TAG, "Wi-Fi UDP: rejected $rejected datagrams")
    }

    /** InputStream over decrypted chunks; times out like the other transports. */
    private class ChunkInput(private val q: LinkedBlockingQueue<ByteArray>) : InputStream() {
        private var cur: ByteArray? = null
        private var pos = 0

        private fun fill(): ByteArray {
            cur?.let { if (pos < it.size) return it }
            val c = q.poll(Connection.IDLE_TIMEOUT_MS, TimeUnit.MILLISECONDS) ?: throw SocketTimeoutException("host silent")
            if (c === EOF) { q.put(EOF); throw EOFException("Wi-Fi link closed") }
            cur = c
            pos = 0
            return c
        }

        override fun read(): Int { val c = fill(); return c[pos++].toInt() and 0xff }

        override fun read(b: ByteArray, off: Int, len: Int): Int {
            if (len == 0) return 0
            val c = fill()
            val n = minOf(len, c.size - pos)
            System.arraycopy(c, pos, b, off, n)
            pos += n
            return n
        }

        override fun available(): Int = cur?.let { it.size - pos } ?: 0
    }

    companion object {
        private const val TAG = "waylus"
        private val EOF = ByteArray(0)
        private const val KIND_CONTROL: Byte = 1
        private const val UDP_HDR = 12
        private const val MAX_RECORD = 16 * 1024 * 1024 + 64
        /** Earlier pen/touch messages repeated in each datagram. */
        private const val REDUNDANCY = 2
        private val REPEAT_MS = longArrayOf(5, 15)
        /** DSCP EF: WMM voice access category. */
        private const val TOS_EF = 46 shl 2
        private val UDP_TYPES = setOf(
            Proto.PEN, Proto.TOUCH, Proto.FRAME_SHOWN, Proto.PONG, Proto.REQUEST_KEYFRAME,
            Proto.FRAME_SKIPPED, Proto.WIFI_FRAME_LOST, Proto.WIFI_STATS,
        )
        private const val PAIR_AAD = "waylus pairing v1"

        fun parseAddress(a: String, defaultPort: Int): InetSocketAddress {
            val s = a.trim()
            val i = s.lastIndexOf(':')
            return if (i > 0 && !s.substring(0, i).contains(':')) InetSocketAddress(s.substring(0, i), s.substring(i + 1).toInt())
            else InetSocketAddress(s, defaultPort)
        }

        private fun tcp(addr: InetSocketAddress): Socket {
            val s = Socket()
            s.tcpNoDelay = true
            s.trafficClass = TOS_EF
            s.receiveBufferSize = 1 shl 20
            s.connect(addr, 1500)
            s.soTimeout = 10_000
            return s
        }

        private fun readExpect(inp: DataInputStream, type: Int): ByteArray {
            val m = Proto.read(inp)
            if (m.type == Proto.AUTH_FAIL) {
                val code = if (m.payload.isNotEmpty()) m.payload[0].toInt() else 0
                throw AuthFailed(code, String(m.payload, 1, maxOf(0, m.payload.size - 1)))
            }
            if (m.type != type) throw IOException("expected message $type, got ${m.type}")
            return m.payload
        }

        /** Connects to a paired host and authenticates. */
        fun connect(store: WifiStore, host: WifiStore.Host, addr: InetSocketAddress): WifiLink {
            val s = tcp(addr)
            try {
                val inp = DataInputStream(s.getInputStream())
                val out = s.getOutputStream()
                val eph = WifiCrypto.ephemeral()
                val nonceC = ByteArray(32).also { SecureRandom().nextBytes(it) }
                val hello = store.deviceId + nonceC + WifiCrypto.encodePublic(eph)
                out.write(Proto.frame(Proto.AUTH_C, hello))
                val h = readExpect(inp, Proto.AUTH_H)
                if (h.size != 16 + 32 + 65 + 32) throw IOException("bad AUTH_H")
                val hostId = h.copyOfRange(0, 16)
                if (!hostId.contentEquals(host.id)) throw AuthFailed(0, "a different Waylus host answers at $addr")
                val nonceH = h.copyOfRange(16, 48)
                val ephH = h.copyOfRange(48, 113)
                val macH = h.copyOfRange(113, 145)
                val th = WifiCrypto.sha256("waylus auth v1".toByteArray(), hello, hostId, nonceH, ephH)
                if (!MessageDigest.isEqual(macH, WifiCrypto.hmac(host.key, "host".toByteArray(), th))) {
                    throw AuthFailed(2, "the host failed authentication")
                }
                out.write(Proto.frame(Proto.AUTH_FIN, WifiCrypto.hmac(host.key, "client".toByteArray(), th)))
                val keys = WifiCrypto.sessionKeys(WifiCrypto.ecdh(eph, ephH), host.key, th)
                s.soTimeout = 0
                val u = DatagramSocket()
                u.trafficClass = TOS_EF
                u.receiveBufferSize = 4 shl 20
                u.soTimeout = 20 // also the tick for resolving stalled frames
                u.connect(InetSocketAddress(s.inetAddress, addr.port))
                Log.i(TAG, "Wi-Fi: authenticated with ${host.name} at $addr (UDP rcvbuf ${u.receiveBufferSize})")
                return WifiLink(s, u, keys)
            } catch (e: Exception) {
                try { s.close() } catch (_: Exception) {}
                throw e
            }
        }

        /**
         * Pairs with the host at [addr]. [askPin] is called with the host's name once the
         * host shows a PIN; it blocks until the user entered it (null = cancelled).
         */
        fun pair(store: WifiStore, addr: InetSocketAddress, askPin: (String) -> String?): String {
            val s = tcp(addr)
            s.use {
                val inp = DataInputStream(s.getInputStream())
                val out = s.getOutputStream()
                val name = store.deviceName.toByteArray().let { it.copyOf(minOf(it.size, 255)) }
                out.write(Proto.frame(Proto.PAIR_REQUEST, store.deviceId + byteArrayOf(name.size.toByte()) + name))
                val r = readExpect(inp, Proto.PAIR_READY)
                val hostId = r.copyOfRange(0, 16)
                val hostName = String(r, 17, (r[16].toInt() and 0xff).coerceAtMost(r.size - 17))
                s.soTimeout = 0 // the host waits for us while the user types
                val pin = askPin(hostName)?.filter { it.isDigit() } ?: throw IOException("pairing cancelled")
                s.soTimeout = 10_000
                val a = Spake2.newA(Spake2.wFromPin(pin, store.deviceId, hostId), store.deviceId, hostId)
                out.write(Proto.frame(Proto.PAIR_A, a.msg))
                val b = readExpect(inp, Proto.PAIR_B)
                val keys = a.finish(b.copyOfRange(0, 65), PAIR_AAD.toByteArray())
                if (!MessageDigest.isEqual(b.copyOfRange(65, 97), keys.confB)) throw AuthFailed(4, "wrong PIN")
                out.write(Proto.frame(Proto.PAIR_CONFIRM, keys.confA))
                readExpect(inp, Proto.PAIR_DONE)
                val pairKey = WifiCrypto.hkdf(null, keys.ke, "waylus pair key v1".toByteArray(), 32)
                store.savePairing(hostId, hostName, pairKey, "${addr.hostString}:${addr.port}")
                Log.i(TAG, "paired with $hostName")
                return hostName
            }
        }
    }
}

/**
 * Holds WIFI_MODE_FULL_LOW_LATENCY (no Wi-Fi power save) while a Wi-Fi session runs and
 * the app is in the foreground; released otherwise.
 */
object WifiPower {
    private var lock: WifiManager.WifiLock? = null
    private var streaming = false
    private var foreground = false
    /** Off for measurements: `--ez wifi_lock false`. */
    var enabled = true

    fun init(ctx: Context) {
        if (lock != null) return
        val wm = ctx.applicationContext.getSystemService(WifiManager::class.java)
        wifi = wm
        lock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "waylus").apply { setReferenceCounted(false) }
    }

    private var wifi: WifiManager? = null

    /** (frequency MHz, RSSI dBm, link speed Mbit/s) of the current Wi-Fi connection. */
    @Suppress("DEPRECATION")
    fun info(): Triple<Int, Int, Int> {
        val i = runCatching { wifi?.connectionInfo }.getOrNull() ?: return Triple(0, 0, 0)
        return Triple(i.frequency, i.rssi, i.linkSpeed)
    }

    @Synchronized fun streaming(on: Boolean) { streaming = on; update() }
    @Synchronized fun foreground(on: Boolean) { foreground = on; update() }

    private fun update() {
        val l = lock ?: return
        val want = enabled && streaming && foreground
        try {
            if (want && !l.isHeld) { l.acquire(); Log.i("waylus", "Wi-Fi low-latency lock acquired") }
            if (!want && l.isHeld) { l.release(); Log.i("waylus", "Wi-Fi low-latency lock released") }
        } catch (e: Exception) {
            Log.w("waylus", "Wi-Fi lock: $e")
        }
    }
}
