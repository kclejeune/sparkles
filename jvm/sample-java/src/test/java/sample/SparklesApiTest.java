package sample;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.github.kclejeune.sparkles.jena.BlankNodeLabels;
import io.github.kclejeune.sparkles.jena.CommitReceipt;
import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles;
import io.github.kclejeune.sparkles.jena.Sparkles;
import io.github.kclejeune.sparkles.jena.SparklesBulkSink;
import io.github.kclejeune.sparkles.jena.SparklesDatasets;
import io.github.kclejeune.sparkles.jena.SparklesError;
import io.github.kclejeune.sparkles.jena.SparklesFallback;
import io.github.kclejeune.sparkles.jena.SparklesOptions;
import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import java.util.Iterator;
import java.util.List;
import org.apache.jena.graph.Node;
import org.apache.jena.graph.NodeFactory;
import org.apache.jena.query.QueryExecution;
import org.apache.jena.query.QueryParseException;
import org.apache.jena.query.ResultSet;
import org.apache.jena.riot.Lang;
import org.apache.jena.sparql.JenaTransactionException;
import org.apache.jena.sparql.core.Quad;
import org.apache.jena.sparql.exec.QueryExec;
import org.apache.jena.sparql.util.Context;
import org.apache.jena.system.Txn;
import org.junit.jupiter.api.Test;

/** The public API as Java sees it. */
class SparklesApiTest {
    private static Node iri(String local) {
        return NodeFactory.createURI("http://example.org/" + local);
    }

    @Test
    void optionsAreBuiltAndCompared() {
        SparklesOptions o = SparklesOptions.builder()
                .unionDefaultGraph(true)
                .fallback(SparklesFallback.NEVER)
                .blankNodeLabels(BlankNodeLabels.TRANSACTION)
                .autocommit(true)
                .build();
        assertTrue(o.getUnionDefaultGraph());
        assertEquals(SparklesFallback.NEVER, o.getFallback());
        assertEquals(o, o.toBuilder().build());
        assertFalse(SparklesOptions.DEFAULT.getAutocommit());
        assertNotNull(Sparkles.version());
    }

    @Test
    void transactionsQueriesAndReceipts() {
        try (DatasetGraphSparkles dsg = SparklesDatasets.memory()) {
            Quad q = Quad.create(iri("g"), iri("s"), iri("p"), NodeFactory.createLiteralString("o"));
            assertThrows(JenaTransactionException.class, () -> dsg.add(q));
            Txn.executeWrite(dsg, () -> dsg.add(q));
            CommitReceipt r = dsg.lastReceipt();
            assertNotNull(r);
            assertTrue(r.isCommitted());
            assertEquals(1L, r.getCommit().getInserted());

            Iterator<Quad> it = dsg.find(null, iri("s"), null, null);
            assertTrue(it.hasNext());
            assertEquals(q, it.next());

            try (QueryExecution qe = QueryExecution.dataset(org.apache.jena.query.DatasetFactory.wrap(dsg))
                    .query("SELECT ?o WHERE { GRAPH ?g { ?s ?p ?o } }")
                    .build()) {
                ResultSet rs = qe.execSelect();
                assertEquals("o", rs.next().getLiteral("o").getString());
            }
            assertEquals(1L, dsg.stats().getNativeQueries());
        }
    }

    @Test
    void sparklesErrorsAreJenaExceptions() {
        try (DatasetGraphSparkles dsg = SparklesDatasets.memory()) {
            dsg.load(new ByteArrayInputStream("<a:s> <a:p> 1 .".getBytes(StandardCharsets.UTF_8)), Lang.TURTLE);
            // a triple without an object
            RuntimeException e = assertThrows(RuntimeException.class, () -> dsg.load(
                    new ByteArrayInputStream("<a:s> <a:p> .".getBytes(StandardCharsets.UTF_8)), Lang.TURTLE));
            assertInstanceOf(SparklesError.class, e);
            assertInstanceOf(org.apache.jena.riot.RiotException.class, e);
            assertThrows(QueryParseException.class, () -> QueryExec.dataset(dsg).query("SELECT * {"));
            Context never = Context.create().set(Sparkles.FALLBACK, SparklesFallback.NEVER);
            assertTrue(QueryExec.dataset(dsg).query("ASK { ?s ?p 1 }").context(never).ask());
        }
    }

    @Test
    void bulkSinkLoadsInOneCommit() {
        try (DatasetGraphSparkles dsg = SparklesDatasets.memory()) {
            try (SparklesBulkSink sink = dsg.bulkSink()) {
                sink.start();
                for (int i = 0; i < 1000; i++) {
                    sink.triple(org.apache.jena.graph.Triple.create(iri("s" + i), iri("p"), iri("o")));
                }
                sink.finish();
                CommitReceipt r = sink.receipt();
                assertNotNull(r);
                assertEquals(1000L, r.getCommit().getInserted());
            }
            List<Node> graphs = Txn.calculateRead(dsg, () -> org.apache.jena.atlas.iterator.Iter.toList(dsg.listGraphNodes()));
            assertTrue(graphs.isEmpty());
            assertEquals(1000, dsg.getDefaultGraph().size());
        }
    }
}
