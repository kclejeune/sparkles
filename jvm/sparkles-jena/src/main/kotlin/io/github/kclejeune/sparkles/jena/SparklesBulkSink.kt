package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.Tag
import io.github.kclejeune.sparkles.jena.internal.TermWriter
import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiWriteTxn
import io.github.kclejeune.sparkles.jena.internal.toReceipt
import org.apache.jena.graph.Node
import org.apache.jena.graph.Triple
import org.apache.jena.riot.system.StreamRDF
import org.apache.jena.sparql.core.Quad

/**
 * A `StreamRDF` that loads what it is sent into one write transaction, sending quads in
 * batches of 65,536, and commits at [finish]. Closing it before [finish] aborts the load.
 * It holds the dataset's writer lock from [start] to [finish], so the thread that feeds
 * it must not begin a write transaction on the dataset meanwhile.
 */
public class SparklesBulkSink internal constructor(private val dsg: DatasetGraphSparkles) : StreamRDF, AutoCloseable {
    private var write: FfiWriteTxn? = null
    private val writer = TermWriter()
    private var ops = 0
    private var finished = false
    private var receipt: CommitReceipt? = null

    /** Quads sent so far. */
    public var count: Long = 0
        private set

    private fun txn(): FfiWriteTxn {
        check(!finished) { "the bulk load has finished" }
        write?.let { return it }
        val w = ffi { dsg.handle.ffi.beginWrite(null, false) }
            ?: throw IllegalStateException("the write transaction did not start")
        write = w
        return w
    }

    override fun start() {
        txn()
    }

    override fun triple(triple: Triple): Unit = add(Quad.defaultGraphIRI, triple.subject, triple.predicate, triple.`object`)

    override fun quad(quad: Quad) {
        val g = quad.graph
        add(if (g == null || g === Quad.tripleInQuad) Quad.defaultGraphIRI else g, quad.subject, quad.predicate, quad.`object`)
    }

    private fun add(g: Node, s: Node, p: Node, o: Node) {
        txn()
        writer.buf.put(Tag.OP_ADD)
        writer.graph(g)
        writer.term(s)
        writer.term(p)
        writer.term(o)
        ops++
        count++
        if (ops >= BATCH) flush()
    }

    private fun flush() {
        if (ops == 0) return
        val bytes = writer.buf.toByteArray()
        writer.reset()
        ops = 0
        val w = txn()
        ffi { w.apply(bytes) }
    }

    override fun base(base: String?) {}

    override fun prefix(prefix: String, iri: String) {
        dsg.prefixes().add(prefix, iri)
    }

    /** Send the rest and commit. */
    override fun finish() {
        val w = txn()
        try {
            flush()
            val r = ffi { w.commit() }.toReceipt()
            receipt = r
            dsg.setLastReceipt(r)
        } finally {
            finished = true
            w.close()
            write = null
        }
    }

    /** The receipt of the commit, once [finish] has run. */
    public fun receipt(): CommitReceipt? = receipt

    /** Abort the load if it has not finished. */
    override fun close() {
        val w = write ?: return
        write = null
        finished = true
        try {
            w.abort()
        } finally {
            w.close()
        }
    }

    private companion object {
        const val BATCH = 65_536
    }
}
