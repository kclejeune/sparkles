package io.github.kclejeune.sparkles.jena
import org.apache.jena.atlas.json.JSON
import org.apache.jena.riot.Lang
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.nio.file.Path

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
