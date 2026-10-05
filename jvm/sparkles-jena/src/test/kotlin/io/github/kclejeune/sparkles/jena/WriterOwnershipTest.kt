package io.github.kclejeune.sparkles.jena

import org.apache.jena.query.TxnType
import org.apache.jena.sparql.JenaTransactionException
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File
import java.nio.file.Path
import java.util.concurrent.TimeUnit

object OwnershipProbe {
    @JvmStatic
    fun main(args: Array<String>) {
        val dataset = if (args[0] == "persistent-main-alias") SparklesDatasets.open(Path.of(args[1])) else SparklesDatasets.memory()
        dataset.use { ds ->
            val sink = ds.bulkSink()
            try {
                when (args[0]) {
                    "memory-main-alias", "persistent-main-alias" -> {
                        ds.branches().create("dev")
                        ds.branch("main").use { alias ->
                            ds.branch("dev").use { dev ->
                                ds.begin(TxnType.WRITE)
                                assertTrue(alias.isInTransaction())
                                assertThrows(JenaTransactionException::class.java) { alias.begin(TxnType.WRITE) }
                                assertThrows(JenaTransactionException::class.java) { alias.compact() }
                                assertThrows(JenaTransactionException::class.java) { alias.branches().previewMerge("dev") }
                                assertThrows(JenaTransactionException::class.java) { dev.branch("main") }
                                ds.abort(); ds.end()
                                alias.begin(TxnType.WRITE); alias.abort(); alias.end()
                            }
                        }
                    }
                    "branch-preview" -> {
                        ds.branches().create("dev")
                        ds.branch("dev").use { branch ->
                            branch.begin(TxnType.WRITE)
                            try { ds.branches().previewMerge("dev"); error("preview accepted an owned branch writer") }
                            catch (_: JenaTransactionException) { }
                            try { ds.branches().get("dev"); error("info accepted an owned branch writer") }
                            catch (_: JenaTransactionException) { }
                            try { ds.branches().list(); error("list accepted an owned branch writer") }
                            catch (_: JenaTransactionException) { }
                            branch.abort(); branch.end()
                        }
                    }
                    "late-sink" -> {
                        ds.begin(TxnType.WRITE)
                        try { sink.start(); error("sink accepted an active transaction") }
                        catch (_: JenaTransactionException) { }
                        ds.abort()
                    }
                    "active-sink" -> {
                        sink.start()
                        try { ds.begin(TxnType.WRITE); error("transaction accepted an active sink") }
                        catch (_: JenaTransactionException) { }
                    }
                    "closed-sink" -> {
                        ds.close()
                        try { sink.start(); error("sink accepted a closed owner") }
                        catch (_: SparklesInvalidException) { }
                    }
                    "owner-close" -> {
                        sink.start()
                        ds.close()
                        try { sink.triple(org.apache.jena.graph.Triple.create(
                            org.apache.jena.graph.NodeFactory.createURI("urn:s"),
                            org.apache.jena.graph.NodeFactory.createURI("urn:p"),
                            org.apache.jena.graph.NodeFactory.createURI("urn:o")))
                            error("sink accepted a closed owner") }
                        catch (_: SparklesInvalidException) { }
                    }
                }
            } finally { sink.close() }
        }
        println("passed")
    }
}

class WriterOwnershipTest {
    @Test
    fun sink_and_transaction_dispatch_never_wait_on_their_own_writer(@TempDir directory: Path) {
        for (case in listOf("late-sink", "active-sink", "closed-sink", "owner-close", "branch-preview", "memory-main-alias", "persistent-main-alias")) {
            val process = ProcessBuilder(File(System.getProperty("java.home"), "bin/java").path,
                "-cp", System.getProperty("java.class.path"), OwnershipProbe::class.java.name, case, directory.resolve(case).toString())
                .redirectErrorStream(true).start()
            try {
                assertTrue(process.waitFor(20, TimeUnit.SECONDS), "writer deadlock: $case")
                val output = process.inputStream.bufferedReader().readText()
                assertEquals(0, process.exitValue(), output)
                assertTrue(output.contains("passed"), output)
            } finally { process.destroyForcibly() }
        }
    }

    @Test
    fun a_shared_native_dataset_refuses_incompatible_options(@TempDir dir: Path) {
        SparklesDatasets.open(dir).use {
            assertThrows(IllegalArgumentException::class.java) {
                SparklesDatasets.open(dir, SparklesOptions.builder().readOnly(true).build())
            }
            SparklesDatasets.open(dir).use { alias -> assertEquals(it.datasetId(), alias.datasetId()) }
        }
        SparklesDatasets.open(dir, SparklesOptions.builder().readOnly(true).build()).use {
            assertThrows(SparklesNotPermittedException::class.java) { it.begin(TxnType.WRITE) }
        }
    }
}
