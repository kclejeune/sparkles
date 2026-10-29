package io.github.kclejeune.sparkles.jena

import org.apache.jena.atlas.json.JSON
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.query.QueryCancelledException
import org.apache.jena.riot.Lang
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream
import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** Preserve the primary failure while reporting cleanup failures. */
private fun <T> withCleanup(action: () -> T, cleanup: () -> Unit): T {
    var failure: Throwable? = null
    try { return action() } catch (error: Throwable) { failure = error; throw error }
    finally {
        try { cleanup() } catch (error: Throwable) {
            if (failure == null) throw error else failure.addSuppressed(error)
        }
    }
}
private fun cleanupAll(vararg actions: () -> Unit) {
    var failure: Throwable? = null
    for (action in actions) try { action() } catch (error: Throwable) {
        if (failure == null) failure = error else failure.addSuppressed(error)
    }
    failure?.let { throw it }
}

/** One isolated child owns the arm and the operation for one probe mode. */
object AlgorithmProbeChild {
    @JvmStatic public fun main(args: Array<String>) {
        val mode = args[0]
        val dir = Path.of(args[1])
        val request = JSON.parse("""{"algorithm":"pageRank","projection":{"predicates":["urn:p"]},"write":{"graph":"urn:out","predicates":{"score":"urn:rank"},"batchSize":2}}""")
        val arm = JsonObject().also { it.put("mode", mode); it.put("author", "__sparkles_jvm_probe__"); it.put("request", request) }
        Files.writeString(dir.resolve("armed.json"), arm.toString())
        SparklesDatasets.memory().use { ds ->
            ds.load(ByteArrayInputStream("<urn:a> <urn:p> <urn:b>. <urn:b> <urn:p> <urn:c>. <urn:c> <urn:p> <urn:a>. <urn:d> <urn:p> <urn:e>.".toByteArray()), Lang.TURTLE)
            val executor = Executors.newSingleThreadExecutor()
            withCleanup({
                SparklesOperation(30_000).use { operation ->
                    val future = executor.submit<Any> {
                        try { ds.algorithms().runWrite(request, AlgorithmOptions(), "__sparkles_jvm_probe__", operation) }
                        catch (error: QueryCancelledException) { error }
                    }
                    withCleanup({
                        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(8)
                        val entered = dir.resolve("entered.json")
                        while (!Files.exists(entered)) {
                            check(!future.isDone) { "algorithm ended before the boundary ACK" }
                            check(System.nanoTime() < deadline) { "algorithm boundary ACK timed out" }
                            Thread.sleep(2)
                        }
                        val first = JSON.parse(Files.readString(entered))
                        check(first.getBoolean("deadlineForwarded") && first.getBoolean("checkedClose"))
                        operation.cancel()
                        Files.writeString(dir.resolve("release"), "release")
                        val result = future.get(15, TimeUnit.SECONDS)
                        val receipt = JSON.parse(Files.readString(dir.resolve("finished.json")))
                        check(receipt.getString("outcome") == "released-by-test")
                        check(receipt.getBoolean("entered") && receipt.getBoolean("deadlineForwarded") && receipt.getBoolean("checkedClose"))
                        val expected = if (mode == "batch2") {
                            check(result is QueryCancelledException && receipt.getString("result") == "Cancelled")
                            2
                        } else {
                            check(result is JsonObject && result.getString("status") == "complete" && receipt.getString("result") == "Ok")
                            5
                        }
                        assertEquals(expected, ds.find(NodeFactory.createURI("urn:out"), Node.ANY, NodeFactory.createURI("urn:rank"), Node.ANY).asSequence().count())
                        ds.load(ByteArrayInputStream("<urn:after> <urn:p> <urn:ok>.".toByteArray()), Lang.TURTLE)
                        Files.deleteIfExists(dir.resolve("release"))
                    }, {
                        cleanupAll(
                            { if (!future.isDone) operation.cancel() },
                            { if (!future.isDone) Files.writeString(dir.resolve("release"), "cleanup") },
                            {
                                executor.shutdown()
                                if (!executor.awaitTermination(5, TimeUnit.SECONDS)) {
                                    executor.shutdownNow()
                                    check(executor.awaitTermination(5, TimeUnit.SECONDS)) { "algorithm worker did not terminate" }
                                }
                            },
                        )
                    })
                }
            }, {
                executor.shutdownNow()
                check(executor.awaitTermination(5, TimeUnit.SECONDS)) { "algorithm executor did not terminate" }
            })
        }
        println("probe passed: $mode")
    }
}

class AlgorithmProbeTest {
    @Test fun cancellation_after_batch_and_after_last_commit_is_deterministic() {
        assumeTrue(System.getenv("SPARKLES_TEST_JVM_ALGORITHM_PROBE") == "1")
        val root = Path.of(requireNotNull(System.getenv("SPARKLES_TEST_JVM_ALGORITHM_EVIDENCE")))
        Files.createDirectories(root)
        val bridge = if (System.getProperty("sparkles.jni") == "false") "uniffi" else "jni"
        for (mode in listOf("batch2", "done")) {
            val dir = Files.createDirectory(root.resolve("$bridge-$mode"))
            val command = mutableListOf(File(System.getProperty("java.home"), "bin/java").path,
                "-Dsparkles.jni=${System.getProperty("sparkles.jni", "true")}")
            System.getProperty("sparkles.native.path")?.let { command.add("-Dsparkles.native.path=$it") }
            command.addAll(listOf("-cp", System.getProperty("java.class.path"), AlgorithmProbeChild::class.java.name, mode, dir.toString()))
            val log = dir.resolve("child.log")
            val process = ProcessBuilder(command).redirectErrorStream(true).redirectOutput(log.toFile()).apply {
                environment()["SPARKLES_TEST_JVM_ALGORITHM_DIR"] = dir.toString()
            }.start()
            withCleanup({
                assertTrue(process.waitFor(45, TimeUnit.SECONDS), "algorithm probe did not terminate: $mode")
                val output = Files.readString(log)
                assertEquals(0, process.exitValue(), output)
                assertTrue(output.contains("probe passed: $mode"), output)
            }, {
                if (process.isAlive) {
                    process.destroyForcibly()
                    check(process.waitFor(10, TimeUnit.SECONDS)) { "algorithm probe child did not terminate: $mode" }
                }
            })
        }
    }
}
