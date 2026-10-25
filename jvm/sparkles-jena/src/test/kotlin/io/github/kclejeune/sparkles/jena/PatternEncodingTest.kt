package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.ByteBuf
import io.github.kclejeune.sparkles.jena.internal.TermWriter
import io.github.kclejeune.sparkles.jena.internal.encodePattern
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.Node_URI
import org.apache.jena.sparql.core.Quad
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class PatternEncodingTest {
    private fun iri(text: String): Node = NodeFactory.createURI(text)

    private fun fresh(g: Node?, s: Node?, p: Node?, o: Node?): ByteArray {
        val w = TermWriter(ByteBuf(128), repeats = false)
        w.graph(g)
        w.slot(s)
        w.slot(p)
        w.slot(o)
        return w.buf.toByteArray()
    }

    @Test
    fun custom_node_getters_can_encode_nested_patterns() {
        val graph = iri("urn:outer:graph")
        val predicate = iri("urn:outer:predicate")
        val value = NodeFactory.createLiteralString("café 🙂")
        val innerSubject = iri("urn:inner:subject")
        val innerExpected = fresh(Quad.defaultGraphIRI, innerSubject, null, null)
        val subject = object : Node_URI("urn:outer:subject") {
            override fun getURI(): String {
                assertArrayEquals(innerExpected, encodePattern(Quad.defaultGraphIRI, innerSubject, null, null))
                return super.getURI()
            }
        }
        assertArrayEquals(
            fresh(graph, iri("urn:outer:subject"), predicate, value),
            encodePattern(graph, subject, predicate, value),
        )
    }

    @Test
    fun failed_and_large_encodings_do_not_pollute_later_patterns() {
        val bad = object : Node_URI("urn:bad") {
            override fun getURI(): String = throw IllegalArgumentException("getter failed")
        }
        val graph = iri("urn:graph")
        assertThrows(IllegalArgumentException::class.java) { encodePattern(graph, bad, null, null) }
        val large = NodeFactory.createLiteralString("🙂".repeat(5000))
        assertArrayEquals(fresh(null, null, null, large), encodePattern(null, null, null, large))
        val subject = NodeFactory.createBlankNode("small")
        assertArrayEquals(fresh(Quad.unionGraph, subject, Node.ANY, null), encodePattern(Quad.unionGraph, subject, Node.ANY, null))
    }

    @Test
    fun simultaneous_patterns_keep_their_threads_and_results_independent() {
        val workers = Executors.newFixedThreadPool(4)
        try {
            val tasks = (0 until 4).map { thread ->
                Callable {
                    repeat(256) { row ->
                        val g = iri("urn:graph:$thread")
                        val s = iri("urn:subject:$row")
                        val p = iri("urn:predicate:$thread:$row")
                        val o = NodeFactory.createLiteralString("résultat 🙂 $thread $row")
                        val encoded = encodePattern(g, s, p, o)
                        encodePattern(null, null, null, null)
                        assertArrayEquals(fresh(g, s, p, o), encoded)
                    }
                }
            }
            workers.invokeAll(tasks).forEach { it.get(15, TimeUnit.SECONDS) }
        } finally {
            workers.shutdownNow()
        }
    }
}
