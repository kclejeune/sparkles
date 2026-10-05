package io.github.kclejeune.sparkles.jena
import io.github.kclejeune.sparkles.jena.internal.ffi
public data class CompactionOptions @JvmOverloads public constructor(public val enabled: Boolean? = null, public val minDeltaQuads: Long? = null, public val deltaRatio: Double? = null, public val maxDeltaQuads: Long? = null, public val maxDeltaMb: Long? = null, public val maxWalMb: Long? = null, public val idleSeconds: Long? = null, public val maxAgeSeconds: Long? = null, public val minIntervalSeconds: Long? = null, public val partial: String? = null)
private fun Long?.unsigned(): ULong? = this?.also { require(it >= 0) }?.toULong()
public class CompactionSetting internal constructor(private val owner: DatasetGraphSparkles) {
    public fun get(): CompactionOptions { owner.checkOpen(); return ffi { owner.handle.ffi.compactionGet() }.let { CompactionOptions(it.enabled, it.minDeltaQuads?.toLong(), it.deltaRatio, it.maxDeltaQuads?.toLong(), it.maxDeltaMb?.toLong(), it.maxWalMb?.toLong(), it.idleSeconds?.toLong(), it.maxAgeSeconds?.toLong(), it.minIntervalSeconds?.toLong(), it.partial) } }
    public fun set(s: CompactionOptions) { owner.checkNoTxn("settings.compaction.set"); require(s.deltaRatio == null || s.deltaRatio.isFinite()); ffi { owner.handle.ffi.compactionSet(io.github.kclejeune.sparkles.jena.internal.ffi.CompactionSettings(s.enabled, s.minDeltaQuads.unsigned(), s.deltaRatio, s.maxDeltaQuads.unsigned(), s.maxDeltaMb.unsigned(), s.maxWalMb.unsigned(), s.idleSeconds.unsigned(), s.maxAgeSeconds.unsigned(), s.minIntervalSeconds.unsigned(), s.partial)) } }
    public fun reset() { owner.checkNoTxn("settings.compaction.reset"); ffi { owner.handle.ffi.compactionReset() } }
}
public data class QuotaStatus(public val maxBytes: Long?, public val defaultMaxBytes: Long?, public val usedBytes: Long, public val source: String)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.QuotaInfo.toStatus(): QuotaStatus = QuotaStatus(maxBytes?.toLong(), defaultMaxBytes?.toLong(), usedBytes.toLong(), source)
public class QuotaSetting internal constructor(private val owner: DatasetGraphSparkles) {
    public fun get(): QuotaStatus { owner.checkOpen(); return ffi { owner.handle.ffi.quotaGet() }.toStatus() }
    public fun set(maxBytes: Long): QuotaStatus { owner.checkNoTxn("settings.quota.set"); require(maxBytes >= 0); return ffi { owner.handle.ffi.quotaSet(maxBytes.toULong()) }.toStatus() }
    public fun reset(): QuotaStatus { owner.checkNoTxn("settings.quota.reset"); return ffi { owner.handle.ffi.quotaReset() }.toStatus() }
}
public data class SnapshotSchedule(public val prefix: String, public val everyMillis: Long, public val keepLast: Int)
public data class RetentionOptions @JvmOverloads public constructor(public val keepCommits: Long? = null, public val keepAgeMillis: Long? = null, public val maxBytes: Long? = null, public val catalogCommits: Long? = null, public val catalogAgeMillis: Long? = null, public val schedules: List<SnapshotSchedule> = emptyList())
public class RetentionSetting internal constructor(private val owner: DatasetGraphSparkles) {
    public fun get(): RetentionOptions { owner.checkOpen(); return ffi { owner.handle.ffi.retentionGet() }.let { RetentionOptions(it.keepCommits?.toLong(), it.keepAgeMs?.toLong(), it.maxBytes?.toLong(), it.catalogCommits?.toLong(), it.catalogAgeMs?.toLong(), it.schedules.map { s -> SnapshotSchedule(s.prefix, s.everyMs.toLong(), s.keepLast.toInt()) }) } }
    public fun set(r: RetentionOptions) {
        owner.checkNoTxn("settings.retention.set")
        val schedules = r.schedules.map { s -> require(s.everyMillis >= 60000 && s.keepLast > 0); io.github.kclejeune.sparkles.jena.internal.ffi.SnapshotSchedule(s.prefix, s.everyMillis.toULong(), s.keepLast.toUInt()) }
        ffi { owner.handle.ffi.retentionSet(io.github.kclejeune.sparkles.jena.internal.ffi.RetentionSettings(r.keepCommits.unsigned(), r.keepAgeMillis.unsigned(), r.maxBytes.unsigned(), r.catalogCommits.unsigned(), r.catalogAgeMillis.unsigned(), schedules)) }
    }
    public fun reset() { owner.checkNoTxn("settings.retention.reset"); ffi { owner.handle.ffi.retentionReset() } }
}
