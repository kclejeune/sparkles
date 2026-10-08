package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.GraphViewSparkles
import io.github.kclejeune.sparkles.jena.internal.Handle
import io.github.kclejeune.sparkles.jena.internal.PrefixMapSparkles
import io.github.kclejeune.sparkles.jena.internal.QuadIter
import io.github.kclejeune.sparkles.jena.internal.Registry
import io.github.kclejeune.sparkles.jena.internal.Source
import io.github.kclejeune.sparkles.jena.internal.Tag
import io.github.kclejeune.sparkles.jena.internal.TxnState
import io.github.kclejeune.sparkles.jena.internal.FIRST_FIND_ROWS
import io.github.kclejeune.sparkles.jena.internal.decodeColumn
import io.github.kclejeune.sparkles.jena.internal.encodePattern
import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.ffi.QueryOpts
import io.github.kclejeune.sparkles.jena.internal.toInfo
import io.github.kclejeune.sparkles.jena.internal.toReceipt
import org.apache.jena.graph.Graph
import org.apache.jena.graph.Node
import org.apache.jena.query.ReadWrite
import org.apache.jena.query.TxnType
import org.apache.jena.riot.Lang
import org.apache.jena.riot.system.PrefixMap
import org.apache.jena.shared.AddDeniedException
import org.apache.jena.shared.DeleteDeniedException
import org.apache.jena.sparql.JenaTransactionException
import io.github.kclejeune.sparkles.jena.internal.DatasetGraphFindCompatibility
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.core.Transactional.Promote
import org.apache.jena.sparql.util.Context
import java.io.InputStream
import java.io.OutputStream
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiReadTxn
import java.nio.file.Path

/**
 * A Jena `DatasetGraph` backed by a Sparkles database (P04 §3). Open one with
 * [SparklesDatasets].
 *
 * Reads outside a transaction each read the newest commit. Writes need a write
 * transaction, unless the dataset was opened with [SparklesOptions.autocommit]. Queries and
 * updates through Jena's `QueryExecution` and `UpdateExec` run in Sparkles' engine when
 * they can, and in ARQ over `find()` when they use something only Java can evaluate.
 */
public class DatasetGraphSparkles internal constructor(
    internal val handle: Handle,
    /** the options this dataset was opened with */
    public val options: SparklesOptions,
    private val pinned: FfiReadTxn? = null,
) : DatasetGraphFindCompatibility(), AutoCloseable {
    @Volatile
    private var closed = false

    private val prefixMap = PrefixMapSparkles(handle)

    init {
        if (options.unionDefaultGraph) context.set(Sparkles.UNION_DEFAULT_GRAPH, true)
    }

    internal fun checkOpen() {
        if (closed) throw SparklesInvalidException("Closed", "the dataset is closed")
        handle.checkOpen()
    }

    /**
     * Where this dataset keeps each thread's transaction. Live datasets on one native handle
     * share the handle's slot, so that every alias sees the thread's transaction. A pinned
     * historical view has a slot of its own, because its read transaction is on another
     * snapshot and must not change what the live dataset reads or writes.
     */
    private val txns: ThreadLocal<TxnState?> = if (pinned == null) handle.txns else ThreadLocal()

    internal fun txn(): TxnState? = txns.get()

    /** What reads run on for this thread: its transaction, or the head snapshot. */
    internal fun source(): Source {
        checkOpen()
        pinned?.let { return Source.Read(it) }
        val t = txn() ?: return Source.Head(handle.ffi)
        val w = t.write
        if (w != null) {
            t.flush()
            return Source.Write(w)
        }
        return Source.Read(t.read ?: throw JenaTransactionException("the transaction has ended"))
    }

    // ------------------------------------------------------------------- reads ----

    internal fun findQuads(g: Node?, s: Node?, p: Node?, o: Node?, namedOnly: Boolean): QuadIter {
        val src = source()
        val first = src.find(encodePattern(g, s, p, o), FIRST_FIND_ROWS)
        return QuadIter(first, txn(), g != null && Quad.isUnionGraph(g), namedOnly)
    }

    internal fun countQuads(g: Node?, s: Node?, p: Node?, o: Node?): Long = source().count(encodePattern(g, s, p, o))

    override fun find(g: Node?, s: Node?, p: Node?, o: Node?): MutableIterator<Quad> =
        findQuads(g, s, p, o, namedOnly = false)

    override fun findNG(g: Node?, s: Node?, p: Node?, o: Node?): MutableIterator<Quad> =
        if (isWildcard(g)) findQuads(null, s, p, o, namedOnly = true) else findQuads(g, s, p, o, namedOnly = false)

    override fun findInDftGraph(s: Node?, p: Node?, o: Node?): MutableIterator<Quad> = findQuads(Quad.defaultGraphIRI, s, p, o, namedOnly = false)
    override fun findInSpecificNamedGraph(g: Node, s: Node?, p: Node?, o: Node?): MutableIterator<Quad> = findQuads(g, s, p, o, namedOnly = false)
    override fun findInAnyNamedGraphs(s: Node?, p: Node?, o: Node?): MutableIterator<Quad> = findQuads(null, s, p, o, namedOnly = true)

    override fun contains(g: Node?, s: Node?, p: Node?, o: Node?): Boolean =
        source().contains(encodePattern(g, s, p, o))

    override fun contains(quad: Quad): Boolean = contains(quad.graph, quad.subject, quad.predicate, quad.`object`)

    override fun isEmpty(): Boolean = !contains(null, null, null, null)

    /** The number of named graphs, as Jena's `DatasetGraph.size` means. */
    override fun size(): Long = graphNames().size.toLong()

    private fun graphNames(): List<Node> = decodeColumn(source().graphNames())

    override fun listGraphNodes(): MutableIterator<Node> = graphNames().toMutableList().iterator()

    override fun containsGraph(graphNode: Node): Boolean {
        if (Quad.isDefaultGraph(graphNode) || Quad.isUnionGraph(graphNode)) return true
        return contains(graphNode, Node.ANY, Node.ANY, Node.ANY)
    }

    override fun getDefaultGraph(): Graph = GraphViewSparkles(this, Quad.defaultGraphNodeGenerated)

    override fun getUnionGraph(): Graph = GraphViewSparkles(this, Quad.unionGraph)

    override fun getGraph(graphNode: Node): Graph = when {
        Quad.isDefaultGraph(graphNode) -> getDefaultGraph()
        Quad.isUnionGraph(graphNode) -> getUnionGraph()
        else -> GraphViewSparkles(this, graphNode)
    }

    override fun prefixes(): PrefixMap = prefixMap

    override fun getContext(): Context = super.getContext()

    // ------------------------------------------------------------------ writes ----

    /** Run a write: in the thread's transaction, promoting it if it may, or on its own. */
    private inline fun mutate(op: (TxnState) -> Unit) {
        checkOpen()
        val t = txn()
        if (t == null) {
            if (!options.autocommit) {
                throw JenaTransactionException("Not in a write transaction (see SparklesOptions.autocommit)")
            }
            begin(TxnType.WRITE)
            try {
                op(txn()!!)
                commit()
            } finally {
                end()
            }
            return
        }
        op(writeState(t))
    }

    /** The thread's transaction as a write transaction, promoting a `READ_PROMOTE` one. */
    internal fun writeState(t: TxnState): TxnState {
        if (t.mode == ReadWrite.WRITE) return t
        if (t.type == TxnType.READ) throw JenaTransactionException("Not in a write transaction: the transaction is READ")
        val mode = if (t.type == TxnType.READ_COMMITTED_PROMOTE) Promote.READ_COMMITTED else Promote.ISOLATED
        if (!promoteState(t, mode)) {
            throw JenaTransactionException("Can't promote the transaction: a commit has landed since it began")
        }
        return t
    }

    /** The thread's write transaction for an operation that needs one (updates). */
    internal fun writeTxnForRequest(): TxnState? {
        checkOpen()
        val t = txn() ?: return null
        return writeState(t)
    }

    /**
     * Run SPARQL Update text in the thread's write transaction. Native errors are not
     * mapped, so that the update engine can fall back on a syntax error.
     */
    internal fun nativeUpdate(text: String, opts: QueryOpts) {
        val t = writeTxnForRequest() ?: throw JenaTransactionException("Not in a write transaction")
        t.flush()
        t.write!!.update(text, opts)
    }

    override fun add(quad: Quad): Unit = add(quad.graph, quad.subject, quad.predicate, quad.`object`)

    override fun delete(quad: Quad): Unit = delete(quad.graph, quad.subject, quad.predicate, quad.`object`)

    override fun add(g: Node?, s: Node, p: Node, o: Node) {
        if (g != null && Quad.isUnionGraph(g)) throw AddDeniedException("Can't add to the union graph")
        mutate { it.addOp(Tag.OP_ADD, graphOrDefault(g), s, p, o) }
    }

    override fun delete(g: Node?, s: Node, p: Node, o: Node) {
        if (g != null && Quad.isUnionGraph(g)) throw DeleteDeniedException("Can't remove from the union graph")
        mutate { it.addOp(Tag.OP_DELETE, graphOrDefault(g), s, p, o) }
    }

    private fun graphOrDefault(g: Node?): Node = if (g == null || g === Quad.tripleInQuad) Quad.defaultGraphIRI else g

    /** One native call: the matches are never sent to Java. */
    override fun deleteAny(g: Node?, s: Node?, p: Node?, o: Node?) {
        if (g != null && Quad.isUnionGraph(g)) throw DeleteDeniedException("Can't remove from the union graph")
        mutate {
            it.flush()
            val w = it.write!!
            ffi { w.removeMatching(encodePattern(g, s, p, o)) }
        }
    }

    override fun clear(): Unit = deleteAny(Node.ANY, Node.ANY, Node.ANY, Node.ANY)

    override fun addGraph(graphName: Node, graph: Graph) {
        mutate { t ->
            val g = graphOrDefault(graphName)
            val it = graph.find()
            try {
                while (it.hasNext()) {
                    val tr = it.next()
                    t.addOp(Tag.OP_ADD, g, tr.subject, tr.predicate, tr.`object`)
                }
            } finally {
                it.close()
            }
        }
        graph.prefixMapping.nsPrefixMap.forEach { (p, u) -> prefixMap.add(p, u) }
    }

    override fun removeGraph(graphName: Node): Unit = deleteAny(graphName, Node.ANY, Node.ANY, Node.ANY)

    // ------------------------------------------------------------ transactions ----

    override fun supportsTransactions(): Boolean = true

    override fun supportsTransactionAbort(): Boolean = true

    override fun begin(type: TxnType) {
        checkOpen()
        handle.checkNoSink()
        if (txn() != null) throw JenaTransactionException("Currently in an active transaction")
        if (type != TxnType.READ && pinned != null) throw JenaTransactionException("historical views are read-only")
        val t = TxnState(handle, type)
        if (type == TxnType.WRITE) {
            val w = ffi { handle.ffi.beginWrite(null, true) }
                ?: throw JenaTransactionException("the write transaction did not start")
            t.write = w
            handle.openWrites.add(t)
        } else {
            val r = pinned?.fork() ?: ffi { handle.ffi.beginRead() }
            t.read = r
            if (type != TxnType.READ) t.baseSeq = r.commitSeq().toLong()
        }
        txns.set(t)
    }

    override fun begin(readWrite: ReadWrite): Unit = begin(TxnType.convert(readWrite))

    override fun begin(): Unit = begin(TxnType.READ_PROMOTE)

    override fun promote(mode: Promote): Boolean {
        val t = txn() ?: throw JenaTransactionException("Not in a transaction")
        if (t.mode == ReadWrite.WRITE) return true
        if (t.type == TxnType.READ) return false
        return promoteState(t, mode)
    }

    /**
     * Promote a read transaction: `ISOLATED` succeeds only if no commit has landed since
     * it began, checked again once the writer lock is held; `READ_COMMITTED` waits for the
     * lock and continues from the newest commit (P04 §3.3).
     */
    private fun promoteState(t: TxnState, mode: Promote): Boolean {
        val expect = if (mode == Promote.ISOLATED) t.baseSeq.toULong() else null
        val w = ffi { handle.ffi.beginWrite(expect, true) } ?: return false
        t.read?.close()
        t.read = null
        t.write = w
        t.mode = ReadWrite.WRITE
        handle.openWrites.add(t)
        return true
    }

    override fun commit() {
        val t = txn() ?: throw JenaTransactionException("Not in an active transaction")
        try {
            t.checkNotAbortedByClose()
            val w = t.write
            if (w != null) {
                t.flush()
                val r = ffi { w.commit() }
                handle.lastReceipt.set(r.toReceipt())
            }
        } catch (e: RuntimeException) {
            t.write?.abort()
            throw if (e is JenaTransactionException) e else JenaTransactionException("the commit failed: ${e.message}", e)
        } finally {
            finish(t)
        }
    }

    override fun abort() {
        val t = txn() ?: throw JenaTransactionException("Not in an active transaction")
        try {
            t.write?.abort()
        } finally {
            finish(t)
        }
    }

    override fun end() {
        val t = txn() ?: return
        if (t.mode == ReadWrite.WRITE) {
            try {
                t.write?.abort()
            } finally {
                finish(t)
            }
            throw JenaTransactionException("end() of a write transaction that was neither committed nor aborted: it has been aborted")
        }
        finish(t)
    }

    private fun finish(t: TxnState) {
        t.release()
        txns.remove()
    }

    override fun transactionMode(): ReadWrite? = txn()?.mode

    override fun transactionType(): TxnType? = txn()?.type

    override fun isInTransaction(): Boolean = txn() != null

    // ---------------------------------------------------------------- Sparkles ----

    /** The receipt of the last commit this thread made on the dataset, or `null`. */
    public fun lastReceipt(): CommitReceipt? = handle.lastReceipt.get()

    /** The newest commit. */
    public fun headCommit(): CommitInfo {
        checkOpen()
        return ffi { handle.ffi.headCommit() }.toInfo()
    }

    /** The dataset's id, a UUID made with the database. */
    public fun datasetId(): String {
        checkOpen()
        return ffi { handle.ffi.datasetId() }
    }

    internal fun checkNoTxn(what: String) {
        checkOpen()
        handle.checkNoSink()
        if (txn() != null) {
            throw JenaTransactionException("$what runs outside a transaction, and this thread is in one")
        }
        if (options.readOnly || pinned != null) throw SparklesNotPermittedException("NotPermitted", "the dataset was opened read-only")
    }

    internal fun checkCapture(what: String) {
        checkOpen()
        handle.checkNoSink()
        if (txn() != null || pinned != null) throw JenaTransactionException("$what runs outside a transaction on the live dataset")
    }

    /** Bulk-load files in one commit. Each file's format comes from its extension (`.ttl`, `.nq.gz`, …). */
    public fun loadFiles(paths: List<Path>): CommitReceipt = loadFiles(paths, null)

    /** Bulk-load files in one commit, with their triples in `graph` (quads keep their graphs). */
    public fun loadFiles(paths: List<Path>, graph: Node?): CommitReceipt {
        checkNoTxn("loadFiles")
        val g = graph?.takeUnless { Quad.isDefaultGraph(it) }?.uri
        val r = ffi { handle.ffi.loadFiles(paths.map { it.toAbsolutePath().toString() }, g) }.toReceipt()
        handle.lastReceipt.set(r)
        return r
    }

    /** Load RDF data with Sparkles' parsers in one commit. */
    public fun load(input: InputStream, lang: Lang): CommitReceipt = load(input, lang, null)

    /** Load RDF data with Sparkles' parsers in one commit, resolving relative IRIs against `base`. */
    public fun load(input: InputStream, lang: Lang, base: String?): CommitReceipt {
        checkNoTxn("load")
        val bytes = input.readAllBytes()
        val r = ffi { handle.ffi.loadBytes(bytes, lang.contentType.contentTypeStr, base, null) }.toReceipt()
        handle.lastReceipt.set(r)
        return r
    }

    /**
     * A `StreamRDF` that writes what Jena's parsers send it to one write transaction, in
     * batches of 65,536 quads, and commits when the stream finishes. Blank node labels are
     * scoped to the load.
     */
    public fun bulkSink(): SparklesBulkSink {
        checkNoTxn("bulkSink")
        return SparklesBulkSink(this)
    }

    internal fun setLastReceipt(r: CommitReceipt) = handle.lastReceipt.set(r)

    /** A read-only dataset pinned to the selected commit. */
    public fun at(reference: String): DatasetGraphSparkles {
        checkOpen()
        val read = ffi { handle.ffi.beginReadAt(reference) }
        handle.refs.incrementAndGet()
        return DatasetGraphSparkles(handle, options.toBuilder().readOnly(true).build(), read)
    }

    public fun at(commit: Long): DatasetGraphSparkles {
        require(commit >= 0) { "commit must be nonnegative" }
        return at("commit:$commit")
    }

    internal fun isPinned(): Boolean = pinned != null
    internal fun pinnedReference(): String? = pinned?.let { "commit:${ffi { it.commitSeq() }}" }

    public fun snapshots(): SparklesSnapshots = SparklesSnapshots(this)
    public fun history(): SparklesHistory = SparklesHistory(this)
    public fun settings(): SparklesSettings = SparklesSettings(this)
    public fun indexes(): SparklesIndexes = SparklesIndexes(this)
    public fun reasoning(): SparklesReasoning = SparklesReasoning(this)
    public fun validation(): SparklesValidation = SparklesValidation(this)
    public fun backups(repository: SparklesBackupRepository): SparklesBackups = SparklesBackups(this, repository)
    public fun queries(): SparklesQueries = SparklesQueries(this)
    public fun schema(): SparklesSchema = SparklesSchema(this)
    public fun graphql(): SparklesGraphQl = SparklesGraphQl(this)

    /** Serialize one committed snapshot with bounded native byte batches. */
    public fun dump(output: OutputStream, lang: Lang) {
        checkOpen()
        if (txn() != null) throw JenaTransactionException("dump runs outside a transaction")
        val read = pinned?.fork() ?: ffi { handle.ffi.beginRead() }
        read.use { r ->
            ffi { r.dumpCursor(lang.contentType.contentTypeStr) }.use { cursor ->
                SparklesOperation().use { operation ->
                    try {
                        do {
                            checkOpen()
                            val batch = ffi { cursor.nextChunk(65536u, operation.native) }
                            output.write(batch.batch)
                        } while (!batch.done)
                    } finally { cursor.release() }
                }
            }
        }
    }

    public fun dump(path: Path) {
        checkOpen()
        if (txn() != null) throw JenaTransactionException("dump runs outside a transaction")
        val lang = org.apache.jena.riot.RDFLanguages.filenameToLang(path.toString())
            ?: throw IllegalArgumentException("unknown RDF format: $path")
        java.nio.file.Files.newOutputStream(path).use { dump(it, lang) }
    }

    public fun compact(): Unit = SparklesOperation().use { compact(it) }
    public fun compact(operation: SparklesOperation) {
        checkNoTxn("compact")
        ffi { handle.ffi.compact(operation.native) }
    }
    /** Drop cached query results without changing the dataset. */
    public fun clearCache() { checkOpen(); ffi { handle.ffi.clearCache() } }
    public fun explain(query: String): org.apache.jena.atlas.json.JsonObject = SparklesOperation().use { explain(query, it) }
    public fun explain(query: String, operation: SparklesOperation): org.apache.jena.atlas.json.JsonObject { checkCapture("explain"); return document(ffi { handle.ffi.explain(query, operation.native) }).asObject }
    public fun cloneToMemory(): DatasetGraphSparkles = SparklesOperation().use { cloneToMemory(it) }
    public fun cloneToMemory(operation: SparklesOperation): DatasetGraphSparkles {
        checkCapture("cloneToMemory")
        return DatasetGraphSparkles(Registry.fromNative(ffi { handle.ffi.cloneToMemory(operation.native) }, options), options)
    }

    public fun cloneTo(path: Path): Unit = SparklesOperation().use { cloneTo(path, it) }
    public fun cloneTo(path: Path, operation: SparklesOperation) {
        checkCapture("cloneTo")
        ffi { handle.ffi.cloneTo(path.toAbsolutePath().toString(), operation.native) }
    }
    public fun backup(path: Path): Path {
        checkCapture("backup")
        return Path.of(ffi { handle.ffi.backup(path.toAbsolutePath().toString()) })
    }

    /** Counts of the queries and updates that ran in Sparkles and in ARQ. */
    public fun stats(): DatasetStats = DatasetStats(
        handle.nativeQueries.get(),
        handle.fallbackQueries.get(),
        handle.nativeUpdates.get(),
        handle.fallbackUpdates.get(),
    )

    /**
     * Release this handle. The native dataset closes, and its directory lock is released,
     * when the last handle on it in this JVM is closed.
     */
    override fun close() {
        if (closed) return
        handle.closeSinksFor(this)
        if (pinned != null) txns.get()?.let { finish(it) }
        pinned?.close()
        closed = true
        Registry.release(handle)
    }

    public fun isClosed(): Boolean = closed
    public fun branches(): SparklesBranches = SparklesBranches(this)
    public fun branch(name: String): DatasetGraphSparkles {
        checkCapture("branch")
        require(pinned == null) { "branch opens operate on the live dataset" }
        handle.checkNoSink()
        Registry.checkNoTransactionsForDataset(handle.ownerDatasetId)
        return DatasetGraphSparkles(Registry.fromNative(ffi { handle.ffi.branch(name) }, options), options)
    }

    override fun toString(): String = "DatasetGraphSparkles(${handle.key ?: "memory"})"
}
