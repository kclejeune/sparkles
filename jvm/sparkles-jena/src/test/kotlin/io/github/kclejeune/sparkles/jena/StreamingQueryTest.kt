package io.github.kclejeune.sparkles.jena

import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.Triple
import org.apache.jena.query.DatasetFactory
import org.apache.jena.query.QueryCancelledException
import org.apache.jena.query.QueryExecution
import org.apache.jena.query.ResultSetFactory
import org.apache.jena.query.ResultSetFormatter
import org.apache.jena.rdf.model.ModelFactory
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.util.concurrent.TimeUnit

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

    /** A dataset with `n` subjects, each with a name and a number, which automatic execution streams. */
    private fun people(n: Int): DatasetGraphSparkles {
        val ds = SparklesDatasets.memory()
        val data = (0 until n).joinToString("\n") { "<urn:s$it> <urn:name> \"name $it\" ; <urn:p> ${it % 7} ." }
        ds.load(ByteArrayInputStream(data.toByteArray()), Lang.TURTLE)
        return ds
    }

    /** The context of each execution: unset is automatic, the default. */
    private fun context(streaming: Boolean?): Context {
        val c = Context().set(Sparkles.FALLBACK, SparklesFallback.NEVER)
        if (streaming != null) c.set(Sparkles.STREAMING_EXECUTION, streaming)
        return c
    }

    private fun execution(ds: DatasetGraph, text: String, streaming: Boolean?): QueryExecution =
        QueryExecution.dataset(DatasetFactory.wrap(ds)).query(text).context(context(streaming)).build()

    private val scan = "SELECT ?s ?n { ?s <urn:name> ?n }"

    @Test fun automatic_execution_is_the_default_and_answers_as_eager_and_streaming() {
        people(5000).use { ds ->
            fun rows(streaming: Boolean?): List<String> = execution(ds, scan, streaming).use { e ->
                val rs = e.execSelect()
                buildList { while (rs.hasNext()) { val r = rs.next(); add("${r.get("s")} ${r.get("n")}") } }.sorted()
            }
            val eager = rows(false)
            assertEquals(5000, eager.size)
            assertEquals(eager, rows(null))
            assertEquals(eager, rows(true))
            // small results, aggregates and ASK
            for (text in listOf("SELECT ?s { ?s <urn:p> 3 } LIMIT 5", "SELECT (COUNT(*) AS ?c) { ?s ?p ?o }")) {
                assertEquals(
                    execution(ds, text, false).use { ResultSetFormatter.asText(it.execSelect()) },
                    execution(ds, text, null).use { ResultSetFormatter.asText(it.execSelect()) },
                )
            }
            assertTrue(execution(ds, "ASK { ?s <urn:p> 3 }", null).use { it.execAsk() })
        }
    }

    /**
     * Automatic execution streams a large scan. Under a memory budget that a batch fits and
     * the whole result does not, the automatic read completes where the eager one fails.
     */
    @Test fun automatic_execution_streams_a_scan_that_eager_execution_cannot_hold() {
        people(150000).use { ds ->
            fun count(streaming: Boolean?): Int =
                QueryExecution.dataset(DatasetFactory.wrap(ds)).query(scan)
                    .context(context(streaming).set(Sparkles.MAX_MEMORY_BYTES, 2L shl 20)).build()
                    .use { ResultSetFormatter.consume(it.execSelect()) }
            assertEquals(150000, count(null))
            assertThrows(SparklesBudgetExceededException::class.java) { count(false) }
        }
    }

    @Test fun closing_a_streamed_result_early_frees_it_and_the_dataset_keeps_answering() {
        people(5000).use { ds ->
            repeat(50) {
                execution(ds, scan, null).use { e ->
                    val rs = e.execSelect()
                    repeat(3) { assertTrue(rs.hasNext()); rs.next() }
                }
            }
            val iterator = QueryExec.dataset(ds).query(scan).context(context(null)).build().select()
            assertTrue(iterator.hasNext())
            iterator.close()
            assertFalse(iterator.hasNext())
            assertEquals(5000, execution(ds, scan, null).use { ResultSetFormatter.consume(it.execSelect()) })
        }
    }

    @Test fun rewindable_copied_and_formatted_results_match_eager() {
        people(5000).use { ds ->
            for (streaming in listOf(null, true)) {
                val rewindable = execution(ds, scan, streaming).use { ResultSetFactory.makeRewindable(it.execSelect()) }
                assertEquals(5000, ResultSetFormatter.consume(rewindable))
                rewindable.reset()
                assertEquals(5000, ResultSetFormatter.consume(rewindable))
                val copied = execution(ds, scan, streaming).use { ResultSetFactory.copyResults(it.execSelect()) }
                assertEquals(5000, ResultSetFormatter.consume(copied))
            }
            fun formatted(streaming: Boolean?, write: (QueryExecution, ByteArrayOutputStream) -> Unit): List<String> =
                ByteArrayOutputStream().also { out -> execution(ds, "$scan ORDER BY ?s", streaming).use { write(it, out) } }
                    .toString(Charsets.UTF_8).lines()
            for (write in listOf<(QueryExecution, ByteArrayOutputStream) -> Unit>(
                { e, out -> ResultSetFormatter.outputAsTSV(out, e.execSelect()) },
                { e, out -> ResultSetFormatter.outputAsJSON(out, e.execSelect()) },
                { e, out -> ResultSetFormatter.outputAsXML(out, e.execSelect()) },
            )) {
                val eager = formatted(false, write)
                assertTrue(eager.size > 5000)
                assertEquals(eager, formatted(null, write))
            }
            // an unordered scan streams, and its formatted rows are the same set
            fun tsv(streaming: Boolean?) = ByteArrayOutputStream().also { out ->
                execution(ds, scan, streaming).use { ResultSetFormatter.outputAsTSV(out, it.execSelect()) }
            }.toString(Charsets.UTF_8).lines().filter { it.isNotEmpty() }
            assertEquals(tsv(false).drop(1).sorted(), tsv(null).drop(1).sorted())
        }
    }

    @Test fun construct_and_describe_through_graph_iterators_match_eager() {
        people(3000).use { ds ->
            val construct = "CONSTRUCT { ?s <urn:label> ?n } WHERE { ?s <urn:name> ?n }"
            fun triples(streaming: Boolean?): Set<Triple> =
                QueryExec.dataset(ds).query(construct).context(context(streaming)).build()
                    .use { it.constructTriples().asSequence().toSet() }
            val eager = triples(false)
            assertEquals(3000, eager.size)
            assertEquals(eager, triples(null))
            val model = execution(ds, construct, null).use { it.execConstruct() }
            assertEquals(3000L, model.size())
            val describe = "DESCRIBE ?s WHERE { ?s <urn:p> 3 }"
            fun described(streaming: Boolean?) = execution(ds, describe, streaming).use { it.execDescribe() }
            val eagerDescription = described(false)
            assertTrue(eagerDescription.size() > 0)
            assertTrue(eagerDescription.isIsomorphicWith(described(null)))
            val streamed = ModelFactory.createDefaultModel()
            execution(ds, describe, null).use { e -> e.execDescribeTriples().forEachRemaining { streamed.graph.add(it) } }
            assertTrue(eagerDescription.isIsomorphicWith(streamed))
        }
    }

    @Test fun timeouts_and_abort_stop_a_streamed_result_between_reads() {
        people(20000).use { ds ->
            for (streaming in listOf(null, true)) {
                val timed = QueryExecution.dataset(DatasetFactory.wrap(ds)).query(scan).context(context(streaming))
                    .timeout(200, TimeUnit.MILLISECONDS).build()
                timed.use { e ->
                    val rs = e.execSelect()
                    assertTrue(rs.hasNext())
                    rs.next()
                    Thread.sleep(500)
                    assertThrows(QueryCancelledException::class.java) { while (rs.hasNext()) rs.next() }
                }
                execution(ds, scan, streaming).use { e ->
                    val rs = e.execSelect()
                    assertTrue(rs.hasNext())
                    rs.next()
                    e.abort()
                    assertThrows(QueryCancelledException::class.java) { while (rs.hasNext()) rs.next() }
                }
            }
            assertEquals(20000, execution(ds, scan, null).use { ResultSetFormatter.consume(it.execSelect()) })
        }
    }

    /**
     * `abort` runs `requestCancel` on another thread while the iterator's thread finishes
     * and releases its native query. The abort must never fail because the native object
     * was closed in between, and a cancellation that wins still ends the read cleanly.
     */
    @Test fun abort_racing_the_release_of_a_query_never_throws() {
        people(300).use { ds ->
            val failures = java.util.concurrent.ConcurrentLinkedQueue<Throwable>()
            for (streaming in listOf(null, true, false)) {
                repeat(3000) {
                    execution(ds, scan, streaming).use { e ->
                        val stop = java.util.concurrent.atomic.AtomicBoolean(false)
                        val canceller = Thread {
                            while (!stop.get()) {
                                try {
                                    e.abort()
                                } catch (t: Throwable) {
                                    failures.add(t)
                                    return@Thread
                                }
                            }
                        }
                        canceller.start()
                        try {
                            val rs = e.execSelect()
                            while (rs.hasNext()) rs.next()
                        } catch (_: QueryCancelledException) {
                            // the abort won before the rows ran out
                        } finally {
                            stop.set(true)
                            canceller.join()
                        }
                    }
                }
            }
            assertTrue(failures.isEmpty(), "abort failed: ${failures.firstOrNull()}")
            assertEquals(300, execution(ds, scan, null).use { ResultSetFormatter.consume(it.execSelect()) })
        }
    }

    @Test fun reading_after_the_read_transaction_ends_fails_cleanly() {
        people(5000).use { ds ->
            for (streaming in listOf(null, true, false)) {
                var seen = 0
                val rs = Txn.calculateRead(ds) {
                    val rs = QueryExec.dataset(ds).query(scan).context(context(streaming)).build().select()
                    assertTrue(rs.hasNext())
                    rs.next()
                    seen++
                    rs
                }
                assertThrows(JenaTransactionException::class.java) { while (rs.hasNext()) { rs.next(); seen++ } }
                assertEquals(1, seen, "no rows after the transaction ended")
                // a whole read inside the transaction gives every row
                assertEquals(5000, Txn.calculateRead(ds) {
                    var n = 0
                    QueryExec.dataset(ds).query(scan).context(context(streaming)).build().select().forEachRemaining { n++ }
                    n
                })
            }
        }
    }
}
