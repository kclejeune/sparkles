package io.github.kclejeune.sparkles.jena.internal

import org.apache.jena.datatypes.TypeMapper
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.TextDirection
import org.apache.jena.graph.Triple
import org.apache.jena.sparql.core.Quad
import java.nio.charset.StandardCharsets

/**
 * The batch encoding of P04 §2.4, the Kotlin half of `crates/sparkles-ffi/src/encode.rs`.
 * Terms go to Rust as tagged byte strings, and results come back as row batches whose
 * cells index a term table that both sides build once per result.
 */
internal object Tag {
    const val NONE = 0
    const val IRI = 1
    const val BNODE = 2
    const val STRING = 3
    const val LANG = 4
    const val DIR_LANG = 5
    const val COMMON = 6
    const val TYPED = 7
    const val TRIPLE = 8
    const val DEFAULT_GRAPH = 9
    const val UNION_GRAPH = 10
    const val REPEAT = 11

    const val OP_ADD = 1
    const val OP_DELETE = 2

    const val FLAG_RESTART = 1
}

/** The version of the encoding this library speaks. */
internal const val ENCODING_VERSION: Int = 1

private const val XSD = "http://www.w3.org/2001/XMLSchema#"

/** The datatypes of tag 6, by index; the Rust side has the same table. */
internal val DATATYPES: Array<String> = arrayOf(
    XSD + "integer",
    XSD + "decimal",
    XSD + "double",
    XSD + "float",
    XSD + "boolean",
    XSD + "dateTime",
    XSD + "date",
    XSD + "time",
    XSD + "duration",
    XSD + "dayTimeDuration",
    XSD + "yearMonthDuration",
    XSD + "long",
    XSD + "int",
    XSD + "short",
    XSD + "byte",
    XSD + "nonNegativeInteger",
    XSD + "positiveInteger",
    XSD + "nonPositiveInteger",
    XSD + "negativeInteger",
    XSD + "unsignedLong",
    XSD + "unsignedInt",
    XSD + "gYear",
    XSD + "anyURI",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON",
)

private val DATATYPE_INDEX: Map<String, Int> = DATATYPES.withIndex().associate { it.value to it.index }
private val XSD_STRING = XSD + "string"

/** A growable byte buffer with the encoding's primitives. */
internal class ByteBuf(initial: Int = 256) {
    private var buf = ByteArray(initial)
    var size: Int = 0
        private set

    private fun ensure(n: Int) {
        if (size + n > buf.size) {
            buf = buf.copyOf(maxOf(buf.size * 2, size + n))
        }
    }

    fun put(b: Int) {
        ensure(1)
        buf[size++] = b.toByte()
    }

    fun putLen(value: Int) {
        var n = value
        ensure(5)
        while (true) {
            val b = n and 0x7f
            n = n ushr 7
            if (n == 0) {
                buf[size++] = b.toByte()
                return
            }
            buf[size++] = (b or 0x80).toByte()
        }
    }

    fun putString(s: String) {
        // ASCII is written in place in one pass. A string with anything else is written
        // again from the start through the UTF-8 encoder.
        val n = s.length
        val start = size
        putLen(n)
        ensure(n)
        val b = buf
        val at = size
        for (i in 0 until n) {
            val c = s[i].code
            if (c >= 0x80) {
                size = start
                val bytes = s.toByteArray(StandardCharsets.UTF_8)
                putLen(bytes.size)
                ensure(bytes.size)
                System.arraycopy(bytes, 0, buf, size, bytes.size)
                size += bytes.size
                return
            }
            b[at + i] = c.toByte()
        }
        size = at + n
    }

    fun toByteArray(): ByteArray = buf.copyOf(size)

    fun clear() {
        size = 0
        if (buf.size > (4 shl 20)) buf = ByteArray(256)
    }
}

/**
 * Writes terms into a batch. A term already written at the top level of the batch is
 * sent as a repeat of its position (tag 11), so a batch of quads sends each shared IRI once.
 */
internal class TermWriter(val buf: ByteBuf = ByteBuf(), repeats: Boolean = true) {
    /** `null` when every term is written in full, which suits a pattern of a few terms */
    private val positions: HashMap<Node, Int>? = if (repeats) HashMap() else null
    private var next = 0

    fun reset() {
        buf.clear()
        positions?.clear()
        next = 0
    }

    fun wildcard() = buf.put(Tag.NONE)

    /** A graph position: wildcard, the default graph, the union graph or a term. */
    fun graph(g: Node?) {
        when {
            g == null || g === Node.ANY || g.isVariable -> wildcard()
            Quad.isDefaultGraph(g) || g === Quad.tripleInQuad -> buf.put(Tag.DEFAULT_GRAPH)
            Quad.isUnionGraph(g) -> buf.put(Tag.UNION_GRAPH)
            else -> term(g)
        }
    }

    /** A subject, predicate or object position of a pattern: `null`, ANY and variables match anything. */
    fun slot(n: Node?) {
        if (n == null || n === Node.ANY || n.isVariable) wildcard() else term(n)
    }

    fun term(n: Node) {
        val positions = positions
        if (positions == null) {
            raw(n)
            return
        }
        val pos = positions[n]
        if (pos != null) {
            buf.put(Tag.REPEAT)
            buf.putLen(pos)
            return
        }
        raw(n)
        positions[n] = next++
    }

    private fun raw(n: Node) {
        when {
            n.isURI -> {
                buf.put(Tag.IRI)
                buf.putString(n.uri)
            }
            n.isBlank -> {
                buf.put(Tag.BNODE)
                buf.putString(n.blankNodeLabel)
            }
            n.isLiteral -> literal(n)
            n.isTripleTerm -> {
                val t = n.triple
                buf.put(Tag.TRIPLE)
                raw(t.subject)
                raw(t.predicate)
                raw(t.`object`)
            }
            else -> throw IllegalArgumentException("not an RDF term: $n")
        }
    }

    private fun literal(n: Node) {
        val lang = n.literalLanguage
        if (!lang.isNullOrEmpty()) {
            val dir = n.literalBaseDirection
            if (dir != null) {
                buf.put(Tag.DIR_LANG)
                buf.putString(n.literalLexicalForm)
                buf.putString(lang)
                buf.put(if (dir == TextDirection.RTL) 1 else 0)
            } else {
                buf.put(Tag.LANG)
                buf.putString(n.literalLexicalForm)
                buf.putString(lang)
            }
            return
        }
        val dt = n.literalDatatypeURI
        if (dt == null || dt == XSD_STRING) {
            buf.put(Tag.STRING)
            buf.putString(n.literalLexicalForm)
            return
        }
        val i = DATATYPE_INDEX[dt]
        if (i != null) {
            buf.put(Tag.COMMON)
            buf.putString(n.literalLexicalForm)
            buf.put(i)
        } else {
            buf.put(Tag.TYPED)
            buf.putString(n.literalLexicalForm)
            buf.putString(dt)
        }
    }
}

/** The encoding of one quad pattern (graph, subject, predicate, object). */
internal fun encodePattern(g: Node?, s: Node?, p: Node?, o: Node?): ByteArray {
    val w = TermWriter(ByteBuf(128), repeats = false)
    w.graph(g)
    w.slot(s)
    w.slot(p)
    w.slot(o)
    return w.buf.toByteArray()
}

/** A batch of terms in the given order, `null` as tag 0. */
internal fun encodeTerms(nodes: List<Node?>): ByteArray {
    val w = TermWriter(ByteBuf(64))
    for (n in nodes) if (n == null) w.wildcard() else w.term(n)
    return w.buf.toByteArray()
}

/** Reads the terms of a batch. */
internal class ByteReader(private val b: ByteArray, var pos: Int = 0) {
    fun u8(): Int = b[pos++].toInt() and 0xff

    fun u16(): Int {
        val v = (b[pos].toInt() and 0xff) or ((b[pos + 1].toInt() and 0xff) shl 8)
        pos += 2
        return v
    }

    fun u32(): Int {
        val v = (b[pos].toInt() and 0xff) or
            ((b[pos + 1].toInt() and 0xff) shl 8) or
            ((b[pos + 2].toInt() and 0xff) shl 16) or
            ((b[pos + 3].toInt() and 0xff) shl 24)
        pos += 4
        return v
    }

    fun len(): Int {
        var n = 0
        var shift = 0
        while (true) {
            val x = u8()
            n = n or ((x and 0x7f) shl shift)
            if (x and 0x80 == 0) return n
            shift += 7
        }
    }

    fun string(): String {
        val n = len()
        val s = String(b, pos, n, StandardCharsets.UTF_8)
        pos += n
        return s
    }

    /** A term; the graph tags give Jena's default graph and union graph nodes. */
    fun term(): Node? = termOf(u8())

    private fun termOf(tag: Int): Node? = when (tag) {
        Tag.NONE -> null
        Tag.IRI -> NodeFactory.createURI(string())
        Tag.BNODE -> NodeFactory.createBlankNode(string())
        Tag.STRING -> NodeFactory.createLiteralString(string())
        Tag.LANG -> {
            val lex = string()
            NodeFactory.createLiteralLang(lex, string())
        }
        Tag.DIR_LANG -> {
            val lex = string()
            val lang = string()
            val dir = if (u8() == 1) TextDirection.RTL else TextDirection.LTR
            NodeFactory.createLiteralDirLang(lex, lang, dir)
        }
        Tag.COMMON -> {
            val lex = string()
            NodeFactory.createLiteralDT(lex, COMMON_TYPES[u8()])
        }
        Tag.TYPED -> {
            val lex = string()
            NodeFactory.createLiteralDT(lex, TypeMapper.getInstance().getSafeTypeByName(string()))
        }
        Tag.TRIPLE -> {
            val s = termOf(u8())
            val p = termOf(u8())
            val o = termOf(u8())
            NodeFactory.createTripleTerm(Triple.create(s, p, o))
        }
        Tag.DEFAULT_GRAPH -> Quad.defaultGraphIRI
        Tag.UNION_GRAPH -> Quad.unionGraph
        else -> throw IllegalStateException("unknown term tag $tag")
    }

    private companion object {
        val COMMON_TYPES = DATATYPES.map { TypeMapper.getInstance().getSafeTypeByName(it) }.toTypedArray()
    }
}

/** One decoded row batch: `cells` holds `rows × columns` indexes into the decoder's table. */
internal class RowBatch(val rows: Int, val columns: Int, val cells: IntArray)

/**
 * Decodes the row batches of one result, keeping the term table across batches so that
 * a term is decoded and allocated once per result.
 */
internal class RowDecoder {
    private val table = ArrayList<Node>()

    fun decode(bytes: ByteArray): RowBatch {
        val r = ByteReader(bytes)
        val version = r.u8()
        check(version == ENCODING_VERSION) { "batch encoding $version, expected $ENCODING_VERSION" }
        val flags = r.u8()
        if (flags and Tag.FLAG_RESTART != 0) table.clear()
        val n = r.u32()
        table.ensureCapacity(table.size + n)
        repeat(n) { table.add(r.term() ?: Node.ANY) }
        val rows = r.u32()
        val cols = r.u16()
        val cells = IntArray(rows * cols)
        for (i in cells.indices) cells[i] = r.u32()
        return RowBatch(rows, cols, cells)
    }

    /** The node of a cell, or `null` for an unbound one. */
    fun node(cell: Int): Node? = if (cell == 0) null else table[cell - 1]
}
