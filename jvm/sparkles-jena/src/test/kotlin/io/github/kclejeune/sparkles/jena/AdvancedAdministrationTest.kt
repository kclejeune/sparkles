package io.github.kclejeune.sparkles.jena

import org.apache.jena.graph.NodeFactory
import org.apache.jena.query.ReadWrite
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.JenaTransactionException
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.nio.file.Path

class AdvancedAdministrationTest {
    private val shapes = """
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix : <http://example/> .
        :Shape a sh:NodeShape ; sh:targetNode :alice ; sh:property [ sh:path :name ; sh:minCount 1 ] .
    """.trimIndent()
    @Test fun validation_and_guard_enforce_writes() {
        SparklesDatasets.memory().use { ds ->
            val validation = ds.validation()
            val invalid = validation.shacl(shapes)
            assertFalse(invalid.conforms)
            assertEquals(NodeFactory.createURI("http://example/alice"), invalid.results.single().focus)
            assertFalse(invalid.graph().isEmpty)
            assertEquals("not-conforming", validation.guard().set(ValidationGuardOptions(shapes)).state)
            ds.load(ByteArrayInputStream("<http://example/alice> <http://example/name> \"Alice\" .".toByteArray()), Lang.NTRIPLES)
            assertTrue(validation.shacl(shapes).conforms)
            assertEquals("installed", validation.guard().set(ValidationGuardOptions(shapes)).state)
            assertEquals("reject", validation.guard().status()!!.state)
            ds.begin(ReadWrite.WRITE)
            ds.delete(Quad.create(Quad.defaultGraphIRI, NodeFactory.createURI("http://example/alice"), NodeFactory.createURI("http://example/name"), NodeFactory.createLiteralString("Alice")))
            assertThrows(RuntimeException::class.java) { ds.commit() }
            ds.end()
            assertTrue(validation.shacl(shapes).conforms)
            validation.guard().reset()
            assertNull(validation.guard().status())
        }
    }
    @Test fun shex_and_reasoning_are_typed() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("""
                @prefix : <http://example/> .
                @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
                :Child rdfs:subClassOf :Parent . :alice a :Child ; :name "Alice" .
            """.trimIndent().toByteArray()), Lang.TURTLE)
            val report = ds.validation().shex("PREFIX : <http://example/>\n:Shape { :name . }", "<http://example/alice>@<http://example/Shape>")
            assertTrue(report.conforms); assertEquals(1, report.results.size)
            val reason = ds.reasoning().run()
            assertTrue(reason.inferred > 0)
            assertTrue(ds.reasoning().clear() > 0)
            val rdfs = ds.reasoning().rdfs()
            ds.load(ByteArrayInputStream("<http://example/Child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example/Parent> .".toByteArray()), Lang.NTRIPLES)
            rdfs.set("http://example/schema")
            assertTrue(rdfs.enabled()); rdfs.reset(); assertFalse(rdfs.enabled())
        }
    }
    @Test fun indexes_and_owner_close(@TempDir dir: Path) {
        val ds = SparklesDatasets.open(dir.resolve("db"))
        val vector = ds.indexes().vector()
        assertTrue(vector.put("embedding", VectorOptions("http://example/embedding", 3, approximate = false)))
        assertEquals(3, vector.get("embedding")!!.dimension)
        assertNotNull(vector.await("embedding")); vector.rebuild("embedding"); vector.delete("embedding")
        assertNull(vector.get("embedding"))
        val geo = ds.indexes().geo()
        assertTrue(geo.enable().enabled); assertNotNull(geo.await()); geo.disable()
        val validation = ds.validation(); ds.close()
        assertThrows(SparklesInvalidException::class.java) { validation.shacl(shapes) }
        assertThrows(SparklesInvalidException::class.java) { vector.list() }
    }
    @Test fun backup_capture_guards_and_controls(@TempDir dir: Path) {
        SparklesDatasets.memory().use { ds ->
            SparklesBackupRepository.open(dir.resolve("repo").toUri().toString(), true).use { repo ->
                val backups = ds.backups(repo)
                ds.begin(ReadWrite.WRITE)
                assertThrows(JenaTransactionException::class.java) { backups.create("inside") }
                ds.abort(); ds.end()
                val b = backups.create("first", "test")
                assertEquals(ds.datasetId(), b.datasetId); assertEquals("test", b.note)
                assertEquals(listOf("first"), backups.list().map { it.name })
                assertEquals("first", backups.get("first")!!.name)
                assertEquals("ok", backups.verify("first").status)
                assertTrue(repo.test().ok)
                assertEquals(listOf("first"), repo.list().map { it.name })
                assertEquals("ok", repo.verify().status)
                assertTrue(repo.gc().dryRun)
                assertTrue(repo.locks().isEmpty())
                SparklesOperation().use { op -> op.cancel(); assertThrows(RuntimeException::class.java) { backups.create("cancelled", null, op) } }
                assertTrue(backups.delete("first")); assertTrue(backups.list().isEmpty())
            }
        }
    }
}
