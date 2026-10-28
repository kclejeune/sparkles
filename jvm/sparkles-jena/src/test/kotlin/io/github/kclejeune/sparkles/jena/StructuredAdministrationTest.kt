package io.github.kclejeune.sparkles.jena
import org.apache.jena.atlas.json.JSON
import org.apache.jena.riot.Lang
import org.apache.jena.query.TxnType
import org.apache.jena.sparql.JenaTransactionException
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.nio.file.Path
import java.nio.file.Files

class StructuredAdministrationTest {
    private val data = "<http://example/alice> a <http://example/Person> ; <http://example/name> \"Alice\" ; <http://example/age> 42 ."
    @Test fun queries_preserve_versions_and_check_parameters() {
        SparklesDatasets.memory().use { ds ->
            val queries = ds.queries()
            val definition = SavedQueryDefinition("SELECT (?n AS ?age) {}", parameters = JSON.parse("""{"n":{"type":"integer"}}"""))
            assertEquals(1, queries.put("age", definition).getNumber("version").toInt())
            queries.run("age", JSON.parse("""{"n":42}""")).use { assertEquals(42, it.select().next().get("age").literalValue) }
            assertThrows(RuntimeException::class.java) { queries.run("age", JSON.parse("""{"n":"bad"}""")) }
            assertThrows(RuntimeException::class.java) { queries.put("age", definition.copy(query = "ASK {}"), DefinitionChange(ifVersion = 0)) }
            assertEquals(1, queries.versions("age")!!.size)
            assertTrue(queries.delete("age", 1)); assertNull(queries.get("age"))
            val retained = ds.queries(); ds.close(); assertThrows(SparklesInvalidException::class.java) { retained.list() }
        }
    }
    @Test fun mapping_runs_preserve_revisions_and_atomicity(@TempDir directory: Path) {
        val csv = directory.resolve("people.csv")
        Files.writeString(csv, "id,name\n1,Ann\n2,Bo\n")
        val mapping = """prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/${'$'}(id)
    graphs: ex:g
    po:
      - [ex:name, ${'$'}(name)]
"""
        SparklesDatasets.memory(SparklesOptions.builder().readOnly(true).build()).use { readonly ->
            assertThrows(SparklesNotPermittedException::class.java) { readonly.mappings().put("x", mapping) }
            assertThrows(SparklesNotPermittedException::class.java) { readonly.runMapping(mapping) }
        }
        SparklesDatasets.memory().use { ds ->
            val mappings = ds.mappings()
            val saved = mappings.put("people", mapping, description = "People")
            assertTrue(saved.getBoolean("created"))
            assertEquals("yarrrml", saved.get("stored").asObject.get("definition").asObject.getString("kind"))
            assertEquals(1, mappings.list().size)
            assertEquals(1, mappings.versions("people")!!.size)
            assertNull(mappings.get("missing")); assertNull(mappings.get("people", 999))
            assertThrows(RuntimeException::class.java) { mappings.run("missing") }
            mappings.put("people", mapping, description = "Revised", change = DefinitionChange(ifVersion = 1))
            assertEquals(2, mappings.versions("people")!!.size)
            assertEquals("People", mappings.get("people", 1)!!.get("definition").asObject.getString("description"))
            assertThrows(RuntimeException::class.java) { mappings.put("people", mapping, change = DefinitionChange(ifVersion = 999)) }
            assertThrows(RuntimeException::class.java) { mappings.delete("people", 999) }
            val inputs = MappingInputs(sources = mapOf("people.csv" to csv.toString()))
            val run = mappings.run("people", inputs, version = 1, author = "Tester", message = "Mapping test")
            assertEquals(2, run.get("stats").asObject.getNumber("rows").toInt())
            assertEquals(2, run.get("stats").asObject.getNumber("quads").toInt())
            assertTrue(run.get("receipt").asObject.getBoolean("committed"))
            assertEquals(2, ds.find().asSequence().count())
            assertEquals(1, mappings.lastRun("people")!!.getNumber("version").toInt())
            assertEquals("Tester", mappings.lastRun("people")!!.getString("author"))
            ds.begin(TxnType.WRITE)
            try {
                assertThrows(JenaTransactionException::class.java) { mappings.run("people", inputs) }
                assertThrows(JenaTransactionException::class.java) { ds.runMapping(mapping, inputs) }
            } finally { ds.abort(); ds.end() }
            SparklesOperation().use { operation ->
                operation.cancel()
                assertThrows(RuntimeException::class.java) { mappings.run("people", inputs, null, false, null, null, operation) }
                assertThrows(RuntimeException::class.java) { ds.runMapping(mapping, inputs, null, null, operation) }
            }
            assertEquals(2, ds.find().asSequence().count())
            assertThrows(RuntimeException::class.java) { mappings.run("people") }
            assertThrows(RuntimeException::class.java) { mappings.run("people", MappingInputs(connections = mapOf("default" to "invalid:"))) }
            assertEquals(2, ds.find().asSequence().count())
            Files.writeString(csv, "id,name\n1,Ann\n")
            mappings.run("people", inputs, replace = true)
            assertEquals(1, ds.find().asSequence().count())
            assertEquals("replace", mappings.lastRun("people")!!.getString("mode"))
            mappings.put("dynamic", mapping.replace("graphs: ex:g", "graphs: ex:${'$'}(id)"))
            assertThrows(RuntimeException::class.java) { mappings.run("dynamic", inputs, replace = true) }
            assertEquals(1, ds.find().asSequence().count())
            SparklesDatasets.memory().use { other ->
                assertEquals(1, other.runMapping(mapping, inputs).get("stats").asObject.getNumber("quads").toInt())
                assertThrows(RuntimeException::class.java) { other.runMapping("mappings: [nope", kind = "yarrrml") }
                assertEquals(1, other.find().asSequence().count())
            }
            assertTrue(mappings.delete("people")); assertNull(mappings.get("people"))
            ds.close(); assertThrows(SparklesInvalidException::class.java) { mappings.list() }
        }
    }
    @Test fun schema_and_graphql_return_structured_documents() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream(data.toByteArray()), Lang.TURTLE)
            assertFalse(ds.schema().report().isEmpty)
            assertEquals(1, ds.schema().classes().get("items").asArray.size)
            assertFalse(ds.schema().predicates().get("items").asArray.isEmpty())
            assertEquals(1, ds.schema().profiles().get("classes").asArray.size)
            assertTrue(ds.schema().draftShapes().getString("shacl").contains("NodeShape"))
            val graphql = ds.graphql()
            val config = GraphQlConfig("""extend schema @rdf(vocab: "http://example/")
                type Person { name: String age: Int }""")
            assertTrue(graphql.put(config).getBoolean("created"))
            assertNotNull(graphql.sdl()); assertEquals(1, graphql.versions().size)
            val response = graphql.execute("{ allPerson { nodes { name age } } }")
            assertNotNull(response.get("data"), response.toString())
            assertEquals("Alice", response.get("data").asObject.get("allPerson").asObject.get("nodes").asArray[0].asObject.getString("name"))
            assertNotNull(graphql.draft()); assertTrue(graphql.reset(1)); assertNull(graphql.get())
        }
    }
    @Test fun storage_settings_and_history_are_persistent(@TempDir directory: Path) {
        SparklesDatasets.open(directory).use { ds ->
            ds.settings().compaction().set(CompactionOptions(enabled = false, minDeltaQuads = 100))
            assertEquals(100L, ds.settings().compaction().get().minDeltaQuads)
            ds.settings().quota().set(100_000_000); assertEquals(100_000_000L, ds.settings().quota().get().maxBytes)
            ds.settings().retention().set(RetentionOptions(keepCommits = 10))
            val before = ds.headCommit().seq
            ds.load(ByteArrayInputStream(data.toByteArray()), Lang.TURTLE)
            assertNotNull(ds.history().commit("head")); assertTrue(ds.history().status().getNumber("head").toLong() > before)
            assertEquals(ds.headCommit().seq, ds.history().waitForCommit(before, 0))
            assertNull(ds.history().waitForCommit(ds.headCommit().seq, 0))
            assertTrue(ds.history().diff("commit:$before").getNumber("added").toLong() > 0)
            ds.settings().compaction().reset(); ds.settings().quota().reset(); ds.settings().retention().reset()
        }
    }
}
