// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.util.Log
import java.net.Inet4Address
import java.util.concurrent.ConcurrentHashMap

/**
 * Finds Waylus hosts on the LAN via DNS-SD (`_waylus._tcp`, advertised by
 * `waylus --wifi`). Hosts are keyed by their id from the TXT record, so a paired host is
 * found again after its address changed.
 */
class WifiDiscovery(ctx: Context, private val onChange: () -> Unit) {
    class Host(val id: String, val name: String, val address: String)

    private val nsd = ctx.applicationContext.getSystemService(NsdManager::class.java)
    val hosts = ConcurrentHashMap<String, Host>()
    private val pending = ArrayDeque<NsdServiceInfo>()
    private var resolving = false
    private var listener: NsdManager.DiscoveryListener? = null

    @Synchronized
    fun start() {
        if (listener != null) return
        val l = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(type: String) { Log.i(TAG, "mDNS: looking for Waylus hosts") }
            override fun onDiscoveryStopped(type: String) {}
            override fun onStartDiscoveryFailed(type: String, code: Int) { Log.w(TAG, "mDNS discovery failed: $code"); synchronized(this@WifiDiscovery) { listener = null } }
            override fun onStopDiscoveryFailed(type: String, code: Int) {}
            override fun onServiceFound(info: NsdServiceInfo) { enqueue(info) }
            override fun onServiceLost(info: NsdServiceInfo) {
                // Keep the last known address; the host may come back at the same one.
            }
        }
        listener = l
        runCatching { nsd.discoverServices(TYPE, NsdManager.PROTOCOL_DNS_SD, l) }.onFailure { Log.w(TAG, "mDNS: $it"); listener = null }
    }

    @Synchronized
    fun stop() {
        val l = listener ?: return
        listener = null
        runCatching { nsd.stopServiceDiscovery(l) }
    }

    @Synchronized
    private fun enqueue(info: NsdServiceInfo) {
        pending.addLast(info)
        resolveNext()
    }

    @Synchronized
    @Suppress("DEPRECATION")
    private fun resolveNext() {
        if (resolving) return
        val info = pending.removeFirstOrNull() ?: return
        resolving = true
        nsd.resolveService(info, object : NsdManager.ResolveListener {
            override fun onResolveFailed(i: NsdServiceInfo, code: Int) {
                Log.d(TAG, "mDNS: resolving ${i.serviceName} failed ($code)")
                done()
            }

            override fun onServiceResolved(i: NsdServiceInfo) {
                val id = i.attributes["id"]?.let { String(it) }
                val host = i.host
                if (id != null && host is Inet4Address) {
                    val name = i.attributes["name"]?.let { String(it) } ?: i.serviceName
                    val h = Host(id, name, "${host.hostAddress}:${i.port}")
                    val old = hosts.put(id, h)
                    if (old?.address != h.address) {
                        Log.i(TAG, "mDNS: found $name at ${h.address}")
                        onChange()
                    }
                }
                done()
            }

            fun done() {
                synchronized(this@WifiDiscovery) { resolving = false }
                resolveNext()
            }
        })
    }

    companion object {
        private const val TAG = "waylus"
        private const val TYPE = "_waylus._tcp"
    }
}
