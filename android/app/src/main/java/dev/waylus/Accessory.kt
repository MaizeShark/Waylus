// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.ParcelFileDescriptor
import android.util.Log
import java.io.EOFException
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.SocketTimeoutException
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * USB transport without adb: Android Open Accessory (see host/src/aoa.rs).
 *
 * One per process. The accessory driver allows a single open and a blocked read()
 * can't be interrupted, so the file stays open across sessions (and activity
 * restarts) until the cable is unplugged. A reader thread moves data into a queue so
 * the session can time out when the host goes silent.
 */
object Accessory : Connection.Link {
    private const val TAG = "waylus"
    private const val ACTION_PERMISSION = "dev.waylus.USB_PERMISSION"
    /** The accessory driver completes reads in units of up to 16 KiB; asking for less can lose data. */
    private const val CHUNK = 16384
    private val EOF = ByteArray(0)

    private var pfd: ParcelFileDescriptor? = null
    private var out: FileOutputStream? = null
    private val chunks = LinkedBlockingQueue<ByteArray>()
    @Volatile private var open = false
    private var askedFor: UsbAccessory? = null

    fun find(ctx: Context): UsbAccessory? =
        ctx.getSystemService(UsbManager::class.java).accessoryList?.firstOrNull { it.manufacturer == "Waylus" && it.model == "Waylus" }

    /** Opens the accessory if needed. Returns null while there is none or permission is pending. */
    @Synchronized
    fun open(ctx: Context): Connection.Link? {
        if (open) return this
        val usb = ctx.getSystemService(UsbManager::class.java)
        val acc = find(ctx) ?: return null
        if (!usb.hasPermission(acc)) {
            if (askedFor != acc) {
                askedFor = acc
                Log.i(TAG, "asking for USB accessory permission")
                val pi = PendingIntent.getBroadcast(ctx, 0, Intent(ACTION_PERMISSION).setPackage(ctx.packageName), PendingIntent.FLAG_MUTABLE)
                usb.requestPermission(acc, pi)
            }
            return null
        }
        val p = usb.openAccessory(acc) ?: run { Log.w(TAG, "openAccessory failed"); return null }
        Log.i(TAG, "USB accessory open: ${acc.description} ${acc.version}")
        pfd = p
        out = FileOutputStream(p.fileDescriptor)
        chunks.clear()
        open = true
        Thread({ readLoop(p) }, "waylus-usb").apply { isDaemon = true; start() }
        return this
    }

    private fun readLoop(p: ParcelFileDescriptor) {
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
        val input = FileInputStream(p.fileDescriptor)
        try {
            while (true) {
                val b = ByteArray(CHUNK)
                val n = input.read(b, 0, CHUNK)
                if (n < 0) break
                if (n > 0) chunks.put(if (n == CHUNK) b else b.copyOf(n))
            }
        } catch (e: IOException) {
            Log.i(TAG, "USB accessory closed: $e")
        }
        synchronized(this) {
            open = false
            try { p.close() } catch (_: IOException) {}
            pfd = null
            out = null
        }
        chunks.put(EOF)
    }

    // --- Connection.Link ---

    override val needsSync = true
    override val input: InputStream = Input()
    override val output: OutputStream = object : OutputStream() {
        override fun write(b: Int) = write(byteArrayOf(b.toByte()))
        /** One write() = one USB transfer; callers pass whole messages. */
        override fun write(b: ByteArray, off: Int, len: Int) = (out ?: throw IOException("accessory closed")).write(b, off, len)
    }

    /** Kept open across sessions; only unplugging ends it. */
    override fun close() {}

    override fun drain() {
        (input as Input).cur = null
        while (true) {
            val c = chunks.poll() ?: break
            if (c === EOF) { chunks.put(EOF); break }
        }
    }

    override fun awaitSync(pattern: ByteArray, timeoutMs: Long): Boolean {
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        var matched = 0
        val inp = input as Input
        while (true) {
            val left = (deadline - System.nanoTime()) / 1_000_000
            if (left <= 0) return false
            val b = inp.readByte(left) ?: return false
            matched = if (b == pattern[matched]) matched + 1 else if (b == pattern[0]) 1 else 0
            if (matched == pattern.size) return true
        }
    }

    private class Input : InputStream() {
        var cur: ByteArray? = null
        var pos = 0

        /** Next chunk with data, waiting up to timeoutMs; null on timeout. */
        private fun fill(timeoutMs: Long): ByteArray? {
            cur?.let { if (pos < it.size) return it }
            val c = chunks.poll(timeoutMs, TimeUnit.MILLISECONDS) ?: return null
            if (c === EOF) { chunks.put(EOF); throw EOFException("accessory closed") }
            cur = c
            pos = 0
            return c
        }

        fun readByte(timeoutMs: Long): Byte? {
            val c = fill(timeoutMs) ?: return null
            return c[pos++]
        }

        override fun read(): Int {
            val c = fill(Connection.IDLE_TIMEOUT_MS) ?: throw SocketTimeoutException("host silent")
            return c[pos++].toInt() and 0xff
        }

        override fun read(b: ByteArray, off: Int, len: Int): Int {
            if (len == 0) return 0
            val c = fill(Connection.IDLE_TIMEOUT_MS) ?: throw SocketTimeoutException("host silent")
            val n = minOf(len, c.size - pos)
            System.arraycopy(c, pos, b, off, n)
            pos += n
            return n
        }

        override fun available(): Int = cur?.let { it.size - pos } ?: 0
    }
}
