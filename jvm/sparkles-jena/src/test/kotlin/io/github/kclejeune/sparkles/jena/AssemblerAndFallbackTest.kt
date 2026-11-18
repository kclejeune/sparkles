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
    @Test fun the_assembler_sets_the_reading_of_errors_in_aggregates() {
        val model = ModelFactory.createDefaultModel()
        RDFDataMgr.read(model, ByteArrayInputStream("""
            @prefix s: <${VocabSparkles.NS}> .
            <urn:dataset> a s:DatasetSparkles ; s:memory true ; s:aggregateErrors "sparkles" .
            <urn:bad> a s:DatasetSparkles ; s:memory true ; s:aggregateErrors "oxigraph" .
        """.trimIndent().toByteArray()), Lang.TURTLE)
        val dataset = Assembler.general.open(model.getResource("urn:dataset")) as org.apache.jena.query.Dataset
        val min = "SELECT (MIN(?v) AS ?m) { VALUES ?v { 3 UNDEF 1 } }"
        QueryExec.dataset(dataset.asDatasetGraph()).query(min).build().use {
            assertEquals("1", it.select().next().get("m").literalLexicalForm)
        }
        // the property is the dataset's setting, which a later change replaces
        val setting = (dataset.asDatasetGraph() as DatasetGraphSparkles).settings().aggregateErrors()
        assertEquals(AggregateErrors.SPARKLES, setting.get())
        assertTrue(setting.isSet())
        // a query's own context comes first
        val jena = Context().set(Sparkles.AGGREGATE_ERRORS, "jena")
        QueryExec.dataset(dataset.asDatasetGraph()).query(min).context(jena).build().use {
            assertNull(it.select().next().get("m"))
        }
        setting.set(AggregateErrors.JENA)
        QueryExec.dataset(dataset.asDatasetGraph()).query(min).build().use {
            assertNull(it.select().next().get("m"))
        }
        dataset.close()
        assertThrows(RuntimeException::class.java) { Assembler.general.open(model.getResource("urn:bad")) }
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
    @Test fun a_query_under_another_reading_of_aggregate_errors_does_not_fall_back_to_arq() {
        SparklesDatasets.memory().use { ds ->
            val context = Context().set(ARQ.stageGenerator, StageGenerator { pattern, input, exec ->
                StageBuilder.standardGenerator().execute(pattern, input, exec)
            })
            val min = "SELECT (MIN(?v) AS ?m) { VALUES ?v { 1 UNDEF } }"
            // Jena's reading is ARQ's, so the query falls back
            QueryExec.dataset(ds).query(min).context(context).build().use { assertNull(it.select().next().get("m")) }
            assertEquals(1, ds.stats().fallbackQueries)
            // under another reading ARQ would answer as Jena does, so the query is refused
            val sparkles = context.copy().set(Sparkles.AGGREGATE_ERRORS, AggregateErrors.SPARKLES)
            assertThrows(org.apache.jena.query.QueryExecException::class.java) {
                QueryExec.dataset(ds).query(min).context(sparkles).build().use { it.select().hasNext() }
            }
            // so is one whose MIN is inside an EXISTS or a NOT EXISTS, in any expression
            val inner = "{ { SELECT (MIN(?v) AS ?m) { VALUES ?v { 1 UNDEF } } } FILTER(BOUND(?m)) }"
            for (q in listOf(
                "SELECT (EXISTS $inner AS ?ok) {}",
                "SELECT ?x { VALUES ?x { 1 } FILTER NOT EXISTS $inner }",
                "SELECT ?x { VALUES ?x { 1 2 } } ORDER BY (IF(?x = 1, EXISTS $inner, false)) ?x",
                "SELECT ?k (COUNT(*) AS ?n) { VALUES ?x { 1 } } GROUP BY (EXISTS $inner AS ?k)",
                "SELECT (SUM(IF(EXISTS $inner, 1, 0)) AS ?n) { VALUES ?x { 1 } }",
                "SELECT (COUNT(*) AS ?n) { VALUES ?x { 1 } } HAVING (EXISTS $inner)",
                "SELECT ?ok { BIND(EXISTS $inner AS ?ok) }",
                "SELECT ?x ?y { VALUES ?x { 1 } OPTIONAL { VALUES ?y { 2 } FILTER(EXISTS $inner) } }",
                "SELECT ?x { VALUES ?x { 1 } FILTER EXISTS { FILTER EXISTS $inner } }",
            )) {
                assertThrows(org.apache.jena.query.QueryExecException::class.java, {
                    QueryExec.dataset(ds).query(q).context(sparkles).build().use { it.select().hasNext() }
                }, q)
            }
            // a query without those aggregates still falls back
            QueryExec.dataset(ds).query("SELECT (COUNT(?v) AS ?n) { VALUES ?v { 1 UNDEF } }").context(sparkles).build().use {
                assertEquals("1", it.select().next().get("n").literalLexicalForm)
            }
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
