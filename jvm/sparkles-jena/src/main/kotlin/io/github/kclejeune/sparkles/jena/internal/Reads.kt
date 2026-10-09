package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.internal.ffi.FfiCursor
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiDataset
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiQuery
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiReadTxn
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiWriteTxn
import io.github.kclejeune.sparkles.jena.internal.ffi.FindResult
import io.github.kclejeune.sparkles.jena.internal.ffi.QueryOpts
import io.github.kclejeune.sparkles.jena.internal.ffi.SparklesJni
import org.apache.jena.graph.Node
import org.apache.jena.graph.Triple
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.core.Quad
import org.apache.jena.util.iterator.ClosableIterator

/** The first `find` batch, and the largest later one (P04 §5.2). */
internal const val FIRST_FIND_ROWS = 64
internal const val MAX_FIND_ROWS = 4096

/** What reads run on: the head snapshot, a read transaction, or a write transaction's view. */
internal sealed interface Source {
    fun find(pattern: ByteArray, firstRows: Int): FindResult
    fun count(pattern: ByteArray): Long
    fun contains(pattern: ByteArray): Boolean
    fun graphNames(): ByteArray
    fun prepareQuery(text: String, opts: QueryOpts): FfiQuery

    // The head and read transactions make their small, frequent calls through JNI (SparklesJni).
    class Head(private val ds: FfiDataset) : Source {
        override fun find(pattern: ByteArray, firstRows: Int) = ffi { SparklesJni.find(ds, pattern, firstRows) }
        override fun count(pattern: ByteArray) = ffi { ds.count(pattern).toLong() }
        override fun contains(pattern: ByteArray) = ffi { SparklesJni.contains(ds, pattern) }
        override fun graphNames() = ffi { ds.graphNames() }
        override fun prepareQuery(text: String, opts: QueryOpts) = ffi { SparklesJni.prepareQuery(ds, text, opts) }
    }

    class Read(private val t: FfiReadTxn) : Source {
        override fun find(pattern: ByteArray, firstRows: Int) = ffi { SparklesJni.find(t, pattern, firstRows) }
        override fun count(pattern: ByteArray) = ffi { t.count(pattern).toLong() }
        override fun contains(pattern: ByteArray) = ffi { SparklesJni.contains(t, pattern) }
        override fun graphNames() = ffi { t.graphNames() }
        override fun prepareQuery(text: String, opts: QueryOpts) = ffi { SparklesJni.prepareQuery(t, text, opts) }
    }

    class Write(private val t: FfiWriteTxn) : Source {
        override fun find(pattern: ByteArray, firstRows: Int) = ffi { t.find(pattern, firstRows.toUInt()) }
        override fun count(pattern: ByteArray) = ffi { t.count(pattern).toLong() }
        override fun contains(pattern: ByteArray) = ffi { t.contains(pattern) }
        override fun graphNames() = ffi { t.graphNames() }
        override fun prepareQuery(text: String, opts: QueryOpts) = ffi { t.prepareQuery(text, opts) }
    }
}

/**
 * The quads of a `find`: the first batch came with the call, and later batches are
 * fetched when the current one is used up, doubling from 64 quads to 4096. Inside a
 * transaction it throws `JenaTransactionException` once the transaction has ended.
 */
internal class QuadIter(
    first: FindResult,
    private val txn: TxnState?,
    /** the quads of the union graph carry `Quad.unionGraph` as their graph, as in Jena */
    private val unionGraph: Boolean,
    /** drop the default graph's quads (`findNG` with a wildcard) */
    private val namedOnly: Boolean,
) : MutableIterator<Quad>, org.apache.jena.atlas.lib.Closeable, AutoCloseable {
    private var cursor: FfiCursor? = first.cursor
    private val decoder = RowDecoder()
    private var batch: RowBatch = decoder.decode(first.batch)
    private var row = 0
    private var nextSize = FIRST_FIND_ROWS * 2
    private var pending: Quad? = null

    private fun checkTxn() {
        if (txn != null && !txn.active) throw JenaTransactionException("the transaction of this iterator has ended")
    }

    private fun fetch(): Boolean {
        val c = cursor ?: return false
        val b = ffi { SparklesJni.nextBatch(c, nextSize) }
        nextSize = minOf(nextSize * 2, MAX_FIND_ROWS)
        batch = decoder.decode(b.batch)
        row = 0
        if (b.done) closeCursor(drained = true)
        return batch.rows > 0
    }

    private fun decodeRow(): Quad {
        val i = row * 4
        val cells = batch.cells
        row++
        val g = if (unionGraph) Quad.unionGraph else decoder.node(cells[i])
        return Quad.create(g, decoder.node(cells[i + 1]), decoder.node(cells[i + 2]), decoder.node(cells[i + 3]))
    }

    override fun hasNext(): Boolean {
        checkTxn()
        while (pending == null) {
            if (row >= batch.rows && !fetch()) {
                closeCursor()
                return false
            }
            val q = decodeRow()
            if (namedOnly && q.isDefaultGraph) continue
            pending = q
        }
        return true
    }

    override fun next(): Quad {
        if (!hasNext()) throw NoSuchElementException()
        val q = pending!!
        pending = null
        return q
    }

    override fun remove() = throw UnsupportedOperationException("remove")

    /** Free the cursor. One that reported `done` has freed its state natively already. */
    private fun closeCursor(drained: Boolean = false) {
        cursor?.let {
            if (!drained) it.release()
            it.close()
        }
        cursor = null
    }

    override fun close() = closeCursor()
}

/** The triples of a [QuadIter], for a graph view. */
internal class TripleIter(private val quads: QuadIter) : ClosableIterator<Triple> {
    override fun hasNext(): Boolean = quads.hasNext()
    override fun next(): Triple = quads.next().asTriple()
    override fun remove() = throw UnsupportedOperationException("remove")
    override fun close() = quads.close()
}

/** The nodes of a batch of one column. */
internal fun decodeColumn(bytes: ByteArray): List<Node> {
    val d = RowDecoder()
    val b = d.decode(bytes)
    return (0 until b.rows).mapNotNull { d.node(b.cells[it]) }
}
