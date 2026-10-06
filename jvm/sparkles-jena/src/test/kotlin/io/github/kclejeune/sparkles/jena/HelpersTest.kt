package io.github.kclejeune.sparkles.jena

import org.apache.jena.atlas.json.JSON
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.query.ReadWrite
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.JenaTransactionException
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.nio.file.Path

@Timeout(30)
class HelpersTest {
    @Test fun standalone_syntax_format_geo_schedule_helpers() {
        assertFalse(SparklesHelpers.checkIri("http://example/a b").get("errors").asArray.isEmpty())
        assertTrue(SparklesHelpers.checkIri("urn:valid").get("errors").asArray.isEmpty())
        assertEquals("en-US", SparklesHelpers.checkLanguageTag("EN-us").getString("canonical"))
        assertNotNull(SparklesHelpers.checkLanguageTag("bad_tag").get("error"))
        assertTrue(SparklesHelpers.checkData("<urn:s> <urn:p> 1 .", "turtle").isNull)
        assertFalse(SparklesHelpers.checkData("<urn:s> <urn:p>", "turtle").isNull)
        assertThrows(SparklesInvalidException::class.java) { SparklesHelpers.checkData("", "unknown") }
        val query = "# kept\nselect * where{?s ?p ?o}"
        val formatted = SparklesHelpers.format(query, "sparql", JSON.parse("{\"lineWidth\":80}"))
        assertTrue(formatted.getString("text").contains("# kept"))
        assertEquals(formatted.getString("text"), SparklesHelpers.format(formatted.getString("text"), "sparql").getString("text"))
        assertThrows(SparklesInvalidException::class.java) { SparklesHelpers.format(query, "sparql", JSON.parse("{\"bogus\":true}")) }
        val lint = SparklesHelpers.lint("PREFIX ex: <urn:ex:>\nSELECT ?x WHERE {?s ?p ?o}", "sparql").get("diagnostics").asArray
        assertFalse(lint.isEmpty())
        assertTrue(lint.any { !it.asObject.get("fix").isNull })
        assertFalse(SparklesHelpers.format("<urn:s> <urn:p> \"1\" .", "ntriples", JSON.parse("{\"canonicalize\":true,\"cursor\":0}")).getString("text").isEmpty())
        val geometry = SparklesHelpers.convertGeometries(JSON.parseAny("""[{"value":"POINT(2 3)","datatype":"http://www.opengis.net/ont/geosparql#wktLiteral"},{"value":"bad","datatype":"urn:no"}]""").asArray)
        assertEquals("Point", geometry[0].asObject.get("geometry").asObject.getString("type"))
        assertNotNull(geometry[1].asObject.get("error"))
        assertEquals(listOf("2026-10-06T00:00:00+00:00", "2026-10-07T00:00:00+00:00"), SparklesHelpers.previewSchedule("0 0 * * *", count = 2, after = "2026-10-05T12:00:00Z"))
    }
    @Test fun cancelled_helpers_fail_before_work() {
        SparklesOperation().use { operation ->
            operation.cancel()
            assertThrows(RuntimeException::class.java) { SparklesHelpers.format("SELECT * WHERE {}", "sparql", JsonObject(), operation) }
            assertThrows(RuntimeException::class.java) { SparklesHelpers.lint("SELECT * WHERE {}", "sparql", emptyMap(), operation) }
            SparklesDatasets.memory().use { ds ->
                assertThrows(RuntimeException::class.java) { ds.cloneToMemory(operation) }
                assertThrows(RuntimeException::class.java) { ds.explain("SELECT * WHERE {}", operation) }
                assertThrows(RuntimeException::class.java) { ds.indexes().vector().embedUntilIdle(operation) }
            }
        }
    }
    @Test fun memory_clone_cache_explain_dag_and_alias_capture_guards() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("<urn:s> <urn:p> 1 .".toByteArray()), Lang.TURTLE)
            ds.clearCache()
            assertNotNull(ds.explain("SELECT * WHERE {?s ?p ?o}").get("plan"))
            assertFalse(ds.branches().commitGraph().get("commits").asArray.isEmpty())
            ds.cloneToMemory().use { clone ->
                assertNotEquals(ds.datasetId(), clone.datasetId())
                assertEquals(1, clone.defaultGraph.size())
                clone.load(ByteArrayInputStream("<urn:clone> <urn:p> 2 .".toByteArray()), Lang.TURTLE)
                assertEquals(1, ds.defaultGraph.size()); assertEquals(2, clone.defaultGraph.size())
            }
            ds.branch("main").use { alias ->
                ds.begin(ReadWrite.WRITE)
                try {
                    assertThrows(JenaTransactionException::class.java) { alias.cloneToMemory() }
                    assertThrows(JenaTransactionException::class.java) { alias.explain("ASK {}") }
                    assertThrows(JenaTransactionException::class.java) { alias.branches().commitGraph() }
                    assertThrows(JenaTransactionException::class.java) { alias.indexes().text().search("a") }
                    assertThrows(JenaTransactionException::class.java) { alias.indexes().geo().features(GeoFeaturesOptions(listOf(0.0, 0.0, 1.0, 1.0))) }
                    assertThrows(JenaTransactionException::class.java) { alias.indexes().vector().recall("v") }
                    assertThrows(JenaTransactionException::class.java) { alias.reasoning().diagnostics() }
                } finally { ds.abort(); ds.end() }
            }
        }
    }
    @Test fun direct_indexes_and_reasoning_documents(@TempDir dir: Path) {
        SparklesDatasets.open(dir.resolve("db")).use { ds ->
            ds.load(ByteArrayInputStream("""
                <urn:s> <urn:text> "hello & world" ; <urn:vector> "[1,0]" ; <http://www.opengis.net/ont/geosparql#asWKT> "POINT(2 3)"^^<http://www.opengis.net/ont/geosparql#wktLiteral> .
                <urn:Child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Parent> . <urn:s> a <urn:Child> .
            """.trimIndent().toByteArray()), Lang.TURTLE)
            ds.indexes().text().enable(TextOptions(predicates = listOf("urn:text")))
            val hits = ds.indexes().text().search("hello")
            assertEquals(1, hits.get("hits").asArray.size)
            assertTrue(hits.get("hits").asArray[0].asObject.getString("snippet").contains("<mark>hello</mark>"))
            val features = ds.indexes().geo().features(GeoFeaturesOptions(listOf(0.0, 0.0, 5.0, 5.0)))
            assertEquals(1, features.get("features").asArray.size)
            assertTrue(ds.indexes().vector().put("v", VectorOptions("urn:vector", 2)))
            ds.indexes().vector().await("v")
            val recall = ds.indexes().vector().recall("v", RecallOptions(samples = 1, k = 1))
            assertNotNull(recall.get("recall"))
            assertNull(ds.indexes().vector().embeddingStatus("v"))
            ds.indexes().vector().setEmbeddingEnvironment(EmbeddingEnvironment(enabled = false))
            ds.indexes().vector().embedUntilIdle(1000)
            ds.indexes().vector().setEmbeddingEnvironment(null)
            assertThrows(RuntimeException::class.java) { ds.indexes().vector().reembed("v") }
            assertNull(ds.reasoning().status())
            assertTrue(ds.reasoning().run().inferred > 0)
            assertNotNull(ds.reasoning().status()!!.get("freshness"))
            ds.cloneToMemory().use { clone ->
                assertNotEquals(ds.datasetId(), clone.datasetId())
                assertNotNull(clone.reasoning().status())
                assertFalse(clone.reasoning().status()!!.get("freshness").asObject.getBoolean("stale"))
            }
            assertNotNull(ds.reasoning().diagnostics(DiagnosticsOptions(checks = listOf("disjoint-classes"))).get("report"))
        }
    }
}
