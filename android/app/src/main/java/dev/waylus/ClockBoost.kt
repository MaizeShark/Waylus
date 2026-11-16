// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.os.Process
import android.os.SystemClock
import android.util.Log

/**
 * Keeps the CPU clocks up for a while after pen/touch input.
 *
 * On some SoCs (measured on the MediaTek MT8781) hardware decode time follows the CPU
 * clock: ~19 ms per 1080p frame at idle clocks, ~11.5 ms while Android's own input
 * boost is active. That boost ends shortly after the pen lifts, so the frames you look
 * at between strokes decode slowly again. While the window is open this runs one busy
 * thread per CPU cluster (pinned with `taskset`, since the scheduler would otherwise
 * put all of them on big cores, and the decoder's driver work runs on the little ones),
 * at the lowest priority: real work always preempts them, but each cluster's frequency
 * governor sees the load and keeps its clock up.
 *
 * Costs battery and heat while active; [holdMs] = 0 disables it.
 */
class ClockBoost(private val holdMs: Long, private val threadsPerCluster: Int) {
    private val lock = Object()
    @Volatile private var until = 0L
    @Volatile private var alive = true
    @Volatile private var sink = 0L
    private val clusters = if (holdMs > 0) cpuClusters() else emptyList()
    private val workers = clusters.flatMap { cpus -> List(threadsPerCluster) { cpus } }.mapIndexed { i, cpus ->
        Thread({ loop(cpus) }, "waylus-boost-$i").apply { isDaemon = true; start() }
    }

    init {
        Log.i(TAG, if (holdMs > 0) "clock boost: ${holdMs / 1000} s after input, clusters $clusters" else "clock boost off")
    }

    /** Input happened: (re)open the boost window. Cheap, call from every event. */
    fun poke() {
        if (holdMs <= 0) return
        val now = SystemClock.uptimeMillis()
        val wasIdle = now >= until
        until = now + holdMs
        if (wasIdle) synchronized(lock) { lock.notifyAll() }
    }

    /** Close the window now (app in background). */
    fun idle() {
        until = 0
    }

    fun shutdown() {
        alive = false
        until = 0
        synchronized(lock) { lock.notifyAll() }
    }

    private fun loop(cpus: List<Int>) {
        Process.setThreadPriority(Process.THREAD_PRIORITY_LOWEST)
        pin(Process.myTid(), cpus)
        var x = sink
        while (alive) {
            if (SystemClock.uptimeMillis() < until) {
                // ~1 ms of pure ALU work per check; no syscalls, no memory traffic.
                for (i in 0 until 200_000) x = x * 6364136223846793005L + 1442695040888963407L
                sink = x
            } else {
                synchronized(lock) {
                    if (alive && SystemClock.uptimeMillis() >= until) lock.wait(1000)
                }
            }
        }
    }

    companion object {
        private const val TAG = "waylus"

        /** CPU clusters (cpufreq policies) as lists of CPU numbers, e.g. [[0..5], [6, 7]]. */
        fun cpuClusters(): List<List<Int>> = runCatching {
            java.io.File("/sys/devices/system/cpu/cpufreq").listFiles { f -> f.name.startsWith("policy") }!!
                .sortedBy { it.name.removePrefix("policy").toInt() }
                .map { p -> java.io.File(p, "related_cpus").readText().trim().split(Regex("\\s+")).map { it.toInt() } }
                .filter { it.isNotEmpty() }
        }.getOrElse { listOf(emptyList()) } // unknown topology: one unpinned thread

        /** sched_setaffinity for one of our own threads, via toybox `taskset` (no NDK needed). */
        private fun pin(tid: Int, cpus: List<Int>) {
            if (cpus.isEmpty()) return
            val mask = cpus.fold(0L) { m, c -> m or (1L shl c) }
            runCatching {
                val p = ProcessBuilder("taskset", "-p", java.lang.Long.toHexString(mask), tid.toString())
                    .redirectErrorStream(true).start()
                val out = p.inputStream.bufferedReader().readText().trim()
                if (p.waitFor() != 0) Log.w(TAG, "taskset failed: $out")
            }.onFailure { Log.w(TAG, "taskset: $it") }
        }
    }
}
