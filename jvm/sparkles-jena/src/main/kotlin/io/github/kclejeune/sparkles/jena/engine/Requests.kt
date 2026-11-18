package io.github.kclejeune.sparkles.jena.engine

import io.github.kclejeune.sparkles.jena.AggregateErrors
import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.Sparkles
import io.github.kclejeune.sparkles.jena.SparklesFallback
import io.github.kclejeune.sparkles.jena.SparklesOutbound
import io.github.kclejeune.sparkles.jena.internal.encodeTerms
import io.github.kclejeune.sparkles.jena.internal.ffi.QueryOpts
import org.apache.jena.graph.Node
import org.apache.jena.query.ARQ
import org.apache.jena.query.Query
import org.apache.jena.sparql.algebra.Algebra
import org.apache.jena.sparql.algebra.OpVisitorBase
import org.apache.jena.sparql.algebra.Op
import org.apache.jena.sparql.algebra.OpWalker
import org.apache.jena.sparql.algebra.op.OpAssign
import org.apache.jena.sparql.algebra.op.OpExtend
import org.apache.jena.sparql.algebra.op.OpFilter
import org.apache.jena.sparql.algebra.op.OpGroup
import org.apache.jena.sparql.algebra.op.OpLeftJoin
import org.apache.jena.sparql.algebra.op.OpOrder
import org.apache.jena.sparql.algebra.op.OpTopN
import org.apache.jena.sparql.engine.binding.Binding
import org.apache.jena.sparql.expr.Expr
import org.apache.jena.sparql.expr.ExprAggregator
import org.apache.jena.sparql.expr.ExprFunctionOp
import org.apache.jena.sparql.expr.ExprVisitorBase
import org.apache.jena.sparql.expr.ExprWalker
import org.apache.jena.sparql.expr.aggregate.Aggregator
import org.apache.jena.sparql.util.Context
import org.apache.jena.sparql.util.Symbol

/** TDB2's and TDB1's `unionDefaultGraph` symbols, matched by IRI so that jena-tdb2 is not needed. */
private val TDB_UNION = listOf(
    Symbol.create("http://jena.apache.org/TDB#unionDefaultGraph"),
    Symbol.create("http://jena.hpl.hp.com/TDB#unionDefaultGraph"),
)

internal fun knownOf(dsg: DatasetGraphSparkles): Known =
    Known(dsg.handle.knownFunctions, dsg.handle.knownAggregates, dsg.handle.knownPropertyFunctions)

/** The fallback mode of a request: the context's, else the dataset's. */
internal fun fallbackMode(dsg: DatasetGraphSparkles, context: Context?): SparklesFallback {
    val v = context?.get<Any>(Sparkles.FALLBACK) ?: return dsg.options.fallback
    return when (v) {
        is SparklesFallback -> v
        else -> runCatching { SparklesFallback.valueOf(v.toString().trim().uppercase()) }.getOrDefault(dsg.options.fallback)
    }
}

/** Whether the request's default graph is the union of the named graphs (P04 §3.7). */
internal fun unionDefaultGraph(dsg: DatasetGraphSparkles, context: Context?): Boolean {
    if (context != null) {
        if (context.isDefined(Sparkles.UNION_DEFAULT_GRAPH)) return context.isTrue(Sparkles.UNION_DEFAULT_GRAPH)
        for (s in TDB_UNION) if (context.isDefined(s)) return context.isTrue(s)
    }
    return dsg.options.unionDefaultGraph
}

private fun Context.long(s: Symbol): Long? = when (val v = get<Any>(s)) {
    null -> null
    is Number -> v.toLong()
    else -> v.toString().trim().toLongOrNull()
}

/** The overall timeout of `ARQ.queryTimeout` in milliseconds, as a backstop to Jena's own. */
private fun timeoutMs(context: Context): Long? = when (val v = context.get<Any>(ARQ.queryTimeout)) {
    null -> null
    is Number -> v.toLong()
    else -> v.toString().split(',').lastOrNull()?.trim()?.toLongOrNull()
}?.takeIf { it > 0 }?.let { it + 1000 }

/** The engine's options for a query or update on `dsg` in `context`. */
internal fun requestOptions(dsg: DatasetGraphSparkles, context: Context, input: Binding?): QueryOpts {
    val names = ArrayList<String>()
    val values = ArrayList<Node?>()
    input?.forEach { v, n ->
        names.add(v.varName)
        values.add(n)
    }
    return QueryOpts(
        baseIri = null,
        unionDefaultGraph = unionDefaultGraph(dsg, context),
        includeInferred = context.isTrue(Sparkles.INCLUDE_INFERRED),
        timeoutMs = timeoutMs(context)?.toULong(),
        maxRows = context.long(Sparkles.MAX_ROWS)?.toULong(),
        maxMemoryBytes = context.long(Sparkles.MAX_MEMORY_BYTES)?.toULong(),
        maxRowsProduced = context.long(Sparkles.MAX_ROWS_PRODUCED)?.toULong(),
        allowService = !context.isFalse(ARQ.httpServiceAllowed),
        allowPrivateNetwork = dsg.options.outboundPolicy == SparklesOutbound.OPEN,
        bindingNames = names,
        bindingValues = if (names.isEmpty()) ByteArray(0) else encodeTerms(values),
        noCache = context.isTrue(Sparkles.NO_CACHE),
        aggregateErrors = aggregateErrors(context)?.name?.lowercase(),
    )
}

/** The reading of errors in aggregates that `context` names, if any. */
internal fun aggregateErrors(context: Context): AggregateErrors? = when (val v = context.get<Any>(Sparkles.AGGREGATE_ERRORS)) {
    null -> null
    is AggregateErrors -> v
    else -> AggregateErrors.entries.firstOrNull { it.name.equals(v.toString().trim(), ignoreCase = true) }
        ?: throw IllegalArgumentException("aggregateErrors is jena, sparkles or spec, not $v")
}

/** The aggregates whose answer over an error depends on the reading. */
private val READING_AGGREGATES = setOf("MIN", "MAX", "AVG", "GROUP_CONCAT")

/**
 * Finds MIN, MAX, AVG or GROUP_CONCAT anywhere in an algebra expression. It visits every
 * operator, and itself the expressions of each operator that carries them, since Jena's
 * walkers leave out sort conditions and aggregate arguments. An EXISTS or NOT EXISTS in
 * any of those expressions is searched the same way.
 */
private class ReadingAggregates {
    var found = false

    private val exprs = object : ExprVisitorBase() {
        override fun visit(func: ExprFunctionOp) {
            func.graphPattern?.let { op(it) }
        }
        override fun visit(eAgg: ExprAggregator) {
            aggregator(eAgg.aggregator)
        }
    }

    fun op(op: Op) {
        OpWalker.walk(op, object : OpVisitorBase() {
            override fun visit(opFilter: OpFilter) = opFilter.exprs.forEach(::expr)
            override fun visit(opExtend: OpExtend) = opExtend.varExprList.exprs.values.forEach(::expr)
            override fun visit(opAssign: OpAssign) = opAssign.varExprList.exprs.values.forEach(::expr)
            override fun visit(opLeftJoin: OpLeftJoin) { opLeftJoin.exprs?.forEach(::expr) }
            override fun visit(opOrder: OpOrder) = opOrder.conditions.forEach { expr(it.expression) }
            override fun visit(opTopN: OpTopN) = opTopN.conditions.forEach { expr(it.expression) }
            override fun visit(opGroup: OpGroup) {
                opGroup.groupVars.exprs.values.forEach(::expr)
                opGroup.aggregators.forEach { aggregator(it.aggregator) }
            }
        })
    }

    private fun aggregator(a: Aggregator) {
        if (a.name.uppercase() in READING_AGGREGATES) found = true
        a.exprList?.forEach(::expr)
    }

    private fun expr(e: Expr?) {
        if (e != null && !found) ExprWalker.walk(exprs, e)
    }
}

/**
 * Why ARQ cannot answer `query` on `dsg` in `context`, or null when it can. ARQ reads
 * errors in aggregates as Jena does, so a query with MIN, MAX, AVG or GROUP_CONCAT under
 * another reading cannot fall back to it.
 */
internal fun arqCannotRead(query: Query, dsg: DatasetGraphSparkles, context: Context): String? {
    val found = ReadingAggregates().also { it.op(Algebra.compile(query)) }.found
    if (!found) return null
    val reading = aggregateErrors(context) ?: dsg.settings().aggregateErrors().get()
    return if (reading == AggregateErrors.JENA) null else
        "ARQ reads errors in aggregates as Jena does, and the query asks for ${reading.name.lowercase()}"
}

