package io.github.kclejeune.sparkles.jena

import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.query.QueryFactory
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.engine.binding.BindingFactory
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * The bridge's ways of making fewer and cheaper native calls: reused call status records,
 * no `release` for a result that freed itself, ASK run as ASK, and query text written
 * without relative IRIs. Each keeps the answers and errors of the plain path.
 */
class BridgeCallsTest {
    private val ex = "http://example/"
    private fun iri(l: String): Node = NodeFactory.createURI(ex + l)
    private val open = ArrayList<DatasetGraphSparkles>()

    private fun memory(): DatasetGraphSparkles = SparklesDatasets.memory().also { open.add(it) }

    @AfterEach
    fun close() {
        open.forEach { it.close() }
    }

    private fun filled(n: Int): DatasetGraphSparkles {
        val dsg = memory()
        Txn.executeWrite(dsg) {
            for (i in 0 until n) {
                dsg.add(Quad.defaultGraphIRI, iri("s$i"), iri("p"), NodeFactory.createLiteralString("v$i"))
            }
        }
        return dsg
    }

    private fun count(dsg: DatasetGraphSparkles, text: String, context: Context? = null): Int {
        val b = QueryExec.dataset(dsg).query(text)
        if (context != null) b.context(context)
        return b.build().use { qe ->
            val rs = qe.select()
            var n = 0
            while (rs.hasNext()) {
                rs.next()
                n++
            }
            n
        }
    }

    @Test
    fun ask_runs_natively_and_answers_both_ways() {
        val dsg = filled(3)
        val before = dsg.stats()
        assertTrue(QueryExec.dataset(dsg).query("ASK { <${ex}s1> <${ex}p> ?o }").ask())
        assertFalse(QueryExec.dataset(dsg).query("ASK { <${ex}s9> <${ex}p> ?o }").ask())
        assertTrue(QueryExec.dataset(dsg).query("ASK {}").ask())
        assertTrue(Txn.calculateRead(dsg) { QueryExec.dataset(dsg).query("ASK { ?s <${ex}p> \"v2\" }").ask() })
        val after = dsg.stats()
        assertEquals(before.nativeQueries + 4, after.nativeQueries)
        assertEquals(before.fallbackQueries, after.fallbackQueries)
    }

    @Test
    fun ask_takes_input_bindings_and_streaming_mode() {
        val dsg = filled(3)
        val s = Var.alloc("s")
        val yes = BindingFactory.binding(s, iri("s2"))
        val no = BindingFactory.binding(s, iri("s7"))
        val q = "ASK { ?s <${ex}p> ?o }"
        assertTrue(QueryExec.dataset(dsg).query(q).substitution(yes).build().use { it.ask() })
        assertFalse(QueryExec.dataset(dsg).query(q).substitution(no).build().use { it.ask() })
        val streaming = Context().set(Sparkles.STREAMING_EXECUTION, true)
        assertTrue(QueryExec.dataset(dsg).query(q).context(streaming).build().use { it.ask() })
        assertFalse(QueryExec.dataset(dsg).query("ASK { ?s <${ex}q> ?o }").context(streaming).build().use { it.ask() })
    }

    @Test
    fun results_that_free_themselves_are_read_to_the_end_or_closed_early() {
        // more rows than the first batch (256) and the first find batch (64)
        val dsg = filled(700)
        assertEquals(700, count(dsg, "SELECT * { ?s ?p ?o }"))
        assertEquals(10, count(dsg, "SELECT * { ?s ?p ?o } LIMIT 10"))
        QueryExec.dataset(dsg).query("SELECT * { ?s ?p ?o }").build().use { qe ->
            val rs = qe.select()
            repeat(300) { rs.next() }
        }
        Txn.executeRead(dsg) {
            val it = dsg.defaultGraph.find()
            var n = 0
            while (it.hasNext()) {
                it.next()
                n++
            }
            assertEquals(700, n)
            val early = dsg.defaultGraph.find()
            repeat(100) { early.next() }
            early.close()
        }
        // the dataset still answers afterwards, so no native state was freed twice
        assertEquals(700, count(dsg, "SELECT * { ?s ?p ?o }"))
        val streaming = Context().set(Sparkles.STREAMING_EXECUTION, true)
        assertEquals(700, count(dsg, "SELECT * { ?s ?p ?o }", streaming))
    }

    @Test
    fun query_text_keeps_iris_absolute_and_an_explicit_base() {
        val dsg = memory()
        // an IRI under Jena's system base, which the serializer used to write relative
        val local = NodeFactory.createURI(org.apache.jena.irix.IRIs.getBaseStr() + "local/thing")
        Txn.executeWrite(dsg) { dsg.add(Quad.defaultGraphIRI, local, iri("p"), iri("o")) }
        assertEquals(1, count(dsg, "SELECT ?o { <${local.uri}> <${ex}p> ?o }"))
        assertEquals(1, count(dsg, "SELECT ?o { <local/thing> <${ex}p> ?o }"))
        // with BASE, IRI() resolves against it at run time
        val resolved = QueryExec.dataset(dsg)
            .query("BASE <http://base.example/dir/> SELECT (IRI(\"rel\") AS ?i) {}")
            .build().use { qe -> qe.select().next().get(Var.alloc("i")) }
        assertEquals("http://base.example/dir/rel", resolved.uri)
        // a prepared query with prefixes, run twice
        val q = QueryFactory.create("PREFIX ex: <$ex> SELECT ?o { <${local.uri}> ex:p ?o }")
        repeat(2) {
            assertEquals(1, QueryExec.dataset(dsg).query(q).build().use { qe -> qe.select().asSequence().count() })
        }
    }

    @Test
    fun native_errors_stay_errors_after_status_reuse() {
        val dsg = filled(5)
        // a result limit that the query exceeds fails in the native call itself
        val limited = Context().set(Sparkles.MAX_ROWS, 2L)
        repeat(3) {
            // a failing call followed by succeeding ones on the same thread
            assertThrows(RuntimeException::class.java) {
                QueryExec.dataset(dsg).query("SELECT * { ?s ?p ?o }").context(limited).build().use { qe ->
                    val rs = qe.select()
                    while (rs.hasNext()) rs.next()
                }
            }
            assertEquals(5, count(dsg, "SELECT * { ?s ?p ?o }"))
            assertTrue(Txn.calculateRead(dsg) { dsg.defaultGraph.contains(iri("s1"), iri("p"), NodeFactory.createLiteralString("v1")) })
        }
    }

    @Test
    fun calls_on_many_threads_keep_their_own_status() {
        val dsg = filled(50)
        val pool = Executors.newFixedThreadPool(8)
        try {
            val tasks = (0 until 64).map { i ->
                Callable {
                    var ok = 0
                    repeat(50) { j ->
                        val k = (i + j) % 50
                        if (Txn.calculateRead(dsg) { dsg.defaultGraph.contains(iri("s$k"), iri("p"), NodeFactory.createLiteralString("v$k")) }) ok++
                        if (!Txn.calculateRead(dsg) { dsg.defaultGraph.contains(iri("s$k"), iri("p"), iri("nothing")) }) ok++
                        if (QueryExec.dataset(dsg).query("ASK { <${ex}s$k> ?p ?o }").ask()) ok++
                    }
                    ok
                }
            }
            val results = pool.invokeAll(tasks).map { it.get(60, TimeUnit.SECONDS) }
            assertTrue(results.all { it == 150 }, results.toString())
        } finally {
            pool.shutdownNow()
        }
    }
}
