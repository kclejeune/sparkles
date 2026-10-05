// FunctionRegistry.get remains the common API of Jena 5 and 6; its replacement is 6-only.
@file:Suppress("DEPRECATION")
package io.github.kclejeune.sparkles.jena.engine

import org.apache.jena.graph.Node
import org.apache.jena.query.ARQ
import org.apache.jena.query.Query
import org.apache.jena.sparql.algebra.walker.Walker
import org.apache.jena.sparql.expr.E_Function
import org.apache.jena.sparql.expr.Expr
import org.apache.jena.sparql.expr.ExprAggregator
import org.apache.jena.sparql.expr.ExprFunction
import org.apache.jena.sparql.expr.ExprFunctionOp
import org.apache.jena.sparql.expr.ExprNone
import org.apache.jena.sparql.expr.ExprTripleTerm
import org.apache.jena.sparql.expr.ExprVar
import org.apache.jena.sparql.expr.ExprVisitorFunction
import org.apache.jena.sparql.expr.NodeValue
import org.apache.jena.sparql.expr.aggregate.AggCustom
import org.apache.jena.sparql.expr.aggregate.AggregateRegistry
import org.apache.jena.sparql.expr.aggregate.lib.AggURI
import org.apache.jena.sparql.function.FunctionRegistry
import org.apache.jena.sparql.modify.request.UpdateDeleteWhere
import org.apache.jena.sparql.modify.request.UpdateModify
import org.apache.jena.sparql.pfunction.PropertyFunctionRegistry
import org.apache.jena.sparql.syntax.Element
import org.apache.jena.sparql.syntax.ElementAssign
import org.apache.jena.sparql.syntax.ElementBind
import org.apache.jena.sparql.syntax.ElementFilter
import org.apache.jena.sparql.syntax.ElementNamedGraph
import org.apache.jena.sparql.syntax.ElementPathBlock
import org.apache.jena.sparql.syntax.ElementSubQuery
import org.apache.jena.sparql.syntax.ElementService
import org.apache.jena.sparql.engine.main.StageBuilder
import org.apache.jena.sparql.engine.main.QC
import org.apache.jena.sparql.engine.main.OpExecutor
import org.apache.jena.sparql.service.ServiceExecutorRegistry
import org.apache.jena.sparql.syntax.ElementTriplesBlock
import org.apache.jena.sparql.syntax.ElementVisitorBase
import org.apache.jena.sparql.syntax.ElementWalker
import org.apache.jena.sparql.util.Context
import org.apache.jena.update.Update

/** What Sparkles evaluates itself, by IRI. */
internal class Known(
    val functions: Set<String>,
    val aggregates: Set<String>,
    val propertyFunctions: Set<String>,
)

private const val XSD = "http://www.w3.org/2001/XMLSchema#"

/**
 * Finds what in a query only Java can evaluate (P04 §3.5, rules 1 to 3): a function that
 * Jena's registry resolves and Sparkles does not list, or a `java:` function; a property
 * function registered in Jena that Sparkles does not list; a custom aggregate registered
 * in Jena that Sparkles does not list. An IRI that both know runs in Sparkles.
 */
internal object FallbackDetector {
    private val functionsAtInit = HashMap<String, Any>()
    private val propertiesAtInit = HashMap<String, Any>()
    private val aggregatesAtInit = HashMap<String, Any>()
    private var stageAtInit: Any? = null
    private var executorAtInit: Any? = null
    private var serviceSingles: List<Any> = emptyList()
    private var serviceBulks: List<Any> = emptyList()

    fun initialize() {
        stageAtInit = StageBuilder.getGenerator()
        executorAtInit = QC.getFactory(ARQ.getContext())
        val f = FunctionRegistry.get()
        f.keys().forEachRemaining { iri -> f.get(iri)?.let { functionsAtInit[iri] = it } }
        val p = PropertyFunctionRegistry.get()
        p.keys().forEachRemaining { iri -> p.get(iri)?.let { propertiesAtInit[iri] = it } }
        // Jena exposes no iterator for its aggregate registry. These are the standard
        // aggregate IRIs in its public AggURI vocabulary, including the legacy aliases.
        for (iri in listOf(AggURI.stdev, AggURI.stdev_samp, AggURI.stdev_pop, AggURI.variance, AggURI.var_samp, AggURI.var_pop)) {
            for (alias in listOf(iri, iri.replace("/aggregate", ""))) {
                AggregateRegistry.getAccumulatorFactory(alias)?.let { aggregatesAtInit[alias] = it }
            }
        }
        serviceSingles = ServiceExecutorRegistry.get().singleChain.toList()
        serviceBulks = ServiceExecutorRegistry.get().bulkChain.toList()
    }

    private fun hooks(context: Context): String? {
        val stage = StageBuilder.getGenerator(context)
        if (stage != null && stage !== stageAtInit && stage !== StageBuilder.standardGenerator() && stage !== StageBuilder.executeInline) return "a custom ARQ stage generator"
        val executor = QC.getFactory(context)
        if (executor != null && executor !== executorAtInit && executor !== OpExecutor.stdFactory) return "a custom ARQ operator executor"
        return null
    }
    /** The reason the query must run in ARQ, or `null`. */
    fun check(query: Query, context: Context, known: Known): String? {
        hooks(context)?.let { return it }
        if (query.isJsonType) return "the JSON query form"
        val w = Walk(context, known)
        w.query(query)
        w.reason?.let { return it }
        // Jena gives its union and default graph names a meaning of their own in FROM and
        // FROM NAMED, and GRAPH on them a meaning relative to the FROM NAMED graphs
        val from = query.graphURIs + query.namedGraphURIs
        if (from.any { it == UNION || it == DEFAULT }) {
            return "the query's dataset names <$UNION> or <$DEFAULT>"
        }
        if (query.namedGraphURIs.isNotEmpty() && w.specialGraph) {
            return "the query's GRAPH <$UNION> or <$DEFAULT> is relative to its FROM NAMED graphs"
        }
        return null
    }

    private const val UNION = "urn:x-arq:UnionGraph"
    private const val DEFAULT = "urn:x-arq:DefaultGraph"

    /** The reason an update operation must run in ARQ, or `null`. */
    fun check(update: Update, context: Context, known: Known): String? {
        hooks(context)?.let { return it }
        val w = Walk(context, known)
        when (update) {
            is UpdateModify -> update.wherePattern?.let(w::element)
            is UpdateDeleteWhere -> {}
            else -> {}
        }
        return w.reason
    }

    private class Walk(private val context: Context, private val known: Known) {
        var reason: String? = null

        /** whether a GRAPH names Jena's union or default graph */
        var specialGraph = false
        private val functions: FunctionRegistry = FunctionRegistry.get(context) ?: FunctionRegistry.get()
        private val pfEnabled: Boolean = context.isTrueOrUndef(ARQ.enablePropertyFunctions)
        private val pfs: PropertyFunctionRegistry? =
            PropertyFunctionRegistry.chooseRegistry(context) ?: PropertyFunctionRegistry.get()

        fun query(q: Query) {
            q.project?.exprs?.values?.forEach(::expr)
            q.groupBy?.exprs?.values?.forEach(::expr)
            q.havingExprs?.forEach(::expr)
            q.orderBy?.forEach { expr(it.expression) }
            q.aggregators?.forEach(::expr)
            q.queryPattern?.let(::element)
        }

        fun element(el: Element) {
            ElementWalker.walk(el, object : ElementVisitorBase() {
                override fun visit(el: ElementFilter) = expr(el.expr)
                override fun visit(el: ElementBind) = expr(el.expr)
                override fun visit(el: ElementAssign) = expr(el.expr)
                override fun visit(el: ElementSubQuery) = query(el.query)
                override fun visit(el: ElementService) {
                    val service = el.serviceNode
                    val iri = if (service.isURI) service.uri else null
                    val builtin = iri != null && iri == "urn:x-sparkles:path#search"
                    val registry = ServiceExecutorRegistry.chooseRegistry(context)
                    if (!builtin && (registry.singleChain != serviceSingles || registry.bulkChain != serviceBulks)) {
                        reason = "a custom Java SERVICE executor"
                    }
                }
                override fun visit(el: ElementNamedGraph) {
                    val g = el.graphNameNode
                    if (g != null && g.isURI && (g.uri == UNION || g.uri == DEFAULT)) specialGraph = true
                }
                override fun visit(el: ElementPathBlock) {
                    for (tp in el.pattern) tp.predicate?.let(::predicate)
                }

                override fun visit(el: ElementTriplesBlock) {
                    for (t in el.pattern) predicate(t.predicate)
                }
            })
        }

        private fun predicate(p: Node) {
            if (reason != null || !p.isURI || !pfEnabled) return
            val iri = p.uri
            if (pfs != null && pfs.isRegistered(iri) && (iri !in known.propertyFunctions || pfs.get(iri) !== propertiesAtInit[iri])) {
                reason = "the property function <$iri> is registered in Jena and not in Sparkles"
            }
        }

        fun expr(e: Expr?) {
            if (e == null || reason != null) return
            Walker.walk(e, visitor)
        }

        private fun function(iri: String) {
            if (reason != null) return
            if (iri.startsWith("java:")) {
                reason = "the function <$iri> is a Java class"
            } else if (functions.isRegistered(iri) && ((iri !in known.functions && !iri.startsWith(XSD)) || (iri in known.functions && functions.get(iri) !== functionsAtInit[iri]))) {
                reason = "the function <$iri> is registered in Jena and not in Sparkles"
            }
        }

        private val visitor = object : ExprVisitorFunction() {
            override fun visitExprFunction(func: ExprFunction) {
                if (func is E_Function) function(func.functionIRI)
            }

            override fun visit(op: ExprFunctionOp) {
                op.element?.let { element(it) }
            }

            override fun visit(nv: NodeValue) {}
            override fun visit(nv: ExprVar) {}
            override fun visit(tripleTerm: ExprTripleTerm) {}
            override fun visit(exprNone: ExprNone) {}

            override fun visit(eAgg: ExprAggregator) {
                val agg = eAgg.aggregator
                if (agg is AggCustom) {
                    val iri = agg.iri
                    if (reason == null && AggregateRegistry.isRegistered(iri) && (iri !in known.aggregates || AggregateRegistry.getAccumulatorFactory(iri) !== aggregatesAtInit[iri])) {
                        reason = "the aggregate <$iri> is registered in Jena and not in Sparkles"
                    }
                }
                agg.exprList?.forEach { expr(it) }
            }
        }
    }
}
