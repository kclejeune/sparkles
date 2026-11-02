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
                    // the other thread's autocommit write waits for the writer lock
                    Thread.sleep(200)
                    ds.abort()
                } finally {
                    ds.end()
                }
                other.join()
                val stored = SparklesOnt.graph(ds, "http://ex.org/onto").contains(t)
                assertEquals(adding, stored, "the dataset after the other thread's write")
                assertEquals(stored, g.contains(t), "the model keeps the committed write")
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
}
