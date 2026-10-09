package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.NativeLoader
import io.github.kclejeune.sparkles.jena.internal.encodePattern
import io.github.kclejeune.sparkles.jena.internal.ffi.ErrorKind
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiException
import io.github.kclejeune.sparkles.jena.internal.ffi.InternalException
import io.github.kclejeune.sparkles.jena.internal.ffi.SparklesJni
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test

/**
 * The hand-written JNI calls of P04 §5.4 and their UniFFI fallback. The suite runs once
 * with them (`test`) and once with `-Dsparkles.jni=false` (`testUniffi`), so every test
 * here, and every other test of the suite, checks that both paths give the same answers
 * and the same exceptions.
 */
class JniCallsTest {
    private val ex = "http://example/"
    private fun iri(l: String): Node = NodeFactory.createURI(ex + l)
    private val open = ArrayList<DatasetGraphSparkles>()

    private fun filled(n: Int): DatasetGraphSparkles {
        val dsg = SparklesDatasets.memory().also { open.add(it) }
        Txn.executeWrite(dsg) {
            for (i in 0 until n) {
                dsg.add(Quad.defaultGraphIRI, iri("s${i % 10}"), iri("p$i"), NodeFactory.createLiteralString("v$i"))
                dsg.add(iri("g"), iri("s${i % 10}"), iri("p$i"), NodeFactory.createLiteralString("v$i"))
            }
        }
        return dsg
    }

    @AfterEach
    fun close() {
        open.forEach { it.close() }
    }

    @Test
    fun the_switch_picks_the_path() {
        filled(1)
        val expected = !System.getProperty("sparkles.jni").equals("false", ignoreCase = true)
        assertEquals(expected, NativeLoader.jni)
        assertEquals(expected, SparklesJni.ENABLED)
    }

    @Test
    fun errors_and_panics_become_the_exceptions_uniffi_throws() {
        filled(1)
        assumeTrue(SparklesJni.ENABLED)
        assertEquals(0, SparklesJni.selfTest(0))
        val e = assertThrows(FfiException.Engine::class.java) { SparklesJni.selfTest(1) }
        assertEquals(ErrorKind.INVALID, e.kind)
        assertEquals("the JNI self-test error", e.detail)
        val p = assertThrows(InternalException::class.java) { SparklesJni.selfTest(2) }
        assertEquals("the JNI self-test panic", p.message)
        // the thread and the library work on after a panic
        assertEquals(3, SparklesJni.selfTest(3))
    }

    private fun countFind(dsg: DatasetGraphSparkles, g: Node?, s: Node?, p: Node?, o: Node?): Int {
        val it = dsg.find(g, s, p, o)
        var n = 0
        while (it.hasNext()) {
            it.next()
            n++
        }
        return n
    }

    @Test
    fun reads_agree_on_the_head_and_in_a_transaction() {
        val dsg = filled(300)
        val v = NodeFactory.createLiteralString("v7")
        val checks = {
            assertTrue(dsg.contains(Quad.defaultGraphIRI, iri("s7"), iri("p7"), v))
            assertFalse(dsg.contains(Quad.defaultGraphIRI, iri("s7"), iri("p7"), iri("v7")))
            assertTrue(dsg.contains(Node.ANY, iri("s7"), Node.ANY, Node.ANY))
            assertTrue(dsg.defaultGraph.contains(iri("s7"), iri("p7"), v))
            // 30 matches fit the first batch, 300 and 600 need the cursor
            assertEquals(30, countFind(dsg, Quad.defaultGraphIRI, iri("s3"), null, null))
            assertEquals(300, countFind(dsg, Quad.defaultGraphIRI, null, null, null))
            assertEquals(600, countFind(dsg, null, null, null, null))
            assertEquals(300, countFind(dsg, Quad.unionGraph, null, null, null))
            assertEquals("v7", dsg.defaultGraph.find(iri("s7"), iri("p7"), Node.ANY).next().`object`.literalLexicalForm)
            assertEquals(30, QueryExec.dataset(dsg).query("SELECT ?o { <${ex}s3> ?p ?o }").select().asSequence().count())
            assertTrue(QueryExec.dataset(dsg).query("ASK { <${ex}s3> <${ex}p3> \"v3\" }").ask())
        }
        checks()
        Txn.executeRead(dsg) { checks() }
    }

    @Test
    fun budgets_fail_with_their_fields() {
        val dsg = filled(300)
        val rows = Context().set(Sparkles.MAX_ROWS, 2L)
        val e = assertThrows(SparklesBudgetExceededException::class.java) {
            Txn.executeRead(dsg) {
                QueryExec.dataset(dsg).query("SELECT * { ?s ?p ?o }").context(rows).select().asSequence().count()
            }
        }
        assertEquals("rows", e.budget)
        assertEquals(2L, e.limit)
        val memory = Context().set(Sparkles.MAX_MEMORY_BYTES, 64L)
        val m = assertThrows(SparklesBudgetExceededException::class.java) {
            QueryExec.dataset(dsg).query("SELECT * { ?s ?p ?o . ?a ?b ?c } ORDER BY ?o ?c").context(memory)
                .select().asSequence().count()
        }
        assertEquals("memory", m.budget)
        assertEquals(64L, m.limit)
    }

    @Test
    fun a_closed_object_is_refused_as_uniffi_refuses_it() {
        val dsg = filled(3)
        val txn = SparklesJni.beginRead(dsg.handle.ffi)
        val pattern = encodePattern(Quad.defaultGraphIRI, iri("s1"), iri("p1"), NodeFactory.createLiteralString("v1"))
        assertTrue(SparklesJni.contains(txn, pattern))
        txn.close()
        assertThrows(IllegalStateException::class.java) { SparklesJni.contains(txn, pattern) }
    }
}
