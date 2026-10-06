package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.toInfo
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiOperation
import java.time.Instant

/** Cancellation, deadline and progress for one native operation. */
public class SparklesOperation @JvmOverloads public constructor(timeoutMillis: Long? = null) : AutoCloseable {
    internal val native: FfiOperation = run { io.github.kclejeune.sparkles.jena.internal.NativeLoader.load(); FfiOperation(timeoutMillis?.also { require(it >= 0) }?.toULong()) }
    public fun cancel(): Unit = native.cancel()
    public fun progress(): OperationProgress = native.progress().let { OperationProgress(it.fraction, it.message) }
    override fun close(): Unit = native.close()
}
public data class OperationProgress(public val fraction: Float, public val message: String)
public data class NamedSnapshot(
    public val name: String, public val commit: Long, public val createdAt: Instant,
    public val note: String?, public val expiresAt: Instant?, public val reconstructable: Boolean, public val warm: Boolean,
)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.SnapshotInfo.toSnapshot(): NamedSnapshot =
    NamedSnapshot(name, seq.toLong(), Instant.ofEpochMilli(createdMs), note, expiresMs?.let(Instant::ofEpochMilli), reconstructable, warm)

/** Named snapshot handles retain their owner and reject use after it closes. */
public class SparklesSnapshots internal constructor(private val owner: DatasetGraphSparkles) {
    public fun list(): List<NamedSnapshot> { owner.checkOpen(); return ffi { owner.handle.ffi.snapshotsList() }.map { it.toSnapshot() } }
    public fun get(name: String): NamedSnapshot? { owner.checkOpen(); return ffi { owner.handle.ffi.snapshotsGet(name) }?.toSnapshot() }
    @JvmOverloads
    public fun create(name: String, at: String = "head", note: String? = null, expiresAt: Instant? = null, warm: Boolean = false): NamedSnapshot {
        owner.checkNoTxn("snapshots.create")
        return ffi { owner.handle.ffi.snapshotsCreate(name, at, note, expiresAt?.toEpochMilli(), warm) }.toSnapshot()
    }
    public fun delete(name: String): Boolean { owner.checkNoTxn("snapshots.delete"); return ffi { owner.handle.ffi.snapshotsDelete(name) } }
}

public class SparklesHistory internal constructor(private val owner: DatasetGraphSparkles) {
    public fun status(): org.apache.jena.atlas.json.JsonObject { owner.checkOpen(); return document(ffi { owner.handle.ffi.historyStatus() }).asObject }
    public fun commit(reference: String): org.apache.jena.atlas.json.JsonObject? { owner.checkOpen(); return ffi { owner.handle.ffi.historyCommit(reference) }?.let { document(it).asObject } }
    @JvmOverloads public fun diff(from: String, to: String = "head", maxQuads: Long = 0): org.apache.jena.atlas.json.JsonObject = SparklesOperation().use { diff(from, to, maxQuads, it) }
    public fun diff(from: String, to: String, maxQuads: Long, operation: SparklesOperation): org.apache.jena.atlas.json.JsonObject { owner.checkOpen(); require(maxQuads >= 0); return document(ffi { owner.handle.ffi.historyDiff(from, to, maxQuads.toULong(), operation.native) }).asObject }
    @JvmOverloads public fun changes(after: Long, maxCommits: Int = 100, maxQuads: Long = 0): org.apache.jena.atlas.json.JsonObject = SparklesOperation().use { changes(after, maxCommits, maxQuads, it) }
    public fun changes(after: Long, maxCommits: Int, maxQuads: Long, operation: SparklesOperation): org.apache.jena.atlas.json.JsonObject { owner.checkOpen(); require(after >= 0 && maxCommits > 0 && maxQuads >= 0); return document(ffi { owner.handle.ffi.historyChanges(after.toULong(), maxCommits.toUInt(), maxQuads.toULong(), operation.native) }).asObject }
    @JvmOverloads public fun query(graph: org.apache.jena.graph.Node? = null, subject: org.apache.jena.graph.Node? = null, predicate: org.apache.jena.graph.Node? = null, value: org.apache.jena.graph.Node? = null, from: String? = null, to: String? = null, limit: Int = 1000, descending: Boolean = false): org.apache.jena.atlas.json.JsonObject = SparklesOperation().use { operation -> owner.checkOpen(); require(limit >= 0); document(ffi { owner.handle.ffi.historyQuery(io.github.kclejeune.sparkles.jena.internal.encodePattern(graph, subject, predicate, value), from, to, limit.toUInt(), descending, operation.native) }).asObject }
    public fun prune(): Long { owner.checkNoTxn("history.prune"); return ffi { owner.handle.ffi.historyPrune() }.toLong() }
    public fun tick(): org.apache.jena.atlas.json.JsonObject { owner.checkNoTxn("history.tick"); return document(ffi { owner.handle.ffi.historyTick() }).asObject }
    public fun waitForCommit(after: Long, timeoutMillis: Long): Long? { owner.checkOpen(); require(after >= 0 && timeoutMillis >= 0); return ffi { owner.handle.ffi.historyWait(after.toULong(), timeoutMillis.toULong()) }?.toLong() }
    @JvmOverloads
    public fun commits(limit: Int = 100): List<CommitInfo> = page("latest", null, limit)
    @JvmOverloads
    public fun before(commit: Long, limit: Int = 100): List<CommitInfo> = page("before", commit, limit)
    @JvmOverloads
    public fun after(commit: Long, limit: Int = 100): List<CommitInfo> = page("after", commit, limit)
    private fun page(direction: String, cursor: Long?, limit: Int): List<CommitInfo> {
        owner.checkOpen(); require(limit >= 0); require(cursor == null || cursor >= 0)
        return ffi { owner.handle.ffi.commits(direction, cursor?.toULong(), limit.toUInt()) }.map { it.toInfo() }
    }
}
public enum class DescribeMode { CBD, SCBD, OUTGOING }
public data class DescribeOptions @JvmOverloads public constructor(
    public val mode: DescribeMode = DescribeMode.CBD, public val labels: Boolean = false,
    public val reifiers: Boolean = true, public val maxTriples: Long? = null, public val maxDepth: Int? = null,
)
public class SparklesSettings internal constructor(private val owner: DatasetGraphSparkles) {
    public fun describe(): DescribeSetting = DescribeSetting(owner)
    public fun compaction(): CompactionSetting = CompactionSetting(owner)
    public fun quota(): QuotaSetting = QuotaSetting(owner)
    public fun retention(): RetentionSetting = RetentionSetting(owner)
}
public class DescribeSetting internal constructor(private val owner: DatasetGraphSparkles) {
    public fun get(): DescribeOptions {
        owner.checkOpen()
        return ffi { owner.handle.ffi.describeGet() }.let {
            DescribeOptions(DescribeMode.valueOf(it.mode.uppercase()), it.labels, it.reifiers, it.maxTriples?.toLong(), it.maxDepth?.toInt())
        }
    }
    public fun set(options: DescribeOptions) {
        owner.checkNoTxn("settings.describe.set")
        require(options.maxTriples == null || options.maxTriples >= 0)
        require(options.maxDepth == null || options.maxDepth >= 0)
        ffi { owner.handle.ffi.describeSet(io.github.kclejeune.sparkles.jena.internal.ffi.DescribeSettings(
            options.mode.name.lowercase(), options.labels, options.reifiers, options.maxTriples?.toULong(), options.maxDepth?.toUInt())) }
    }
    public fun reset() { owner.checkNoTxn("settings.describe.reset"); ffi { owner.handle.ffi.describeReset() } }
}
public data class TextOptions @JvmOverloads public constructor(
    public val predicates: List<String>? = null, public val maxTextBytes: Long = 1048576, public val maxHits: Long = 10000,
)
public data class TextStatus(public val enabled: Boolean, public val state: String, public val documents: Long,
    public val commit: Long, public val storeCommit: Long, public val diskBytes: Long)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.TextInfo.toStatus(): TextStatus =
    TextStatus(enabled, state, docs.toLong(), seq.toLong(), storeSeq.toLong(), diskBytes.toLong())
public class SparklesIndexes internal constructor(private val owner: DatasetGraphSparkles) {
    public fun text(): SparklesTextIndex = SparklesTextIndex(owner)
    public fun vector(): SparklesVectorIndexes = SparklesVectorIndexes(owner)
    public fun geo(): SparklesGeoIndex = SparklesGeoIndex(owner)
}
public class SparklesTextIndex internal constructor(private val owner: DatasetGraphSparkles) {
    @JvmOverloads public fun search(query: String, options: TextSearchOptions = TextSearchOptions()): org.apache.jena.atlas.json.JsonObject = SparklesOperation().use { search(query, options, it) }
    public fun search(query: String, options: TextSearchOptions, operation: SparklesOperation): org.apache.jena.atlas.json.JsonObject {
        owner.checkCapture("indexes.text.search"); require(options.limit >= 0)
        return document(ffi { owner.handle.ffi.textSearch(io.github.kclejeune.sparkles.jena.internal.ffi.TextSearchRequest(query, options.predicates, options.language, options.graph, options.limit.toUInt(), options.highlight), operation.native) }).asObject
    }
    public fun status(): TextStatus? { owner.checkOpen(); return ffi { owner.handle.ffi.textStatus() }?.toStatus() }
    @JvmOverloads
    public fun enable(options: TextOptions = TextOptions()): TextStatus {
        owner.checkNoTxn("indexes.text.enable"); require(options.maxTextBytes > 0); require(options.maxHits > 0)
        return ffi { owner.handle.ffi.textEnable(io.github.kclejeune.sparkles.jena.internal.ffi.TextSettings(
            options.predicates, options.maxTextBytes.toULong(), options.maxHits.toULong())) }.toStatus()
    }
    public fun disable() { owner.checkNoTxn("indexes.text.disable"); ffi { owner.handle.ffi.textDisable() } }
    public fun rebuild(): TextStatus { owner.checkNoTxn("indexes.text.rebuild"); return ffi { owner.handle.ffi.textRebuild() }.toStatus() }
}
