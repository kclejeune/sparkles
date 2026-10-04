package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.ImportOptions
import io.github.kclejeune.sparkles.jena.ImportReport
import io.github.kclejeune.sparkles.jena.SparklesDatasets
import io.github.kclejeune.sparkles.jena.SparklesInvalidException
import org.apache.jena.query.ReadWrite
import org.apache.jena.tdb2.DatabaseMgr
import org.apache.jena.tdb2.sys.TDBInternal
import java.nio.file.Path
import java.time.Duration

/** The TDB2 import; only loaded once `jena-tdb2` is known to be on the classpath. */
internal object Tdb2Import {
    fun run(tdbDir: Path, sparklesDir: Path, options: ImportOptions): ImportReport {
        val start = System.nanoTime()
        val tdb = DatabaseMgr.connectDatasetGraph(tdbDir.toString())
        try {
            tdb.begin(ReadWrite.READ)
            try {
                val dsg = SparklesDatasets.open(sparklesDir)
                try {
                    if (!options.append && !dsg.isEmpty) {
                        throw SparklesInvalidException(
                            "Invalid",
                            "$sparklesDir has data: import into an empty database, or pass ImportOptions.append(true)",
                        )
                    }
                    val sink = dsg.bulkSink()
                    sink.use {
                        sink.start()
                        val it = tdb.find()
                        while (it.hasNext()) sink.quad(it.next())
                        var prefixes = 0
                        tdb.prefixes().forEach { p, iri ->
                            sink.prefix(p, iri)
                            prefixes++
                        }
                        sink.finish()
                        var graphs = 0L
                        val g = tdb.listGraphNodes()
                        while (g.hasNext()) {
                            g.next()
                            graphs++
                        }
                        return ImportReport(
                            sink.count,
                            graphs,
                            prefixes,
                            sink.receipt()!!,
                            Duration.ofNanos(System.nanoTime() - start),
                        )
                    }
                } finally {
                    dsg.close()
                }
            } finally {
                tdb.end()
            }
        } finally {
            TDBInternal.expel(tdb)
        }
    }
}
