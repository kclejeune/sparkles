package io.github.kclejeune.sparkles.jena

import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.Triple
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.Context
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import java.io.ByteArrayInputStream

@Timeout(30)
class StreamingQueryTest {
    @Test fun streaming_bindings_match_eager_across_wire_batches() {
        SparklesDatasets.memory().use { ds ->
            val data = (0 until 10000).joinToString("\n") { "<urn:s$it> <urn:p> ${it % 7} ." }
            ds.load(ByteArrayInputStream(data.toByteArray()), Lang.TURTLE)
            val text = "SELECT ?s ?x { ?s <urn:p> ?o BIND(?o * 2 AS ?x) }"
            fun rows(streaming: Boolean): List<String> = QueryExec.dataset(ds).query(text)
                .context(Context().set(Sparkles.STREAMING_EXECUTION, streaming).set(Sparkles.FALLBACK, SparklesFallback.NEVER))
                .build().use { execution ->
                    val rows = execution.select()
                    buildList {
                        while (rows.hasNext()) {
                            val row = rows.next()
                            add("${row.get(Var.alloc("s"))}:${row.get(Var.alloc("x"))}")
                        }
                    }.sorted()
                }
            assertEquals(rows(false), rows(true))
            QueryExec.dataset(ds).query(text).context(Context().set(Sparkles.STREAMING_EXECUTION, true)).build().use {
                assertTrue(it.select().hasNext())
            }
            assertEquals(10000, rows(true).size)
        }
    }

    @Test fun streaming_construct_uses_incremental_where_solutions() {
        SparklesDatasets.memory().use { ds ->
            val data = (0 until 5000).joinToString("\n") { "<urn:s$it> <urn:p> $it ." }
            ds.load(ByteArrayInputStream(data.toByteArray()), Lang.TURTLE)
            fun triples(streaming: Boolean): Set<Triple> =
                QueryExec.dataset(ds).query("CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }")
                    .context(Context().set(Sparkles.STREAMING_EXECUTION, streaming).set(Sparkles.FALLBACK, SparklesFallback.NEVER))
                    .build().use { it.constructTriples().asSequence().toSet() }
            val streamed = triples(true)
            assertEquals(5000, streamed.size)
            val q = NodeFactory.createURI("urn:q")
            assertTrue(streamed.all { it.predicate == q })
            assertTrue(Triple.create(NodeFactory.createURI("urn:s42"), q, NodeFactory.createLiteralDT("42", org.apache.jena.datatypes.xsd.XSDDatatype.XSDinteger)) in streamed)
            assertEquals(triples(false), streamed)
        }
    }
}
