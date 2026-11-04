package io.github.kclejeune.sparkles.jena

import org.apache.jena.ontapi.OntSpecification
import org.apache.jena.ontapi.model.OntClass
import org.apache.jena.query.ReadWrite
import org.apache.jena.update.UpdateAction
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

/** Spec F21 Phase 2 on the JVM: jena-ontapi over a Sparkles graph, and the native handle. */
class OntologyTest {
    private val prefixes = """
        PREFIX : <http://ex.org/>
        PREFIX owl: <http://www.w3.org/2002/07/owl#>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    """.trimIndent()

    private fun a1(ds: DatasetGraphSparkles, graph: String = "http://ex.org/onto") {
        UpdateAction.parseExecute(
            """$prefixes
            INSERT DATA { GRAPH <$graph> {
              :onto a owl:Ontology .
              :Professor a owl:Class . :Course a owl:Class . :teaches a owl:ObjectProperty .
              :Professor rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :teaches ; owl:someValuesFrom :Course ] .
            } }""",
            ds,
        )
    }

    /** A9: OntModel over a Sparkles graph lists the classes of A1, and a transaction's edits make one commit. */
    @Test fun ontapi_reads_the_graph_and_writes_in_the_callers_transaction() {
        SparklesDatasets.memory().use { ds ->
            a1(ds)
            val m = SparklesOnt.model(ds, "http://ex.org/onto", OntSpecification.OWL2_DL_MEM)
            val classes = m.classes().map { it.uri }.toList().filterNotNull().sorted()
            assertEquals(listOf("http://ex.org/Course", "http://ex.org/Professor"), classes)
            val professor = m.getOntClass("http://ex.org/Professor")!!
            assertEquals(1L, professor.superClasses(true).filter { !it.isURIResource }.count())
            val before = ds.headCommit().seq
            ds.begin(ReadWrite.WRITE)
            try {
                val person: OntClass = m.createOntClass("http://ex.org/Person")
                professor.addSuperClass(person)
                ds.commit()
            } finally {
                ds.end()
            }
            assertEquals(before + 1, ds.headCommit().seq, "one Sparkles commit")
            val g = ds.getGraph(org.apache.jena.graph.NodeFactory.createURI("http://ex.org/onto"))
            assertTrue(g.contains(
                org.apache.jena.graph.NodeFactory.createURI("http://ex.org/Professor"),
                org.apache.jena.vocabulary.RDFS.subClassOf.asNode(),
                org.apache.jena.graph.NodeFactory.createURI("http://ex.org/Person"),
            ))
            // a new model reads what the edit wrote
            val again = SparklesOnt.model(ds, "http://ex.org/onto")
            assertNotNull(again.getOntClass("http://ex.org/Person"))
        }
    }

    /** An aborted transaction undoes the model's additions and deletions in its copy too. */
    @Test fun an_aborted_transaction_leaves_the_model_as_the_dataset_is() {
        SparklesDatasets.memory().use { ds ->
            a1(ds)
            val m = SparklesOnt.model(ds, "http://ex.org/onto")
            val before = ds.headCommit().seq
            ds.begin(ReadWrite.WRITE)
            try {
                m.createOntClass("urn:X")
                m.getOntClass("http://ex.org/Course")!!.removeProperties()
                assertNotNull(m.getOntClass("urn:X"), "the transaction sees its own writes")
                ds.abort()
            } finally {
                ds.end()
            }
            assertEquals(before, ds.headCommit().seq)
            assertNull(m.getOntClass("urn:X"))
            assertNotNull(m.getOntClass("http://ex.org/Course"))
            assertEquals(SparklesOnt.graph(ds, "http://ex.org/onto").size(), m.graph.size())
            // a committed transaction keeps its writes
            ds.begin(ReadWrite.WRITE)
            try {
                m.createOntClass("urn:Y")
                ds.commit()
            } finally {
                ds.end()
            }
            assertNotNull(m.getOntClass("urn:Y"))
            assertNotNull(SparklesOnt.model(ds, "http://ex.org/onto").getOntClass("urn:Y"))
        }
    }

    /** A read on another thread during a transaction keeps the transaction's writes, which then commit. */
    @Test fun a_read_on_another_thread_keeps_an_open_transactions_writes() {
        SparklesDatasets.memory().use { ds ->
            a1(ds)
            val m = SparklesOnt.model(ds, "http://ex.org/onto")
            ds.begin(ReadWrite.WRITE)
            try {
                m.createOntClass("urn:X")
                val other = Thread { m.classes().count() }
                other.start()
                other.join()
                ds.commit()
            } finally {
                ds.end()
            }
            assertNotNull(m.getOntClass("urn:X"))
            assertNotNull(SparklesOnt.model(ds, "http://ex.org/onto").getOntClass("urn:X"))
        }
    }

    /**
     * A write on another thread that waits for a transaction which then aborts leaves the
     * model with that write: an addition stays, and a removal stays removed.
     */
    @Test fun a_write_that_waits_for_an_aborted_transaction_stays() {
        val t = org.apache.jena.graph.Triple.create(
            org.apache.jena.graph.NodeFactory.createURI("urn:s"),
            org.apache.jena.vocabulary.RDFS.label.asNode(),
            org.apache.jena.graph.NodeFactory.createLiteralString("x"),
        )
        for (adding in listOf(true, false)) {
            SparklesDatasets.memory(SparklesOptions.builder().autocommit(true).build()).use { ds ->
                a1(ds)
                val m = SparklesOnt.model(ds, "http://ex.org/onto")
                val g = m.graph
                if (!adding) g.add(t)
                ds.begin(ReadWrite.WRITE)
                val other = Thread { if (adding) g.add(t) else g.delete(t) }
                try {
                    if (adding) g.add(t) else g.delete(t)
                    other.start()
                    awaitWriterWait(other)
                } finally {
                    ds.abort()
                    ds.end()
                }
                other.join()
                val stored = SparklesOnt.graph(ds, "http://ex.org/onto").contains(t)
                assertEquals(adding, stored, "the dataset after the other thread's write")
                assertEquals(stored, g.contains(t), "the model keeps the committed write")
            }
        }
    }

    /**
     * Wait until `thread` is in the native start of its own write transaction, which waits
     * for the writer lock that this thread's transaction holds.
     */
    private fun awaitWriterWait(thread: Thread) {
        val until = System.nanoTime() + 10_000_000_000L
        while (thread.stackTrace.none { it.methodName.startsWith("uniffi_sparkles_ffi_fn_method_ffidataset_begin_write") }) {
            assertTrue(System.nanoTime() < until) {
                "the other thread never waited for the writer lock: " +
                    thread.stackTrace.joinToString { "${it.className}.${it.methodName}" }
            }
            Thread.sleep(5)
        }
    }

    /**
     * A transaction that follows a committed one without a read in between, and aborts,
     * leaves the model with the committed state, for an addition, a deletion and a
     * removal by pattern.
     */
    @Test fun successive_transactions_keep_the_committed_state() {
        val s = org.apache.jena.graph.NodeFactory.createURI("urn:s")
        val label = org.apache.jena.vocabulary.RDFS.label.asNode()
        val t = org.apache.jena.graph.Triple.create(s, label, org.apache.jena.graph.NodeFactory.createLiteralString("x"))
        for (op in listOf("add", "delete", "remove")) {
            SparklesDatasets.memory().use { ds ->
                a1(ds)
                val m = SparklesOnt.model(ds, "http://ex.org/onto")
                val g = m.graph
                fun txn(commit: Boolean, write: () -> Unit) {
                    ds.begin(ReadWrite.WRITE)
                    try {
                        write()
                        if (commit) ds.commit() else ds.abort()
                    } finally {
                        ds.end()
                    }
                }
                // the committed transactions set the state the last one changes
                txn(true) { g.add(t) }
                if (op == "add") txn(true) { g.delete(t) }
                txn(false) {
                    when (op) {
                        "add" -> g.add(t)
                        "delete" -> g.delete(t)
                        else -> g.remove(s, null, null)
                    }
                }
                val stored = SparklesOnt.graph(ds, "http://ex.org/onto").contains(t)
                assertEquals(op != "add", stored, op)
                assertEquals(stored, g.contains(t), op)
            }
        }
    }

    /** The copy holds a larger ontology in full: it is filled by one native read, not by batches. */
    @Test fun the_model_copies_a_large_graph_in_one_read() {
        SparklesDatasets.memory().use { ds ->
            val body = StringBuilder()
            for (n in 0 until 10_000) body.append(":C$n a owl:Class . :C$n rdfs:subClassOf :C${n + 1} .\n")
            UpdateAction.parseExecute("$prefixes INSERT DATA { GRAPH <http://ex.org/big> { $body } }", ds)
            val g = SparklesOnt.graph(ds, "http://ex.org/big")
            assertEquals(20_000, g.size())
            val m = SparklesOnt.model(ds, "http://ex.org/big")
            assertEquals(10_000L, m.classes().count())
        }
    }

    @Test fun the_native_handle_reads_checks_edits_and_diffs(@TempDir dir: java.nio.file.Path) {
        // on disk, so that the diff can read the earlier commit
        SparklesDatasets.open(dir.resolve("db")).use { ds ->
            a1(ds)
            val read = OntologyRead(graph = "http://ex.org/onto")
            val onto = ds.ontology()
            assertEquals("http://ex.org/onto", onto.summary(read).get("header").asObject.getString("iri"))
            val page = SparklesOnt.axioms(ds, "http://ex.org/onto", AxiomQuery(types = listOf("SubClassOf")))
            assertEquals(1, page.getNumber("total").toInt())
            val axiom = page.get("axioms").asArray[0].asObject.get("axiom")
            val check = SparklesOnt.check(ds, "http://ex.org/onto", listOf("el", "ql"))
            assertTrue(check.getBoolean("conforms"))
            assertEquals(2, check.get("reports").asArray.size)
            // roadmap item 540: the entity accessors
            val classes = SparklesOnt.entities(ds, "http://ex.org/onto", listOf("Class"))
            assertEquals(2, classes.getNumber("total").toInt())
            val prof = SparklesOnt.entity(ds, "http://ex.org/Professor", "http://ex.org/onto")
            val sup = prof.get("class").asObject.get("superClasses").asArray[0].asObject
            assertEquals("ObjectSomeValuesFrom", sup.get("expression").asObject.getString("type"))
            assertThrows(SparklesNotFoundException::class.java) { onto.entity("http://ex.org/nothing", read) }
            // roadmap item 616: the entity search, by the local name here
            val found = SparklesOnt.search(ds, "prof", "http://ex.org/onto")
            assertEquals("http://ex.org/Professor", found.get("results").asArray[0].asObject.getString("iri"))
            val typo = onto.search("profesor", read, kinds = listOf("class")).get("results").asArray[0].asObject
            assertEquals("http://ex.org/Professor", typo.getString("iri"))
            assertEquals("fuzzy", typo.get("match").asObject.getString("kind"))
            // the browser's reads: one hierarchy level and one entity
            val roots = SparklesOnt.hierarchy(ds, null, "http://ex.org/onto")
            assertTrue(roots.get("children").asArray.size > 0)
            val tree = onto.hierarchy("http://ex.org/Professor", read, ancestors = true)
            assertEquals("http://ex.org/Professor", tree.get("node").asObject.getString("iri"))
            val term = SparklesOnt.term(ds, "http://ex.org/Professor", "http://ex.org/onto")
            assertEquals(listOf("class"), term.get("kinds").asArray.map { it.asString.value() })
            assertTrue(term.get("axioms").asArray.any { it.asObject.getString("type") == "SubClassOf" })
            val before = ds.headCommit().seq
            val done = SparklesOnt.edit(
                ds,
                AxiomChange(add = listOf("SubClassOf(<http://ex.org/Course> owl:Thing)"), remove = listOf(axiom)),
                "http://ex.org/onto",
            )
            assertEquals(1, done.getNumber("removed").toInt())
            assertEquals(1, done.getNumber("added").toInt())
            val diff = SparklesOnt.diff(ds, "commit:$before", null, "http://ex.org/onto")
            assertEquals(1, diff.getNumber("addedCount").toInt())
            assertEquals(1, diff.getNumber("removedCount").toInt())
            assertTrue(onto.removeEntity("http://ex.org/Course", read).getNumber("removed").toInt() >= 1)
            assertThrows(SparklesInvalidException::class.java) { onto.edit(AxiomChange(), read) }
            ds.begin(ReadWrite.WRITE)
            try {
                assertThrows(RuntimeException::class.java) { onto.edit(AxiomChange(add = listOf("Declaration(Class(<http://ex.org/X>))")), read) }
            } finally {
                ds.abort()
                ds.end()
            }
        }
    }

    /** Roadmap item 580: the OWL 2 EL classification of a graph, and its taxonomy written to another. */
    @Test fun the_native_handle_classifies_and_writes_the_taxonomy() {
        SparklesDatasets.memory().use { ds ->
            a1(ds)
            UpdateAction.parseExecute(
                """$prefixes
                INSERT DATA { GRAPH <http://ex.org/onto> {
                  :Teacher a owl:Class ;
                    owl:equivalentClass [ a owl:Restriction ; owl:onProperty :teaches ; owl:someValuesFrom :Course ] .
                } }""",
                ds,
            )
            val r = SparklesOnt.classify(ds, "http://ex.org/onto", ClassifyQuery(about = "http://ex.org/Professor"))
            assertTrue(r.getBoolean("consistent"))
            assertTrue(r.getBoolean("complete"))
            val sups = r.get("class").asObject.get("superClasses").asArray.map { it.asString.value() }
            assertTrue("http://ex.org/Teacher" in sups, sups.toString())
            assertTrue(SparklesOnt.classify(ds, "http://ex.org/onto").getBoolean("reused"))
            assertThrows(SparklesNotFoundException::class.java) {
                SparklesOnt.classify(ds, "http://ex.org/onto", ClassifyQuery(about = "http://ex.org/Nobody"))
            }
            val before = ds.headCommit().seq
            val w = SparklesOnt.classify(ds, "http://ex.org/onto", ClassifyQuery(target = "http://ex.org/taxonomy"))
            assertTrue(w.get("written").asObject.getBoolean("committed"))
            assertEquals(before + 1, ds.headCommit().seq)
            val tax = SparklesOnt.axioms(ds, "http://ex.org/taxonomy", AxiomQuery(types = listOf("SubClassOf")))
            assertTrue(tax.getNumber("total").toInt() >= 1)
        }
    }

    /** Item 603's cases, as the Rust tests write them: a classification the disjunctive rules take. */
    private fun cases(n: Int, inconsistent: Boolean): String {
        fun some(p: String, c: String) = "[ a owl:Restriction ; owl:onProperty $p ; owl:someValuesFrom $c ]"
        val out = StringBuilder(":r a owl:ObjectProperty . :s a owl:ObjectProperty .\n")
        for (i in 0 until n) {
            val b = (7 * i + 3) % n
            val d = (13 * i + 5) % n
            out.append(
                """:C$i a owl:Class ; rdfs:subClassOf [ owl:unionOf ( :B$i :E$i ) ] .
                :B$i rdfs:subClassOf :K$i , :C$b . :E$i rdfs:subClassOf :K$i .
                :K$i rdfs:subClassOf ${some(":r", ":C$d")} .
                :E$i rdfs:subClassOf [ a owl:Restriction ;
                  owl:onProperty [ owl:inverseOf :r ] ; owl:allValuesFrom :F$d ] .
                ${some(":r", ":F$i")} rdfs:subClassOf :G$b .
                """,
            )
        }
        if (inconsistent) {
            out.append(
                """owl:Thing rdfs:subClassOf ${some(":s", "[ owl:unionOf ( :N1 :N2 ) ]")} .
                :N1 rdfs:subClassOf owl:Nothing . :N2 rdfs:subClassOf :N3 . :N3 owl:disjointWith :N2 .""",
            )
        }
        return "$prefixes\nINSERT DATA { $out }"
    }

    /** Classifies the cases under deadlines from a fraction of a full run's time upwards, and returns the partial answers' consistencies. */
    private fun partialAnswers(inconsistent: Boolean): List<String> {
        val body = cases(300, inconsistent)
        val want = if (inconsistent) "inconsistent" else "consistent"
        val query = ClassifyQuery(partial = true, threads = 1, limit = 10000)
        val (full, took) = SparklesDatasets.memory().use { ds ->
            UpdateAction.parseExecute(body, ds)
            val start = System.nanoTime()
            val r = ds.ontology().classify(OntologyRead(), query)
            r to (System.nanoTime() - start) / 1_000_000.0
        }
        assertTrue(full.getBoolean("complete"))
        assertFalse(full.getBoolean("partial"))
        assertEquals(want, full.getString("consistency"))
        assertEquals("disjunctive", full.getString("tier"))
        val seen = mutableListOf<String>()
        SparklesDatasets.memory().use { ds ->
            UpdateAction.parseExecute(body, ds)
            var f = 0.02
            while (f < 1.5) {
                val ms = maxOf(1L, (took * f).toLong())
                f *= 1.25
                val r = try {
                    SparklesOperation(ms).use { ds.ontology().classify(OntologyRead(), query, it) }
                } catch (e: org.apache.jena.query.QueryCancelledException) {
                    continue
                }
                assertEquals(full.getNumber("classes"), r.getNumber("classes"))
                if (!r.getBoolean("partial")) {
                    assertTrue(r.getBoolean("complete"))
                    assertEquals(want, r.getString("consistency"))
                    continue
                }
                assertFalse(r.getBoolean("complete"), "a partial answer is presented as complete")
                val open = r.get("incomplete").asArray
                assertEquals(open.size, r.getNumber("incompleteCount").toInt())
                val c = r.getString("consistency")
                assertTrue(c == want || c == "unknown", c)
                if (c == "unknown") assertTrue(open.size + 2 >= r.getNumber("classes").toInt())
                seen.add(c)
            }
        }
        return seen
    }

    /** Item 603 phase 7: a classification answers in part under a deadline, with its open classes and consistency. */
    @Test fun the_native_handle_answers_a_classification_in_part() {
        assertTrue(partialAnswers(false).isNotEmpty(), "no deadline gave a partial answer")
        assertTrue("unknown" in partialAnswers(true))
        SparklesDatasets.memory().use { ds ->
            a1(ds)
            val r = SparklesOnt.classify(ds, "http://ex.org/onto", ClassifyQuery(partial = true, expansion = "eager"))
            assertTrue(r.getBoolean("complete"))
            assertFalse(r.getBoolean("partial"))
            assertEquals("consistent", r.getString("consistency"))
            assertEquals("el", r.getString("tier"))
            assertEquals(0, r.get("incomplete").asArray.size)
            assertThrows(IllegalArgumentException::class.java) { ClassifyQuery(expansion = "greedy") }
            assertThrows(SparklesInvalidException::class.java) {
                SparklesOnt.classify(ds, "http://ex.org/onto", ClassifyQuery(partial = true, target = "http://ex.org/taxonomy"))
            }
        }
    }

    /** Items 608 and 457: Manchester text read against the graph, and a DL query of it. */
    @Test fun the_native_handle_reads_manchester_and_answers_dl_queries() {
        SparklesDatasets.memory().use { ds ->
            a1(ds)
            UpdateAction.parseExecute(
                """$prefixes
                INSERT DATA { GRAPH <http://ex.org/onto> { :ann a :Professor . :Course rdfs:label "Course unit" } }""",
                ds,
            )
            val p = SparklesOnt.parse(ds, "teaches some 'Course unit'", graphIri = "http://ex.org/onto")
            assertEquals("http://ex.org/Course", p.get("value").asObject.getString("class"))
            val e = assertThrows(RuntimeException::class.java) {
                SparklesOnt.parse(ds, "teaches some Lecture", graphIri = "http://ex.org/onto")
            }
            assertTrue(e is SparklesError && "Lecture" in e.engineMessage, e.toString())
            val r = SparklesOnt.dlQuery(
                ds,
                DlQuery("teaches some Course", answers = listOf("subclasses", "instances")),
                "http://ex.org/onto",
            )
            assertEquals("el", r.get("reasoning").asObject.getString("engine"))
            assertEquals(listOf("http://ex.org/Professor"), r.get("subclasses").asArray.map { it.asString.value() })
            assertEquals(listOf("http://ex.org/ann"), r.get("instances").asArray.map { it.asString.value() })
        }
    }
}
