package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.CommitInfo
import io.github.kclejeune.sparkles.jena.CommitReceipt
import io.github.kclejeune.sparkles.jena.SparklesInvalidException
import io.github.kclejeune.sparkles.jena.SparklesOptions
import io.github.kclejeune.sparkles.jena.internal.ffi.Capabilities
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiDataset
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiReadTxn
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiWriteTxn
import io.github.kclejeune.sparkles.jena.internal.ffi.Receipt
import org.apache.jena.query.ReadWrite
import org.apache.jena.query.TxnType
import java.time.Instant
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/** Operations sent to the worker in one batch at most, and bytes. */
internal const val WRITE_BATCH_OPS = 4096
internal const val WRITE_BATCH_BYTES = 1 shl 20

/**
 * One open native dataset, shared by every `DatasetGraphSparkles` that opened the same
 * directory in this JVM. The transaction of each thread lives here, so that all of them
 * see it.
 */
internal class Handle(val key: String?, val ffi: FfiDataset, val options: SparklesOptions) {
    val refs = AtomicInteger(1)

    @Volatile
    var closed = false

    val txns: ThreadLocal<TxnState?> = ThreadLocal()
    val lastReceipt: ThreadLocal<CommitReceipt?> = ThreadLocal()

    /** Write transactions that have not ended, aborted when the dataset closes. */
    val openWrites: MutableSet<TxnState> = ConcurrentHashMap.newKeySet()

    val capabilities: Capabilities by lazy { ffi { ffi.capabilities() } }

    /** The functions, aggregates and property functions Sparkles evaluates. */
    val knownFunctions: Set<String> by lazy { capabilities.functions.toHashSet() }
    val knownAggregates: Set<String> by lazy { capabilities.aggregates.toHashSet() }
    val knownPropertyFunctions: Set<String> by lazy { capabilities.propertyFunctions.toHashSet() }

    /** The dataset's prefixes, read once and kept up to date by this JVM's changes. */
    val prefixes: ConcurrentHashMap<String, String> by lazy { ConcurrentHashMap(ffi { ffi.prefixes() }) }

    val nativeQueries = AtomicLong()
    val fallbackQueries = AtomicLong()
    val nativeUpdates = AtomicLong()
    val fallbackUpdates = AtomicLong()

    fun checkOpen() {
        if (closed) throw SparklesInvalidException("Closed", "the dataset is closed")
    }

    /** Abort what is still open and free the native dataset. */
    fun shutdown() {
        closed = true
        for (t in openWrites) {
            try {
                t.write?.abort()
            } catch (_: RuntimeException) {
            }
        }
        openWrites.clear()
        ffi.close()
    }
}

/** The transaction of one thread. */
internal class TxnState(val handle: Handle, val type: TxnType) {
    var mode: ReadWrite = if (type == TxnType.WRITE) ReadWrite.WRITE else ReadWrite.READ
    var read: FfiReadTxn? = null
    var write: FfiWriteTxn? = null

    /** The commit the transaction started from (for promotion). */
    var baseSeq: Long = 0

    /** False once the transaction has ended; its iterators then throw. */
    @Volatile
    var active = true

    val writer = TermWriter()
    var ops = 0

    fun addOp(op: Int, g: org.apache.jena.graph.Node?, s: org.apache.jena.graph.Node, p: org.apache.jena.graph.Node, o: org.apache.jena.graph.Node) {
        writer.buf.put(op)
        writer.graph(g)
        writer.term(s)
        writer.term(p)
        writer.term(o)
        ops++
        if (ops >= WRITE_BATCH_OPS || writer.buf.size >= WRITE_BATCH_BYTES) flush()
    }

    /** Send the buffered writes to the worker. */
    fun flush() {
        if (ops == 0) return
        val bytes = writer.buf.toByteArray()
        writer.reset()
        ops = 0
        val w = write ?: return
        ffi { w.apply(bytes) }
    }

    /** Free the native objects. */
    fun release() {
        active = false
        read?.close()
        read = null
        write?.close()
        write = null
        handle.openWrites.remove(this)
    }
}

internal fun Receipt.toReceipt(): CommitReceipt = CommitReceipt(committed, commit.toInfo(), datasetId)

internal fun io.github.kclejeune.sparkles.jena.internal.ffi.CommitInfo.toInfo(): CommitInfo = CommitInfo(
    seq.toLong(),
    Instant.ofEpochMilli(timestampMs),
    kind,
    inserted.toLong(),
    deleted.toLong(),
    quads.toLong(),
)
