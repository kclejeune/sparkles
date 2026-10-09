package io.github.kclejeune.sparkles.jena.engine

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.SparklesFallback
import io.github.kclejeune.sparkles.jena.Sparkles
import io.github.kclejeune.sparkles.jena.internal.CancelWatcher
import io.github.kclejeune.sparkles.jena.internal.RowBatch
import io.github.kclejeune.sparkles.jena.internal.RowDecoder
import io.github.kclejeune.sparkles.jena.internal.ffi.ErrorKind
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiException
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiQuery
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiSelectCursor
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiQueryKind
import io.github.kclejeune.sparkles.jena.internal.ffi.InternalException
import io.github.kclejeune.sparkles.jena.internal.ffi.QueryOpts
import io.github.kclejeune.sparkles.jena.internal.ffi.SparklesJni
import io.github.kclejeune.sparkles.jena.internal.mapError
import io.github.kclejeune.sparkles.jena.SparklesInternalException
import org.apache.jena.atlas.io.IndentedLineBuffer
import org.apache.jena.atlas.io.IndentedWriter
import org.apache.jena.graph.Node
import org.apache.jena.query.Query
import org.apache.jena.query.QueryExecException
import org.apache.jena.query.Syntax
import org.apache.jena.sparql.algebra.Algebra
import org.apache.jena.sparql.algebra.Op
import org.apache.jena.sparql.algebra.Transformer
import org.apache.jena.sparql.algebra.op.OpLabel
import org.apache.jena.sparql.algebra.op.OpTable
import org.apache.jena.sparql.algebra.AlgebraQuad
import org.apache.jena.sparql.algebra.TransformGraphRename
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.core.Prologue
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.engine.Plan
import org.apache.jena.sparql.engine.PlanOp
import org.apache.jena.sparql.engine.QueryEngineFactory
import org.apache.jena.sparql.engine.QueryEngineRegistry
import org.apache.jena.sparql.engine.QueryIterator
import org.apache.jena.sparql.engine.binding.Binding
import org.apache.jena.sparql.engine.binding.BindingBase
import org.apache.jena.sparql.engine.iterator.QueryIteratorBase
import org.apache.jena.sparql.engine.main.QueryEngineMain
import org.apache.jena.sparql.serializer.SerializationContext
import org.apache.jena.sparql.serializer.SerializerRegistry
import org.apache.jena.sparql.syntax.ElementGroup
import org.apache.jena.sparql.util.Context
import org.slf4j.LoggerFactory

/**
 * The query engine (P04 §3.4): a whole query runs in Sparkles' planner and executor, and
 * its solutions come back to Jena as bindings in batches. A query that uses something only
 * Java can evaluate runs in ARQ over the dataset's `find()` (§3.5).
 */
public object QueryEngineSparkles {
    private val log = LoggerFactory.getLogger(QueryEngineSparkles::class.java)

    /** The factory registered with Jena's `QueryEngineRegistry`. */
    @JvmField
    public val factory: QueryEngineFactory = Factory

    /** Register the engine with Jena (done by Jena's initialization). */
    @JvmStatic
    @Synchronized
    public fun register() {
        if (!QueryEngineRegistry.get().contains(factory)) QueryEngineRegistry.addFactory(factory)
    }

    /** Remove the engine from Jena's registry. */
    @JvmStatic
    @Synchronized
    public fun unregister() {
        QueryEngineRegistry.removeFactory(factory)
    }

    private object Factory : QueryEngineFactory {
        override fun accept(query: Query, dataset: DatasetGraph, context: Context): Boolean = dataset is DatasetGraphSparkles

        override fun create(query: Query, dataset: DatasetGraph, inputBinding: Binding, context: Context): Plan =
            plan(query, dataset as DatasetGraphSparkles, inputBinding, context)

        /** Algebra executed directly runs in ARQ in Phase 1. */
        override fun accept(op: Op, dataset: DatasetGraph, context: Context?): Boolean =
            dataset is DatasetGraphSparkles && runCatching { org.apache.jena.sparql.algebra.OpAsQuery.asQuery(op) }.isSuccess

        override fun create(op: Op, dataset: DatasetGraph, inputBinding: Binding?, context: Context?): Plan =
            plan(org.apache.jena.sparql.algebra.OpAsQuery.asQuery(op), dataset as DatasetGraphSparkles,
                inputBinding ?: org.apache.jena.sparql.engine.binding.BindingFactory.empty(), context ?: dataset.context)
    }

    private fun plan(query: Query, dsg: DatasetGraphSparkles, input: Binding, context: Context): Plan {
        DescribeSparkles.select(query, context)
        val at = context.get<Any>(Sparkles.AT)
        if (at != null && !dsg.isPinned()) {
            val view = when (at) {
                is Number -> dsg.at(at.toLong())
                else -> dsg.at(at.toString())
            }
            try {
                context.set(Sparkles.RESOLVED_AT, view.pinnedReference())
                val inner = plan(query, view, input, context)
                return PlanOp(inner.op, null, OwnedViewIterator(inner.iterator(), view, context))
            } catch (e: Throwable) {
                view.close()
                throw e
            }
        }
        val mode = fallbackMode(dsg, context)
        val reason = if (mode == SparklesFallback.ALWAYS) {
            "the fallback mode is ALWAYS"
        } else {
            FallbackDetector.check(query, context, knownOf(dsg))
        }
        if (reason != null) {
            if (mode == SparklesFallback.NEVER) {
                throw QueryExecException("the query needs ARQ, and the fallback mode is NEVER: $reason")
            }
            log.debug("query runs in ARQ: {}", reason)
            dsg.handle.fallbackQueries.incrementAndGet()
            return arqPlan(query, dsg, input, context)
        }
        if (mode == SparklesFallback.AUTO && query.isDescribeType && query.queryPattern == null) {
            // DESCRIBE of named resources alone has one empty solution to find, and the
            // DESCRIBE handler asks Sparkles for the descriptions in one query
            dsg.handle.nativeQueries.incrementAndGet()
            return QueryEngineMain(query, dsg, input, context).plan
        }
        if (mode == SparklesFallback.AUTO && SmallQueries.route(query, dsg, context, input)) {
            dsg.handle.smallQueries.incrementAndGet()
            return arqPlan(query, dsg, input, context)
        }
        dsg.handle.nativeQueries.incrementAndGet()
        val text = sparklesText(query)
        val opts = requestOptions(dsg, context, input.takeUnless { it.isEmpty })
        val iter = QueryIterSparkles(dsg, query, text, opts, input, context, mode)
        return PlanOp(OpLabel.create("sparkles", OpTable.unit()), null, iter)
    }

    /** ARQ's plan for the query, with the union default graph when the request has it. */
    internal fun arqPlan(query: Query, dsg: DatasetGraphSparkles, input: Binding, context: Context): Plan {
        if (!unionDefaultGraph(dsg, context)) return QueryEngineMain(query, dsg, input, context).plan
        // as QueryEngineTDB does: quads, with the default graph renamed to the union graph
        var op = Algebra.compile(query)
        op = AlgebraQuad.quadize(op)
        op = Transformer.transform(TransformGraphRename(Quad.defaultGraphNodeGenerated, Quad.unionGraph), op)
        return QueryEngineMain(op, dsg, input, context).plan
    }

    /**
     * The text Sparkles runs. SELECT and ASK run as they are, and an ASK whose answer is
     * true gives Jena one empty solution. Jena applies the CONSTRUCT template and runs the
     * DESCRIBE handlers itself, so those forms become a SELECT.
     */
    internal fun sparklesText(query: Query): String {
        if (query.isSelectType || query.isAskType) return serialize(query)
        val q = query.cloneQuery()
        q.setQuerySelectType()
        q.setQueryResultStar(true)
        if (q.queryPattern == null) q.queryPattern = ElementGroup()
        return serialize(q)
    }

    /**
     * The query as ARQ syntax. Jena's `Query.serialize` tries to write each IRI relative to
     * the query's base, which costs two IRI parses per IRI. The parser has already made
     * every IRI absolute, and the base it used is Jena's system base unless the query set
     * one with `BASE`. Only then does the text need the base, and only then is it used.
     */
    private fun serialize(query: Query): String {
        if (query.explicitlySetBaseURI()) return query.serialize(Syntax.syntaxARQ)
        val out = IndentedLineBuffer()
        val factory = SerializerRegistry.get().getQuerySerializerFactory(Syntax.syntaxARQ)
        query.visit(factory.create(Syntax.syntaxARQ, Prologue(query.prefixMapping), out))
        return out.toString()
    }
}

/**
 * The solutions of a query run in Sparkles. The query is prepared when the iterator is
 * made and runs when Jena first asks for a binding, which is when Jena's first timeout
 * starts counting. A syntax error or `Unsupported` from Sparkles before any row replaces
 * it with ARQ's plan for the same query (the late check of P04 §3.5).
 */
internal class QueryIterSparkles(
    private val dsg: DatasetGraphSparkles,
    private val query: Query,
    text: String,
    opts: QueryOpts,
    input: Binding,
    private val context: Context,
    private val mode: SparklesFallback,
) : QueryIteratorBase(Context.getCancelSignal(context)) {
    private val input: Binding = input
    private val cancelSignal = Context.getCancelSignal(context)
    private val parent: Binding? = input.takeUnless { it.isEmpty }
    private var ffiQuery: FfiQuery? = dsg.source().prepareQuery(text, opts)
    private var cursor: FfiSelectCursor? = null
    private var started = false
    private var done = false
    /**
     * The prepared query holds no rows: its eager result reported `done`, which frees them
     * natively, or a cursor holds them. Its `release` is then skipped. A cursor's own
     * `release` always runs, because it also records the cursor's final statistics.
     */
    private var drained = false
    private var delegate: QueryIterator? = null
    private val decoder = RowDecoder()
    private var batch: RowBatch? = null
    private var row = 0
    private var vars: Array<Var> = emptyArray()

    private fun start() {
        started = true
        val q = ffiQuery ?: return
        // an ASK has one boolean to return, which needs no cursor
        if (context.get<Boolean>(Sparkles.STREAMING_EXECUTION) == true && !query.isAskType) {
            startCursor(q)
            return
        }
        val signal = cancelSignal
        if (signal != null) CancelWatcher.watch(q, signal)
        val e = try {
            SparklesJni.execute(q, FIRST_ROWS)
        } catch (e: FfiException.Engine) {
            if (e.kind == ErrorKind.SPARQL_SYNTAX || e.kind == ErrorKind.UNSUPPORTED) {
                fallBack(e.detail)
                return
            }
            release()
            throw mapError(e)
        } catch (e: InternalException) {
            release()
            throw SparklesInternalException("Internal", e.message ?: "a failure in the native library")
        } finally {
            CancelWatcher.unwatch(q)
        }
        vars = e.variables.map { Var.alloc(it) }.toTypedArray()
        if (e.kind == FfiQueryKind.ASK) {
            done = true
            drained = true
            release()
            // true is one solution that binds nothing, which is how Jena reads an ASK
            if (e.boolean) batch = RowBatch(1, 0, IntArray(0))
            return
        }
        batch = decoder.decode(e.batch)
        row = 0
        if (e.done) {
            done = true
            drained = true
            release()
        }
    }

    private fun startCursor(q: FfiQuery) {
        val signal = cancelSignal
        if (signal != null) CancelWatcher.watch(q, signal)
        val first = try {
            val c = q.openCursor(4096u, 1048576uL, context.get<Boolean>(Sparkles.STREAMING_STRICT) != true)
            cursor = c
            // the rows are the cursor's, so the prepared query holds none to release
            drained = true
            vars = c.variables().map { Var.alloc(it) }.toTypedArray()
            c.nextBatch(FIRST_ROWS.toUInt())
        } catch (e: FfiException.Engine) {
            if (e.kind == ErrorKind.SPARQL_SYNTAX || e.kind == ErrorKind.UNSUPPORTED) {
                fallBack(e.detail)
                return
            }
            release()
            throw mapError(e)
        } catch (e: InternalException) {
            release()
            throw SparklesInternalException("Internal", e.message ?: "a failure in the native library")
        } finally {
            CancelWatcher.unwatch(q)
        }
        batch = decoder.decode(first.batch)
        row = 0
        if (first.done) {
            done = true
            release()
        }
    }

    private fun fallBack(why: String) {
        release()
        if (mode == SparklesFallback.NEVER) {
            throw QueryExecException("Sparkles cannot run the query, and the fallback mode is NEVER: $why")
        }
        LOG.debug("query runs in ARQ after Sparkles refused it: {}", why)
        dsg.handle.nativeQueries.decrementAndGet()
        dsg.handle.fallbackQueries.incrementAndGet()
        delegate = QueryEngineSparkles.arqPlan(query, dsg, input, context).iterator()
    }

    private fun fetch(): Boolean {
        if (done) return false
        val q = ffiQuery ?: return false
        val watch = cursor != null && cancelSignal != null
        if (watch) CancelWatcher.watch(q, cancelSignal)
        val b = try {
            cursor?.nextBatch(LATER_ROWS.toUInt()) ?: q.nextBatch(LATER_ROWS.toUInt())
        } catch (e: FfiException.Engine) {
            release()
            throw mapError(e)
        } finally {
            if (watch) CancelWatcher.unwatch(q)
        }
        batch = decoder.decode(b.batch)
        row = 0
        if (b.done) {
            done = true
            drained = true
            release()
        }
        return batch!!.rows > 0
    }

    override fun hasNextBinding(): Boolean {
        if (!started) start()
        delegate?.let { return it.hasNext() }
        val b = batch ?: return false
        if (row < b.rows) return true
        return fetch()
    }

    override fun moveToNextBinding(): Binding {
        delegate?.let { return it.nextBinding() }
        val b = batch!!
        val n = vars.size
        val values = arrayOfNulls<Node>(n)
        val base = row * b.columns
        for (i in 0 until n) values[i] = decoder.node(b.cells[base + i])
        row++
        return BindingSparkles(parent, vars, values)
    }

    private fun release() {
        cursor?.let {
            it.release()
            it.close()
        }
        cursor = null
        ffiQuery?.let {
            if (!drained) it.release()
            it.close()
        }
        ffiQuery = null
    }

    override fun closeIterator() {
        delegate?.close()
        release()
    }

    override fun requestCancel() {
        delegate?.cancel()
        cursor?.cancel()
        ffiQuery?.cancel()
    }

    override fun output(out: IndentedWriter, sCxt: SerializationContext?) {
        out.print("QueryIterSparkles")
    }

    private companion object {
        val LOG = LoggerFactory.getLogger(QueryIterSparkles::class.java)
        const val FIRST_ROWS = 256
        const val LATER_ROWS = 8192
    }
}

/** A solution from Sparkles: the result's variables and an array of nodes (null is unbound). */
internal class BindingSparkles(
    parent: Binding?,
    private val vars: Array<Var>,
    private val values: Array<Node?>,
) : BindingBase(parent) {
    private fun index(v: Var): Int {
        for (i in vars.indices) if (vars[i] == v) return i
        return -1
    }

    override fun vars1(): MutableIterator<Var> =
        vars.indices.filter { values[it] != null }.map { vars[it] }.toMutableList().iterator()

    override fun size1(): Int = values.count { it != null }

    override fun isEmpty1(): Boolean = values.all { it == null }

    override fun contains1(v: Var): Boolean {
        val i = index(v)
        return i >= 0 && values[i] != null
    }

    override fun get1(v: Var): Node? {
        val i = index(v)
        return if (i >= 0) values[i] else null
    }

    override fun detachWithNewParent(newParent: Binding?): Binding = BindingSparkles(newParent, vars, values)
}

/** Releases a historical request view on completion, failure or cancellation. */
private class OwnedViewIterator(
    private val delegate: QueryIterator,
    private val owner: DatasetGraphSparkles,
    context: Context,
) : QueryIteratorBase(Context.getCancelSignal(context)) {
    override fun hasNextBinding(): Boolean = try {
        delegate.hasNext().also { if (!it) closeIterator() }
    } catch (e: Throwable) { closeIterator(); throw e }
    override fun moveToNextBinding(): Binding = delegate.nextBinding()
    override fun closeIterator() { try { delegate.close() } finally { owner.close() } }
    override fun requestCancel() { delegate.cancel() }
    override fun output(out: IndentedWriter, sCxt: SerializationContext?) { out.print("HistoricalSparkles") }
}
