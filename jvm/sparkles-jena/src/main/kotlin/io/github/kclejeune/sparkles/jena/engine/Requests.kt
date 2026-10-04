package io.github.kclejeune.sparkles.jena.engine

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.Sparkles
import io.github.kclejeune.sparkles.jena.SparklesFallback
import io.github.kclejeune.sparkles.jena.SparklesOutbound
import io.github.kclejeune.sparkles.jena.internal.encodeTerms
import io.github.kclejeune.sparkles.jena.internal.ffi.QueryOpts
import org.apache.jena.graph.Node
import org.apache.jena.query.ARQ
import org.apache.jena.sparql.engine.binding.Binding
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
    )
}
