package io.github.kclejeune.sparkles.jena

import org.apache.jena.atlas.iterator.Iter
import org.apache.jena.datatypes.xsd.XSDDatatype
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.TextDirection
import org.apache.jena.graph.Triple
import org.apache.jena.query.DatasetFactory
import org.apache.jena.query.QueryCancelledException
import org.apache.jena.query.QueryExecException
import org.apache.jena.query.QueryExecution
import org.apache.jena.query.QueryParseException
import org.apache.jena.query.ReadWrite
import org.apache.jena.query.TxnType
import org.apache.jena.rdf.model.ModelFactory
import org.apache.jena.riot.Lang
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.exec.UpdateExec
import org.apache.jena.sparql.expr.NodeValue
import org.apache.jena.sparql.function.FunctionBase1
import org.apache.jena.sparql.function.FunctionRegistry
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.apache.jena.tdb2.DatabaseMgr
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/** The acceptance examples of P04 §9 that Phase 1 covers. */
class AcceptanceTest {
    private fun rowsOf(rs: org.apache.jena.sparql.exec.RowSet): List<org.apache.jena.sparql.engine.binding.Binding> {
        val out = ArrayList<org.apache.jena.sparql.engine.binding.Binding>()
        rs.forEachRemaining { out.add(it) }
        return out
    }

    private val ex = "http://example/"
    private fun iri(l: String): Node = NodeFactory.createURI(ex + l)
    private val open = ArrayList<DatasetGraphSparkles>()

    private fun memory(o: SparklesOptions = SparklesOptions.DEFAULT) = SparklesDatasets.memory(o).also { open.add(it) }

    @AfterEach
    fun close() {
        open.forEach { it.close() }
    }

    @Test
    fun a1_write_then_read() {
        val dsg = memory()
        val q = Quad.create(iri("g"), iri("s"), iri("p"), iri("o"))
        Txn.executeWrite(dsg) { dsg.add(q) }
        assertTrue(Txn.calculateRead(dsg) { dsg.contains(q) })
        assertEquals(listOf(iri("g")), Txn.calculateRead(dsg) { Iter.toList(dsg.listGraphNodes()) })
        assertTrue(Txn.calculateRead(dsg) { dsg.defaultGraph.isEmpty })
        assertEquals(1L, dsg.size())
    }

    @Test
    fun a2_writes_outside_a_transaction() {
        val q = Quad.create(Quad.defaultGraphIRI, iri("s"), iri("p"), iri("o"))
        assertThrows(JenaTransactionException::class.java) { memory().add(q) }
        val auto = memory(SparklesOptions.builder().autocommit(true).build())
        auto.add(q)
        val seq = auto.lastReceipt()!!.commit.seq
        auto.add(Quad.create(Quad.defaultGraphIRI, iri("s"), iri("p"), iri("o2")))
        assertEquals(seq + 1, auto.lastReceipt()!!.commit.seq)
        assertTrue(auto.contains(q))
    }

    @Test
    fun a3_persistent_dataset(@TempDir dir: Path) {
        val db = dir.resolve("db")
        val q = Quad.create(iri("g"), iri("s"), iri("p"), NodeFactory.createLiteralString("x"))
        SparklesDatasets.open(db).use { dsg ->
            Txn.executeWrite(dsg) { dsg.add(q) }
            // a second open of the directory shares the native dataset
            val again = SparklesDatasets.open(db)
            assertTrue(again.contains(q))
            again.close()
            assertTrue(dsg.contains(q))
        }
        SparklesDatasets.open(db).use { dsg -> assertTrue(dsg.contains(q)) }
    }

    @Test
    fun a4_select_matches_tdb2() {
        val data = """
            PREFIX : <http://example/>
            :a :p 1 . :b :p 2 . :c :q 3 .
            GRAPH :g { :a :p 9 }
        """.trimIndent()
        val dsg = memory()
        dsg.load(ByteArrayInputStream(data.toByteArray()), Lang.TRIG)
        val tdb = DatabaseMgr.createDatasetGraph()
        Txn.executeWrite(tdb) { RDFDataMgr.read(tdb, ByteArrayInputStream(data.toByteArray()), Lang.TRIG) }
        val query = "SELECT ?s ?o ?x WHERE { ?s ?p ?o OPTIONAL { ?s <http://example/none> ?x } } ORDER BY ?o"
        fun rows(d: org.apache.jena.sparql.core.DatasetGraph): List<String> = Txn.calculateRead<org.apache.jena.sparql.core.DatasetGraph, List<String>>(d) {
            rowsOf(QueryExec.dataset(d).query(query).select()).map { it.toString() }
        }
        val before = dsg.stats().nativeQueries
        assertEquals(rows(tdb), rows(dsg))
        assertEquals(before + 1, dsg.stats().nativeQueries)
    }

    @Test
    fun a5_construct_ask_describe() {
        val data = "PREFIX : <http://example/> :a :p :b . :b :q 1 . GRAPH :g { :a :p :c }"
        val dsg = memory()
        dsg.load(ByteArrayInputStream(data.toByteArray()), Lang.TRIG)
        val tdb = DatabaseMgr.createDatasetGraph()
        Txn.executeWrite(tdb) { RDFDataMgr.read(tdb, ByteArrayInputStream(data.toByteArray()), Lang.TRIG) }
        for (d in listOf(dsg, tdb)) {
            Txn.executeRead(d) {
                val c = QueryExec.dataset(d)
                    .query("CONSTRUCT { GRAPH <http://example/out> { ?s ?p ?o } } WHERE { GRAPH ?g { ?s ?p ?o } }")
                    .build().constructDataset()
                assertEquals(1, Iter.count(c.find()))
                assertTrue(QueryExec.dataset(d).query("ASK { ?s <http://example/q> 1 }").ask())
                assertFalse(QueryExec.dataset(d).query("ASK { ?s <http://example/q> 2 }").ask())
                val g = QueryExec.dataset(d).query("DESCRIBE <http://example/b>").describe()
                assertEquals(1, g.size())
            }
        }
    }

    class Twice : FunctionBase1() {
        override fun exec(v: NodeValue): NodeValue = NodeValue.makeInteger(v.integer.multiply(java.math.BigInteger.TWO))
    }

    @Test
    fun a6_java_functions_fall_back() {
        val fn = "http://example/fn#twice"
        FunctionRegistry.get().put(fn, Twice::class.java)
        val dsg = memory()
        Txn.executeWrite(dsg) { dsg.add(Quad.create(Quad.defaultGraphIRI, iri("s"), iri("p"), NodeFactory.createLiteralDT("21", XSDDatatype.XSDinteger))) }
        val q = "SELECT (<$fn>(?o) AS ?x) { ?s ?p ?o }"
        val before = dsg.stats().fallbackQueries
        val x = QueryExec.dataset(dsg).query(q).select().next().get("x")
        assertEquals("42", x.literalLexicalForm)
        assertEquals(before + 1, dsg.stats().fallbackQueries)
        val cxt = Context().set(Sparkles.FALLBACK, SparklesFallback.NEVER)
        val e = assertThrows(QueryExecException::class.java) {
            QueryExec.dataset(dsg).query(q).context(cxt).select().hasNext()
        }
        assertTrue(e.message!!.contains(fn))
        val native = dsg.stats().nativeQueries
        val l = QueryExec.dataset(dsg)
            .query("PREFIX afn: <http://jena.apache.org/ARQ/function#> SELECT (afn:localname(?s) AS ?l) { ?s ?p ?o }")
            .select().next().get("l")
        assertEquals("s", l.literalLexicalForm)
        assertEquals(native + 1, dsg.stats().nativeQueries)
    }

    @Test
    fun a7_syntax_errors_and_late_fallback() {
        val ds = DatasetFactory.wrap(memory())
        assertThrows(QueryParseException::class.java) { QueryExecution.dataset(ds).query("SELECT * WHERE {").build() }
        val dsg = ds.asDatasetGraph() as DatasetGraphSparkles
        val before = dsg.stats().fallbackQueries
        // ARQ's two-argument IRI(), which Sparkles does not parse (it does parse LET)
        val q = org.apache.jena.query.QueryFactory.create(
            "SELECT ?x { BIND(IRI(<http://example/>, \"a\") AS ?x) }",
            org.apache.jena.query.Syntax.syntaxARQ,
        )
        val r = QueryExec.dataset(dsg).query(q).select()
        assertEquals("http://example/a", r.next().get("x").uri)
        assertEquals(before + 1, dsg.stats().fallbackQueries)
        val let = org.apache.jena.query.QueryFactory.create("SELECT ?x { LET (?x := 1) }", org.apache.jena.query.Syntax.syntaxARQ)
        assertEquals("1", QueryExec.dataset(dsg).query(let).select().next().get("x").literalLexicalForm)
        assertEquals(before + 1, dsg.stats().fallbackQueries)
    }

    @Test
    fun a8_timeout_cancels() {
        val dsg = memory()
        val sb = StringBuilder()
        for (i in 0 until 5000) sb.append("<http://example/s$i> <http://example/p> $i .\n")
        dsg.load(ByteArrayInputStream(sb.toString().toByteArray()), Lang.TURTLE)
        // 25 million rows, each through a regular expression
        val q = "SELECT (COUNT(*) AS ?n) { ?a ?p ?b . ?c ?q ?d FILTER(REGEX(CONCAT(STR(?b), STR(?d)), '^(1|2)+9$')) }"
        val start = System.nanoTime()
        assertThrows(QueryCancelledException::class.java) {
            QueryExec.dataset(dsg).query(q).timeout(100, TimeUnit.MILLISECONDS).select().hasNext()
        }
        val ms = (System.nanoTime() - start) / 1_000_000
        assertTrue(ms < 2000, "cancelled after $ms ms")
        assertTrue(QueryExec.dataset(dsg).query("ASK { ?s ?p 1 }").ask())
    }

    @Test
    fun a9_promotion() {
        val dsg = memory()
        val q1 = Quad.create(Quad.defaultGraphIRI, iri("s"), iri("p"), iri("o1"))
        val q2 = Quad.create(Quad.defaultGraphIRI, iri("s"), iri("p"), iri("o2"))
        // a write promotes a READ_PROMOTE transaction
        dsg.begin(TxnType.READ_PROMOTE)
        dsg.add(q1)
        assertEquals(ReadWrite.WRITE, dsg.transactionMode())
        dsg.commit()
        // another thread's commit between begin and the write makes the write fail
        for (type in listOf(TxnType.READ_PROMOTE, TxnType.READ_COMMITTED_PROMOTE)) {
            dsg.begin(type)
            val other = Thread { Txn.executeWrite(dsg) { dsg.add(Quad.create(Quad.defaultGraphIRI, iri("t$type"), iri("p"), iri("o"))) } }
            other.start()
            other.join()
            if (type == TxnType.READ_PROMOTE) {
                assertThrows(JenaTransactionException::class.java) { dsg.add(q2) }
                dsg.end()
            } else {
                dsg.add(q2)
                // promotion continued from the other commit
                assertTrue(dsg.contains(Quad.create(Quad.defaultGraphIRI, iri("t$type"), iri("p"), iri("o"))))
                dsg.commit()
            }
        }
        assertTrue(dsg.contains(q2))
    }

    @Test
    fun a10_updates_in_and_out_of_transactions() {
        val dsg = memory()
        val seen = CountDownLatch(1)
        val done = CountDownLatch(1)
        var outside: Boolean? = null
        val reader = Thread {
            seen.await()
            outside = QueryExec.dataset(dsg).query("ASK { <a:s> <a:p> 1 }").ask()
            done.countDown()
        }
        reader.start()
        Txn.executeWrite(dsg) {
            UpdateExec.dataset(dsg).update("INSERT DATA { <a:s> <a:p> 1 }").execute()
            assertTrue(dsg.find(null, NodeFactory.createURI("a:s"), null, null).hasNext())
            seen.countDown()
            done.await()
        }
        assertEquals(false, outside)
        val before = dsg.headCommit().seq
        UpdateExec.dataset(dsg).update("INSERT DATA { <a:x> <a:p> 2 } ; INSERT { ?s <a:q> ?o } WHERE { ?s <a:p> ?o }").execute()
        assertEquals(before + 1, dsg.headCommit().seq)
        assertEquals(2L, Txn.calculateRead(dsg) { Iter.count(dsg.find(null, null, NodeFactory.createURI("a:q"), null)).toLong() })
    }

    @Test
    fun a11_update_with_a_java_function_in_the_middle() {
        val fn = "http://example/fn#twice"
        FunctionRegistry.get().put(fn, Twice::class.java)
        val dsg = memory()
        val before = dsg.headCommit().seq
        val s = dsg.stats()
        UpdateExec.dataset(dsg).update(
            """
            INSERT DATA { <a:s> <a:p> 1 } ;
            INSERT { <a:s> <a:twice> ?y } WHERE { <a:s> <a:p> ?x BIND(<$fn>(?x) AS ?y) } ;
            INSERT { <a:s> <a:after> ?y } WHERE { <a:s> <a:twice> ?y }
            """.trimIndent(),
        ).execute()
        assertEquals(before + 1, dsg.headCommit().seq)
        val after = dsg.find(null, NodeFactory.createURI("a:s"), NodeFactory.createURI("a:after"), null).next()
        assertEquals("2", after.`object`.literalLexicalForm)
        assertEquals(s.fallbackUpdates + 1, dsg.stats().fallbackUpdates)
        assertEquals(s.nativeUpdates + 2, dsg.stats().nativeUpdates)
    }

    @Test
    fun a12_blank_node_labels() {
        for (mode in BlankNodeLabels.entries) {
            val dsg = memory(SparklesOptions.builder().blankNodeLabels(mode).build())
            val b = NodeFactory.createBlankNode()
            Txn.executeWrite(dsg) { dsg.add(Quad.create(Quad.defaultGraphIRI, b, iri("p"), iri("o"))) }
            val found = Txn.calculateRead(dsg) { Iter.toList(dsg.find(null, b, null, null)) }
            if (mode == BlankNodeLabels.DATASET) {
                assertEquals(1, found.size)
                assertEquals(b, found[0].subject)
            } else {
                assertTrue(found.isEmpty())
            }
        }
    }

    @Test
    fun a13_find_in_batches_and_iterators_end_with_their_transaction() {
        val dsg = memory()
        Txn.executeWrite(dsg) {
            for (i in 0 until 10_000) dsg.add(Quad.create(Quad.defaultGraphIRI, iri("s${i % 100}"), iri("p"), NodeFactory.createLiteralDT("$i", XSDDatatype.XSDinteger)))
        }
        assertEquals(10_000, Txn.calculateRead(dsg) { Iter.count(dsg.find()) })
        dsg.begin(TxnType.READ)
        val it = dsg.find()
        it.next()
        dsg.end()
        assertThrows(JenaTransactionException::class.java) { it.hasNext() }
    }

    @Test
    fun a14_terms_round_trip() {
        val dsg = memory()
        val nodes = ArrayList<Node>()
        for (dt in listOf(
            XSDDatatype.XSDinteger, XSDDatatype.XSDdecimal, XSDDatatype.XSDdouble, XSDDatatype.XSDfloat,
            XSDDatatype.XSDboolean, XSDDatatype.XSDdateTime, XSDDatatype.XSDdate, XSDDatatype.XSDtime,
            XSDDatatype.XSDduration, XSDDatatype.XSDdayTimeDuration, XSDDatatype.XSDyearMonthDuration,
            XSDDatatype.XSDlong, XSDDatatype.XSDint, XSDDatatype.XSDshort, XSDDatatype.XSDbyte,
            XSDDatatype.XSDnonNegativeInteger, XSDDatatype.XSDpositiveInteger, XSDDatatype.XSDnonPositiveInteger,
            XSDDatatype.XSDnegativeInteger, XSDDatatype.XSDunsignedLong, XSDDatatype.XSDunsignedInt,
            XSDDatatype.XSDgYear, XSDDatatype.XSDanyURI,
        )) {
            val lex = when (dt) {
                XSDDatatype.XSDboolean -> "true"
                XSDDatatype.XSDdateTime -> "2024-01-02T03:04:05Z"
                XSDDatatype.XSDdate -> "2024-01-02"
                XSDDatatype.XSDtime -> "03:04:05"
                XSDDatatype.XSDduration -> "P1D"
                XSDDatatype.XSDdayTimeDuration -> "PT1H"
                XSDDatatype.XSDyearMonthDuration -> "P1Y"
                XSDDatatype.XSDnonPositiveInteger, XSDDatatype.XSDnegativeInteger -> "-5"
                XSDDatatype.XSDgYear -> "2024"
                XSDDatatype.XSDanyURI -> "http://example/x"
                XSDDatatype.XSDdouble, XSDDatatype.XSDfloat -> "1.5E0"
                XSDDatatype.XSDdecimal -> "1.5"
                else -> "5"
            }
            nodes.add(NodeFactory.createLiteralDT(lex, dt))
        }
        nodes.add(NodeFactory.createLiteralString("plain"))
        nodes.add(NodeFactory.createLiteralLang("chat", "fr"))
        nodes.add(NodeFactory.createLiteralDirLang("salaam", "ar", TextDirection.RTL))
        nodes.add(NodeFactory.createLiteralDT("{}", org.apache.jena.datatypes.TypeMapper.getInstance().getSafeTypeByName("http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON")))
        nodes.add(NodeFactory.createLiteralDT("x", org.apache.jena.datatypes.TypeMapper.getInstance().getSafeTypeByName("http://example/dt")))
        nodes.add(NodeFactory.createTripleTerm(Triple.create(iri("a"), iri("b"), NodeFactory.createLiteralString("c"))))
        Txn.executeWrite(dsg) {
            nodes.forEachIndexed { i, n -> dsg.add(Quad.create(Quad.defaultGraphIRI, iri("s"), iri("p$i"), n)) }
        }
        Txn.executeRead(dsg) {
            nodes.forEachIndexed { i, n ->
                val got = Iter.toList(dsg.find(null, iri("s"), iri("p$i"), null))
                assertEquals(1, got.size, "$n")
                assertEquals(n, got[0].`object`, "$n")
                assertTrue(dsg.contains(Quad.defaultGraphIRI, iri("s"), iri("p$i"), n))
            }
        }
    }

    @Test
    fun a18_tdb2_import(@TempDir dir: Path) {
        val tdbDir = dir.resolve("tdb")
        val tdb = DatabaseMgr.connectDatasetGraph(tdbDir.toString())
        val data = """
            PREFIX : <http://example/>
            PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
            :a :p "01"^^xsd:integer . _:b :p :a .
            GRAPH :g1 { :a :p _:c . _:c :q "x"@en }
            GRAPH :g2 { :a :p :b }
        """.trimIndent()
        Txn.executeWrite(tdb) { RDFDataMgr.read(tdb, ByteArrayInputStream(data.toByteArray()), Lang.TRIG) }
        org.apache.jena.tdb2.sys.TDBInternal.expel(tdb)
        val report = SparklesDatasets.importTdb2(tdbDir, dir.resolve("sparkles"))
        assertEquals(5, report.quads)
        assertEquals(2, report.namedGraphs)
        assertTrue(report.receipt.isCommitted())
        SparklesDatasets.open(dir.resolve("sparkles")).use { dsg ->
            assertEquals(5, Iter.count(dsg.find()))
            // A19: TDB2 stored the integer by value, and the import reads it back as TDB2 does
            val one = Iter.toList(dsg.find(null, iri("a"), iri("p"), null)).map { it.`object` }
            assertTrue(one.any { it.isLiteral && it.literalLexicalForm == "1" }, "$one")
            assertEquals("http://example/", dsg.prefixes().get(""))
            // a literal written through the binding keeps its lexical form
            Txn.executeWrite(dsg) {
                dsg.add(Quad.create(Quad.defaultGraphIRI, iri("n"), iri("v"), NodeFactory.createLiteralDT("01", XSDDatatype.XSDinteger)))
            }
            val v = Iter.toList(dsg.find(null, iri("n"), iri("v"), null))[0].`object`
            assertEquals("01", v.literalLexicalForm)
            assertTrue(QueryExec.dataset(dsg).query("ASK { <http://example/n> <http://example/v> ?x FILTER(?x = 1) }").ask())
        }
    }

    @Test
    fun a20_union_default_graph() {
        val dsg = memory()
        dsg.load(ByteArrayInputStream("PREFIX : <http://example/> :d :p 0 . GRAPH :g1 { :a :p 1 } GRAPH :g2 { :b :p 2 }".toByteArray()), Lang.TRIG)
        val cxt = Context().set(org.apache.jena.sparql.util.Symbol.create("http://jena.apache.org/TDB#unionDefaultGraph"), true)
        val n = QueryExec.dataset(dsg).query("SELECT * { ?s ?p ?o }").context(cxt).select().let { rowsOf(it).size }
        assertEquals(2, n)
        val g = QueryExec.dataset(dsg).query("SELECT * { GRAPH ?g { ?s ?p ?o } }").context(cxt).select().let { rowsOf(it).size }
        assertEquals(2, g)
        assertEquals(1, Iter.count(dsg.defaultGraph.find()))
        // the fallback sees the same default graph
        val all = QueryExec.dataset(dsg).query("SELECT * { ?s ?p ?o }")
            .context(Context().set(Sparkles.UNION_DEFAULT_GRAPH, true).set(Sparkles.FALLBACK, SparklesFallback.ALWAYS))
            .select().let { rowsOf(it).size }
        assertEquals(2, all)
    }

    @Test
    fun loads_receipts_and_the_bulk_sink(@TempDir dir: Path) {
        val dsg = memory()
        val file = dir.resolve("d.ttl")
        Files.writeString(file, "<http://example/a> <http://example/p> 1 .")
        val r = dsg.loadFiles(listOf(file), iri("g"))
        assertTrue(r.isCommitted())
        assertEquals(r, dsg.lastReceipt())
        assertTrue(dsg.contains(iri("g"), iri("a"), iri("p"), null))
        val sink = dsg.bulkSink()
        sink.start()
        val b = NodeFactory.createBlankNode()
        sink.triple(Triple.create(b, iri("p"), iri("o")))
        sink.quad(Quad.create(iri("h"), b, iri("q"), iri("o")))
        sink.prefix("ex", ex)
        sink.finish()
        assertEquals(2L, sink.receipt()!!.commit.inserted)
        assertEquals(ex, dsg.prefixes().get("ex"))
        // one blank node in both quads
        val s1 = dsg.find(null, null, iri("p"), iri("o")).next().subject
        val s2 = dsg.find(iri("h"), null, null, null).next().subject
        assertEquals(s1, s2)
        Txn.executeWrite(dsg) {
            assertThrows(JenaTransactionException::class.java) { dsg.loadFiles(listOf(file)) }
        }
        assertNotNull(dsg.headCommit())
        val model = ModelFactory.createModelForGraph(dsg.defaultGraph)
        assertEquals(1, model.size())
        assertNull(SparklesDatasets.memory().use { it.lastReceipt() })
    }
}
