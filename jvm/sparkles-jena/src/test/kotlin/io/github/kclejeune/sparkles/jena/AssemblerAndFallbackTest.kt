package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.assembler.VocabSparkles
import org.apache.jena.assembler.Assembler
import org.apache.jena.query.ARQ
import org.apache.jena.query.QueryFactory
import org.apache.jena.rdf.model.ModelFactory
import org.apache.jena.riot.Lang
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.algebra.Algebra
import org.apache.jena.sparql.engine.main.StageBuilder
import org.apache.jena.sparql.engine.main.StageGenerator
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.expr.NodeValue
import org.apache.jena.sparql.expr.aggregate.AggregateRegistry
import org.apache.jena.sparql.expr.aggregate.lib.AggURI
import org.apache.jena.sparql.function.FunctionBase1
import org.apache.jena.sparql.function.FunctionRegistry
import org.apache.jena.sparql.util.Context
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream

class AssemblerAndFallbackTest {
    @Test fun assembler_opens_memory_and_rejects_ambiguous_location() {
        val model = ModelFactory.createDefaultModel()
        RDFDataMgr.read(model, ByteArrayInputStream("""
            @prefix s: <${VocabSparkles.NS}> .
            <urn:dataset> a s:DatasetSparkles ; s:memory true ; s:unionDefaultGraph true .
        """.trimIndent().toByteArray()), Lang.TURTLE)
        val resource = model.getResource("urn:dataset")
        val dataset = Assembler.general.open(resource) as org.apache.jena.query.Dataset
        assertTrue(dataset.asDatasetGraph() is DatasetGraphSparkles)
        assertTrue((dataset.asDatasetGraph() as DatasetGraphSparkles).options.unionDefaultGraph)
        dataset.close()
        resource.addLiteral(model.createProperty(VocabSparkles.NS + "location"), "/unused")
        assertThrows(RuntimeException::class.java) { Assembler.general.open(resource) }
    }
    @Test fun stage_hooks_fall_back_in_auto_and_refuse_in_never() {
        SparklesDatasets.memory().use { ds ->
            var invoked = false
            val context = Context().set(ARQ.stageGenerator, StageGenerator { pattern, input, exec ->
                invoked = true; StageBuilder.standardGenerator().execute(pattern, input, exec)
            })
            val query = "SELECT * { ?s ?p ?o }"
            QueryExec.dataset(ds).query(query).context(context).build().use { it.select().hasNext() }
            assertTrue(invoked); assertEquals(1, ds.stats().fallbackQueries)
            val never = context.copy().set(Sparkles.FALLBACK, SparklesFallback.NEVER)
            assertThrows(org.apache.jena.query.QueryExecException::class.java) { QueryExec.dataset(ds).query(query).context(never).build().use { it.select().hasNext() } }
        }
    }
    @Test fun known_function_override_executes_java_instead_of_native() {
        SparklesDatasets.memory().use { ds ->
            val iri = "http://jena.apache.org/ARQ/function#sqrt"
            val old = FunctionRegistry.get().get(iri)
            val registry = FunctionRegistry()
            registry.put(iri, object : org.apache.jena.sparql.function.FunctionFactory {
                override fun create(uri: String): org.apache.jena.sparql.function.Function = object : FunctionBase1() {
                    override fun exec(v: NodeValue): NodeValue = NodeValue.makeInteger(99)
                }
            })
            val context = Context().also { FunctionRegistry.set(it, registry) }
            val result = QueryExec.dataset(ds).query("SELECT (<$iri>(4) AS ?n) {}").context(context).build().use { it.select().next().get("n").literalValue.toString() }
            assertEquals("99", result); assertEquals(1, ds.stats().fallbackQueries)
            assertSame(old, FunctionRegistry.get().get(iri))
        }
    }
    @Test fun known_aggregate_override_is_detected() {
        SparklesDatasets.memory().use { ds ->
            val iri = AggURI.stdev
            val old = AggregateRegistry.getAccumulatorFactory(iri)
            try {
                AggregateRegistry.register(iri) { _, _ -> object : org.apache.jena.sparql.expr.aggregate.Accumulator {
                    override fun accumulate(binding: org.apache.jena.sparql.engine.binding.Binding, env: org.apache.jena.sparql.function.FunctionEnv) {}
                    override fun getValue(): NodeValue = NodeValue.makeInteger(99)
                } }
                val value = QueryExec.dataset(ds).query("SELECT (<$iri>(?n) AS ?v) { VALUES ?n {1 2} }").build().use { it.select().next().get("v").literalValue.toString() }
                assertEquals("99", value); assertEquals(1, ds.stats().fallbackQueries)
            } finally { AggregateRegistry.register(iri, old) }
        }
    }
}
