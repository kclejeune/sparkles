package io.github.kclejeune.sparkles.jena.engine

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.Sparkles
import io.github.kclejeune.sparkles.jena.internal.CancelWatcher
import io.github.kclejeune.sparkles.jena.internal.RowDecoder
import io.github.kclejeune.sparkles.jena.internal.ffi
import org.apache.jena.graph.Node
import org.apache.jena.graph.Triple
import org.apache.jena.rdf.model.Model
import org.apache.jena.rdf.model.Resource
import org.apache.jena.sparql.ARQConstants
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.core.describe.DescribeHandler
import org.apache.jena.sparql.core.describe.DescribeHandlerRegistry
import org.apache.jena.sparql.engine.binding.BindingBuilder
import org.apache.jena.sparql.util.Context
import org.apache.jena.sparql.util.Symbol

/** Keeps the existing Jena handlers for other datasets and batches Sparkles descriptions. */
internal object DescribeSparkles {
    private val selection = Symbol.create("urn:x-sparkles:describe-dataset")
    private data class Selection(val defaultGraphs: List<String>, val namedGraphs: List<String>)

    fun select(query: org.apache.jena.query.Query, context: Context) {
        context.set(selection, Selection(query.graphURIs.toList(), query.namedGraphURIs.toList()))
    }
    fun register() {
        val registry = DescribeHandlerRegistry.get()
        val originals = registry.handlers().asSequence().toList()
        registry.clear()
        registry.add {
            object : DescribeHandler {
                private var delegates: List<DescribeHandler> = emptyList()
                private var dataset: DatasetGraphSparkles? = null
                private var model: Model? = null
                private var context: Context? = null
                private val resources = LinkedHashSet<Node>()
                override fun start(model: Model, context: Context) {
                    this.model = model
                    this.context = context
                    dataset = context.get<Any>(ARQConstants.sysCurrentDataset) as? DatasetGraphSparkles
                    if (dataset == null) {
                        delegates = originals.map { it.create() }
                        delegates.forEach { it.start(model, context) }
                    }
                }
                override fun describe(resource: Resource) {
                    if (dataset == null) delegates.forEach { it.describe(resource) }
                    else resources.add(resource.asNode())
                }
                override fun finish() {
                    val ds = dataset
                    if (ds == null) { delegates.forEach { it.finish() }; return }
                    if (resources.isEmpty()) return
                    val ctx = context!!
                    val at = ctx.get<Any>(Sparkles.RESOLVED_AT) ?: ctx.get<Any>(Sparkles.AT)
                    val view = if (at == null || ds.isPinned()) ds else if (at is Number) ds.at(at.toLong()) else ds.at(at.toString())
                    try {
                        val binding = BindingBuilder.create()
                        resources.forEachIndexed { i, node -> binding.add(Var.alloc("resource$i"), node) }
                        val vars = (0 until resources.size).joinToString(" ") { "?description$it" }
                        val binds = (0 until resources.size).joinToString(" ") { "BIND(?resource$it AS ?description$it)" }
                        val selected = ctx.get<Selection>(selection)
                        val queryText = org.apache.jena.query.QueryFactory.create("DESCRIBE $vars WHERE { $binds }")
                        selected?.defaultGraphs?.forEach(queryText::addGraphURI)
                        selected?.namedGraphs?.forEach(queryText::addNamedGraphURI)
                        val query = view.source().prepareQuery(queryText.toString(), requestOptions(view, ctx, binding.build()))
                        query.use { q ->
                            val signal = Context.getCancelSignal(ctx)
                            if (signal != null) CancelWatcher.watch(q, signal)
                            try {
                                val decoder = RowDecoder()
                                val execution = ffi { q.execute(256u) }
                                var bytes = execution.batch
                                var done = execution.done
                                while (true) {
                                    val batch = decoder.decode(bytes)
                                    for (r in 0 until batch.rows) {
                                        val base = r * batch.columns
                                        model!!.graph.add(Triple.create(decoder.node(batch.cells[base + 1]),
                                            decoder.node(batch.cells[base + 2]), decoder.node(batch.cells[base + 3])))
                                    }
                                    if (done) break
                                    val next = ffi { q.nextBatch(8192u) }; bytes = next.batch; done = next.done
                                }
                            } finally { CancelWatcher.unwatch(q); q.release() }
                        }
                    } finally { if (view !== ds) view.close() }
                }
            }
        }
    }
}
