package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.engine.requestOptions
import io.github.kclejeune.sparkles.jena.internal.RowDecoder
import io.github.kclejeune.sparkles.jena.internal.Source
import org.apache.jena.query.ARQ
import org.apache.jena.query.QueryFactory
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.Context
import org.apache.jena.system.Txn
import org.apache.jena.tdb2.DatabaseMgr
import java.nio.file.Files
import java.nio.file.Path

/**
 * A small performance check of the binding (P04 §5.4), run with `./gradlew
 * :sparkles-jena:perfCheck -Pdata=DATA.nt -Pqueries=QUERIES.tsv`. For each query it prints
 * the median milliseconds of: the query through Jena's `QueryExec` on Sparkles, the same
 * query through the native library alone (no Jena bindings built), the engine's own
 * `total_ms`, and the query through `QueryExec` on TDB2 in memory with the same data.
 * Sparkles' result cache is off, so that every run executes the query.
 */
object PerfCheck {
    private fun median(xs: List<Double>): Double = xs.sorted()[xs.size / 2]

    /** The median of `iters` runs after as many warm-up runs; NaN when the query fails. */
    private fun time(iters: Int, f: () -> Long): Pair<Double, Long> {
        var rows = 0L
        val times = ArrayList<Double>()
        try {
            repeat(iters * 2) { i ->
                val t = System.nanoTime()
                rows = f()
                val ms = (System.nanoTime() - t) / 1e6
                if (i >= iters) times.add(ms)
            }
        } catch (e: Throwable) {
            System.err.println("failed: $e")
            return Double.NaN to -1
        }
        return median(times) to rows
    }

    private fun jena(d: DatasetGraph, q: String, cxt: Context): Long = Txn.calculateRead(d) {
        var n = 0L
        val rs = QueryExec.dataset(d).query(q).context(cxt).select()
        while (rs.hasNext()) {
            rs.next()
            n++
        }
        n
    }

    @JvmStatic
    fun main(args: Array<String>) {
        val data = Path.of(args[0])
        val queries = Files.readAllLines(Path.of(args[1])).filter { it.contains('\t') }.map {
            val (n, q) = it.split('\t', limit = 2)
            n to q
        }
        val iters = args.getOrNull(2)?.toInt() ?: 10
        val withTdb = args.getOrNull(3) != "notdb"

        val dsg = SparklesDatasets.memory()
        var t = System.nanoTime()
        dsg.loadFiles(listOf(data))
        System.err.printf("sparkles: loaded %d quads in %.0f ms%n", dsg.headCommit().quads, (System.nanoTime() - t) / 1e6)
        val tdb = DatabaseMgr.createDatasetGraph()
        if (withTdb) {
            t = System.nanoTime()
            Txn.executeWrite(tdb) { RDFDataMgr.read(tdb, data.toString()) }
            System.err.printf("tdb2: loaded in %.0f ms%n", (System.nanoTime() - t) / 1e6)
        }
        val noCache = Context().set(Sparkles.NO_CACHE, true)
        val opts = requestOptions(dsg, ARQ.getContext().copy().set(Sparkles.NO_CACHE, true), null)

        println("query\tjena-sparkles-ms\tnative-ms\tengine-ms\ttdb2-ms\trows")
        for ((name, q) in queries) {
            QueryFactory.create(q)
            val (viaJena, rows) = time(iters) { jena(dsg, q, noCache) }
            val engine = ArrayList<Double>()
            val (native, _) = time(iters) {
                val fq = Source.Head(dsg.handle.ffi).prepareQuery(q, opts)
                val d = RowDecoder()
                val e = fq.execute(256u)
                engine.add(e.timing.totalMs)
                var n = d.decode(e.batch).rows.toLong()
                var done = e.done
                while (!done) {
                    val b = fq.nextBatch(8192u)
                    n += d.decode(b.batch).rows
                    done = b.done
                }
                fq.release()
                fq.close()
                n
            }
            val tdbMs = if (withTdb) time(iters) { jena(tdb, q, Context()) }.first else Double.NaN
            println(String.format("%s\t%.3f\t%.3f\t%.3f\t%.3f\t%d", name, viaJena, native, median(engine), tdbMs, rows))
        }
        dsg.close()
    }
}
