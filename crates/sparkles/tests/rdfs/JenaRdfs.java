// Jena's side of the RDFS-on-read comparison (scripts/rdfs-jena-expected.sh runs it).
// For each suite directory given, wraps data.trig with RDFS on read over the suite's
// schema.ttl, as `ja:DatasetRDFS` and Fuseki's `--rdfs` do, and runs every query of its
// queries.txt. Prints a JSON object that maps each suite to an object that maps each
// query's name to its SPARQL JSON result.
//
//   java -cp "$JENA_HOME/lib/*" JenaRdfs.java full properties …

import java.io.ByteArrayOutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import org.apache.jena.query.Dataset;
import org.apache.jena.query.QueryExecution;
import org.apache.jena.query.QueryExecutionFactory;
import org.apache.jena.query.ResultSetFormatter;
import org.apache.jena.rdfs.RDFSFactory;
import org.apache.jena.riot.RDFDataMgr;

public class JenaRdfs {
    static final String PREFIXES = """
        PREFIX ex: <http://example.org/>
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        """;

    public static void main(String[] suites) throws Exception {
        StringBuilder out = new StringBuilder("{\n");
        for (int i = 0; i < suites.length; i++) {
            String suite = suites[i];
            Dataset data = RDFDataMgr.loadDataset("data.trig");
            Dataset ds = RDFSFactory.datasetRDFS(data, RDFDataMgr.loadGraph(suite + "/schema.ttl"));
            out.append(i == 0 ? "" : ",\n").append('"').append(suite).append("\": {\n");
            boolean first = true;
            for (String line : Files.readAllLines(Path.of(suite, "queries.txt"))) {
                if (line.isBlank() || line.startsWith("#")) continue;
                String[] f = line.split("\t", 2);
                ByteArrayOutputStream b = new ByteArrayOutputStream();
                try (QueryExecution qe = QueryExecutionFactory.create(PREFIXES + f[1], ds)) {
                    if (qe.getQuery().isAskType()) {
                        ResultSetFormatter.outputAsJSON(b, qe.execAsk());
                    } else {
                        ResultSetFormatter.outputAsJSON(b, qe.execSelect());
                    }
                }
                out.append(first ? "" : ",\n").append('"').append(f[0]).append("\": ").append(b);
                first = false;
            }
            out.append("\n}");
        }
        System.out.println(out.append("\n}"));
    }
}
