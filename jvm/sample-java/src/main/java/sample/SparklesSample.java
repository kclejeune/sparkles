package sample;

import io.github.kclejeune.sparkles.jena.CommitReceipt;
import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles;
import io.github.kclejeune.sparkles.jena.SparklesDatasets;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import org.apache.jena.query.Dataset;
import org.apache.jena.query.DatasetFactory;
import org.apache.jena.query.QueryExecution;
import org.apache.jena.query.ResultSetFormatter;
import org.apache.jena.rdf.model.Model;
import org.apache.jena.rdf.model.Resource;
import org.apache.jena.system.Txn;
import org.apache.jena.update.UpdateExecution;

/** Opens a Sparkles database through Jena's API, loads, writes, updates and queries it. */
public final class SparklesSample {
    private SparklesSample() {}

    public static void main(String[] args) throws IOException {
        Path dir = args.length > 0 ? Path.of(args[0]) : Files.createTempDirectory("sparkles-sample");
        Path data = dir.resolve("data.ttl");
        Files.writeString(
                data,
                "@prefix ex: <http://example.org/> .\n"
                        + "ex:alice a ex:Person ; ex:name \"Alice\" ; ex:knows ex:bob .\n"
                        + "ex:bob a ex:Person ; ex:name \"Bob\" .\n");

        try (DatasetGraphSparkles dsg = SparklesDatasets.open(dir.resolve("db"))) {
            Dataset ds = DatasetFactory.wrap(dsg);

            // one bulk commit
            CommitReceipt loaded = dsg.loadFiles(List.of(data));
            System.out.println("loaded " + loaded.getCommit().getInserted() + " quads in commit "
                    + loaded.getCommit().getSeq());

            // the Model API in a write transaction
            Txn.executeWrite(ds, () -> {
                Model m = ds.getDefaultModel();
                Resource carol = m.createResource("http://example.org/carol");
                carol.addProperty(m.createProperty("http://example.org/name"), "Carol");
            });

            // SPARQL Update outside a transaction is one commit
            UpdateExecution.dataset(ds)
                    .update("PREFIX ex: <http://example.org/> INSERT DATA { ex:carol a ex:Person }")
                    .execute();

            Txn.executeRead(ds, () -> {
                try (QueryExecution qe = QueryExecution.dataset(ds)
                        .query("PREFIX ex: <http://example.org/> "
                                + "SELECT ?name WHERE { ?p a ex:Person ; ex:name ?name } ORDER BY ?name")
                        .build()) {
                    ResultSetFormatter.out(System.out, qe.execSelect());
                }
            });

            CommitReceipt last = dsg.lastReceipt();
            System.out.println("last commit: " + (last == null ? "none" : last.getCommit().getSeq()));
        }
    }
}
