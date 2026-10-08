package io.github.kclejeune.sparkles.jena

import java.time.Duration
import java.time.Instant
import java.util.Objects

/** One commit of a dataset's history. */
public class CommitInfo internal constructor(
    /** the commit's number; each commit's is one more than its parent's */
    public val seq: Long,
    public val timestamp: Instant,
    /** what made it: `transaction`, `update`, `load`, … */
    public val kind: String,
    /** quads added, relative to the parent commit */
    public val inserted: Long,
    /** quads removed, relative to the parent commit */
    public val deleted: Long,
    /** quads in the dataset after the commit */
    public val quads: Long,
) {
    override fun equals(other: Any?): Boolean = other is CommitInfo && seq == other.seq &&
        timestamp == other.timestamp && kind == other.kind && inserted == other.inserted &&
        deleted == other.deleted && quads == other.quads

    override fun hashCode(): Int = Objects.hash(seq, timestamp, kind, inserted, deleted, quads)

    override fun toString(): String =
        "CommitInfo(seq=$seq, timestamp=$timestamp, kind=$kind, inserted=$inserted, deleted=$deleted, quads=$quads)"
}

/**
 * What a commit did. [isCommitted] is false when the transaction changed nothing, and
 * [commit] is then the head it left unchanged.
 */
public class CommitReceipt internal constructor(
    private val committed: Boolean,
    public val commit: CommitInfo,
    public val datasetId: String,
) {
    public fun isCommitted(): Boolean = committed

    override fun equals(other: Any?): Boolean = other is CommitReceipt && committed == other.committed &&
        commit == other.commit && datasetId == other.datasetId

    override fun hashCode(): Int = Objects.hash(committed, commit, datasetId)

    override fun toString(): String = "CommitReceipt(committed=$committed, commit=$commit, datasetId=$datasetId)"
}

/** Options of [SparklesDatasets.importTdb2]. */
public class ImportOptions private constructor(
    /** add to a Sparkles database that already has data, rather than refusing it */
    public val append: Boolean,
) {
    public fun append(value: Boolean): ImportOptions = ImportOptions(value)

    override fun equals(other: Any?): Boolean = other is ImportOptions && append == other.append
    override fun hashCode(): Int = append.hashCode()
    override fun toString(): String = "ImportOptions(append=$append)"

    public companion object {
        @JvmField
        public val DEFAULT: ImportOptions = ImportOptions(false)
    }
}

/** What [DatasetGraphSparkles.applyPatch] did. */
public class PatchReport internal constructor(
    /** the commit the patch made, or the head when it changed nothing */
    public val receipt: CommitReceipt,
    /** the patch rows read */
    public val rows: Long,
    /** quads the patch added */
    public val inserted: Long,
    /** quads the patch removed */
    public val deleted: Long,
    private val aborted: Boolean,
    private val prevChecked: Boolean,
    /** prefixes the patch set */
    public val prefixesSet: Long,
    /** prefixes the patch removed */
    public val prefixesRemoved: Long,
) {
    /** Whether the patch ended with `TA`, so that nothing was committed. */
    public fun isAborted(): Boolean = aborted

    /** Whether the patch's `H prev` header named a commit of this dataset that was the head. */
    public fun isPrevChecked(): Boolean = prevChecked

    override fun equals(other: Any?): Boolean = other is PatchReport && receipt == other.receipt &&
        rows == other.rows && inserted == other.inserted && deleted == other.deleted &&
        aborted == other.aborted && prevChecked == other.prevChecked &&
        prefixesSet == other.prefixesSet && prefixesRemoved == other.prefixesRemoved

    override fun hashCode(): Int =
        Objects.hash(receipt, rows, inserted, deleted, aborted, prevChecked, prefixesSet, prefixesRemoved)

    override fun toString(): String = "PatchReport(receipt=$receipt, rows=$rows, inserted=$inserted, " +
        "deleted=$deleted, aborted=$aborted, prevChecked=$prevChecked, prefixesSet=$prefixesSet, " +
        "prefixesRemoved=$prefixesRemoved)"
}

/** What [SparklesDatasets.importTdb2] did. */
public class ImportReport internal constructor(
    public val quads: Long,
    public val namedGraphs: Long,
    public val prefixes: Int,
    public val receipt: CommitReceipt,
    public val duration: Duration,
) {
    override fun equals(other: Any?): Boolean = other is ImportReport && quads == other.quads &&
        namedGraphs == other.namedGraphs && prefixes == other.prefixes && receipt == other.receipt &&
        duration == other.duration

    override fun hashCode(): Int = Objects.hash(quads, namedGraphs, prefixes, receipt, duration)

    override fun toString(): String =
        "ImportReport(quads=$quads, namedGraphs=$namedGraphs, prefixes=$prefixes, receipt=$receipt, duration=$duration)"
}

/** Counts of the queries and updates a dataset ran, by where they ran. */
public class DatasetStats internal constructor(
    /** queries that ran in Sparkles */
    public val nativeQueries: Long,
    /** queries that ran in ARQ over `find()` */
    public val fallbackQueries: Long,
    /** update operations that ran in Sparkles */
    public val nativeUpdates: Long,
    /** update operations that ran in ARQ */
    public val fallbackUpdates: Long,
) {
    override fun equals(other: Any?): Boolean = other is DatasetStats && nativeQueries == other.nativeQueries &&
        fallbackQueries == other.fallbackQueries && nativeUpdates == other.nativeUpdates &&
        fallbackUpdates == other.fallbackUpdates

    override fun hashCode(): Int = Objects.hash(nativeQueries, fallbackQueries, nativeUpdates, fallbackUpdates)

    override fun toString(): String = "DatasetStats(nativeQueries=$nativeQueries, fallbackQueries=$fallbackQueries, " +
        "nativeUpdates=$nativeUpdates, fallbackUpdates=$fallbackUpdates)"
}
