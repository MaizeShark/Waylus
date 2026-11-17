// SPDX-License-Identifier: GPL-3.0-or-later
package dev.waylus

/**
 * Reed-Solomon erasure decoding over GF(2^8), compatible with the host's
 * `reed-solomon-erasure` crate (the Backblaze construction: polynomial 0x11D, encoding
 * matrix = Vandermonde(n x k) * inverse(top k x k)). Only decoding is needed here.
 */
object ReedSolomon {
    private val EXP = IntArray(512)
    private val LOG = IntArray(256)
    /** MUL[c][x] = c * x. */
    private val MUL: Array<ByteArray>

    init {
        var b = 1
        for (i in 0 until 255) {
            EXP[i] = b
            LOG[b] = i
            b = b shl 1
            if (b and 0x100 != 0) b = b xor 0x11D
        }
        for (i in 255 until 512) EXP[i] = EXP[i - 255]
        MUL = Array(256) { c -> ByteArray(256) { x -> mul(c, x).toByte() } }
    }

    fun mul(a: Int, b: Int): Int = if (a == 0 || b == 0) 0 else EXP[LOG[a] + LOG[b]]
    private fun inv(a: Int): Int = EXP[255 - LOG[a]]
    private fun exp(a: Int, n: Int): Int = if (n == 0) 1 else if (a == 0) 0 else EXP[(LOG[a] * n) % 255]

    private val matrices = HashMap<Long, Array<IntArray>>()

    /** Encoding matrix (k+m rows, k columns), cached. */
    @Synchronized
    fun encodingMatrix(k: Int, m: Int): Array<IntArray> = matrices.getOrPut(k.toLong() shl 32 or m.toLong()) {
        val n = k + m
        val v = Array(n) { r -> IntArray(k) { c -> exp(r, c) } }
        val topInv = invert(Array(k) { v[it].copyOf() })
        Array(n) { r -> IntArray(k) { c -> var s = 0; for (j in 0 until k) s = s xor mul(v[r][j], topInv[j][c]); s } }
    }

    /** Gauss-Jordan inversion over GF(2^8). Throws if singular (never for valid inputs). */
    fun invert(a: Array<IntArray>): Array<IntArray> {
        val k = a.size
        val inv = Array(k) { r -> IntArray(k) { c -> if (r == c) 1 else 0 } }
        for (col in 0 until k) {
            var piv = col
            while (piv < k && a[piv][col] == 0) piv++
            require(piv < k) { "singular matrix" }
            if (piv != col) {
                val t = a[piv]; a[piv] = a[col]; a[col] = t
                val u = inv[piv]; inv[piv] = inv[col]; inv[col] = u
            }
            val s = inv(a[col][col])
            for (c in 0 until k) { a[col][c] = mul(a[col][c], s); inv[col][c] = mul(inv[col][c], s) }
            for (r in 0 until k) {
                val f = a[r][col]
                if (r == col || f == 0) continue
                for (c in 0 until k) {
                    a[r][c] = a[r][c] xor mul(f, a[col][c])
                    inv[r][c] = inv[r][c] xor mul(f, inv[col][c])
                }
            }
        }
        return inv
    }

    /**
     * Fills the missing data shards (indices < k) of [shards] (k data + m parity, null =
     * missing, all present ones [size] bytes). Needs at least k present shards. Returns
     * how many data shards were recovered.
     */
    fun reconstructData(shards: Array<ByteArray?>, k: Int, m: Int, size: Int): Int {
        val missing = (0 until k).filter { shards[it] == null }
        if (missing.isEmpty()) return 0
        val present = shards.indices.filter { shards[it] != null }.take(k)
        require(present.size == k) { "not enough shards" }
        val enc = encodingMatrix(k, m)
        val dec = invert(Array(k) { enc[present[it]].copyOf() })
        for (mi in missing) {
            val out = ByteArray(size)
            val row = dec[mi]
            for (j in 0 until k) {
                val c = row[j]
                if (c == 0) continue
                val t = MUL[c]
                val src = shards[present[j]]!!
                for (x in 0 until size) out[x] = (out[x].toInt() xor t[src[x].toInt() and 0xff].toInt()).toByte()
            }
            shards[mi] = out
        }
        return missing.size
    }
}
