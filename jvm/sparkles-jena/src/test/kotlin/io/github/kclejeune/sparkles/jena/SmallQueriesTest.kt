package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.engine.SmallQueries
import io.github.kclejeune.sparkles.jena.internal.ffi.SparklesJni
import org.apache.jena.query.QueryFactory
import org.apache.jena.riot.Lang
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.engine.binding.BindingFactory
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.exec.QueryExecDatasetBuilder
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.apache.jena.graph.NodeFactory
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream

/**
 * Small queries that run in ARQ over `find()` (P04 §5.4): which qualify, and that they
 * give the answers Sparkles' engine gives.
 */
class SmallQueriesTest {
    private val prefixes = "PREFIX : <http://example/> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> "
    private val open = ArrayList<DatasetGraphSparkles>()

    private fun estimate(q: String, vararg bound: String): Double? {
        val input = if (bound.isEmpty()) BindingFactory.empty() else {
            val b = BindingFactory.builder()
            for (v in bound) b.add(Var.alloc(v), NodeFactory.createURI("http://example/x"))
            b.build()
        }
        return SmallQueries.estimate(QueryFactory.create(prefixes + q), input, setOf("http://jena.apache.org/text#query"), Context())
    }

    private fun data(): DatasetGraphSparkles {
        val ttl = StringBuilder("@prefix : <http://example/> .\n")
        for (i in 0 until 50) {
            ttl.append(":p$i a :Person ; :name \"Person $i\" ; :age ${20 + i % 40} ; :label \"p$i\"@en ; :worksFor :org${i % 5} ")
            for (k in 1..(i % 4)) ttl.append("; :knows :p${(i * 7 + k) % 50} ")
            ttl.append(".\n")
        }
        ttl.append(":org0 :name \"Org 0\" .\n")
        val dsg = SparklesDatasets.memory().also { open.add(it) }
        dsg.load(ByteArrayInputStream(ttl.toString().toByteArray()), Lang.TURTLE)
        return dsg
    }

    @AfterEach
    fun close() = open.forEach { it.close() }

    @Test
    fun lookups_on_bound_subjects_qualify() {
        assertEquals(1.0, estimate("ASK { :p1 a :Person }"))
        assertEquals(1.0, estimate("SELECT ?o { :p1 :name ?o }"))
        assertEquals(1.0, estimate("SELECT * { :p1 ?p ?o }"))
        assertEquals(1.0, estimate("CONSTRUCT WHERE { :p1 ?p ?o }"))
        // the second pattern runs once for each of the first's assumed two matches
        assertEquals(3.0, estimate("SELECT ?f ?n { :p1 :knows ?f . ?f :name ?n }"))
        // five VALUES rows, each with three patterns: 1 + 2 + 4 finds
        assertEquals(35.0, estimate("SELECT * { VALUES ?p { :p1 :p2 :p3 :p4 :p5 } ?p :name ?n ; :age ?a ; :knows ?k }"))
        assertEquals(1.0, estimate("SELECT ?n { ?s :name ?n }", "s"))
        assertEquals(1.0, estimate("SELECT DISTINCT ?o { :p1 :knows ?o } LIMIT 3"))
    }

    @Test
    fun anything_else_runs_in_sparkles() {
        // an unbound subject can match any number of triples
        assertNull(estimate("SELECT ?p { ?p :worksFor :org1 ; :name ?n }"))
        assertNull(estimate("SELECT ?n { ?s :name ?n }"))
        // VALUES after the patterns, or for the whole query, joins with unbound matches
        assertNull(estimate("SELECT * { ?p :name ?n VALUES ?p { :p1 } }"))
        assertNull(estimate("SELECT * { ?p :name ?n } VALUES ?p { :p1 }"))
        assertNull(estimate("SELECT * { VALUES ?p { :p1 UNDEF } ?p :name ?n }"))
        for (q in listOf(
            "SELECT ?o { :p1 :name ?o FILTER(?o != \"x\") }",
            "SELECT ?o { :p1 :name ?o OPTIONAL { :p1 :age ?a } }",
            "SELECT ?o { { :p1 :name ?o } UNION { :p2 :name ?o } }",
            "SELECT ?o { GRAPH :g { :p1 :name ?o } }",
            "SELECT ?o { :p1 :knows/:name ?o }",
            "SELECT ?o { :p1 :name ?o } ORDER BY ?o",
            "SELECT (COUNT(*) AS ?c) { :p1 :knows ?o }",
            "SELECT (STR(?o) AS ?s) { :p1 :name ?o }",
            "SELECT ?o FROM :g { :p1 :name ?o }",
            "ASK { :p1 :label \"p1\"@en }",
            "SELECT ?o { :p1 <http://jena.apache.org/text#query> ?o }",
            "SELECT ?o { :p1 <http://www.opengis.net/ont/geosparql#sfWithin> ?o }",
            "DESCRIBE :p1",
        )) {
            assertNull(estimate(q), q)
        }
    }

    private fun rows(dsg: DatasetGraphSparkles, q: String, smallQueries: Boolean): List<String> =
        Txn.calculateRead(dsg) {
            val b = QueryExec.dataset(dsg).query(prefixes + q)
            if (!smallQueries) b.set(Sparkles.SMALL_QUERY_FINDS, 0)
            b.build().use { qe ->
                when {
                    qe.query.isAskType -> listOf(qe.ask().toString())
                    qe.query.isConstructType -> qe.construct().find().toList().map { it.toString() }.sorted()
                    else -> {
                        val rs = qe.select()
                        val out = ArrayList<String>()
                        while (rs.hasNext()) {
                            val b = rs.next()
                            out.add(rs.resultVars.joinToString(" ") { v -> "$v=${b.get(v)}" })
                        }
                        out.sorted()
                    }
                }
            }
        }

    @Test
    fun routed_queries_answer_as_sparkles_does() {
        val dsg = data()
        val queries = listOf(
            "ASK { :p1 a :Person }",
            "ASK { :p1 a :Robot }",
            "SELECT ?o { :p3 :name ?o }",
            "SELECT ?o { :nobody :name ?o }",
            "SELECT ?p ?o { :p3 ?p ?o }",
            "SELECT * { :p3 ?p ?o }",
            "SELECT ?f ?n { :p3 :knows ?f . ?f :name ?n }",
            "SELECT DISTINCT ?w { :p7 :knows ?f . ?f :worksFor ?w }",
            "SELECT * { VALUES ?p { :p1 :p2 :p3 :nobody } ?p :name ?n ; :age ?a ; :knows ?k }",
            "SELECT ?n { VALUES (?p ?q) { (:p1 :p2) (:p3 :p4) } ?p :name ?n . ?q :age ?a }",
            "CONSTRUCT WHERE { :p5 ?p ?o }",
            "CONSTRUCT { ?f :friendOf :p5 } WHERE { :p5 :knows ?f }",
            "SELECT ?o { :p1 :age ?o } LIMIT 1",
            "ASK { :p2 :age 22 }",
            "ASK { :p2 :age \"22\"^^<http://www.w3.org/2001/XMLSchema#integer> }",
            "SELECT ?l { :p2 :label ?l }",
        )
        val before = dsg.stats()
        for (q in queries) assertEquals(rows(dsg, q, smallQueries = false), rows(dsg, q, smallQueries = true), q)
        val after = dsg.stats()
        val expected = if (SparklesJni.FIND) queries.size.toLong() else 0L
        assertEquals(expected, after.smallQueries - before.smallQueries)
        assertEquals(queries.size * 2 - expected, after.nativeQueries - before.nativeQueries)
    }

    @Test
    fun an_initial_binding_bounds_the_subject() {
        val dsg = data()
        val q = QueryFactory.create(prefixes + "SELECT ?n ?a { ?s :name ?n ; :age ?a }")
        val before = dsg.stats().smallQueries
        val rows = Txn.calculateRead(dsg) {
            @Suppress("DEPRECATION")
            QueryExecDatasetBuilder.create().dataset(dsg).query(q)
                .initialBinding(BindingFactory.binding(Var.alloc("s"), NodeFactory.createURI("http://example/p4")))
                .select().materialize().let { rs -> buildList { while (rs.hasNext()) add(rs.next().get("n").literalLexicalForm) } }
        }
        assertEquals(listOf("Person 4"), rows)
        assertEquals(if (SparklesJni.FIND) 1L else 0L, dsg.stats().smallQueries - before)
    }

    @Test
    fun write_transactions_and_engine_settings_keep_sparkles() {
        val dsg = data()
        val before = dsg.stats().smallQueries
        Txn.executeWrite(dsg) {
            assertTrue(QueryExec.dataset(dsg).query(prefixes + "ASK { :p1 a :Person }").ask())
        }
        Txn.executeRead(dsg) {
            assertTrue(QueryExec.dataset(dsg).query(prefixes + "ASK { :p1 a :Person }").set(Sparkles.MAX_ROWS, 1000).ask())
            assertTrue(QueryExec.dataset(dsg).query(prefixes + "ASK { :p1 a :Person }").set(Sparkles.INCLUDE_INFERRED, true).ask())
        }
        assertEquals(before, dsg.stats().smallQueries)
    }

    @Test
    fun rdfs_on_read_keeps_sparkles() {
        val dsg = data()
        Txn.executeWrite(dsg) {
            dsg.add(
                NodeFactory.createURI("http://example/schema"), NodeFactory.createURI("http://example/Person"),
                NodeFactory.createURI("http://www.w3.org/2000/01/rdf-schema#subClassOf"), NodeFactory.createURI("http://example/Agent"),
            )
        }
        dsg.reasoning().rdfs().set("http://example/schema")
        val before = dsg.stats().smallQueries
        // the type comes from the schema, which only Sparkles' engine applies
        val inferred = Txn.calculateRead(dsg) { QueryExec.dataset(dsg).query(prefixes + "ASK { :p1 a :Agent }").ask() }
        assertTrue(inferred)
        assertEquals(before, dsg.stats().smallQueries)
    }
}
