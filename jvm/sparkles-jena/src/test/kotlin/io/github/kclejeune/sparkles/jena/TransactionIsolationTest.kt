package io.github.kclejeune.sparkles.jena

import org.apache.jena.graph.NodeFactory
import org.apache.jena.query.ReadWrite
import org.apache.jena.query.TxnType
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.system.Txn
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class TransactionIsolationTest {
    private fun quad(o: String) = Quad.create(
        Quad.defaultGraphIRI,
        NodeFactory.createURI("urn:s"),
        NodeFactory.createURI("urn:p"),
        NodeFactory.createURI("urn:$o"),
    )

    private fun count(ds: DatasetGraphSparkles): Long =
        QueryExec.dataset(ds).query("SELECT (COUNT(*) AS ?n) { ?s ?p ?o }").select().next().get("n").literalValue.toString().toLong()

    @Test
    fun a_pinned_view_transaction_does_not_leak_into_the_live_dataset_on_the_same_thread() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("<urn:s> <urn:p> <urn:a> .".toByteArray()), Lang.NTRIPLES)
            ds.snapshots().create("before")
            ds.load(ByteArrayInputStream("<urn:s> <urn:p> <urn:b> .".toByteArray()), Lang.NTRIPLES)
            ds.at("snapshot:before").use { view ->
                view.begin(TxnType.READ)
                assertTrue(view.isInTransaction())
                assertFalse(ds.isInTransaction())
                assertNull(ds.transactionMode())

                // Live reads on this thread still see the newest commit.
                assertTrue(ds.contains(quad("b")))
                assertEquals(2, ds.find().asSequence().count())
                assertEquals(2, count(ds))
                assertFalse(view.contains(quad("b")))
                assertEquals(1, count(view))

                // The live dataset can run its own write transaction meanwhile.
                ds.begin(TxnType.WRITE)
                assertEquals(ReadWrite.WRITE, ds.transactionMode())
                assertEquals(ReadWrite.READ, view.transactionMode())
                ds.add(quad("c"))
                ds.commit()
                ds.end()

                assertTrue(view.isInTransaction())
                assertFalse(view.contains(quad("c")))
                view.end()
                assertFalse(view.isInTransaction())
            }
            assertTrue(ds.contains(quad("c")))
        }
    }

    @Test
    fun txn_execute_read_on_a_view_leaves_live_writes_working() {
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("<urn:s> <urn:p> <urn:a> .".toByteArray()), Lang.NTRIPLES)
            ds.snapshots().create("before")
            ds.at("snapshot:before").use { view ->
                Txn.executeRead(view) {
                    assertFalse(ds.isInTransaction())
                    Txn.executeWrite(ds) { ds.add(quad("b")) }
                    assertTrue(ds.contains(quad("b")))
                    assertFalse(view.contains(quad("b")))
                }
                assertFalse(view.isInTransaction())
            }
        }
    }

    @Test
    fun commit_throws_when_another_thread_closed_the_dataset() {
        val ds = SparklesDatasets.memory()
        val begun = CountDownLatch(1)
        val closed = CountDownLatch(1)
        val pool = Executors.newSingleThreadExecutor()
        try {
            val result = pool.submit<Throwable?> {
                ds.begin(TxnType.WRITE)
                ds.add(quad("a"))
                begun.countDown()
                closed.await(10, TimeUnit.SECONDS)
                try {
                    ds.commit()
                    null
                } catch (e: Throwable) {
                    e
                }
            }
            assertTrue(begun.await(10, TimeUnit.SECONDS))
            ds.close()
            closed.countDown()
            val error = result.get(10, TimeUnit.SECONDS)
            assertInstanceOf(JenaTransactionException::class.java, error)
            assertTrue(error!!.message!!.contains("closed"), error.message)
        } finally {
            pool.shutdownNow()
        }
    }
}
