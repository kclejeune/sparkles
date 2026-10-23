package io.github.kclejeune.sparkles.jena
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.graph.NodeFactory
import org.apache.jena.query.QueryCancelledException
import org.apache.jena.query.ReadWrite
import org.apache.jena.sparql.core.Quad
import org.apache.jena.riot.Lang
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream

/** `graph().groups` returns the answer of the library's `ds.graph().groups`. */
class GraphOverviewTest {
    private val ex = "http://ex.org/"
    private val data = """
        @prefix ex: <http://ex.org/> .
        ex:a a ex:Person ; ex:livesIn ex:p1 ; ex:knows ex:b .
        ex:b a ex:Person ; ex:livesIn ex:p1 ; ex:knows ex:c .
        ex:c a ex:Person ; ex:livesIn ex:p2 .
        ex:d a ex:Person ; ex:livesIn ex:p2 .
        ex:p1 a ex:Place .
        ex:p2 a ex:Place .
    """.trimIndent()

    private fun load(ds: DatasetGraphSparkles, turtle: String) = ds.load(ByteArrayInputStream(turtle.toByteArray()), Lang.TURTLE)
    private fun person() = GroupsRequest(GroupsWithin.OfClass(ex + "Person"))
    private fun place() = GroupsRequest(GroupsWithin.OfClass(ex + "Place"))

    /** The sorted predicate (without the namespace), target and count of the edges from [source], or of all edges. */
    private fun edges(answer: JsonObject, source: String? = null): List<Triple<String, String, Int>> =
        answer.get("edges").asArray.map { it.asObject }
            .filter { source == null || it.getString("source") == source }
            .map { Triple(it.getString("predicate").removePrefix(ex), it.getString("target"), it.getNumber("count").toInt()) }
            .sortedWith(compareBy({ it.first }, { it.second }))

    @Test fun a_class_opens_into_groups_and_a_group_into_its_members() {
        SparklesDatasets.memory().use { ds ->
            load(ds, data)
            val answer = ds.graph().groups(person().copy(limit = 3))
            assertEquals(1, answer.getNumber("commit").toInt())
            assertEquals(ex + "Person", answer.get("within").asObject.getString("class"))
            assertEquals(4, answer.get("within").asObject.getNumber("size").toInt())
            assertEquals("values", answer.get("grouping").asObject.getString("kind"))
            assertEquals(ex + "livesIn", answer.get("grouping").asObject.getString("predicate"))
            assertFalse(answer.getBoolean("truncated"))
            val groups = answer.get("groups").asArray.map { it.asObject }
            assertEquals(listOf(ex + "p1" to 2, ex + "p2" to 2), groups.map { it.getString("label") to it.getNumber("size").toInt() })
            assertNull(answer.get("members"))
            val first = "g:" + groups[0].getString("id")
            val second = "g:" + groups[1].getString("id")
            // a knows b inside the first group, b knows c across to the second, and both live in places
            assertEquals(
                listOf(Triple("knows", first, 1), Triple("knows", second, 1), Triple("livesIn", "c:" + ex + "Place", 2)),
                edges(answer, first),
            )
            assertEquals(listOf(Triple("livesIn", "c:" + ex + "Place", 2)), edges(answer, second))
            assertEquals(4, answer.get("edges").asArray.size)

            val members = ds.graph().groups(GroupsRequest(GroupsWithin.OfGroup(groups[0].getString("id")), limit = 3))
            assertEquals(groups[0].getString("id"), members.get("within").asObject.getString("group"))
            assertEquals(2, members.get("within").asObject.getNumber("size").toInt())
            assertEquals("members", members.get("grouping").asObject.getString("kind"))
            assertTrue(members.get("groups").asArray.isEmpty())
            assertEquals(listOf(ex + "a", ex + "b"), members.get("members").asArray.map { it.asObject.getString("iri") })
            assertEquals(
                listOf(Triple("knows", "m:" + ex + "b", 1), Triple("livesIn", "c:" + ex + "Place", 1)),
                edges(members, "m:" + ex + "a"),
            )
        }
    }

    @Test fun members_by_default_and_by_property() {
        SparklesDatasets.memory().use { ds ->
            load(ds, data)
            val places = ds.graph().groups(place())
            assertEquals(listOf(ex + "p1", ex + "p2"), places.get("members").asArray.map { it.asObject.getString("iri") })
            assertEquals(
                listOf(Triple("livesIn", "m:" + ex + "p1", 2), Triple("livesIn", "m:" + ex + "p2", 2)),
                edges(places),
            )
            val people = ds.graph().groups(person().copy(by = GroupsBy.Members))
            assertEquals(listOf("a", "b", "c", "d").map { ex + it }, people.get("members").asArray.map { it.asObject.getString("iri") })
            assertEquals(6, people.get("edges").asArray.size)
            val byProperty = ds.graph().groups(person().copy(by = GroupsBy.Property(ex + "livesIn", 4)))
            assertEquals(listOf(2, 2), byProperty.get("groups").asArray.map { it.asObject.getNumber("size").toInt() })
            assertEquals(byProperty, ds.graph().groups(person().copy(limit = 3)))
        }
    }

    @Test fun the_point_in_history_the_view_and_the_budgets() {
        SparklesDatasets.memory().use { ds ->
            load(ds, data)
            val head = ds.graph().groups(place())
            assertEquals(head, ds.graph().groups(place().copy(at = "commit:1")))
            assertEquals(head, ds.graph().groups(place().copy(at = "head")))
            ds.snapshots().create("before")
            load(ds, "<${ex}p3> a <${ex}Place> .")
            assertEquals(3, ds.graph().groups(place()).get("within").asObject.getNumber("size").toInt())
            assertEquals(head, ds.graph().groups(place().copy(at = "snapshot:before")))
            // a view that holds no Place triple
            val empty = ds.graph().groups(place(), GroupsOptions(defaultGraph = listOf(ex + "none")))
            assertEquals(0, empty.get("within").asObject.getNumber("size").toInt())
            assertThrows(SparklesBudgetExceededException::class.java) { ds.graph().groups(place(), GroupsOptions(maxRows = 1)) }
        }
    }

    @Test fun a_pinned_dataset_reads_its_own_commit() {
        SparklesDatasets.memory().use { ds ->
            load(ds, data)
            val pinned = ds.at(ds.headCommit().seq)
            load(ds, "<${ex}p3> a <${ex}Place> .")
            pinned.use {
                assertEquals(2, it.graph().groups(place()).get("within").asObject.getNumber("size").toInt())
                assertThrows(IllegalArgumentException::class.java) { it.graph().groups(place().copy(at = "head")) }
            }
            assertEquals(3, ds.graph().groups(place()).get("within").asObject.getNumber("size").toInt())
        }
    }

    private fun size(ds: DatasetGraphSparkles, request: GroupsRequest, options: GroupsOptions = GroupsOptions()): Int =
        ds.graph().groups(request, options).get("within").asObject.getNumber("size").toInt()

    private fun placeQuad(name: String) = Quad.create(
        Quad.defaultGraphIRI,
        NodeFactory.createURI(ex + name),
        NodeFactory.createURI("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        NodeFactory.createURI(ex + "Place"),
    )

    @Test fun a_read_transaction_keeps_its_snapshot() {
        SparklesDatasets.memory().use { ds ->
            load(ds, data)
            ds.begin(ReadWrite.READ)
            try {
                assertEquals(2, size(ds, place()))
                // another thread commits a third place
                val writer = Thread { load(ds, "<${ex}p3> a <${ex}Place> .") }
                writer.start(); writer.join()
                assertEquals(2, size(ds, place()))
                assertThrows(IllegalArgumentException::class.java) { ds.graph().groups(place().copy(at = "head")) }
            } finally {
                ds.end()
            }
            assertEquals(3, size(ds, place()))
        }
    }

    @Test fun a_write_transaction_sees_its_own_changes() {
        SparklesDatasets.memory().use { ds ->
            load(ds, data)
            ds.begin(ReadWrite.WRITE)
            try {
                ds.add(placeQuad("p3"))
                assertEquals(3, size(ds, place()))
            } finally {
                ds.abort(); ds.end()
            }
            assertEquals(2, size(ds, place()))
        }
    }

    @Test fun the_union_default_graph_follows_the_dataset_settings_and_its_context() {
        val named = "@prefix ex: <http://ex.org/> . <${ex}g> { ex:x a ex:Thing . ex:y a ex:Thing }"
        val thing = GroupsRequest(GroupsWithin.OfClass(ex + "Thing"))
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream(named.toByteArray()), Lang.TRIG)
            assertEquals(0, size(ds, thing))
            ds.context.set(Sparkles.UNION_DEFAULT_GRAPH, true)
            assertEquals(2, size(ds, thing))
        }
        SparklesDatasets.memory(SparklesOptions.builder().unionDefaultGraph(true).build()).use { ds ->
            ds.load(ByteArrayInputStream(named.toByteArray()), Lang.TRIG)
            assertEquals(2, size(ds, thing))
            ds.context.set(Sparkles.UNION_DEFAULT_GRAPH, false)
            assertEquals(0, size(ds, thing))
        }
    }

    @Test fun malformed_requests_cancellation_and_a_closed_dataset_fail() {
        SparklesDatasets.memory().use { ds ->
            load(ds, data)
            assertThrows(SparklesInvalidException::class.java) { ds.graph().groups(GroupsRequest(GroupsWithin.OfClass("not an iri"))) }
            assertThrows(SparklesInvalidException::class.java) { ds.graph().groups(person().copy(limit = 0)) }
            assertThrows(SparklesInvalidException::class.java) { ds.graph().groups(person().copy(by = GroupsBy.Property(ex + "livesIn", 1))) }
            assertThrows(SparklesInvalidException::class.java) { ds.graph().groups(person().copy(at = "nowhere")) }
            SparklesOperation().use { op ->
                op.cancel()
                assertThrows(QueryCancelledException::class.java) { ds.graph().groups(person(), GroupsOptions(), op) }
            }
            val handle = ds.graph()
            ds.close()
            assertThrows(SparklesInvalidException::class.java) { handle.groups(person()) }
        }
    }

    @Test fun the_timeout_covers_all_of_the_queries() {
        // the answer takes many queries, each of which is shorter than the timeout
        val rows = (0 until 200000).joinToString("\n") { i ->
            "<${ex}s$i> a <${ex}T> ; <${ex}region> <${ex}r${i % 12}> ; <${ex}knows> <${ex}s${(i * 7) % 200000}> ."
        }
        val request = GroupsRequest(GroupsWithin.OfClass(ex + "T"), GroupsBy.Property(ex + "region"))
        val uncached = GroupsOptions(noCache = true)
        SparklesDatasets.memory().use { ds ->
            load(ds, rows)
            fun timed(): Long {
                val start = System.nanoTime()
                val answer = ds.graph().groups(request, uncached)
                assertEquals(12, answer.get("groups").asArray.size)
                return (System.nanoTime() - start) / 1_000_000
            }
            // The first runs warm the dataset up, so the measurement is the best of three more.
            repeat(3) { timed() }
            val whole = minOf(timed(), timed(), timed())
            assertTrue(whole > 40, "the answer took $whole ms, too fast to bound")
            // Each query takes much less than half of the whole, so a timeout of that half ends the call
            // only when the queries share it. A run that happens to be much quicker than the best of
            // the measurements does not end the call, so the timeout gets a few tries.
            var ended = false
            var tries = 0
            while (!ended && tries++ < 6) {
                try {
                    SparklesOperation(whole / 2).use { op -> ds.graph().groups(request, uncached, op) }
                } catch (e: QueryCancelledException) {
                    ended = true
                }
            }
            assertTrue(ended, "a timeout of half of the $whole ms that the answer takes never ended the call in $tries tries")
        }
    }
}
