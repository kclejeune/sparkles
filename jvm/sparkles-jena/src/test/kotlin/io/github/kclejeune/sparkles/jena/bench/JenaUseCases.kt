package io.github.kclejeune.sparkles.jena.bench

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.Sparkles
import io.github.kclejeune.sparkles.jena.SparklesDatasets
import org.apache.jena.fuseki.main.FusekiServer
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.Triple
import org.apache.jena.query.ARQ
import org.apache.jena.query.Dataset
import org.apache.jena.query.DatasetFactory
import org.apache.jena.query.ParameterizedSparqlString
import org.apache.jena.query.Query
import org.apache.jena.query.QueryExecution
import org.apache.jena.query.QueryFactory
import org.apache.jena.rdf.model.InfModel
import org.apache.jena.rdf.model.Model
import org.apache.jena.rdf.model.ModelFactory
import org.apache.jena.rdf.model.Property
import org.apache.jena.rdf.model.RDFNode
import org.apache.jena.rdf.model.Resource
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.core.DatasetGraphFactory
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.engine.binding.BindingFactory
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.exec.QueryExecDatasetBuilder
import org.apache.jena.sparql.exec.UpdateExec
import org.apache.jena.sparql.exec.http.QueryExecHTTP
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.apache.jena.system.progress.MonitorOutput
import org.apache.jena.tdb2.DatabaseMgr
import org.apache.jena.tdb2.loader.LoaderFactory
import org.apache.jena.tdb2.sys.TDBInternal
import org.apache.jena.vocabulary.RDF
import java.nio.file.Files
import java.nio.file.Path
import java.util.Comparator
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlin.system.exitProcess

/**
 * One process of the realistic Jena comparison of `scripts/bench-bindings/jena.py`: the
 * work an application does through Jena's Model, Graph, SPARQL, update, inference, loading
 * and Fuseki APIs, on Sparkles (`DatasetGraphSparkles` in memory or on disk), on TDB2 on
 * disk and on Jena's transactional in-memory dataset (TIM), through the same calls.
 *
 * The only argument is a JSON configuration written by the driver. The process reports on
 * standard output as JSON lines: `versions`, `ready`, then for each case either a `check`
 * line (mode `check`: an order-independent fingerprint of the case's answers over a fixed
 * set of operations) or one `case` line per thread count (mode `time`: operations run back
 * to back for a warm-up period and a measured one, with the throughput and the median and
 * 99th percentile latency). Mode `load` loads the data into a store on disk.
 *
 * Each operation is one unit of an application's work, in its own transaction as an
 * application would run it. Reads run in a read transaction; writes run in a write
 * transaction each. Every result is consumed, every row and term.
 */
internal object JenaUseCases {
    private val out = System.out

    @Volatile
    private var sinkValue = 0L

    private fun emit(o: JsonObject) {
        synchronized(out) {
            out.println(o.toString())
            out.flush()
        }
    }

    private fun event(name: String, vararg kv: Pair<String, Any?>): JsonObject {
        val o = JsonObject()
        o.addProperty("event", name)
        for ((k, v) in kv) {
            when (v) {
                null -> o.add(k, com.google.gson.JsonNull.INSTANCE)
                is Number -> o.addProperty(k, v)
                is Boolean -> o.addProperty(k, v)
                is JsonObject -> o.add(k, v)
                else -> o.addProperty(k, v.toString())
            }
        }
        return o
    }

    private const val EX = "http://example.org/"
    private const val FOAF = "http://xmlns.com/foaf/0.1/"
    private const val PREFIXES = "PREFIX ex: <$EX> PREFIX foaf: <$FOAF> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> "

    private class Config(json: JsonObject) {
        val engine: String = json["engine"].asString
        val mode: String = json["mode"].asString
        val data: String = json["data"].asString
        val smallData: String? = json.get("small_data")?.takeIf { !it.isJsonNull }?.asString
        val store: String? = json.get("store")?.takeIf { !it.isJsonNull }?.asString
        val scratch: String? = json.get("scratch")?.takeIf { !it.isJsonNull }?.asString
        val cases: List<String> = json.getAsJsonArray("cases")?.map { it.asString } ?: emptyList()
        val threads: List<Int> = json.getAsJsonArray("threads")?.map { it.asInt } ?: listOf(1, 4)
        val seconds: Double = json["seconds"]?.asDouble ?: 5.0
        val warmupSeconds: Double = json["warmup_seconds"]?.asDouble ?: 2.0
        val people: Int = json["people"].asInt
        val orgs: Int = json["orgs"].asInt
        val checkOps: Int = json["check_ops"]?.asInt ?: 64
        val probes: List<List<String>> = json.getAsJsonArray("probes").map { p -> p.asJsonArray.map { it.asString } }
        val tag: String = json["tag"]?.asString ?: "r"
    }

    private val quiet = MonitorOutput { _, _ -> }

    private fun isSparkles(engine: String) = engine.startsWith("sparkles")

    private fun load(cfg: Config): DatasetGraph {
        val data = Path.of(cfg.data)
        return when (cfg.engine) {
            "sparkles" -> SparklesDatasets.open(Path.of(cfg.store!!)).also { it.loadFiles(listOf(data)) }
            "sparkles-mem" -> SparklesDatasets.memory().also { it.loadFiles(listOf(data)) }
            "tdb2" -> DatabaseMgr.connectDatasetGraph(cfg.store!!).also { dsg ->
                val loader = LoaderFactory.createLoader(dsg, quiet)
                loader.startBulk()
                try {
                    loader.load(data.toString())
                    loader.finishBulk()
                } catch (e: Exception) {
                    loader.finishException(e)
                    throw e
                }
            }
            "tim" -> DatasetGraphFactory.createTxnMem().also { dsg -> Txn.executeWrite(dsg) { RDFDataMgr.read(dsg, cfg.data) } }
            else -> error("unknown engine ${cfg.engine}")
        }
    }

    private fun open(cfg: Config): DatasetGraph = when (cfg.engine) {
        "sparkles" -> SparklesDatasets.open(Path.of(cfg.store!!))
        "tdb2" -> DatabaseMgr.connectDatasetGraph(cfg.store!!)
        else -> load(cfg)
    }

    // ---------------------------------------------------------------- consumption

    /**
     * What an operation's results add up to. Timing only touches each term; a check sums a
     * hash of each row's terms, so the total does not depend on the order of the rows.
     */
    private class Acc(val check: Boolean) {
        var v = 0L

        fun row(vararg ns: Node?) {
            if (!check) {
                for (n in ns) if (n != null) v += touch(n)
                return
            }
            var h = -0x340d631b7bdddcdbL
            for (n in ns) {
                for (c in canon(n)) {
                    h = h xor c.code.toLong()
                    h *= 0x100000001b3L
                }
                h = h xor 0x1f
                h *= 0x100000001b3L
            }
            v += h
        }

        fun rdf(vararg ns: RDFNode?) = row(*Array(ns.size) { ns[it]?.asNode() })

        private fun touch(n: Node): Int = when {
            n.isURI -> n.uri.length
            n.isBlank -> 1
            n.isLiteral -> n.literalLexicalForm.length
            else -> 1
        }

        /** Blank nodes have no common label across engines; literals compare by term. */
        private fun canon(n: Node?): String = when {
            n == null -> "-"
            n.isURI -> "<${n.uri}>"
            n.isBlank -> "_:"
            n.isLiteral -> "\"${n.literalLexicalForm}\"@${n.literalLanguage}^^${n.literalDatatypeURI}"
            else -> n.toString()
        }
    }

    // ------------------------------------------------------------------ the cases

    private class Env(val cfg: Config, val dsg: DatasetGraph) {
        val dataset: Dataset = DatasetFactory.wrap(dsg)
        val graph = dsg.defaultGraph
        val model: Model = ModelFactory.createModelForGraph(graph)
        val sparkles = isSparkles(cfg.engine)
        val noCache: Context = Context().set(Sparkles.NO_CACHE, true)

        fun p(local: String): Property = model.createProperty(local)
        val name = p(FOAF + "name")
        val age = p(FOAF + "age")
        val knows = p(FOAF + "knows")
        val worksFor = p(EX + "worksFor")
        val score = p(EX + "score")
        val researcher: Resource = model.createResource(EX + "Researcher")
        val manager: Resource = model.createResource(EX + "Manager")
        val personClass: Resource = model.createResource(EX + "Person")

        /** People spread over the data, and every organization. */
        val people: List<Resource> = (0 until 1000).map { model.createResource("${EX}person/${(it.toLong() * cfg.people / 1000).toInt()}") }
        val orgs: List<Resource> = (0 until cfg.orgs).map { model.createResource("${EX}org/$it") }
        val probes = cfg.probes.map { (s, p, o) -> Triple.create(NodeFactory.createURI(s), NodeFactory.createURI(p), NodeFactory.createURI(o)) }

        fun person(i: Int) = people[Math.floorMod(i, people.size)]
        fun org(i: Int) = orgs[Math.floorMod(i, orgs.size)]

        /** Jena's RDFS reasoner over the dataset's default graph; the schema is in the data. */
        val inf: InfModel by lazy {
            Txn.calculateRead(dsg) { ModelFactory.createRDFSModel(model).also { it.prepare() } }
        }

        val pssTemplate = "${PREFIXES}SELECT ?n ?a WHERE { ?s foaf:name ?n ; foaf:age ?a }"
        val prepared: Query = QueryFactory.create(pssTemplate)
        val sVar: Var = Var.alloc("s")

        var fuseki: FusekiServer? = null
        val fusekiUrl: String by lazy {
            val server = FusekiServer.create().port(0).loopback(true).add("/ds", dsg).build()
            server.start()
            fuseki = server
            "http://localhost:${server.httpPort}/ds"
        }

        val writes = AtomicInteger()
    }

    private fun Env.select(q: String, acc: Acc, cache: Boolean = false) {
        val b = QueryExec.dataset(dsg).query(q)
        if (sparkles && !cache) b.context(noCache)
        b.build().use { qe ->
            val rs = qe.select()
            val vars = rs.resultVars.map { Var.alloc(it) }
            while (rs.hasNext()) {
                val row = rs.next()
                acc.row(*Array(vars.size) { row.get(vars[it]) })
            }
        }
    }

    private fun Env.triples(g: org.apache.jena.graph.Graph, acc: Acc) {
        val it = g.find()
        try {
            while (it.hasNext()) {
                val t = it.next()
                acc.row(t.subject, t.predicate, t.`object`)
            }
        } finally {
            it.close()
        }
    }

    private fun Env.read(f: () -> Unit) = Txn.executeRead(dsg) { f() }

    private fun Env.write(f: () -> Unit) = Txn.executeWrite(dsg) { f() }

    /** Cases that write, which run after the reads and are not checked. */
    private val WRITES = setOf("update-insert-data", "update-modify", "model-add-txn", "model-update-txn", "bulk-load")

    /** Cases that run on one thread only. */
    private val ONE_THREAD = setOf("bulk-load")

    /** Runs operation `i` of case `name` and adds its results to `acc`. */
    private fun Env.op(case: String, i: Int, acc: Acc, thread: Int) {
        when (case) {
            // ---- Graph and Model API
            "graph-contains" -> read {
                val t = probes[Math.floorMod(i, probes.size)]
                acc.row(NodeFactory.createLiteralString(graph.contains(t).toString()))
            }
            "model-getProperty" -> read { acc.rdf(person(i).getProperty(name)?.`object`) }
            "model-listProperties" -> read {
                val it = person(i).listProperties()
                try { while (it.hasNext()) { val s = it.next(); acc.rdf(s.predicate, s.`object`) } } finally { it.close() }
            }
            "model-listStatements-sp" -> read {
                val it = model.listStatements(person(i), knows, null as RDFNode?)
                try { while (it.hasNext()) acc.rdf(it.next().`object`) } finally { it.close() }
            }
            "model-listSubjectsWithProperty" -> read {
                val it = model.listSubjectsWithProperty(worksFor, org(i))
                try { while (it.hasNext()) acc.rdf(it.next()) } finally { it.close() }
            }
            "model-listResourcesOfType" -> read {
                val it = model.listResourcesWithProperty(RDF.type, if (i % 2 == 0) manager else researcher)
                try { while (it.hasNext()) acc.rdf(it.next()) } finally { it.close() }
            }
            "model-property-chain" -> read {
                val p = person(i)
                val org = p.getProperty(worksFor)?.resource
                acc.rdf(p.getProperty(name)?.`object`, org, org?.getProperty(name)?.`object`)
            }
            "model-navigate" -> read {
                val p = person(i)
                acc.row(NodeFactory.createLiteralString(p.hasProperty(RDF.type, researcher).toString()))
                val it = p.listProperties(knows)
                try {
                    while (it.hasNext()) {
                        val f = it.next().resource
                        acc.rdf(f, f.getProperty(name)?.`object`, f.getProperty(age)?.`object`)
                    }
                } finally {
                    it.close()
                }
            }
            "model-iterate-all" -> read { triples(graph, acc) }
            // ---- inference
            "rdfs-hasType" -> read { acc.row(NodeFactory.createLiteralString(inf.contains(person(i), RDF.type, personClass).toString())) }
            "rdfs-listTypes" -> read {
                val it = inf.listObjectsOfProperty(person(i), RDF.type)
                try { while (it.hasNext()) acc.rdf(it.next()) } finally { it.close() }
            }
            // ---- SPARQL
            "sparql-ask" -> read {
                val t = probes[Math.floorMod(i, probes.size)]
                val b = QueryExec.dataset(dsg).query("ASK { <${t.subject.uri}> <${t.predicate.uri}> <${t.`object`.uri}> }")
                if (sparkles) b.context(noCache)
                b.build().use { qe -> acc.row(NodeFactory.createLiteralString(qe.ask().toString())) }
            }
            "sparql-select-o" -> read { select("SELECT ?o WHERE { <${person(i).uri}> <${FOAF}name> ?o }", acc) }
            "sparql-select-po" -> read { select("SELECT ?p ?o WHERE { <${person(i).uri}> ?p ?o }", acc) }
            "sparql-friends" -> read {
                select("${PREFIXES}SELECT ?f ?n WHERE { <${person(i).uri}> foaf:knows ?f . ?f foaf:name ?n }", acc)
            }
            "sparql-star-lookup" -> read {
                select("${PREFIXES}SELECT ?p ?n ?a ?s WHERE { ?p ex:worksFor <${org(i).uri}> ; foaf:name ?n ; foaf:age ?a ; ex:salary ?s }", acc)
            }
            "sparql-values-star" -> read {
                val vs = (0 until 5).joinToString(" ") { "<${person(i * 5 + it).uri}>" }
                select("${PREFIXES}SELECT ?p ?n ?a ?k WHERE { VALUES ?p { $vs } ?p foaf:name ?n ; foaf:age ?a ; foaf:knows ?k }", acc)
            }
            "sparql-filter" -> read {
                select("${PREFIXES}SELECT ?p ?a WHERE { ?p ex:worksFor <${org(i).uri}> ; foaf:age ?a FILTER(?a > 50) }", acc)
            }
            "sparql-count" -> read {
                select("${PREFIXES}SELECT (COUNT(?p) AS ?c) WHERE { ?p ex:worksFor <${org(i).uri}> }", acc)
            }
            "sparql-repeated" -> read {
                // the same text every time, as a dashboard asks it; a result cache may answer
                select("${PREFIXES}SELECT ?t (COUNT(?p) AS ?c) WHERE { ?p a ?t } GROUP BY ?t", acc, cache = true)
            }
            "sparql-construct" -> read {
                val b = QueryExec.dataset(dsg).query("CONSTRUCT WHERE { <${person(i).uri}> ?p ?o }")
                if (sparkles) b.context(noCache)
                b.build().use { qe -> triples(qe.construct(), acc) }
            }
            "sparql-describe" -> read {
                val b = QueryExec.dataset(dsg).query("DESCRIBE <${person(i).uri}>")
                if (sparkles) b.context(noCache)
                b.build().use { qe -> triples(qe.describe(), acc) }
            }
            "sparql-pss" -> read {
                val pss = ParameterizedSparqlString(pssTemplate)
                pss.setIri("s", person(i).uri)
                val b = QueryExecution.dataset(dataset).query(pss.asQuery())
                if (sparkles) b.context(noCache)
                b.build().use { qe ->
                    val rs = qe.execSelect()
                    while (rs.hasNext()) { val r = rs.next(); acc.rdf(r["n"], r["a"]) }
                }
            }
            "sparql-substitution" -> read {
                val b = QueryExec.dataset(dsg).query(prepared).substitution(sVar, person(i).asNode())
                if (sparkles) b.context(noCache)
                b.build().use { qe ->
                    val rs = qe.select()
                    while (rs.hasNext()) { val r = rs.next(); acc.row(r.get("n"), r.get("a")) }
                }
            }
            "sparql-initial-binding" -> read {
                @Suppress("DEPRECATION")
                val b = QueryExecDatasetBuilder.create().dataset(dsg).query(prepared).initialBinding(BindingFactory.binding(sVar, person(i).asNode()))
                if (sparkles) b.context(noCache)
                b.build().use { qe ->
                    val rs = qe.select()
                    while (rs.hasNext()) { val r = rs.next(); acc.row(r.get("n"), r.get("a")) }
                }
            }
            "fuseki-select" -> {
                QueryExecHTTP.service(fusekiUrl).query("SELECT ?o WHERE { <${person(i).uri}> <${FOAF}name> ?o }").build().use { qe ->
                    val rs = qe.select()
                    while (rs.hasNext()) acc.row(rs.next().get("o"))
                }
            }
            "fuseki-star-lookup" -> {
                val q = "${PREFIXES}SELECT ?p ?n ?a ?s WHERE { ?p ex:worksFor <${org(i).uri}> ; foaf:name ?n ; foaf:age ?a ; ex:salary ?s }"
                QueryExecHTTP.service(fusekiUrl).query(q).build().use { qe ->
                    val rs = qe.select()
                    val vars = rs.resultVars.map { Var.alloc(it) }
                    while (rs.hasNext()) { val r = rs.next(); acc.row(*Array(vars.size) { r.get(vars[it]) }) }
                }
            }
            // ---- writes, each in its own write transaction
            "update-insert-data" -> {
                val n = writes.incrementAndGet()
                write {
                    UpdateExec.dataset(dsg)
                        .update("INSERT DATA { <${EX}bench/u/${cfg.tag}/$thread/$n> <${EX}p> \"v$n\" ; <${EX}q> $n }")
                        .execute()
                }
            }
            "update-modify" -> write {
                val s = "<${EX}bench/score/${cfg.tag}/${Math.floorMod(i, 1000)}>"
                UpdateExec.dataset(dsg)
                    .update("DELETE { $s <${EX}score> ?o } INSERT { $s <${EX}score> $i } WHERE { OPTIONAL { $s <${EX}score> ?o } }")
                    .execute()
            }
            "model-add-txn" -> {
                val n = writes.incrementAndGet()
                write {
                    val r = model.createResource("${EX}bench/m/${cfg.tag}/$thread/$n")
                    r.addProperty(RDF.type, researcher)
                    r.addProperty(name, "Bench $n")
                    r.addLiteral(age, (n % 60 + 20).toLong())
                    r.addProperty(worksFor, org(n))
                    r.addProperty(knows, person(n))
                }
            }
            "model-update-txn" -> write {
                // read-modify-write on one resource, as a counter or a status field is kept
                val r = model.createResource("${EX}bench/counter/${cfg.tag}/${Math.floorMod(i, 1000)}")
                val old = r.getProperty(score)?.long ?: 0L
                r.removeAll(score)
                r.addLiteral(score, old + 1)
                acc.v += old
            }
            "bulk-load" -> bulkLoad(acc)
            else -> error("unknown case $case")
        }
    }

    /** Reads the small data file with `RDFDataMgr` into a new, empty store of the engine. */
    private fun Env.bulkLoad(acc: Acc) {
        val file = cfg.smallData ?: error("no small_data for bulk-load")
        val dir = cfg.scratch?.let { Files.createTempDirectory(Path.of(it), "load") }
        val target: DatasetGraph = when (cfg.engine) {
            "sparkles" -> SparklesDatasets.open(dir!!)
            "sparkles-mem" -> SparklesDatasets.memory()
            "tdb2" -> DatabaseMgr.connectDatasetGraph(dir.toString())
            "tim" -> DatasetGraphFactory.createTxnMem()
            else -> error("unknown engine ${cfg.engine}")
        }
        try {
            Txn.executeWrite(target) { RDFDataMgr.read(target, file) }
            acc.v += Txn.calculateRead(target) { target.defaultGraph.size().toLong() }
        } finally {
            if (cfg.engine == "tdb2") TDBInternal.expel(target) else target.close()
            dir?.let { d -> Files.walk(d).sorted(Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    // ---------------------------------------------------------------- modes

    private fun check(env: Env) {
        for (c in env.cfg.cases) {
            if (c in WRITES) continue
            emit(event("start", "case" to c))
            val result = event("check", "case" to c)
            try {
                val acc = Acc(check = true)
                for (i in 0 until env.cfg.checkOps) env.op(c, i, acc, 0)
                result.addProperty("fingerprint", java.lang.Long.toHexString(acc.v))
                result.addProperty("status", "ok")
            } catch (e: Throwable) {
                result.addProperty("status", "error")
                result.addProperty("error", e.toString().take(300))
            }
            emit(result)
        }
    }

    private fun time(env: Env) {
        // reads first, so that the writes do not change what they read
        val order = env.cfg.cases.filter { it !in WRITES } + env.cfg.cases.filter { it in WRITES }
        for (c in order) {
            for (threads in env.cfg.threads) {
                if (threads > 1 && c in ONE_THREAD) continue
                emit(event("start", "case" to "$c@$threads"))
                val result = event("case", "case" to c, "threads" to threads)
                try {
                    // one operation untimed, so that lazy setup (the reasoner, Fuseki) is not timed
                    env.op(c, 0, Acc(false), 0)
                    for ((k, v) in runThreads(env, c, threads)) {
                        when (v) {
                            is Number -> result.addProperty(k, v)
                            else -> result.addProperty(k, v.toString())
                        }
                    }
                    result.addProperty("status", "ok")
                } catch (e: Throwable) {
                    result.addProperty("status", "error")
                    result.addProperty("error", e.toString().take(300))
                }
                emit(result)
            }
        }
    }

    private fun runThreads(env: Env, case: String, threads: Int): Map<String, Any> {
        val cfg = env.cfg
        val measuring = AtomicBoolean(false)
        val stop = AtomicBoolean(false)
        val lat = Array(threads) { LongArrayList() }
        val failure = AtomicReference<Throwable?>()
        val started = CountDownLatch(threads)
        val ts = (0 until threads).map { t ->
            Thread(null, {
                var i = 1 + t * 7919
                val acc = Acc(false)
                started.countDown()
                try {
                    while (!stop.get()) {
                        val s = System.nanoTime()
                        env.op(case, i++, acc, t)
                        val d = System.nanoTime() - s
                        if (measuring.get()) lat[t].add(d)
                    }
                } catch (e: Throwable) {
                    failure.compareAndSet(null, e)
                    stop.set(true)
                }
                sinkValue += acc.v
            }, "op-$t", 256L shl 20)
        }
        ts.forEach { it.start() }
        started.await()
        Thread.sleep((cfg.warmupSeconds * 1000).toLong())
        measuring.set(true)
        val t0 = System.nanoTime()
        Thread.sleep((cfg.seconds * 1000).toLong())
        measuring.set(false)
        val elapsed = (System.nanoTime() - t0) / 1e9
        stop.set(true)
        ts.forEach { it.join(600_000) }
        failure.get()?.let { throw it }
        val all = LongArrayList()
        for (l in lat) all.addAll(l)
        val sorted = all.sorted()
        fun pct(p: Double) = if (sorted.isEmpty()) Double.NaN else sorted[minOf(sorted.size - 1, (p * sorted.size).toInt())] / 1e3
        return mapOf(
            "ops" to sorted.size,
            "seconds" to elapsed,
            "ops_per_s" to sorted.size / elapsed,
            "p50_us" to pct(0.5),
            "p99_us" to pct(0.99),
            "mean_us" to if (sorted.isEmpty()) Double.NaN else sorted.sum() / 1e3 / sorted.size,
        )
    }

    private class LongArrayList {
        var a = LongArray(1024)
        var size = 0

        fun add(v: Long) {
            if (size == a.size) a = a.copyOf(size * 2)
            a[size++] = v
        }

        fun addAll(o: LongArrayList) {
            for (i in 0 until o.size) add(o.a[i])
        }

        fun sorted(): LongArray = a.copyOf(size).also { it.sort() }
    }

    @JvmStatic
    fun main(args: Array<String>) {
        val cfg = Config(JsonParser.parseString(Files.readString(Path.of(args[0]))).asJsonObject)
        val versions = JsonObject()
        versions.addProperty("java", System.getProperty("java.runtime.version"))
        versions.addProperty("jena", ARQ.VERSION)
        versions.addProperty("processors", Runtime.getRuntime().availableProcessors())
        versions.addProperty("max_heap_mib", Runtime.getRuntime().maxMemory() shr 20)
        emit(event("versions", "versions" to versions))
        var env: Env? = null
        var dsg: DatasetGraph? = null
        try {
            val t = System.nanoTime()
            if (cfg.mode == "load") {
                val d = load(cfg)
                val quads = Txn.calculateRead(d) { d.defaultGraph.size() }
                d.close()
                emit(event("loaded", "ms" to (System.nanoTime() - t) / 1e6, "triples" to quads))
            } else {
                dsg = open(cfg)
                val triples = Txn.calculateRead(dsg) {
                    if (dsg is DatasetGraphSparkles) dsg.headCommit().quads else dsg.defaultGraph.size().toLong()
                }
                emit(event("ready", "ms" to (System.nanoTime() - t) / 1e6, "triples" to triples))
                env = Env(cfg, dsg)
                when (cfg.mode) {
                    "check" -> check(env)
                    "time" -> time(env)
                    else -> error("unknown mode ${cfg.mode}")
                }
            }
        } catch (e: Throwable) {
            emit(event("fatal", "error" to e.toString().take(500)))
            e.printStackTrace()
            exitProcess(2)
        } finally {
            env?.fuseki?.stop()
            dsg?.close()
            emit(event("done", "sink" to sinkValue))
        }
        exitProcess(0)
    }
}
