package io.github.kclejeune.sparkles.jena.engine

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.Sparkles
import io.github.kclejeune.sparkles.jena.internal.ffi.SparklesJni
import org.apache.jena.graph.Node
import org.apache.jena.graph.Triple
import org.apache.jena.query.ARQ
import org.apache.jena.query.Query
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.engine.binding.Binding
import org.apache.jena.sparql.expr.E_Add
import org.apache.jena.sparql.expr.E_Bound
import org.apache.jena.sparql.expr.E_Equals
import org.apache.jena.sparql.expr.E_GreaterThan
import org.apache.jena.sparql.expr.E_GreaterThanOrEqual
import org.apache.jena.sparql.expr.E_IsBlank
import org.apache.jena.sparql.expr.E_IsIRI
import org.apache.jena.sparql.expr.E_IsLiteral
import org.apache.jena.sparql.expr.E_LessThan
import org.apache.jena.sparql.expr.E_LessThanOrEqual
import org.apache.jena.sparql.expr.E_LogicalAnd
import org.apache.jena.sparql.expr.E_LogicalNot
import org.apache.jena.sparql.expr.E_LogicalOr
import org.apache.jena.sparql.expr.E_Multiply
import org.apache.jena.sparql.expr.E_NotEquals
import org.apache.jena.sparql.expr.E_SameTerm
import org.apache.jena.sparql.expr.E_Subtract
import org.apache.jena.sparql.expr.E_UnaryMinus
import org.apache.jena.sparql.expr.Expr
import org.apache.jena.sparql.expr.ExprAggregator
import org.apache.jena.sparql.expr.ExprVar
import org.apache.jena.sparql.expr.NodeValue
import org.apache.jena.sparql.expr.aggregate.AggCount
import org.apache.jena.sparql.expr.aggregate.AggCountDistinct
import org.apache.jena.sparql.expr.aggregate.AggCountVar
import org.apache.jena.sparql.expr.aggregate.AggCountVarDistinct
import org.apache.jena.sparql.expr.aggregate.Aggregator
import org.apache.jena.sparql.pfunction.PropertyFunctionRegistry
import org.apache.jena.sparql.syntax.Element
import org.apache.jena.sparql.syntax.ElementData
import org.apache.jena.sparql.syntax.ElementFilter
import org.apache.jena.sparql.syntax.ElementGroup
import org.apache.jena.sparql.syntax.ElementPathBlock
import org.apache.jena.sparql.syntax.ElementTriplesBlock
import org.apache.jena.sparql.util.Context

/**
 * Small queries that ARQ answers faster over the dataset's `find()` than Sparkles' engine
 * does through its parser and planner (P04 §5.4).
 *
 * A query in Sparkles' engine pays a fixed cost: Jena writes it as text, and Sparkles parses
 * and plans it before it runs. For a lookup on one resource, such as `ASK { <s> <p> <o> }`
 * or `SELECT ?o { <s> <p> ?o }`, that cost is most of the query's time. ARQ evaluates the
 * same pattern with a few `find` calls, each a single JNI call that answers in a microsecond
 * or two. So a query whose pattern is a small basic graph pattern on bound subjects, with
 * optional VALUES, runs in ARQ when the number of `find` calls it is estimated to make is
 * at most a limit.
 *
 * The estimate follows ARQ's evaluation of a basic graph pattern: patterns in order of how
 * many of their terms are bound, each evaluated once for every solution of the patterns
 * before it. A pattern whose subject is bound is assumed to match [SP_MATCHES] triples
 * when its predicate is bound and [S_MATCHES] when it is not. A pattern with an unbound
 * subject and a constant predicate and object, such as `?p ex:worksFor <org>`, is counted
 * in the index, one native call that reads no triples. Any other pattern with an unbound
 * subject can match any number of triples, so a query with one does not qualify.
 *
 * Only what ARQ and Sparkles answer alike qualifies. A qualifying query has no OPTIONAL,
 * UNION, GRAPH, subquery, ORDER BY, HAVING or dataset clause, no property paths, property
 * functions, GeoSPARQL or Sparkles predicates, quoted triples or language-tagged literals,
 * and is a SELECT, ASK or CONSTRUCT. Its FILTERs use only comparisons, logical operators,
 * addition, subtraction, multiplication, BOUND, sameTerm and the isIRI, isBlank and
 * isLiteral tests, which the SPARQL specification defines exactly. Its only expressions
 * are COUNT aggregates, grouped by plain variables when grouped at all. The request must
 * not ask for what only Sparkles' engine applies: RDFS on read, the reasoner's inferred
 * graph, or a row or memory budget. Inside a write transaction, or when the JNI `find` is
 * off, a `find` costs more than the fixed cost it saves, so nothing qualifies there.
 */
internal object SmallQueries {
    /**
     * The most `find` calls a query may be estimated to make and run in ARQ. The system
     * property `sparkles.smallQueries` sets it, and 0 turns the routing off. The context
     * symbol [Sparkles.SMALL_QUERY_FINDS] sets it for one request.
     */
    @JvmField
    val LIMIT: Int = Integer.getInteger("sparkles.smallQueries", 32)

    /** Matches assumed for a pattern with a bound subject and predicate. */
    const val SP_MATCHES = 2.0

    /** Matches assumed for a pattern with a bound subject and an unbound predicate. */
    const val S_MATCHES = 15.0

    /** The largest basic graph pattern considered, and the most rows of a VALUES block before it. */
    private const val MAX_PATTERNS = 8
    private const val MAX_VALUES_ROWS = 64

    private const val GEO = "http://www.opengis.net/ont/geosparql#"

    /** Whether `query` runs in ARQ over `find()` on `dsg` in this request. */
    fun route(query: Query, dsg: DatasetGraphSparkles, context: Context, input: Binding): Boolean {
        val limit = limit(context)
        if (limit <= 0 || !SparklesJni.FIND) return false
        if (dsg.txn()?.write != null) return false
        if (context.isTrue(Sparkles.INCLUDE_INFERRED) || context.isDefined(Sparkles.MAX_ROWS) ||
            context.isDefined(Sparkles.MAX_MEMORY_BYTES) || context.isDefined(Sparkles.MAX_ROWS_PRODUCED)
        ) {
            return false
        }
        val graph = if (unionDefaultGraph(dsg, context)) Quad.unionGraph else Quad.defaultGraphIRI
        val count = { p: Node, o: Node -> dsg.countQuads(graph, null, p, o) }
        val finds = estimate(query, input, knownOf(dsg).propertyFunctions, context, count) ?: return false
        if (finds > limit) return false
        // RDFS on read is a setting of the dataset that only Sparkles' engine applies
        return !dsg.rdfsOnRead()
    }

    private fun limit(context: Context): Int = when (val v = context.get<Any>(Sparkles.SMALL_QUERY_FINDS)) {
        null -> LIMIT
        is Number -> v.toInt()
        else -> v.toString().trim().toIntOrNull() ?: LIMIT
    }

    /**
     * The `find` calls ARQ is estimated to make for `query`, or null when the query does
     * not qualify. `input` holds variables bound before the query runs. `count` gives the
     * number of triples that match an unbound subject with a predicate and an object, and
     * without it a pattern with an unbound subject does not qualify.
     */
    fun estimate(
        query: Query,
        input: Binding,
        sparklesPfs: Set<String>,
        context: Context,
        count: ((Node, Node) -> Long)? = null,
    ): Double? {
        if (!(query.isSelectType || query.isAskType || query.isConstructType)) return null
        if (query.hasHaving() || query.hasOrderBy()) return null
        if (query.hasDatasetDescription()) return null
        // COUNT is the only aggregate, and the only expression, that qualifies
        if (query.hasGroupBy() && !query.groupBy.exprs.isEmpty()) return null
        if (query.hasAggregators() && !query.aggregators.all { counts(it.aggregator) }) return null
        if (query.isSelectType && query.project.exprs.values.any { it !is ExprAggregator }) return null
        val group = query.queryPattern as? ElementGroup ?: return null
        val bound = HashSet<Var>()
        input.vars().forEachRemaining { bound.add(it) }
        var rows = 1.0
        val triples = ArrayList<Triple>()
        for (el: Element in group.elements) {
            when (el) {
                is ElementPathBlock -> for (tp in el.pattern) {
                    triples.add(tp.asTriple() ?: return null)
                }
                is ElementTriplesBlock -> triples.addAll(el.pattern.list)
                // ARQ applies a group's filters to its solutions, as Sparkles does
                is ElementFilter -> if (!exact(el.expr)) return null
                is ElementData -> {
                    // VALUES binds the patterns after it, when every row binds every variable;
                    // after a pattern, ARQ joins it with the pattern's unbound matches
                    if (triples.isNotEmpty() || el.rows.size > MAX_VALUES_ROWS) return null
                    if (el.rows.any { r -> el.vars.any { !r.contains(it) } }) return null
                    rows *= el.rows.size.coerceAtLeast(1)
                    bound.addAll(el.vars)
                }
                else -> return null
            }
        }
        // the query's trailing VALUES joins with the pattern's unbound matches too
        if (query.hasValues()) return null
        if (triples.isEmpty() || triples.size > MAX_PATTERNS) return null
        val pfs = if (context.isTrueOrUndef(ARQ.enablePropertyFunctions)) {
            PropertyFunctionRegistry.chooseRegistry(context) ?: PropertyFunctionRegistry.get()
        } else {
            null
        }
        for (t in triples) {
            if (!plain(t.subject) || !plain(t.`object`)) return null
            val p = t.predicate
            if (p.isURI) {
                val iri = p.uri
                if (iri in sparklesPfs || iri.startsWith("urn:x-sparkles:") || iri.startsWith(GEO)) return null
                if (pfs != null && pfs.isRegistered(iri)) return null
            } else if (!p.isVariable) {
                return null
            }
        }
        // ARQ's order: the pattern with the most bound terms first, the query's order on ties
        var finds = 0.0
        var counted = false
        val left = ArrayList(triples)
        while (left.isNotEmpty()) {
            var best = 0
            var bestScore = -1
            for ((i, t) in left.withIndex()) {
                val score = (if (isBound(t.subject, bound)) 4 else 0) +
                    (if (isBound(t.`object`, bound)) 2 else 0) + (if (isBound(t.predicate, bound)) 1 else 0)
                if (score > bestScore) {
                    best = i
                    bestScore = score
                }
            }
            val t = left.removeAt(best)
            finds += rows
            rows *= when {
                !isBound(t.subject, bound) -> {
                    // one pattern of constants may be counted; a second would cost a second call
                    val p = t.predicate
                    val o = t.`object`
                    if (count == null || counted || !p.isURI || o.isVariable || o.isBlank) return null
                    counted = true
                    // the count is exact, and a pattern that matches more than the limit cannot qualify
                    count(p, o).toDouble().coerceAtLeast(1.0)
                }
                isBound(t.predicate, bound) && isBound(t.`object`, bound) -> 1.0
                isBound(t.predicate, bound) -> SP_MATCHES
                isBound(t.`object`, bound) -> SP_MATCHES
                else -> S_MATCHES
            }
            for (n in arrayOf(t.subject, t.predicate, t.`object`)) if (n.isVariable) bound.add(n as Var)
        }
        return finds
    }

    private fun counts(a: Aggregator): Boolean =
        a is AggCount || a is AggCountVar || a is AggCountDistinct || a is AggCountVarDistinct

    /** An expression whose value the SPARQL specification fixes, so that ARQ and Sparkles agree. */
    private fun exact(e: Expr): Boolean = when (e) {
        is ExprVar -> true
        is NodeValue -> !e.asNode().isTripleTerm
        is E_Equals, is E_NotEquals, is E_LessThan, is E_LessThanOrEqual, is E_GreaterThan,
        is E_GreaterThanOrEqual, is E_LogicalAnd, is E_LogicalOr, is E_LogicalNot, is E_Bound,
        is E_Add, is E_Subtract, is E_Multiply, is E_UnaryMinus, is E_SameTerm, is E_IsIRI,
        is E_IsBlank, is E_IsLiteral,
        -> e.args.all(::exact)
        else -> false
    }

    /** A term that matches the same triples in a `find` as in Sparkles' engine. */
    private fun plain(n: Node): Boolean = when {
        n.isVariable || n.isURI || n.isBlank -> true
        n.isLiteral -> n.literalLanguage.isEmpty()
        else -> false
    }

    /** Blank nodes in a query pattern are variables that ARQ never binds in advance. */
    private fun isBound(n: Node, bound: Set<Var>): Boolean = when {
        n.isVariable -> n in bound
        n.isBlank -> false
        else -> true
    }
}
