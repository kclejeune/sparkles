package io.github.kclejeune.sparkles.jena

import org.apache.jena.atlas.json.JSON
import org.apache.jena.graph.NodeFactory
import org.apache.jena.query.TxnType
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.core.Quad
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream

class AlgorithmsTest {
    private val initial = "<urn:a> <urn:p> <urn:b>. <urn:b> <urn:p> <urn:c>. <urn:c> <urn:p> <urn:a>. <urn:d> <urn:p> <urn:e>."
    private val definition = JSON.parse("""{"predicates":["urn:p"],"direction":"undirected"}""")
    private val request = JSON.parse("""{"algorithm":"wcc","projection":"p"}""")
    private val rank = JSON.parse("""{"algorithm":"pageRank","projection":"p"}""")
    private val write = JSON.parse("""{"algorithm":"pageRank","projection":"p","write":{"graph":"urn:out","predicates":{"score":"urn:rank"},"batchSize":2}}""")

    @Test fun algorithms_preserve_views_revisions_and_controls() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream(initial.toByteArray()), Lang.TURTLE)
            val algorithms = ds.algorithms()
            val projections = algorithms.projections()
            assertNull(projections.get("missing"))
            assertTrue(projections.put("p", definition, "tester", 0).getBoolean("created"))
            assertEquals(1, projections.get("p")!!.getNumber("revision").toInt())
            assertEquals(1, projections.list().size)
            assertThrows(RuntimeException::class.java) { projections.put("p", definition, ifRevision = 7) }
            assertThrows(RuntimeException::class.java) { projections.delete("p", 7) }
            assertThrows(RuntimeException::class.java) { projections.put("invalid name", definition) }
            assertTrue(algorithms.queryText(request).contains("SELECT"))
            algorithms.run(request).use { assertEquals(5, it.select().asSequence().count()) }
            assertEquals(5, algorithms.stats(rank).getNumber("nodes").toInt())
            assertThrows(RuntimeException::class.java) { algorithms.run(JSON.parse("""{"algorithm":"nope"}""")) }
            assertThrows(RuntimeException::class.java) { algorithms.stats(JSON.parse("""{"algorithm":"wcc","projection":"missing"}""")) }
            assertThrows(RuntimeException::class.java) { algorithms.queryText(write) }
            val at = ds.headCommit().seq
            ds.at(at).use { pinned ->
                ds.load(ByteArrayInputStream("<urn:f> <urn:p> <urn:g>.".toByteArray()), Lang.TURTLE)
                assertEquals(5, pinned.algorithms().stats(rank).getNumber("nodes").toInt())
                pinned.algorithms().run(request).use { assertEquals(5, it.select().asSequence().count()) }
                assertThrows(RuntimeException::class.java) { pinned.algorithms().runWrite(write) }
                assertThrows(RuntimeException::class.java) { pinned.algorithms().projections().put("x", definition) }
            }
            assertEquals(7, algorithms.stats(rank).getNumber("nodes").toInt())
            assertEquals(7, algorithms.stats(write).getNumber("nodes").toInt())
            ds.begin(TxnType.WRITE)
            try {
                ds.add(Quad.create(Quad.defaultGraphNodeGenerated, NodeFactory.createURI("urn:h"), NodeFactory.createURI("urn:p"), NodeFactory.createURI("urn:i")))
                assertEquals(9, algorithms.stats(rank).getNumber("nodes").toInt())
                algorithms.run(request).use { assertEquals(9, it.select().asSequence().count()) }
                assertThrows(JenaTransactionException::class.java) { algorithms.runWrite(write) }
                assertThrows(JenaTransactionException::class.java) { projections.put("x", definition) }
            } finally { ds.abort(); ds.end() }
            assertEquals(7, algorithms.stats(rank).getNumber("nodes").toInt())
            SparklesOperation().use { operation ->
                operation.cancel()
                assertThrows(RuntimeException::class.java) { algorithms.stats(rank, AlgorithmOptions(), operation) }
                assertThrows(RuntimeException::class.java) { algorithms.runWrite(write, AlgorithmOptions(), null, operation) }
            }
            assertEquals(5, ds.find().asSequence().count())
            val report = algorithms.runWrite(write, author = "tester")
            assertEquals("complete", report.getString("status"))
            assertEquals(7, report.getNumber("triples").toInt())
            assertEquals(4, report.getNumber("commits").toInt())
            assertTrue(projections.delete("p", 1)); assertTrue(projections.list().isEmpty())
            ds.close()
            assertThrows(SparklesInvalidException::class.java) { projections.list() }
            assertThrows(SparklesInvalidException::class.java) { algorithms.queryText(request) }
        }
        SparklesDatasets.memory().use { empty ->
            val summary = empty.algorithms().stats(JSON.parse("""{"algorithm":"pageRank","projection":{"predicates":["urn:absent"]}}"""))
            assertEquals(0, summary.getNumber("nodes").toInt())
            assertEquals(0, summary.getNumber("edges").toInt())
            assertEquals(0, summary.getNumber("iterations").toInt())
            assertTrue(summary.getBoolean("converged"))
        }
        SparklesDatasets.memory(SparklesOptions.builder().readOnly(true).build()).use { readonly ->
            assertThrows(SparklesNotPermittedException::class.java) { readonly.algorithms().projections().put("p", definition) }
            assertThrows(SparklesNotPermittedException::class.java) { readonly.algorithms().runWrite(write) }
        }
    }
}
