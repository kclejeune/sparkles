// Jena's own HTTP clients against a running Sparkles server.
//
// Run with the jars of an Apache Jena distribution on the class path (Java 21 runs this
// single source file directly):
//
//   java -cp "$JENA_HOME/lib/*" testsuite/jena-clients/JenaClients.java http://127.0.0.1:5230
//
// scripts/test-jena-clients.sh starts a server on a temporary directory and runs this.
// The server must be open (no --auth-config) and empty, and serve direct Graph Store
// naming (--gsp-direct-naming). Every check prints PASS or FAIL; the exit status is the
// number of failures (capped at 100).

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.StringReader;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.function.Supplier;
import java.util.zip.GZIPOutputStream;

import org.apache.jena.atlas.json.JSON;
import org.apache.jena.atlas.json.JsonArray;
import org.apache.jena.atlas.json.JsonObject;
import org.apache.jena.atlas.json.JsonValue;
import org.apache.jena.graph.Graph;
import org.apache.jena.graph.Node;
import org.apache.jena.graph.NodeFactory;
import org.apache.jena.http.HttpOp;
import org.apache.jena.query.Dataset;
import org.apache.jena.query.DatasetFactory;
import org.apache.jena.query.QueryExecution;
import org.apache.jena.query.ResultSet;
import org.apache.jena.query.ResultSetFormatter;
import org.apache.jena.rdf.model.Model;
import org.apache.jena.rdf.model.ModelFactory;
import org.apache.jena.rdfconnection.RDFConnection;
import org.apache.jena.rdfconnection.RDFConnectionFuseki;
import org.apache.jena.rdfconnection.RDFConnectionRemote;
import org.apache.jena.riot.Lang;
import org.apache.jena.riot.RDFDataMgr;
import org.apache.jena.riot.RDFFormat;
import org.apache.jena.riot.RDFLanguages;
import org.apache.jena.riot.RDFParser;
import org.apache.jena.riot.RDFWriter;
import org.apache.jena.riot.WebContent;
import org.apache.jena.riot.resultset.ResultSetLang;
import org.apache.jena.sparql.core.DatasetGraph;
import org.apache.jena.sparql.core.DatasetGraphFactory;
import org.apache.jena.sparql.exec.QueryExec;
import org.apache.jena.sparql.exec.RowSet;
import org.apache.jena.sparql.exec.http.DSP;
import org.apache.jena.sparql.exec.http.GSP;
import org.apache.jena.sparql.exec.http.QueryExecHTTP;
import org.apache.jena.sparql.exec.http.QueryExecHTTPBuilder;
import org.apache.jena.sparql.exec.http.QuerySendMode;
import org.apache.jena.sparql.exec.http.UpdateExecHTTP;
import org.apache.jena.sparql.exec.http.UpdateSendMode;
import org.apache.jena.sparql.graph.GraphFactory;
import org.apache.jena.sparql.util.IsoMatcher;
import org.apache.jena.sys.JenaSystem;

public class JenaClients {
    static String server;
    static int passed = 0;
    static final List<String> failures = new ArrayList<>();
    static final HttpClient http = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(10)).build();

    static final String PREFIXES = """
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        """;
    // canonical lexical forms: Sparkles stores numbers by value
    static final String TRIPLES = PREFIXES + """
        ex:alice a ex:Person ; ex:name "Alice" ; ex:age 42 ; ex:height 1.68 ;
            ex:score "3.5E0"^^xsd:double ; ex:born "1984-02-01"^^xsd:date ; ex:label "Alicia"@es ;
            ex:knows ex:bob , _:anon .
        _:anon ex:name "Somebody" ; ex:flag true .
        ex:bob a ex:Person ; ex:name "Bob" ; ex:note "line one\\nline \\"two\\"" .
        """;
    static final String OTHER = PREFIXES + """
        ex:carol a ex:Person ; ex:name "Carol" ; ex:age 7 .
        """;
    static final String QUADS = """
        PREFIX ex: <http://example.org/>
        ex:x ex:p ex:y .
        ex:g1 { ex:a ex:p "in g1" . ex:a ex:q 1 . }
        ex:g2 { ex:b ex:p "in g2"@en . _:b1 ex:q ex:a . }
        """;

    public static void main(String[] args) throws Exception {
        JenaSystem.init();
        server = args.length > 0 ? args[0] : "http://127.0.0.1:5230";
        server = server.replaceAll("/+$", "");

        adminFuseki();
        String ds = server + "/jc";
        queries(ds);
        updates(ds);
        gsp(ds);
        datasetProtocol(ds);
        connections(ds);
        compression(ds);
        directNaming(ds);
        shacl(ds);
        validators();
        adminTasks();
        adminLifecycle();

        System.out.println();
        System.out.printf("%d passed, %d failed%n", passed, failures.size());
        for ( String f : failures )
            System.out.println("  FAIL " + f);
        System.exit(Math.min(failures.size(), 100));
    }

    // ---------------------------------------------------------------- harness ----

    interface Check { void run() throws Exception; }

    static void check(String name, Check c) {
        try {
            c.run();
            passed++;
            System.out.println("PASS " + name);
        } catch (Throwable t) {
            String msg = t.getClass().getSimpleName() + ": " + t.getMessage();
            failures.add(name + " -- " + msg);
            System.out.println("FAIL " + name + " -- " + msg);
        }
    }

    static void expect(boolean ok, String what) {
        if ( !ok )
            throw new AssertionError(what);
    }

    static void expectEq(Object expected, Object actual, String what) {
        if ( expected == null ? actual != null : !expected.equals(actual) )
            throw new AssertionError(what + ": expected <" + expected + "> but was <" + actual + ">");
    }

    static Graph graph(String turtle) {
        Graph g = GraphFactory.createDefaultGraph();
        RDFParser.fromString(turtle, Lang.TURTLE).parse(g);
        return g;
    }

    static DatasetGraph dataset(String trig) {
        DatasetGraph dsg = DatasetGraphFactory.createTxnMem();
        RDFParser.fromString(trig, Lang.TRIG).parse(dsg);
        return dsg;
    }

    static void expectIso(Graph expected, Graph actual, String what) {
        if ( !IsoMatcher.isomorphic(expected, actual) ) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            RDFDataMgr.write(out, actual, Lang.NT);
            throw new AssertionError(what + ": graphs differ (" + expected.size() + " expected, "
                                     + actual.size() + " got):\n" + out.toString(StandardCharsets.UTF_8));
        }
    }

    static void expectIso(DatasetGraph expected, DatasetGraph actual, String what) {
        if ( !IsoMatcher.isomorphic(expected, actual) ) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            RDFDataMgr.write(out, actual, Lang.NQ);
            throw new AssertionError(what + ": datasets differ:\n" + out.toString(StandardCharsets.UTF_8));
        }
    }

    record Resp(int status, String body, java.net.http.HttpHeaders headers) {
        JsonObject json() { return JSON.parse(body); }
        String header(String name) { return headers.firstValue(name).orElse(null); }
    }

    static Resp send(String method, String path, String contentType, byte[] body, String... headers) throws Exception {
        String url = path.startsWith("http") ? path : server + path;
        HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(url)).timeout(Duration.ofSeconds(60));
        if ( contentType != null )
            b.header("Content-Type", contentType);
        for ( int i = 0; i + 1 < headers.length; i += 2 )
            b.header(headers[i], headers[i + 1]);
        HttpRequest.BodyPublisher p = body == null ? HttpRequest.BodyPublishers.noBody()
                                                   : HttpRequest.BodyPublishers.ofByteArray(body);
        b.method(method, p);
        HttpResponse<byte[]> r = http.send(b.build(), HttpResponse.BodyHandlers.ofByteArray());
        return new Resp(r.statusCode(), new String(r.body(), StandardCharsets.UTF_8), r.headers());
    }

    static Resp get(String path, String... headers) throws Exception {
        return send("GET", path, null, null, headers);
    }

    static Resp post(String path) throws Exception {
        return send("POST", path, null, null);
    }

    static Resp postForm(String path, String form) throws Exception {
        return send("POST", path, WebContent.contentTypeHTMLForm, form.getBytes(StandardCharsets.UTF_8));
    }

    static byte[] utf8(String s) { return s.getBytes(StandardCharsets.UTF_8); }

    static String enc(String s) { return URLEncoder.encode(s, StandardCharsets.UTF_8); }

    static void expectStatus(int expected, Resp r, String what) {
        if ( r.status() != expected )
            throw new AssertionError(what + ": expected HTTP " + expected + " but got " + r.status() + ": " + r.body());
    }

    static void expect2xx(Resp r, String what) {
        if ( r.status() / 100 != 2 )
            throw new AssertionError(what + ": expected 2xx but got " + r.status() + ": " + r.body());
    }

    /** Wait for a Fuseki task to finish and return its description. */
    static JsonObject awaitTask(String taskId) throws Exception {
        for ( int i = 0; i < 600; i++ ) {
            Resp r = get("/$/tasks/" + taskId);
            expectStatus(200, r, "GET /$/tasks/" + taskId);
            JsonObject t = r.json();
            if ( t.hasKey("finished") )
                return t;
            Thread.sleep(50);
        }
        throw new AssertionError("task " + taskId + " did not finish");
    }

    static String str(JsonObject o, String key) {
        JsonValue v = o.get(key);
        if ( v == null )
            throw new AssertionError("no '" + key + "' in " + o);
        return v.isString() ? v.getAsString().value() : v.toString();
    }

    // ----------------------------------------------------------- Fuseki admin ----

    static void adminFuseki() {
        check("admin: GET /$/ping", () -> {
            Resp r = get("/$/ping");
            expectStatus(200, r, "ping");
            expect(!r.body().isBlank(), "ping has a body");
        });
        check("admin: POST /$/datasets with a form (dbName, dbType=tdb2)", () -> {
            Resp r = postForm("/$/datasets", "dbName=jc&dbType=tdb2");
            expect2xx(r, "create jc");
        });
        check("admin: POST /$/datasets with query parameters (dbType=mem)", () -> {
            Resp r = post("/$/datasets?dbName=/jm&dbType=mem");
            expect2xx(r, "create jm");
        });
        check("admin: POST /$/datasets with an existing name is 409", () -> {
            Resp r = postForm("/$/datasets", "dbName=jc&dbType=tdb2");
            expectStatus(409, r, "create jc again");
        });
        check("admin: POST /$/datasets with an assembler (text/turtle)", () -> {
            String cfg = """
                @prefix fuseki: <http://jena.apache.org/fuseki#> .
                @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
                @prefix ja: <http://jena.hpl.hp.com/2005/11/Assembler#> .
                @prefix tdb2: <http://jena.apache.org/2016/tdb#> .
                <#service> rdf:type fuseki:Service ;
                    fuseki:name "ja" ;
                    fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "sparql" ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "query" ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:update ; fuseki:name "update" ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:gsp-r ; fuseki:name "get" ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:gsp-rw ; fuseki:name "data" ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:upload ; fuseki:name "upload" ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:query ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:update ] ;
                    fuseki:endpoint [ fuseki:operation fuseki:gsp-rw ] ;
                    fuseki:dataset <#dataset> .
                <#dataset> rdf:type tdb2:DatasetTDB2 ;
                    tdb2:location "--mem--" .
                """;
            Resp r = send("POST", "/$/datasets", "text/turtle", utf8(cfg));
            expect2xx(r, "create ja from an assembler");
            Resp q = get("/ja/query?query=" + enc("ASK {}"), "Accept", "application/sparql-results+json");
            expectStatus(200, q, "query /ja");
        });
        check("admin: an assembler with an unsupported dataset is refused with 400", () -> {
            String cfg = """
                @prefix fuseki: <http://jena.apache.org/fuseki#> .
                @prefix ja: <http://jena.hpl.hp.com/2005/11/Assembler#> .
                @prefix text: <http://jena.apache.org/text#> .
                <#service> a fuseki:Service ; fuseki:name "jx" ; fuseki:dataset <#ds> .
                <#ds> a text:TextDataset .
                """;
            Resp r = send("POST", "/$/datasets", "text/turtle", utf8(cfg));
            expectStatus(400, r, "assembler with text:TextDataset");
            expect(r.body().contains("text:TextDataset") || r.body().contains("TextDataset"), "the error names the type: " + r.body());
        });
        check("admin: GET /$/server describes datasets the Fuseki way", () -> {
            Resp r = get("/$/server");
            expectStatus(200, r, "server");
            JsonObject o = r.json();
            expect(o.hasKey("version") || o.hasKey("startDateTime"), "server fields: " + o);
            expect(o.hasKey("uptime"), "uptime: " + o);
            boolean found = false;
            for ( JsonValue v : o.get("datasets").getAsArray() )
                if ( "/jc".equals(str(v.getAsObject(), "ds.name")) )
                    found = true;
            expect(found, "/jc in /$/server");
        });
        check("admin: GET /$/datasets/jc has ds.name, ds.state and ds.services", () -> {
            Resp r = get("/$/datasets/jc");
            expectStatus(200, r, "dataset");
            JsonObject o = r.json();
            expectEq("/jc", str(o, "ds.name"), "ds.name");
            expect(o.get("ds.state").getAsBoolean().value(), "ds.state");
            JsonArray services = o.get("ds.services").getAsArray();
            boolean query = false;
            for ( JsonValue s : services ) {
                JsonObject so = s.getAsObject();
                if ( "query".equals(str(so, "srv.type")) ) {
                    query = true;
                    expect(so.get("srv.endpoints").getAsArray().size() > 0, "query endpoints");
                }
            }
            expect(query, "a query service: " + services);
        });
        check("admin: GET /$/datasets lists /jc and /jm", () -> {
            Resp r = get("/$/datasets");
            JsonArray a = r.json().get("datasets").getAsArray();
            List<String> names = new ArrayList<>();
            for ( JsonValue v : a )
                names.add(str(v.getAsObject(), "ds.name"));
            expect(names.contains("/jc") && names.contains("/jm"), "names: " + names);
        });
    }

    static void adminTasks() {
        check("admin: POST /$/compact/jc?deleteOld=true is a task that succeeds", () -> {
            Resp r = post("/$/compact/jc?deleteOld=true");
            expect2xx(r, "compact");
            String id = str(r.json(), "taskId");
            JsonObject t = awaitTask(id);
            expect(t.get("success").getAsBoolean().value(), "success: " + t);
            expect(t.hasKey("started") && t.hasKey("task"), "task fields: " + t);
        });
        check("admin: POST /$/backup/jc writes a backup, listed by /$/backups-list", () -> {
            Resp r = post("/$/backup/jc");
            expect2xx(r, "backup");
            JsonObject t = awaitTask(str(r.json(), "taskId"));
            expect(t.get("success").getAsBoolean().value(), "success: " + t);
            Resp l = get("/$/backups-list");
            expectStatus(200, l, "backups-list");
            JsonArray a = l.json().get("backups").getAsArray();
            boolean found = false;
            for ( JsonValue v : a )
                if ( v.getAsString().value().startsWith("jc_") )
                    found = true;
            expect(found, "a jc_ backup in " + a);
        });
        check("admin: POST /$/backups/jc (Fuseki's alias) writes an N-Quads backup", () -> {
            Resp r = post("/$/backups/jc");
            expect2xx(r, "backups alias");
            JsonObject t = awaitTask(str(r.json(), "taskId"));
            expect(t.get("success").getAsBoolean().value(), "success: " + t);
        });
        check("admin: POST /$/backups/jc with a JSON body is the repository API", () -> {
            Resp r = send("POST", "/$/backups/jc", "application/json", utf8("{\"repository\":\"nowhere\"}"));
            expectStatus(404, r, "a backup into an unknown repository");
            expect(r.body().contains("no-such-repository"), "repository error: " + r.body());
        });
        check("admin: GET /$/tasks lists tasks with Fuseki's fields", () -> {
            Resp r = get("/$/tasks");
            expectStatus(200, r, "tasks");
            JsonArray a = JSON.parseAny(r.body()).getAsArray();
            expect(a.size() >= 3, "three tasks or more");
            JsonObject t = a.get(0).getAsObject();
            expect(t.hasKey("taskId") && t.hasKey("task") && t.hasKey("started"), "fields: " + t);
        });
        check("admin: GET /$/tasks/{unknown} is 404", () -> {
            expectStatus(404, get("/$/tasks/no-such-task"), "unknown task");
        });
        check("admin: GET /$/stats and /$/stats/jc have Fuseki's request counters", () -> {
            Resp all = get("/$/stats");
            expectStatus(200, all, "stats");
            JsonObject datasets = all.json().get("datasets").getAsObject();
            expect(datasets.hasKey("/jc"), "/jc in " + datasets.keys());
            JsonObject jc = datasets.get("/jc").getAsObject();
            expect(jc.hasKey("Requests") && jc.hasKey("RequestsGood") && jc.hasKey("RequestsBad"), "counters: " + jc);
            expect(jc.get("Requests").getAsNumber().value().longValue() > 0, "requests counted: " + jc);
            expect(jc.hasKey("endpoints"), "endpoints: " + jc);
            Resp one = get("/$/stats/jc");
            expectStatus(200, one, "stats/jc");
            expect(one.json().get("datasets").getAsObject().hasKey("/jc"), "datasets./jc: " + one.body());
        });
        check("admin: GET /$/metrics is Prometheus text", () -> {
            Resp r = get("/$/metrics");
            expectStatus(200, r, "metrics");
            expect(r.body().contains("# TYPE"), "metrics text");
        });
    }

    static void adminLifecycle() {
        check("admin: POST /$/datasets/jm?state=offline takes the dataset offline", () -> {
            expectStatus(200, post("/$/datasets/jm?state=offline"), "offline");
            Resp d = get("/$/datasets/jm");
            expect(!d.json().get("ds.state").getAsBoolean().value(), "ds.state false: " + d.body());
            Resp q = get("/jm/query?query=" + enc("ASK {}"));
            expectStatus(503, q, "query on an offline dataset");
            expectStatus(200, post("/$/datasets/jm?state=active"), "active");
            Resp q2 = get("/jm/query?query=" + enc("ASK {}"));
            expectStatus(200, q2, "query on an active dataset");
        });
        check("admin: POST /$/datasets/jm with an unknown state is 400", () -> {
            expectStatus(400, post("/$/datasets/jm?state=sleeping"), "bad state");
        });
        check("admin: DELETE /$/datasets/jm", () -> {
            Resp r = send("DELETE", "/$/datasets/jm", null, null);
            expect2xx(r, "delete jm");
            expectStatus(404, get("/$/datasets/jm"), "jm is gone");
        });
        check("admin: DELETE /$/datasets/ja", () -> {
            expect2xx(send("DELETE", "/$/datasets/ja", null, null), "delete ja");
        });
    }

    // --------------------------------------------------------------- queries ----

    static void loadBase(String ds) {
        RDFConnection c = RDFConnectionRemote.service(ds).build();
        c.put(ModelFactory.createModelForGraph(graph(TRIPLES)));
        c.put("http://example.org/other", ModelFactory.createModelForGraph(graph(OTHER)));
        c.close();
    }

    static final String SELECT = "PREFIX ex: <http://example.org/> SELECT ?s ?name WHERE { ?s a ex:Person ; ex:name ?name } ORDER BY ?name";

    static void queries(String ds) {
        check("query: load data with RDFConnectionRemote.put", () -> loadBase(ds));

        String[][] selectFormats = {
            { "json", WebContent.contentTypeResultsJSON },
            { "xml", WebContent.contentTypeResultsXML },
            { "csv", WebContent.contentTypeTextCSV },
            { "tsv", WebContent.contentTypeTextTSV },
            { "thrift (falls back)", WebContent.contentTypeResultsThrift },
            { "protobuf (falls back)", WebContent.contentTypeResultsProtobuf },
            { "default", null },
        };
        for ( QuerySendMode mode : QuerySendMode.values() ) {
            for ( String[] f : selectFormats ) {
                check("query: SELECT, " + mode + ", " + f[0], () -> {
                    QueryExecHTTPBuilder b = QueryExecHTTP.service(ds + "/sparql").query(SELECT).sendMode(mode);
                    if ( f[1] != null )
                        b.acceptHeader(f[1]);
                    try ( QueryExec qe = b.build() ) {
                        RowSet rs = qe.select();
                        List<String> names = new ArrayList<>();
                        rs.forEachRemaining(row -> names.add(row.get("name").getLiteralLexicalForm()));
                        expectEq(List.of("Alice", "Bob"), names, "names");
                    }
                });
            }
        }
        // Jena's own TSV reader takes every TSV body for a row set, so an ASK in TSV is
        // checked as text below
        String[][] askFormats = {
            { "json", WebContent.contentTypeResultsJSON },
            { "xml", WebContent.contentTypeResultsXML },
            { "csv", WebContent.contentTypeTextCSV },
            { "default", null },
        };
        for ( String[] f : askFormats ) {
            check("query: ASK, " + f[0], () -> {
                QueryExecHTTPBuilder b = QueryExecHTTP.service(ds + "/query")
                    .query("PREFIX ex: <http://example.org/> ASK { ex:alice ex:knows ex:bob }");
                if ( f[1] != null )
                    b.acceptHeader(f[1]);
                try ( QueryExec qe = b.build() ) {
                    expect(qe.ask(), "ASK true");
                }
                QueryExecHTTPBuilder b2 = QueryExecHTTP.service(ds + "/query")
                    .query("PREFIX ex: <http://example.org/> ASK { ex:bob ex:knows ex:alice }");
                if ( f[1] != null )
                    b2.acceptHeader(f[1]);
                try ( QueryExec qe = b2.build() ) {
                    expect(!qe.ask(), "ASK false");
                }
            });
        }
        check("query: ASK, tsv (Fuseki's ?_askResult form)", () -> {
            Resp r = get("/jc/sparql?query=" + enc("ASK {}"), "Accept", WebContent.contentTypeTextTSV);
            expectStatus(200, r, "ASK as TSV");
            expectEq("?_askResult\ntrue\n", r.body(), "TSV body");
        });
        Lang[] graphLangs = { Lang.TURTLE, Lang.NTRIPLES, Lang.RDFXML, Lang.JSONLD, Lang.RDFTHRIFT, Lang.RDFPROTO, Lang.TRIG, Lang.NQUADS, Lang.RDFJSON, Lang.TRIX };
        for ( Lang lang : graphLangs ) {
            check("query: CONSTRUCT, " + lang.getName(), () -> {
                try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                        .query("CONSTRUCT WHERE { ?s ?p ?o }")
                        .acceptHeader(lang.getHeaderString()).build() ) {
                    expectIso(graph(TRIPLES), qe.construct(), "CONSTRUCT");
                }
            });
        }
        check("query: DESCRIBE", () -> {
            try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                    .query("DESCRIBE <http://example.org/bob>").build() ) {
                Graph g = qe.describe();
                expect(g.size() >= 3, "DESCRIBE has bob's triples: " + g.size());
            }
        });
        check("query: CONSTRUCT quads (TriG and N-Quads)", () -> {
            for ( Lang lang : new Lang[] { Lang.TRIG, Lang.NQUADS, Lang.TRIX } ) {
                try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                        .query("CONSTRUCT { ?s ?p ?o } WHERE { GRAPH ?g { ?s ?p ?o } }")
                        .acceptHeader(lang.getHeaderString()).build() ) {
                    Graph g = qe.construct();
                    expectIso(graph(OTHER), g, "CONSTRUCT from named graphs as " + lang.getName());
                }
            }
        });
        check("query: default-graph-uri and named-graph-uri", () -> {
            try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                    .query("PREFIX ex: <http://example.org/> SELECT ?n { ?s ex:name ?n }")
                    .addDefaultGraphURI("http://example.org/other").build() ) {
                RowSet rs = qe.select();
                expectEq("Carol", rs.next().get("n").getLiteralLexicalForm(), "name in the other graph");
                expect(!rs.hasNext(), "only Carol");
            }
            try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                    .query("SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g")
                    .addNamedGraphURI("http://example.org/other").build() ) {
                RowSet rs = qe.select();
                var row = rs.next();
                expectEq("http://example.org/other", row.get("g").getURI(), "graph");
                expectEq(3, ((Number) row.get("n").getLiteralValue()).intValue(), "count");
            }
        });
        check("query: on the dataset URL (/jc)", () -> {
            try ( QueryExec qe = QueryExecHTTP.service(ds).query(SELECT).build() ) {
                expectEq(2, count(qe.select()), "rows");
            }
        });
        check("query: a syntax error is an HTTP 400", () -> {
            Resp r = get("/jc/sparql?query=" + enc("SELECT * WHERE {"));
            expectStatus(400, r, "syntax error");
        });
        check("query: format= parameter (json, xml, csv, tsv, text?)", () -> {
            for ( String f : new String[] { "json", "xml", "csv", "tsv" } ) {
                Resp r = get("/jc/sparql?format=" + f + "&query=" + enc(SELECT));
                expectStatus(200, r, "format=" + f);
            }
        });
        check("query: SELECT in SPARQL Results Thrift when asked for it", () -> {
            Resp r = get("/jc/sparql?query=" + enc(SELECT), "Accept", WebContent.contentTypeResultsThrift);
            expectStatus(200, r, "thrift results");
            expectEq(WebContent.contentTypeResultsThrift, r.header("Content-Type"), "Content-Type");
            Resp g = get("/jc/sparql?query=" + enc("CONSTRUCT WHERE { ?s ?p ?o }"), "Accept", "application/rdf+thrift");
            expectEq("application/rdf+thrift", g.header("Content-Type"), "CONSTRUCT Content-Type");
        });
        check("query: Fuseki's output=, results= and force-accept=", () -> {
            Resp x = get("/jc/sparql?output=sparql&query=" + enc(SELECT));
            expect(x.header("Content-Type").startsWith("application/sparql-results+xml"), "output=sparql: " + x.header("Content-Type"));
            Resp c = get("/jc/sparql?results=csv&query=" + enc(SELECT));
            expect(c.header("Content-Type").startsWith("text/csv"), "results=csv: " + c.header("Content-Type"));
            Resp g = get("/jc/sparql?output=nt&query=" + enc("CONSTRUCT WHERE { ?s ?p ?o }"));
            expect(g.header("Content-Type").startsWith("application/n-triples"), "output=nt: " + g.header("Content-Type"));
            Resp f = get("/jc/sparql?force-accept=text/plain&query=" + enc(SELECT));
            expect(f.header("Content-Type").startsWith("text/plain"), "force-accept: " + f.header("Content-Type"));
        });
        check("query: Accept negotiation with q-values", () -> {
            Resp r = get("/jc/sparql?query=" + enc(SELECT), "Accept",
                         "application/sparql-results+xml;q=0.5, text/csv;q=0.9, */*;q=0.1");
            expectStatus(200, r, "q-values");
            expect(r.header("Content-Type").startsWith("text/csv"), "CSV chosen: " + r.header("Content-Type"));
        });
    }

    static int count(RowSet rs) {
        int n = 0;
        while ( rs.hasNext() ) { rs.next(); n++; }
        return n;
    }

    // --------------------------------------------------------------- updates ----

    static void updates(String ds) {
        for ( UpdateSendMode mode : UpdateSendMode.values() ) {
            check("update: INSERT DATA / DELETE DATA, " + mode, () -> {
                UpdateExecHTTP.service(ds + "/update").sendMode(mode)
                    .update("PREFIX ex: <http://example.org/> INSERT DATA { ex:dave ex:name \"Dave\" }").execute();
                try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                        .query("PREFIX ex: <http://example.org/> ASK { ex:dave ex:name \"Dave\" }").build() ) {
                    expect(qe.ask(), "inserted");
                }
                UpdateExecHTTP.service(ds + "/update").sendMode(mode)
                    .update("PREFIX ex: <http://example.org/> DELETE DATA { ex:dave ex:name \"Dave\" }").execute();
                try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                        .query("PREFIX ex: <http://example.org/> ASK { ex:dave ex:name \"Dave\" }").build() ) {
                    expect(!qe.ask(), "deleted");
                }
            });
        }
        check("update: on the dataset URL (/jc)", () -> {
            UpdateExecHTTP.service(ds)
                .update("PREFIX ex: <http://example.org/> INSERT DATA { GRAPH ex:tmp { ex:a ex:b ex:c } }").execute();
            UpdateExecHTTP.service(ds).update("DROP GRAPH <http://example.org/tmp>").execute();
        });
        check("update: using-graph-uri", () -> {
            UpdateExecHTTP.service(ds + "/update")
                .addUsingGraphURI("http://example.org/other")
                .update("PREFIX ex: <http://example.org/> INSERT { GRAPH ex:copy { ?s ex:name ?n } } WHERE { ?s ex:name ?n }")
                .execute();
            try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                    .query("SELECT (COUNT(*) AS ?n) { GRAPH <http://example.org/copy> { ?s ?p ?o } }").build() ) {
                expectEq(1, ((Number) qe.select().next().get("n").getLiteralValue()).intValue(), "copied from the other graph only");
            }
            UpdateExecHTTP.service(ds + "/update").update("DROP GRAPH <http://example.org/copy>").execute();
        });
        check("update: a syntax error is an HTTP 400", () -> {
            Resp r = send("POST", "/jc/update", WebContent.contentTypeSPARQLUpdate, utf8("INSERT DATA {"));
            expectStatus(400, r, "syntax error");
        });
        check("update: GET is refused", () -> {
            Resp r = get("/jc/update?update=" + enc("CLEAR DEFAULT"));
            expect(r.status() == 405 || r.status() == 400, "update by GET: " + r.status());
        });
    }

    // ----------------------------------------------------- Graph Store Protocol ----

    static final RDFFormat[] TRIPLE_FORMATS = {
        RDFFormat.TURTLE, RDFFormat.NTRIPLES, RDFFormat.RDFXML, RDFFormat.JSONLD,
        RDFFormat.RDF_THRIFT, RDFFormat.RDF_THRIFT_VALUES, RDFFormat.RDF_PROTO, RDFFormat.RDFJSON,
        RDFFormat.TRIG, RDFFormat.NQUADS, RDFFormat.TRIX,
    };

    static void gsp(String ds) {
        String gsp = ds + "/data";
        for ( RDFFormat fmt : TRIPLE_FORMATS ) {
            String f = fmt.toString();
            check("gsp: default graph PUT/GET/POST/DELETE as " + f, () -> {
                GSP.service(gsp).defaultGraph().contentType(fmt).PUT(graph(TRIPLES));
                expectIso(graph(TRIPLES), GSP.service(gsp).defaultGraph().accept(fmt.getLang()).GET(), "GET after PUT");
                GSP.service(gsp).defaultGraph().contentType(fmt).POST(graph(OTHER));
                Graph both = graph(TRIPLES);
                graph(OTHER).find().forEach(both::add);
                expectIso(both, GSP.service(gsp).defaultGraph().GET(), "GET after POST");
                GSP.service(gsp).defaultGraph().DELETE();
                expectEq(0, GSP.service(gsp).defaultGraph().GET().size(), "empty after DELETE");
            });
            check("gsp: named graph PUT/GET/POST/DELETE as " + f, () -> {
                String g = "http://example.org/graph/" + fmt.getLang().getLabel().replaceAll("\\W", "");
                GSP.service(gsp).graphName(g).contentType(fmt).PUT(graph(TRIPLES));
                expectIso(graph(TRIPLES), GSP.service(gsp).graphName(g).accept(fmt.getLang()).GET(), "GET after PUT");
                GSP.service(gsp).graphName(g).contentType(fmt).POST(graph(OTHER));
                Graph both = graph(TRIPLES);
                graph(OTHER).find().forEach(both::add);
                expectIso(both, GSP.service(gsp).graphName(g).GET(), "GET after POST");
                GSP.service(gsp).graphName(g).DELETE();
                try {
                    GSP.service(gsp).graphName(g).GET();
                    throw new AssertionError("GET of a deleted graph should be 404");
                } catch (org.apache.jena.atlas.web.HttpException ex) {
                    expectEq(404, ex.getStatusCode(), "status of a deleted graph");
                }
            });
        }
        check("gsp: files with every extension (GSP.PUT(file))", () -> {
            Path dir = Files.createTempDirectory("jena-clients");
            for ( Lang lang : new Lang[] { Lang.TURTLE, Lang.NTRIPLES, Lang.RDFXML, Lang.JSONLD, Lang.RDFTHRIFT, Lang.N3, Lang.TRIX } ) {
                Path file = dir.resolve("data." + lang.getFileExtensions().get(0));
                RDFDataMgr.write(Files.newOutputStream(file), graph(TRIPLES), lang);
                GSP.service(gsp).graphName("http://example.org/file").PUT(file.toString());
                expectIso(graph(TRIPLES), GSP.service(gsp).graphName("http://example.org/file").GET(), "PUT " + file.getFileName());
            }
            GSP.service(gsp).graphName("http://example.org/file").DELETE();
        });
        check("gsp: GET of a missing named graph is 404", () -> {
            Resp r = get("/jc/data?graph=" + enc("http://example.org/nothing"));
            expectStatus(404, r, "missing graph");
        });
        check("gsp: HEAD of the default graph", () -> {
            Resp r = send("HEAD", "/jc/data?default", null, null, "Accept", "text/turtle");
            expectStatus(200, r, "HEAD");
        });
        check("gsp: an unsupported content type is 415", () -> {
            Resp r = send("PUT", "/jc/data?default", "application/x-unknown", utf8("x"));
            expectStatus(415, r, "unknown content type");
        });
        check("gsp: a parse error is 400", () -> {
            Resp r = send("PUT", "/jc/data?graph=" + enc("http://example.org/bad"), "text/turtle", utf8("<a> <b> ."));
            expectStatus(400, r, "bad Turtle");
        });
        check("gsp: ?graph=default and the read-only /get endpoint", () -> {
            GSP.service(gsp).defaultGraph().PUT(graph(TRIPLES));
            Resp r = get("/jc/data?graph=default", "Accept", "application/n-triples");
            expectStatus(200, r, "graph=default");
            Graph g = GraphFactory.createDefaultGraph();
            RDFParser.fromString(r.body(), Lang.NT).parse(g);
            expectIso(graph(TRIPLES), g, "graph=default");
            expectIso(graph(TRIPLES), GSP.service(ds + "/get").defaultGraph().GET(), "/get");
        });
        check("gsp: on the dataset URL (/jc?graph=)", () -> {
            GSP.service(ds).graphName("http://example.org/viaroot").PUT(graph(OTHER));
            expectIso(graph(OTHER), GSP.service(ds).graphName("http://example.org/viaroot").GET(), "GET");
            GSP.service(ds).graphName("http://example.org/viaroot").DELETE();
        });
    }

    static void datasetProtocol(String ds) {
        String gsp = ds + "/data";
        RDFFormat[] quadFormats = { RDFFormat.TRIG, RDFFormat.NQUADS, RDFFormat.JSONLD, RDFFormat.RDF_THRIFT, RDFFormat.RDF_PROTO, RDFFormat.TRIX };
        for ( RDFFormat fmt : quadFormats ) {
            check("dsp: PUT/GET/POST/clear as " + fmt, () -> {
                DSP.service(gsp).contentType(fmt).PUT(dataset(QUADS));
                expectIso(dataset(QUADS), DSP.service(gsp).accept(fmt.getLang()).GET(), "GET after PUT");
                DSP.service(gsp).contentType(fmt).POST(dataset("PREFIX ex: <http://example.org/> ex:g3 { ex:c ex:p ex:d }"));
                DatasetGraph after = DSP.service(gsp).GET();
                expect(after.containsGraph(NodeFactory.createURI("http://example.org/g3")), "g3 added");
                DSP.service(gsp).clear();
                expect(DSP.service(gsp).GET().isEmpty(), "empty after clear");
            });
        }
        check("gsp: ?graph=union reads the union of the named graphs", () -> {
            DSP.service(gsp).PUT(dataset(QUADS + "PREFIX ex: <http://example.org/> ex:g3 { ex:a ex:p \"in g1\" }"));
            Graph union = GSP.service(gsp).graphName("union").GET();
            Graph expected = GraphFactory.createDefaultGraph();
            dataset(QUADS).find(null, null, null, null).forEachRemaining(q -> {
                if ( !q.isDefaultGraph() )
                    expected.add(q.asTriple());
            });
            expectIso(expected, union, "union graph");
            Resp put = send("PUT", "/jc/data?graph=union", "text/turtle", utf8(OTHER));
            expectStatus(400, put, "PUT to the union graph");
            DSP.service(gsp).clear();
        });
        check("dsp: GSP on the dataset URL (/jc)", () -> {
            DSP.service(ds).PUT(dataset(QUADS));
            expectIso(dataset(QUADS), DSP.service(ds).GET(), "GET /jc");
            DSP.service(ds).clear();
        });
        check("gsp: GET with no graph parameter returns the dataset", () -> {
            DSP.service(gsp).PUT(dataset(QUADS));
            Resp r = get("/jc/data", "Accept", "application/n-quads");
            expectStatus(200, r, "dataset GET");
            DatasetGraph dsg = DatasetGraphFactory.createTxnMem();
            RDFParser.fromString(r.body(), Lang.NQUADS).parse(dsg);
            expectIso(dataset(QUADS), dsg, "dataset");
            DSP.service(gsp).clear();
        });
    }

    // ------------------------------------------------------------ connections ----

    static void connections(String ds) {
        check("RDFConnectionRemote: put, fetch, load, delete, query, update", () -> {
            try ( RDFConnection c = RDFConnectionRemote.service(ds).build() ) {
                exerciseConnection(c);
            }
        });
        check("RDFConnectionFuseki: put, fetch, load, delete, query, update (RDF Thrift)", () -> {
            try ( RDFConnection c = RDFConnectionFuseki.service(ds).build() ) {
                exerciseConnection(c);
            }
        });
        check("RDFConnectionRemote with separate endpoints", () -> {
            try ( RDFConnection c = RDFConnectionRemote.newBuilder().destination(ds)
                    .queryEndpoint("query").updateEndpoint("update").gspEndpoint("data").build() ) {
                exerciseConnection(c);
            }
        });
        check("RDFConnectionFuseki: queryResultSet and a SELECT with every term kind", () -> {
            try ( RDFConnection c = RDFConnectionFuseki.service(ds).build() ) {
                c.put(ModelFactory.createModelForGraph(graph(TRIPLES)));
                c.queryResultSet("SELECT * { ?s ?p ?o }", rs -> {
                    int n = ResultSetFormatter.consume(rs);
                    expectEq(graph(TRIPLES).size(), n, "rows");
                });
                c.querySelect("PREFIX ex: <http://example.org/> SELECT ?v { ex:alice ex:age ?v }",
                              row -> expectEq(42, row.getLiteral("v").getInt(), "age"));
                c.delete();
            }
        });
        check("RDFConnectionFuseki: a transaction (txn wrappers) and loadDataset", () -> {
            try ( RDFConnection c = RDFConnectionFuseki.service(ds).build() ) {
                Dataset d = DatasetFactory.wrap(dataset(QUADS));
                c.putDataset(d);
                c.executeRead(() -> expectIso(dataset(QUADS), c.fetchDataset().asDatasetGraph(), "fetchDataset"));
                c.update("CLEAR ALL");
                expect(c.fetchDataset().asDatasetGraph().isEmpty(), "cleared");
            }
        });
    }

    static void exerciseConnection(RDFConnection c) throws Exception {
        Model m = ModelFactory.createModelForGraph(graph(TRIPLES));
        c.put(m);
        expectIso(m.getGraph(), c.fetch().getGraph(), "fetch default");
        c.put("http://example.org/conn", ModelFactory.createModelForGraph(graph(OTHER)));
        expectIso(graph(OTHER), c.fetch("http://example.org/conn").getGraph(), "fetch named");
        c.load("http://example.org/conn", ModelFactory.createModelForGraph(graph(TRIPLES)));
        expectEq(graph(TRIPLES).size() + graph(OTHER).size(), (int) c.fetch("http://example.org/conn").size(), "load adds");
        Path dir = Files.createTempDirectory("jena-clients");
        Path file = dir.resolve("data.ttl");
        Files.writeString(file, OTHER);
        c.load(file.toString());
        expect(c.queryAsk("PREFIX ex: <http://example.org/> ASK { ex:carol ex:age 7 }"), "loaded from a file");
        c.update("PREFIX ex: <http://example.org/> INSERT DATA { ex:erin ex:name \"Erin\" }");
        try ( QueryExecution qe = c.query("PREFIX ex: <http://example.org/> SELECT ?n { ex:erin ex:name ?n }") ) {
            ResultSet rs = qe.execSelect();
            expectEq("Erin", rs.next().getLiteral("n").getString(), "updated");
        }
        Model built = c.queryConstruct("CONSTRUCT { ?s ?p ?o } WHERE { GRAPH <http://example.org/conn> { ?s ?p ?o } }");
        expectEq((long) graph(TRIPLES).size() + graph(OTHER).size(), built.size(), "CONSTRUCT size");
        c.delete("http://example.org/conn");
        c.delete();
        expectEq(0L, c.fetch().size(), "default graph deleted");
        Dataset d = c.fetchDataset();
        expect(!d.containsNamedModel("http://example.org/conn"), "named graph deleted");
    }

    // ------------------------------------------------------------ compression ----

    static byte[] gzip(String s) throws Exception {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        try ( GZIPOutputStream z = new GZIPOutputStream(out) ) {
            z.write(utf8(s));
        }
        return out.toByteArray();
    }

    static void compression(String ds) {
        check("gzip: responses to Jena clients that accept gzip", () -> {
            GSP.service(ds + "/data").defaultGraph().PUT(graph(TRIPLES));
            Graph g = GSP.service(ds + "/data").defaultGraph().httpHeader("Accept-Encoding", "gzip").GET();
            expectIso(graph(TRIPLES), g, "gzip GSP GET");
            try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql").query(SELECT)
                    .httpHeader("Accept-Encoding", "gzip").build() ) {
                expectEq(2, count(qe.select()), "gzip SELECT");
            }
            Resp r = get("/jc/data?default", "Accept-Encoding", "gzip", "Accept", "text/turtle");
            expectEq("gzip", r.header("Content-Encoding"), "Content-Encoding");
        });
        check("gzip: compressed request bodies (GSP PUT and an update)", () -> {
            Resp r = send("PUT", "/jc/data?graph=" + enc("http://example.org/gz"), "text/turtle", gzip(OTHER),
                          "Content-Encoding", "gzip");
            expect2xx(r, "gzip PUT");
            expectIso(graph(OTHER), GSP.service(ds + "/data").graphName("http://example.org/gz").GET(), "gzip PUT content");
            Resp u = send("POST", "/jc/update", WebContent.contentTypeSPARQLUpdate,
                          gzip("DROP GRAPH <http://example.org/gz>"), "Content-Encoding", "gzip");
            expect2xx(u, "gzip update");
            expectStatus(404, get("/jc/data?graph=" + enc("http://example.org/gz")), "dropped");
        });
        check("upload: multipart form (Fuseki's /upload)", () -> {
            String boundary = "----jena-clients";
            String body = "--" + boundary + "\r\n"
                + "Content-Disposition: form-data; name=\"files[]\"; filename=\"data.ttl\"\r\n"
                + "Content-Type: text/turtle\r\n\r\n" + OTHER + "\r\n--" + boundary + "--\r\n";
            Resp r = send("POST", "/jc/upload?graph=" + enc("http://example.org/up"),
                          "multipart/form-data; boundary=" + boundary, utf8(body));
            expect2xx(r, "upload");
            expectIso(graph(OTHER), GSP.service(ds + "/data").graphName("http://example.org/up").GET(), "uploaded");
            GSP.service(ds + "/data").graphName("http://example.org/up").DELETE();
            GSP.service(ds + "/data").defaultGraph().DELETE();
        });
    }

    // ----------------------------------------------------------- direct naming ----

    static void directNaming(String ds) {
        check("gsp direct naming: PUT/GET/POST/DELETE /jc/graphs/one", () -> {
            String g = ds + "/graphs/one";
            GSP.service(ds).directGraphName(g).PUT(graph(TRIPLES));
            expectIso(graph(TRIPLES), GSP.service(ds).directGraphName(g).GET(), "GET after PUT");
            try ( QueryExec qe = QueryExecHTTP.service(ds + "/sparql")
                    .query("ASK { GRAPH <" + g + "> { ?s ?p ?o } }").build() ) {
                expect(qe.ask(), "the graph is named by the request URL");
            }
            GSP.service(ds).directGraphName(g).POST(graph(OTHER));
            expectEq(graph(TRIPLES).size() + graph(OTHER).size(), GSP.service(ds).directGraphName(g).GET().size(), "POST adds");
            GSP.service(ds).directGraphName(g).DELETE();
            Resp r = get(g);
            expectStatus(404, r, "deleted");
        });
        check("gsp direct naming: a query string is not a direct name", () -> {
            Resp r = get("/jc/graphs/one?x=1");
            expect(r.status() == 400 || r.status() == 404, "query string: " + r.status());
        });
    }

    // ------------------------------------------------------------------- SHACL ----

    static final String SHAPES = """
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
            sh:property [ sh:path ex:age ; sh:minCount 1 ; sh:datatype xsd:integer ] .
        """;

    static void shacl(String ds) {
        check("shacl: POST /jc/shacl?graph=default", () -> {
            GSP.service(ds + "/data").defaultGraph().PUT(graph(TRIPLES));
            Resp r = send("POST", "/jc/shacl?graph=default", "text/turtle", utf8(SHAPES), "Accept", "text/turtle");
            expectStatus(200, r, "shacl");
            Graph report = graph(r.body());
            expect(r.body().contains("conforms") && r.body().contains("false"), "bob has no age: " + r.body());
            expect(report.size() > 0, "a report");
        });
        check("shacl: ?target= validates one node (prefixed name)", () -> {
            Resp r = send("POST", "/jc/shacl?graph=default&target=" + enc("http://example.org/alice"), "text/turtle",
                          utf8(SHAPES), "Accept", "application/n-triples");
            expectStatus(200, r, "shacl target alice");
            expect(r.body().contains("<http://www.w3.org/ns/shacl#conforms> \"true\""), "alice conforms: " + r.body());
            Resp b = send("POST", "/jc/shacl?graph=default&target=" + enc("http://example.org/bob"), "text/turtle",
                          utf8(SHAPES), "Accept", "application/n-triples");
            expect(b.body().contains("<http://www.w3.org/ns/shacl#conforms> \"false\""), "bob does not: " + b.body());
            send("PUT", "/jc/prefixes?prefix=ex&uri=" + enc("http://example.org/"), null, null);
            Resp p = send("POST", "/jc/shacl?graph=default&target=ex:bob", "text/turtle",
                          utf8(SHAPES), "Accept", "application/n-triples");
            expectStatus(200, p, "prefixed target");
            expect(p.body().contains("\"false\""), "ex:bob does not conform: " + p.body());
            GSP.service(ds + "/data").defaultGraph().DELETE();
        });
    }

    // -------------------------------------------------------------- validators ----

    static void validators() {
        check("validate: /$/validate/query (JSON)", () -> {
            Resp ok = send("POST", "/$/validate/query", WebContent.contentTypeHTMLForm,
                           utf8("query=" + enc("SELECT * { ?s ?p ?o }")), "Accept", "application/json");
            expectStatus(200, ok, "valid query");
            JsonObject o = ok.json();
            expect(o.hasKey("input") && o.hasKey("formatted") && o.hasKey("algebra"), "fields: " + o);
            expect(!o.hasKey("errors"), "no errors");
            Resp bad = get("/$/validate/query?query=" + enc("SELECT * {") , "Accept", "application/json");
            expectStatus(200, bad, "invalid query");
            JsonObject e = bad.json().get("errors").getAsArray().get(0).getAsObject();
            expect(e.hasKey("parse-error") && e.hasKey("parse-error-line") && e.hasKey("parse-error-column"), "error: " + e);
        });
        check("validate: /$/validate/update (JSON)", () -> {
            Resp ok = get("/$/validate/update?update=" + enc("INSERT DATA { <a:a> <a:b> <a:c> }"), "Accept", "application/json");
            expectStatus(200, ok, "valid update");
            expect(ok.json().hasKey("formatted"), "formatted");
            Resp bad = get("/$/validate/update?update=" + enc("INSERT DATA {"), "Accept", "application/json");
            expect(bad.json().hasKey("errors"), "errors");
        });
        check("validate: /$/validate/iri (JSON)", () -> {
            Resp r = get("/$/validate/iri?iri=" + enc("http://example.org/ok") + "&iri=" + enc("http://exa mple/bad")
                         + "&iri=" + enc("relative"), "Accept", "application/json");
            expectStatus(200, r, "iri");
            JsonArray a = r.json().get("iris").getAsArray();
            expectEq(3, a.size(), "three IRIs");
            expectEq(0, a.get(0).getAsObject().get("errors").getAsArray().size(), "ok IRI");
            expect(a.get(1).getAsObject().get("errors").getAsArray().size() > 0, "bad IRI");
            expect(a.get(2).getAsObject().get("warning").getAsArray().size() > 0, "relative IRI warns");
        });
        check("validate: /$/validate/data (JSON)", () -> {
            Resp ok = postFormJson("/$/validate/data", "languageSyntax=Turtle&data=" + enc(TRIPLES));
            expectStatus(200, ok, "valid data");
            expect(!ok.json().hasKey("errors"), "no errors: " + ok.body());
            Resp bad = postFormJson("/$/validate/data", "languageSyntax=N-Triples&data=" + enc("<a> <b> ."));
            JsonObject e = bad.json().get("errors").getAsArray().get(0).getAsObject();
            expect(e.hasKey("parse-error"), "error: " + e);
        });
        check("validate: /$/validate/langtag and HTML output", () -> {
            Resp html = get("/$/validate/langtag?langtag=en-US");
            expectStatus(200, html, "langtag");
            expect(html.header("Content-Type").startsWith("text/html"), "HTML: " + html.header("Content-Type"));
            Resp q = get("/$/validate/query?query=" + enc("SELECT * {?s ?p ?o}"));
            expect(q.header("Content-Type").startsWith("text/html"), "HTML by default");
        });
    }

    static Resp postFormJson(String path, String form) throws Exception {
        return send("POST", path, WebContent.contentTypeHTMLForm, utf8(form), "Accept", "application/json");
    }

    // keep imports of classes used only in some Jena versions
    static final Class<?>[] USED = { ByteArrayInputStream.class, StringReader.class, Map.class, Supplier.class,
        Node.class, RDFLanguages.class, RDFWriter.class, ResultSetLang.class, HttpOp.class };
}
