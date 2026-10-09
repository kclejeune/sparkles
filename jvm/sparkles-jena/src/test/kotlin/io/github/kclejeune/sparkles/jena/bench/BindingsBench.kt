package io.github.kclejeune.sparkles.jena.bench

import com.google.gson.JsonArray
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.Sparkles
import io.github.kclejeune.sparkles.jena.SparklesDatasets
import org.apache.jena.graph.Graph
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.Triple
import org.apache.jena.query.ARQ
import org.apache.jena.rdf.model.ModelFactory
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.core.DatasetGraphFactory
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.apache.jena.system.progress.MonitorOutput
import org.apache.jena.tdb2.DatabaseMgr
import org.apache.jena.tdb2.loader.LoaderFactory
import java.io.BufferedWriter
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.Callable
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.system.exitProcess

/**
 * One process of the binding comparison of `scripts/bench-bindings/bench.py`: Jena's API
 * on Sparkles (`DatasetGraphSparkles`, on disk and in memory), on TDB2 on disk and on
 * Jena's transactional in-memory dataset (TIM), through the same Jena calls.
 *
 * The only argument is a JSON configuration written by the driver. The process reports
 * on standard output as JSON lines: `versions`, `ready` after opening or loading the
 * store, `start` and `tick` while a case runs (the driver's watchdog uses them), and one
 * `case` line per case. Modes:
 *
 * * `load` loads the data into a fresh store and reports the time;
 * * `answers` runs every case once and writes its rows for the answer check;
 * * `time` runs every case with warm-up and measured samples;
 * * `throughput` runs the small operations of P04 §5.4 back to back on several threads;
 * * `calls` counts the native calls per operation, which needs the counting copy of the
 *   generated bindings on the classpath (the `bindingsBenchClasspath` task writes it).
 */
internal object BindingsBench {
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
        for ((k, v) in kv) put(o, k, v)
        return o
    }

    private fun put(o: JsonObject, k: String, v: Any?) {
        when (v) {
            null -> o.add(k, com.google.gson.JsonNull.INSTANCE)
            is Number -> o.addProperty(k, v)
            is Boolean -> o.addProperty(k, v)
            is String -> o.addProperty(k, v)
            is JsonObject -> o.add(k, v)
            is JsonArray -> o.add(k, v)
            is List<*> -> {
                val a = JsonArray()
                for (x in v) {
                    when (x) {
                        is Number -> a.add(x)
                        else -> a.add(x.toString())
                    }
                }
                o.add(k, a)
            }
            else -> o.addProperty(k, v.toString())
        }
    }

    // ------------------------------------------------------------------ configuration

    private class Case(val name: String, val kind: String, val json: JsonObject) {
        val query: String? = json.get("query")?.asString
        val vars: List<String> = json.getAsJsonArray("vars")?.map { it.asString } ?: emptyList()
    }

    private class Config(val json: JsonObject) {
        val engine: String = json["engine"].asString
        val mode: String = json["mode"].asString
        val data: String = json["data"].asString
        val store: String? = json.get("store")?.takeIf { !it.isJsonNull }?.asString
        val warmup: Int = json["warmup"]?.asInt ?: 2
        val runs: Int = json["runs"]?.asInt ?: 10
        val budgetMs: Double = (json["budget_s"]?.asDouble ?: 120.0) * 1000
        val timeoutMs: Long = ((json["timeout_s"]?.asDouble ?: 300.0) * 1000).toLong()
        val answersDir: String? = json.get("answers_dir")?.takeIf { !it.isJsonNull }?.asString
        val skip: Set<String> = json.getAsJsonArray("skip")?.map { it.asString }?.toSet() ?: emptySet()
        val cases: List<Case> = json.getAsJsonArray("cases").map {
            val o = it.asJsonObject
            Case(o["name"].asString, o["kind"].asString, o)
        }
        val subjects: List<String> = json.getAsJsonArray("subjects").map { it.asString }
        val probes: List<List<String>> = json.getAsJsonArray("probes").map { p -> p.asJsonArray.map { it.asString } }
        val addCount: Int = json["add_count"]?.asInt ?: 10000
        val threads: List<Int> = json.getAsJsonArray("threads")?.map { it.asInt } ?: listOf(1)
        val seconds: Double = json["seconds"]?.asDouble ?: 10.0
        val warmupSeconds: Double = json["warmup_seconds"]?.asDouble ?: 3.0
    }

    // ------------------------------------------------------------------- the engines

    private const val RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
    private const val FOAF_NAME = "http://xmlns.com/foaf/0.1/name"
    private const val EX = "http://example.org/"
    private val quiet = MonitorOutput { _, _ -> }

    private fun isSparkles(engine: String) = engine.startsWith("sparkles")

    /** Loads the data into a new store of the engine, in `store` for the ones on disk. */
    private fun load(cfg: Config): DatasetGraph {
        val data = Path.of(cfg.data)
        return when (cfg.engine) {
            "sparkles" -> SparklesDatasets.open(Path.of(cfg.store!!)).also { it.loadFiles(listOf(data)) }
            "sparkles-mem" -> SparklesDatasets.memory().also { it.loadFiles(listOf(data)) }
            "tdb2" -> DatabaseMgr.connectDatasetGraph(cfg.store!!).also { dsg ->
                // the default loader of tdb2.tdbloader
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

    /** Opens the loaded store of an engine on disk, or loads an in-memory one. */
    private fun open(cfg: Config): DatasetGraph = when (cfg.engine) {
        "sparkles" -> SparklesDatasets.open(Path.of(cfg.store!!))
        "tdb2" -> DatabaseMgr.connectDatasetGraph(cfg.store!!)
        else -> load(cfg)
    }

    private fun size(dsg: DatasetGraph): Long = Txn.calculateRead(dsg) {
        if (dsg is DatasetGraphSparkles) dsg.headCommit().quads else dsg.defaultGraph.size().toLong()
    }

    // ------------------------------------------------------------------- consumption

    /** Receives the terms of a case: hashes them when timing, writes them for answers. */
    private class Sink(private val writer: BufferedWriter?) {
        var rows = 0L
        var acc = 0L

        fun row(terms: Array<Node?>) {
            rows++
            if (writer == null) {
                for (n in terms) {
                    if (n != null) acc += touch(n)
                }
                return
            }
            val sb = StringBuilder("[")
            for ((i, n) in terms.withIndex()) {
                if (i > 0) sb.append(',')
                term(sb, n)
            }
            writer.write(sb.append("]\n").toString())
        }

        private fun touch(n: Node): Int = when {
            n.isURI -> n.uri.length
            n.isBlank -> n.blankNodeLabel.length
            n.isLiteral -> n.literalLexicalForm.length + n.literalDatatypeURI.length + n.literalLanguage.length
            else -> 1
        }

        private fun str(sb: StringBuilder, s: String) {
            sb.append('"')
            for (c in s) {
                when {
                    c == '"' -> sb.append("\\\"")
                    c == '\\' -> sb.append("\\\\")
                    c < ' ' -> sb.append(String.format("\\u%04x", c.code))
                    else -> sb.append(c)
                }
            }
            sb.append('"')
        }

        private fun term(sb: StringBuilder, n: Node?) {
            when {
                n == null -> sb.append("null")
                n.isURI -> { sb.append("{\"type\":\"uri\",\"value\":"); str(sb, n.uri); sb.append('}') }
                n.isBlank -> { sb.append("{\"type\":\"bnode\",\"value\":"); str(sb, n.blankNodeLabel); sb.append('}') }
                n.isLiteral -> {
                    sb.append("{\"type\":\"literal\",\"value\":")
                    str(sb, n.literalLexicalForm)
                    if (n.literalLanguage.isNotEmpty()) {
                        sb.append(",\"xml:lang\":")
                        str(sb, n.literalLanguage)
                    } else {
                        sb.append(",\"datatype\":")
                        str(sb, n.literalDatatypeURI)
                    }
                    sb.append('}')
                }
                else -> { sb.append("{\"type\":\"other\",\"value\":"); str(sb, n.toString()); sb.append('}') }
            }
        }
    }

    private fun iri(s: String): Node = NodeFactory.createURI(s)

    private fun bool(b: Boolean): Node = NodeFactory.createLiteralDT(b.toString(), org.apache.jena.datatypes.xsd.XSDDatatype.XSDboolean)

    private class Ops(val cfg: Config, val dsg: DatasetGraph) {
        val graph: Graph = dsg.defaultGraph
        val model = ModelFactory.createModelForGraph(graph)
        val subjects = cfg.subjects.map(::iri)
        val probes = cfg.probes.map { (s, p, o) -> Triple.create(iri(s), iri(p), iri(o)) }
        val name: Node = iri(FOAF_NAME)
        val nameProp = model.createProperty(FOAF_NAME)
        val noCache: Context = Context().set(Sparkles.NO_CACHE, true)
        var addRound = 0

        /** The SELECT query through Jena's `QueryExec`, every row and term consumed. */
        fun query(text: String, vars: List<String>, sink: Sink, abort: AtomicReference<QueryExec?>) {
            val vs = vars.map { Var.alloc(it) }
            val b = QueryExec.dataset(dsg).query(text)
            if (isSparkles(cfg.engine)) b.context(noCache)
            b.build().use { qe ->
                abort.set(qe)
                if (qe.query.isAskType) {
                    sink.row(arrayOf(bool(qe.ask())))
                } else {
                    val rs = qe.select()
                    while (rs.hasNext()) {
                        val binding = rs.next()
                        sink.row(Array(vs.size) { binding.get(vs[it]) })
                    }
                }
                abort.set(null)
            }
        }

        fun triples(it: Iterator<Triple>, sink: Sink) {
            while (it.hasNext()) {
                val t = it.next()
                sink.row(arrayOf(t.subject, t.predicate, t.`object`))
            }
        }

        /** Runs one sample of a case; returns the number of operations it made. */
        fun run(c: Case, sink: Sink, abort: AtomicReference<QueryExec?>, ops: Int = Int.MAX_VALUE): Long {
            if (c.kind == "adds") {
                val round = addRound++
                val p = iri(EX + "bench/p")
                Txn.executeWrite(dsg) {
                    for (i in 0 until cfg.addCount) {
                        graph.add(Triple.create(iri("${EX}bench/add/${cfg.engine}/$round/$i"), p, NodeFactory.createLiteralString("v$i")))
                    }
                }
                sink.rows += cfg.addCount
                return cfg.addCount.toLong()
            }
            return Txn.calculateRead(dsg) {
                when (c.kind) {
                    "query" -> {
                        query(c.query!!, c.vars, sink, abort)
                        1L
                    }
                    "iter-all" -> {
                        val it = graph.find()
                        try { triples(it, sink) } finally { it.close() }
                        1L
                    }
                    "pattern-s" -> {
                        val n = minOf(ops, subjects.size)
                        for (i in 0 until n) {
                            val it = graph.find(subjects[i], Node.ANY, Node.ANY)
                            try { triples(it, sink) } finally { it.close() }
                        }
                        n.toLong()
                    }
                    "pattern-po" -> {
                        val it = graph.find(Node.ANY, iri(RDF_TYPE), iri(EX + "Researcher"))
                        try { triples(it, sink) } finally { it.close() }
                        1L
                    }
                    "contains" -> {
                        val n = minOf(ops, probes.size)
                        for (i in 0 until n) sink.row(arrayOf(bool(graph.contains(probes[i]))))
                        n.toLong()
                    }
                    "value" -> {
                        val n = minOf(ops, subjects.size)
                        for (i in 0 until n) {
                            val st = model.getProperty(model.wrapAsResource(subjects[i]), nameProp)
                            sink.row(arrayOf(st?.`object`?.asNode()))
                        }
                        n.toLong()
                    }
                    else -> error("unknown case kind ${c.kind}")
                }
            }
        }
    }

    // ------------------------------------------------------------------------ timing

    private val worker = Executors.newSingleThreadExecutor { r -> Thread(null, r, "bench", 1L shl 30).apply { isDaemon = true } }

    /** Runs `f` on the worker thread with the case timeout; a sample that does not stop
     * after an abort ends the process, and the driver resumes after this case. */
    private fun <T> bounded(cfg: Config, c: Case, abort: AtomicReference<QueryExec?>, f: () -> T): T {
        val fut = worker.submit(Callable { f() })
        try {
            return fut.get(cfg.timeoutMs, TimeUnit.MILLISECONDS)
        } catch (e: TimeoutException) {
            abort.get()?.abort()
            fut.cancel(true)
            try {
                fut.get(10, TimeUnit.SECONDS)
            } catch (_: Exception) {
            }
            if (!fut.isDone) {
                emit(event("case", "case" to c.name, "status" to "timeout", "samples_ms" to emptyList<Double>()))
                Runtime.getRuntime().halt(3)
            }
            throw e
        } catch (e: ExecutionException) {
            throw e.cause ?: e
        }
    }

    private fun answers(cfg: Config, ops: Ops) {
        val dir = Path.of(cfg.answersDir!!)
        Files.createDirectories(dir)
        for (c in cfg.cases) {
            if (c.name in cfg.skip || c.kind == "adds") continue
            emit(event("start", "case" to c.name))
            val abort = AtomicReference<QueryExec?>()
            val file = dir.resolve(c.name.replace(':', '_') + ".jsonl")
            val result = event("case", "case" to c.name)
            try {
                Files.newBufferedWriter(file).use { w ->
                    val vars = JsonArray()
                    c.vars.forEach(vars::add)
                    w.write("{\"vars\":$vars}\n")
                    val sink = Sink(w)
                    val t = System.nanoTime()
                    bounded(cfg, c, abort) { ops.run(c, sink, abort) }
                    put(result, "rows", sink.rows)
                    put(result, "ms", (System.nanoTime() - t) / 1e6)
                    put(result, "status", "ok")
                }
            } catch (e: TimeoutException) {
                put(result, "status", "timeout")
            } catch (e: Throwable) {
                put(result, "status", "error")
                put(result, "error", e.toString().take(300))
            }
            emit(result)
        }
    }

    private fun time(cfg: Config, ops: Ops) {
        for (c in cfg.cases) {
            if (c.name in cfg.skip) continue
            emit(event("start", "case" to c.name))
            val abort = AtomicReference<QueryExec?>()
            val warm = ArrayList<Double>()
            val samples = ArrayList<Double>()
            val result = event("case", "case" to c.name)
            var rows = -1L
            var opsPerSample = 1L
            val begin = System.nanoTime()
            try {
                var i = 0
                while (i < cfg.warmup + cfg.runs) {
                    val sink = Sink(null)
                    val t = System.nanoTime()
                    opsPerSample = bounded(cfg, c, abort) { ops.run(c, sink, abort) }
                    val ms = (System.nanoTime() - t) / 1e6
                    sinkValue += sink.acc
                    rows = sink.rows
                    if (i < cfg.warmup) warm.add(ms) else samples.add(ms)
                    emit(event("tick", "case" to c.name))
                    i++
                    // past its budget, a case skips the rest of its warm-up and stops after
                    // its first measured sample
                    if ((System.nanoTime() - begin) / 1e6 > cfg.budgetMs) {
                        if (samples.isNotEmpty()) break
                        i = maxOf(i, cfg.warmup)
                    }
                }
                put(result, "status", "ok")
            } catch (e: TimeoutException) {
                put(result, "status", "timeout")
            } catch (e: Throwable) {
                put(result, "status", "error")
                put(result, "error", e.toString().take(300))
            }
            put(result, "rows", rows)
            put(result, "ops", opsPerSample)
            put(result, "warmup_ms", warm)
            put(result, "samples_ms", samples)
            emit(result)
        }
    }

    // ------------------------------------------------------------- small operations

    /** One small operation of P04 §5.4, for subject or probe `i`. */
    private fun smallOp(ops: Ops, kind: String, i: Int): Long {
        val dsg = ops.dsg
        val s = ops.subjects[i % ops.subjects.size]
        return Txn.calculateRead(dsg) {
            when (kind) {
                "sq-select-o" -> {
                    var n = 0L
                    val b = QueryExec.dataset(dsg).query("SELECT ?o WHERE { <${s.uri}> <$FOAF_NAME> ?o }")
                    if (isSparkles(ops.cfg.engine)) b.context(ops.noCache)
                    b.build().use { qe ->
                        val rs = qe.select()
                        while (rs.hasNext()) {
                            n += rs.next().get(Var.alloc("o")).literalLexicalForm.length
                        }
                    }
                    n
                }
                "sq-ask" -> {
                    val t = ops.probes[i % ops.probes.size]
                    val b = QueryExec.dataset(dsg).query("ASK { <${t.subject.uri}> <${t.predicate.uri}> <${t.`object`.uri}> }")
                    if (isSparkles(ops.cfg.engine)) b.context(ops.noCache)
                    b.build().use { qe -> if (qe.ask()) 1L else 0L }
                }
                "op-getProperty" -> {
                    val st = ops.model.getProperty(ops.model.wrapAsResource(s), ops.nameProp)
                    st?.string?.length?.toLong() ?: 0L
                }
                "op-contains" -> if (ops.graph.contains(ops.probes[i % ops.probes.size])) 1L else 0L
                "op-find-s" -> {
                    var n = 0L
                    val it = ops.graph.find(s, Node.ANY, Node.ANY)
                    try {
                        while (it.hasNext()) n += it.next().predicate.uri.length
                    } finally {
                        it.close()
                    }
                    n
                }
                else -> error("unknown small operation $kind")
            }
        }
    }

    private fun smallQuery(ops: Ops, c: Case): Long = Txn.calculateRead(ops.dsg) {
        var n = 0L
        val b = QueryExec.dataset(ops.dsg).query(c.query!!)
        if (isSparkles(ops.cfg.engine)) b.context(ops.noCache)
        b.build().use { qe ->
            val rs = qe.select()
            while (rs.hasNext()) {
                rs.next()
                n++
            }
        }
        n
    }

    private fun throughput(cfg: Config, ops: Ops) {
        for (c in cfg.cases) {
            for (threads in cfg.threads) {
                val name = "${c.name}@$threads"
                if (name in cfg.skip) continue
                emit(event("start", "case" to name))
                val op: (Int) -> Long = if (c.kind == "small-query") { _ -> smallQuery(ops, c) } else { i -> smallOp(ops, c.kind, i) }
                val result = event("case", "case" to name, "threads" to threads)
                try {
                    val r = runThreads(cfg, threads, op)
                    for ((k, v) in r) put(result, k, v)
                    put(result, "status", "ok")
                } catch (e: Throwable) {
                    put(result, "status", "error")
                    put(result, "error", e.toString().take(300))
                }
                emit(result)
            }
        }
    }

    /** Runs `op` back to back on `threads` threads: a warm-up period, then a measured one,
     * recording every operation's latency. */
    private fun runThreads(cfg: Config, threads: Int, op: (Int) -> Long): Map<String, Any> {
        val measuring = AtomicBoolean(false)
        val stop = AtomicBoolean(false)
        val lat = Array(threads) { LongArrayList() }
        val failure = AtomicReference<Throwable?>()
        val started = CountDownLatch(threads)
        val ts = (0 until threads).map { t ->
            Thread(null, {
                var i = t * 7919
                var acc = 0L
                started.countDown()
                try {
                    while (!stop.get()) {
                        val s = System.nanoTime()
                        acc += op(i++)
                        val d = System.nanoTime() - s
                        if (measuring.get()) lat[t].add(d)
                    }
                } catch (e: Throwable) {
                    failure.compareAndSet(null, e)
                    stop.set(true)
                }
                sinkValue += acc
            }, "tp-$t", 256L shl 20)
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
        ts.forEach { it.join(cfg.timeoutMs) }
        failure.get()?.let { throw it }
        val all = LongArrayList()
        for (l in lat) all.addAll(l)
        val sorted = all.sorted()
        fun pct(p: Double) = if (sorted.isEmpty()) Double.NaN else sorted[minOf(sorted.size - 1, (p * sorted.size).toInt())] / 1e6
        return mapOf(
            "ops" to sorted.size,
            "seconds" to elapsed,
            "qps" to sorted.size / elapsed,
            "p50_ms" to pct(0.5),
            "p99_ms" to pct(0.99),
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

    // ------------------------------------------------------------- native call counts

    /** A counter that the counting copy of the generated bindings increments: `calls` for
     * every native call, `jniCalls` for those of them through the hand-written JNI calls. */
    private fun callCounter(name: String): java.util.concurrent.atomic.LongAdder {
        val cls = Class.forName("io.github.kclejeune.sparkles.jena.internal.ffi.UniffiCallCounter")
        return cls.getField(name).get(null) as java.util.concurrent.atomic.LongAdder
    }

    private fun calls(cfg: Config, ops: Ops) {
        val total = callCounter("calls")
        val jni = callCounter("jniCalls")
        val abort = AtomicReference<QueryExec?>()
        val n = 20
        for (c in cfg.cases) {
            if (c.name in cfg.skip) continue
            emit(event("start", "case" to c.name))
            val result = event("case", "case" to c.name)
            try {
                // the calls of each counter per operation, over the same operations
                var before = LongArray(2)
                val mark = { before = longArrayOf(total.sum(), jni.sum()) }
                val per = { ops: Long -> doubleArrayOf((total.sum() - before[0]).toDouble() / ops, (jni.sum() - before[1]).toDouble() / ops) }
                val perOp: DoubleArray = when (c.kind) {
                    "small-query" -> {
                        smallQuery(ops, c)
                        mark()
                        repeat(n) { smallQuery(ops, c) }
                        per(n.toLong())
                    }
                    "sq-select-o", "sq-ask", "op-getProperty", "op-contains", "op-find-s" -> {
                        smallOp(ops, c.kind, 0)
                        mark()
                        for (i in 1..n) smallOp(ops, c.kind, i)
                        per(n.toLong())
                    }
                    else -> {
                        ops.run(c, Sink(null), abort)
                        mark()
                        val made = ops.run(c, Sink(null), abort)
                        per(made)
                    }
                }
                put(result, "calls_per_op", perOp[0])
                put(result, "jni_calls_per_op", perOp[1])
                put(result, "status", "ok")
            } catch (e: Throwable) {
                put(result, "status", "error")
                put(result, "error", e.toString().take(300))
            }
            emit(result)
        }
    }

    // -------------------------------------------------------------------------- main

    @JvmStatic
    fun main(args: Array<String>) {
        val cfg = Config(JsonParser.parseString(Files.readString(Path.of(args[0]))).asJsonObject)
        val versions = JsonObject()
        versions.addProperty("java", System.getProperty("java.runtime.version"))
        versions.addProperty("java_vendor", System.getProperty("java.vendor"))
        versions.addProperty("jena", ARQ.VERSION)
        versions.addProperty("processors", Runtime.getRuntime().availableProcessors())
        versions.addProperty("max_heap_mib", Runtime.getRuntime().maxMemory() shr 20)
        emit(event("versions", "versions" to versions))
        var dsg: DatasetGraph? = null
        try {
            if (cfg.mode == "load") {
                val t = System.nanoTime()
                val d = load(cfg)
                val quads = size(d)
                d.close()
                val ms = (System.nanoTime() - t) / 1e6
                emit(event("case", "case" to "load", "status" to "ok", "rows" to quads, "samples_ms" to listOf(ms)))
            } else {
                val t = System.nanoTime()
                dsg = open(cfg)
                emit(event("ready", "ms" to (System.nanoTime() - t) / 1e6, "quads" to size(dsg)))
                val ops = Ops(cfg, dsg)
                when (cfg.mode) {
                    "answers" -> answers(cfg, ops)
                    "time" -> time(cfg, ops)
                    "throughput" -> throughput(cfg, ops)
                    "calls" -> calls(cfg, ops)
                    else -> error("unknown mode ${cfg.mode}")
                }
            }
        } catch (e: Throwable) {
            emit(event("fatal", "error" to e.toString().take(500)))
            e.printStackTrace()
            exitProcess(2)
        } finally {
            dsg?.close()
            emit(event("done", "sink" to sinkValue))
        }
        worker.shutdownNow()
        exitProcess(0)
    }
}
