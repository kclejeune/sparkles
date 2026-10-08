package io.github.kclejeune.sparkles.jena

import org.apache.jena.query.QueryFactory
import org.apache.jena.query.TxnType
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.algebra.Algebra
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.Context
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.nio.file.Path

class AdministrationTest {
    @Test
    fun describe_retains_from_and_from_named_graph_selection() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("""
                <urn:s> <urn:p> <urn:default> .
                <urn:ga> { <urn:s> <urn:p> <urn:oa> . }
                <urn:gb> { <urn:s> <urn:p> <urn:ob> . }
            """.toByteArray()), Lang.TRIG)
            fun objects(query: String, fallback: SparklesFallback): Set<String> =
                QueryExec.dataset(ds).query(query).context(Context().set(Sparkles.FALLBACK, fallback)).build().use {
                    it.describe().find().toList().map { triple -> triple.`object`.uri }.toSet()
                }
            for (fallback in listOf(SparklesFallback.AUTO, SparklesFallback.ALWAYS)) {
                assertEquals(setOf("urn:oa"), objects("DESCRIBE <urn:s> FROM <urn:ga>", fallback))
                assertEquals(setOf("urn:ob"), objects("DESCRIBE <urn:s> FROM NAMED <urn:gb>", fallback))
                assertEquals(setOf("urn:oa", "urn:ob"), objects("DESCRIBE ?s FROM <urn:ga> FROM NAMED <urn:gb> WHERE { ?s <urn:p> ?o }", fallback))
                assertEquals(setOf("urn:default", "urn:oa", "urn:ob"), objects("DESCRIBE <urn:s>", fallback))
            }
        }
    }
    private fun count(ds: DatasetGraphSparkles, context: Context = Context()): Long =
        QueryExec.dataset(ds).query("SELECT (COUNT(*) AS ?n) { ?s ?p ?o }").context(context).select().next().get("n").literalValue.toString().toLong()

    @Test
    fun apply_patch_commits_rows_and_prefixes_and_honours_abort() {
        SparklesDatasets.memory().use { ds ->
            val patch = """
                TX .
                PA "ex" <http://example/> .
                A <http://example/s> <http://example/p> "1" .
                A <http://example/s> <http://example/p> "2" .
                D <http://example/s> <http://example/p> "2" .
                TC .
            """.trimIndent()
            val report = ds.applyPatch(ByteArrayInputStream(patch.toByteArray()))
            assertTrue(report.receipt.isCommitted())
            assertEquals(1, report.prefixesSet)
            assertFalse(report.isAborted())
            assertEquals(1, count(ds))
            assertEquals(report.receipt, ds.lastReceipt())
            assertEquals("http://example/", ds.prefixes().get("ex"))

            val head = ds.headCommit().seq
            val aborted = ds.applyPatch(ByteArrayInputStream("TX .\nA <urn:a> <urn:b> <urn:c> .\nTA .\n".toByteArray()))
            assertTrue(aborted.isAborted())
            assertEquals(head, ds.headCommit().seq)
            assertEquals(1, count(ds))

            ds.begin(TxnType.READ)
            try {
                assertThrows(org.apache.jena.sparql.JenaTransactionException::class.java) {
                    ds.applyPatch(ByteArrayInputStream(patch.toByteArray()))
                }
            } finally { ds.end() }
        }
    }

    @Test
    fun pinned_views_context_and_fallback_share_the_selected_snapshot() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("<urn:s> <urn:p> 1 .".toByteArray()), Lang.TURTLE)
            val first = ds.headCommit().seq
            ds.snapshots().create("before")
            ds.at("snapshot:before").use { view ->
                ds.load(ByteArrayInputStream("<urn:s> <urn:p> 2 .".toByteArray()), Lang.TURTLE)
                assertEquals(1, count(view))
                assertEquals(2, count(ds))
                assertThrows(org.apache.jena.sparql.JenaTransactionException::class.java) { view.begin(TxnType.WRITE) }
                assertThrows(SparklesNotPermittedException::class.java) { view.snapshots().delete("before") }
                val bytes = ByteArrayOutputStream(); view.dump(bytes, Lang.NQUADS)
                assertTrue(bytes.toString().contains("1")); assertFalse(bytes.toString().contains("\"2\""))
            }
            assertEquals(1, count(ds, Context().set(Sparkles.AT, first)))
            assertEquals(1, count(ds, Context().set(Sparkles.AT, "snapshot:before").set(Sparkles.FALLBACK, SparklesFallback.ALWAYS)))
            assertNotNull(ds.snapshots().get("before"))
            assertTrue(ds.history().commits().size >= 3)
        }
    }

    @Test
    fun describe_uses_the_configured_engine_strategy_and_other_datasets_keep_jena_handlers() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("<urn:s> <urn:p> _:b . _:b <urn:q> 2 .".toByteArray()), Lang.TURTLE)
            assertEquals(2, QueryExec.dataset(ds).query("DESCRIBE <urn:s>").describe().size())
            ds.settings().describe().set(DescribeOptions(mode = DescribeMode.OUTGOING))
            assertEquals(1, QueryExec.dataset(ds).query("DESCRIBE <urn:s>").describe().size())
            ds.settings().describe().reset()
            assertEquals(DescribeMode.CBD, ds.settings().describe().get().mode)
        }
        val other = org.apache.jena.sparql.core.DatasetGraphFactory.createTxnMem()
        org.apache.jena.system.Txn.executeWrite(other) {
            other.add(org.apache.jena.sparql.core.Quad.create(org.apache.jena.sparql.core.Quad.defaultGraphIRI,
                org.apache.jena.graph.NodeFactory.createURI("urn:s"),org.apache.jena.graph.NodeFactory.createURI("urn:p"),
                org.apache.jena.graph.NodeFactory.createLiteralString("value")))
        }
        assertEquals(1, QueryExec.dataset(other).query("DESCRIBE <urn:s>").describe().size())
        other.close()
    }

    @Test
    fun algebra_routes_to_the_native_engine() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("<urn:s> <urn:p> 1 .".toByteArray()), Lang.TURTLE)
            val before = ds.stats().nativeQueries
            val iterator = Algebra.exec(Algebra.compile(QueryFactory.create("SELECT ?s { ?s <urn:p> 1 }")), ds)
            try { assertTrue(iterator.hasNext()); assertEquals("urn:s",iterator.next().get("s").uri) }
            finally { iterator.close() }
            assertEquals(before + 1, ds.stats().nativeQueries)
        }
    }

    @Test
    fun handles_are_invalidated_by_owner_close_and_do_not_keep_the_directory_locked(@TempDir dir: Path) {
        val ds = SparklesDatasets.open(dir)
        val snapshots = ds.snapshots()
        val describe = ds.settings().describe()
        val text = ds.indexes().text()
        ds.close()
        assertThrows(SparklesInvalidException::class.java) { snapshots.list() }
        assertThrows(SparklesInvalidException::class.java) { describe.get() }
        assertThrows(SparklesInvalidException::class.java) { text.status() }
        SparklesDatasets.open(dir).close()
    }

    @Test
    fun capture_calls_refuse_the_current_transaction_without_mutating_it(@TempDir dir: Path) {
        SparklesDatasets.memory().use { ds ->
            ds.begin(TxnType.WRITE)
            try {
                assertThrows(org.apache.jena.sparql.JenaTransactionException::class.java) { ds.cloneTo(dir.resolve("clone")) }
                assertThrows(org.apache.jena.sparql.JenaTransactionException::class.java) { ds.backup(dir.resolve("backup")) }
                assertThrows(org.apache.jena.sparql.JenaTransactionException::class.java) { ds.compact() }
            } finally { ds.abort() }
        }
    }
}
