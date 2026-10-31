package io.github.kclejeune.sparkles.jena

import org.apache.jena.datatypes.xsd.XSDDatatype
import org.apache.jena.graph.NodeFactory
import org.apache.jena.riot.Lang
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.core.DatasetGraphFactory
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.util.IsoMatcher
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.nio.file.Files
import java.nio.file.Path
import java.util.zip.GZIPOutputStream

/**
 * Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON load through `load` and `loadFiles` as the
 * W3C syntaxes do, and a malformed body is an error that names the syntax.
 */
class JenaSyntaxLoadTest {
    private val ex = "http://example/"
    private fun iri(l: String) = NodeFactory.createURI(ex + l)

    /** Default-graph triples with a blank node, and one quad in a named graph. */
    private fun source(quads: Boolean) = DatasetGraphFactory.create().also {
        it.add(Quad.defaultGraphIRI, iri("a"), iri("p"), NodeFactory.createLiteralDT("1", XSDDatatype.XSDinteger))
        it.add(Quad.defaultGraphIRI, iri("a"), iri("p"), NodeFactory.createLiteralLang("deux", "fr"))
        val b = NodeFactory.createBlankNode()
        it.add(Quad.defaultGraphIRI, b, iri("q"), iri("a"))
        it.add(Quad.defaultGraphIRI, iri("a"), iri("r"), b)
        if (quads) it.add(iri("g"), iri("c"), iri("p"), iri("a"))
    }

    private val syntaxes = listOf(
        Triple(Lang.TRIX, "d.trix", true),
        Triple(Lang.RDFTHRIFT, "d.rt", true),
        Triple(Lang.RDFPROTO, "d.rpb", true),
        Triple(Lang.RDFJSON, "d.rj", false),
    )

    private fun encode(lang: Lang, quads: Boolean): ByteArray {
        val out = ByteArrayOutputStream()
        val src = source(quads)
        if (quads) RDFDataMgr.write(out, src, lang) else RDFDataMgr.write(out, src.defaultGraph, lang)
        return out.toByteArray()
    }

    private fun gzip(bytes: ByteArray): ByteArray {
        val out = ByteArrayOutputStream()
        GZIPOutputStream(out).use { it.write(bytes) }
        return out.toByteArray()
    }

    private fun assertIsomorphic(want: org.apache.jena.sparql.core.DatasetGraph, got: org.apache.jena.sparql.core.DatasetGraph, what: String) {
        val a = want.find().asSequence().toList()
        val b = got.find().asSequence().toList()
        assertEquals(a.size, b.size, what)
        assertTrue(IsoMatcher.isomorphic(want, got), what)
    }

    @Test
    fun load_reads_every_jena_syntax_from_a_stream() {
        for ((lang, _, quads) in syntaxes) {
            val dsg = SparklesDatasets.memory()
            dsg.use {
                it.load(ByteArrayInputStream(encode(lang, quads)), lang)
                assertIsomorphic(source(quads), it, lang.name)
            }
        }
    }

    @Test
    fun load_files_reads_every_jena_syntax_by_extension_plain_and_compressed(@TempDir dir: Path) {
        for ((lang, name, quads) in syntaxes) {
            val bytes = encode(lang, quads)
            val plain = dir.resolve(name)
            val zipped = dir.resolve("$name.gz")
            Files.write(plain, bytes)
            Files.write(zipped, gzip(bytes))
            for (file in listOf(plain, zipped)) {
                SparklesDatasets.memory().use {
                    it.loadFiles(listOf(file))
                    assertIsomorphic(source(quads), it, "${lang.name} $file")
                }
            }
            SparklesDatasets.memory().use {
                it.loadFiles(listOf(plain), iri("t"))
                assertTrue(it.contains(iri("t"), iri("a"), iri("p"), null), lang.name)
            }
        }
    }

    @Test
    fun a_malformed_body_is_an_error_that_names_the_syntax() {
        for ((lang, _, quads) in syntaxes) {
            val bytes = encode(lang, quads)
            val cut = bytes.copyOf(bytes.size / 2)
            SparklesDatasets.memory().use {
                val e = assertThrows(Exception::class.java) { it.load(ByteArrayInputStream(cut), lang) }
                assertTrue(e.message!!.contains(lang.name) || e.message!!.contains("Thrift") || e.message!!.contains("Protobuf") ||
                    e.message!!.contains("TriX") || e.message!!.contains("JSON"), "${lang.name}: ${e.message}")
                assertEquals(0L, it.find().asSequence().count().toLong())
            }
        }
    }
}
