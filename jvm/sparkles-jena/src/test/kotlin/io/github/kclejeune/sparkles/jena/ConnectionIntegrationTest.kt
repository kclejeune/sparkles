package io.github.kclejeune.sparkles.jena

import org.apache.jena.fuseki.main.FusekiServer
import org.apache.jena.query.DatasetFactory
import org.apache.jena.rdf.model.ModelFactory
import org.apache.jena.rdfconnection.RDFConnection
import org.apache.jena.riot.Lang
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.core.DatasetGraphFactory
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream

/** The same connection operations against the reference store, embedded Sparkles and Fuseki. */
class ConnectionIntegrationTest {
    private fun exercise(connection: RDFConnection) {
        connection.use { c ->
            c.update("INSERT DATA { <urn:example:alice> <urn:example:name> \"Alice\" . GRAPH <urn:example:people> { <urn:example:bob> <urn:example:name> \"Bob\" } }")
            assertTrue(c.queryAsk("ASK { <urn:example:alice> <urn:example:name> \"Alice\" }"))
            c.query("SELECT ?name { GRAPH <urn:example:people> { ?person <urn:example:name> ?name } }").use { query ->
                val rows = query.execSelect()
                assertEquals("Bob", rows.next().getLiteral("name").string)
                assertFalse(rows.hasNext())
            }
            assertEquals(1L, c.fetch("urn:example:people").size())
            val replacement = ModelFactory.createDefaultModel()
            replacement.add(replacement.createResource("urn:example:carol"), replacement.createProperty("urn:example:name"), "Carol")
            c.put("urn:example:people", replacement)
            assertEquals(1L, c.fetch("urn:example:people").size())
            assertTrue(c.queryAsk("ASK { GRAPH <urn:example:people> { <urn:example:carol> <urn:example:name> \"Carol\" } }"))
            c.delete("urn:example:people")
            assertFalse(c.queryAsk("ASK { GRAPH <urn:example:people> { ?s ?p ?o } }"))
            assertEquals(1L, c.queryConstruct("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }").size())
        }
    }
    @Test fun reference_connection() {
        val ds = DatasetFactory.wrap(DatasetGraphFactory.createTxnMem())
        try { exercise(RDFConnection.connect(ds)) } finally { ds.close() }
    }
    @Test fun sparkles_connection() {
        val ds = DatasetFactory.wrap(SparklesDatasets.memory())
        try { exercise(RDFConnection.connect(ds)) } finally { ds.close() }
    }
    @Test fun fuseki_assembler_connection() {
        val model = ModelFactory.createDefaultModel()
        RDFDataMgr.read(model, ByteArrayInputStream("""
            @prefix fuseki: <http://jena.apache.org/fuseki#> .
            @prefix s: <urn:x-sparkles:assembler#> .
            <urn:example:service> a fuseki:Service ; fuseki:name "ds" ; fuseki:dataset <urn:example:dataset> ;
              fuseki:endpoint [ fuseki:operation fuseki:query ] ;
              fuseki:endpoint [ fuseki:operation fuseki:update ] ;
              fuseki:endpoint [ fuseki:operation fuseki:gsp-rw ] .
            <urn:example:dataset> a s:DatasetSparkles ; s:memory true .
        """.trimIndent().toByteArray()), Lang.TURTLE)
        val server = FusekiServer.create().port(0).parseConfig(model).build()
        try { server.start(); exercise(RDFConnection.connect("http://localhost:${server.port}/ds")) }
        finally { server.stop() }
    }
}
