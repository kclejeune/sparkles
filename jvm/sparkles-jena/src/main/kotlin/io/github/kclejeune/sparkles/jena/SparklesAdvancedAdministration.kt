package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiBackupRepository
import io.github.kclejeune.sparkles.jena.internal.ffi.VectorSettings
import io.github.kclejeune.sparkles.jena.internal.ffi.GeoSettings
import io.github.kclejeune.sparkles.jena.internal.ffi.ReasonSettings
import io.github.kclejeune.sparkles.jena.internal.ffi.GuardSettings
import org.apache.jena.graph.Graph
import org.apache.jena.graph.Node
import org.apache.jena.rdf.model.ModelFactory
import org.apache.jena.riot.Lang
import org.apache.jena.riot.RDFDataMgr
import org.apache.jena.sparql.util.NodeFactoryExtra
import java.io.ByteArrayInputStream

public enum class VectorMetric { COSINE, EUCLIDEAN, DOT }
public data class VectorOptions @JvmOverloads public constructor(
    public val predicate: String, public val dimension: Int, public val metric: VectorMetric = VectorMetric.COSINE,
    public val model: String? = null, public val approximate: Boolean = true,
)
public data class VectorStatus(public val name: String, public val dimension: Int, public val metric: String, public val state: String)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.VectorInfo.toStatus(): VectorStatus = VectorStatus(name, dimension.toInt(), metric, state)
public class SparklesVectorIndexes internal constructor(private val owner: DatasetGraphSparkles) {
    public fun list(): List<VectorStatus> { owner.checkOpen(); return ffi { owner.handle.ffi.vectorList() }.map { it.toStatus() } }
    public fun get(name: String): VectorStatus? { owner.checkOpen(); return ffi { owner.handle.ffi.vectorGet(name) }?.toStatus() }
    public fun put(name: String, options: VectorOptions): Boolean {
        owner.checkNoTxn("indexes.vector.put"); require(options.dimension > 0)
        return ffi { owner.handle.ffi.vectorPut(name, VectorSettings(options.predicate, options.dimension.toUInt(), options.metric.name.lowercase(), options.model, options.approximate)) }
    }
    public fun delete(name: String) { owner.checkNoTxn("indexes.vector.delete"); ffi { owner.handle.ffi.vectorDelete(name) } }
    public fun rebuild(name: String) { owner.checkNoTxn("indexes.vector.rebuild"); ffi { owner.handle.ffi.vectorRebuild(name) } }
    public fun await(name: String): VectorStatus? { owner.checkOpen(); return ffi { owner.handle.ffi.vectorWait(name) }?.toStatus() }
}
public data class GeoOptions @JvmOverloads public constructor(public val wgs84: Boolean = false, public val queryRewrite: Boolean = true)
public data class GeoStatus(public val enabled: Boolean, public val state: String)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.GeoInfo.toStatus(): GeoStatus = GeoStatus(enabled, state)
public class SparklesGeoIndex internal constructor(private val owner: DatasetGraphSparkles) {
    public fun status(): GeoStatus? { owner.checkOpen(); return ffi { owner.handle.ffi.geoStatus() }?.toStatus() }
    @JvmOverloads public fun enable(options: GeoOptions = GeoOptions()): GeoStatus { owner.checkNoTxn("indexes.geo.enable"); return ffi { owner.handle.ffi.geoEnable(GeoSettings(options.wgs84, options.queryRewrite)) }.toStatus() }
    public fun disable() { owner.checkNoTxn("indexes.geo.disable"); ffi { owner.handle.ffi.geoDisable() } }
    public fun rebuild(): GeoStatus { owner.checkNoTxn("indexes.geo.rebuild"); return ffi { owner.handle.ffi.geoRebuild() }.toStatus() }
    public fun await(): GeoStatus? { owner.checkOpen(); return ffi { owner.handle.ffi.geoWait() }?.toStatus() }
}
public enum class ReasonProfile { RDFS, RDFS_SIMPLE, OWL_RL, RULES }
public data class ReasonOptions @JvmOverloads public constructor(public val profile: ReasonProfile = ReasonProfile.RDFS, public val rules: String? = null, public val incremental: Boolean = false)
public data class ReasonReport(public val profile: String, public val inferred: Long, public val iterations: Long, public val millis: Long, public val warnings: List<String>)
public class SparklesReasoning internal constructor(private val owner: DatasetGraphSparkles) {
    public fun rdfs(): SparklesRdfs = SparklesRdfs(owner)
    @JvmOverloads public fun run(options: ReasonOptions = ReasonOptions()): ReasonReport = SparklesOperation().use { run(options, it) }
    public fun run(options: ReasonOptions, operation: SparklesOperation): ReasonReport {
        owner.checkNoTxn("reasoning.run")
        val profile = when (options.profile) { ReasonProfile.RDFS -> "rdfs"; ReasonProfile.RDFS_SIMPLE -> "rdfs-simple"; ReasonProfile.OWL_RL -> "owl-rl"; ReasonProfile.RULES -> "rules" }
        return ffi { owner.handle.ffi.reasonRun(ReasonSettings(profile, options.rules, options.incremental), operation.native) }.let { ReasonReport(it.profile, it.inferred.toLong(), it.iterations.toLong(), it.millis.toLong(), it.warnings) }
    }
    public fun clear(): Long { owner.checkNoTxn("reasoning.clear"); return ffi { owner.handle.ffi.reasonClear() }.toLong() }
}
public class SparklesRdfs internal constructor(private val owner: DatasetGraphSparkles) {
    public fun enabled(): Boolean { owner.checkOpen(); return ffi { owner.handle.ffi.rdfsEnabled() } }
    public fun set(graph: String) { owner.checkNoTxn("reasoning.rdfs.set"); ffi { owner.handle.ffi.rdfsSetGraph(graph) } }
    public fun reset() { owner.checkNoTxn("reasoning.rdfs.reset"); ffi { owner.handle.ffi.rdfsReset() } }
}
public data class ShaclResult(public val focus: Node, public val path: Node?, public val value: Node?, public val sourceShape: Node, public val severity: String, public val messages: List<String>)
public data class ShaclReport(public val conforms: Boolean, public val results: List<ShaclResult>, public val turtle: String) {
    public fun graph(): Graph = ModelFactory.createDefaultModel().also { RDFDataMgr.read(it, ByteArrayInputStream(turtle.toByteArray(Charsets.UTF_8)), Lang.TURTLE) }.graph
}
public data class ShexResult(public val node: Node, public val shape: String, public val conformant: Boolean, public val reason: String?)
public data class ShexReport(public val conforms: Boolean, public val results: List<ShexResult>, public val warnings: List<String>, public val millis: Long)
public enum class ValidationMode { REJECT, WARN, OFF }
public data class ValidationGuardOptions @JvmOverloads public constructor(
    public val shapes: String, public val format: String = "text/turtle", public val mode: ValidationMode = ValidationMode.REJECT,
    public val includeInferred: Boolean = false, public val reportLimit: Int = 1000, public val timeoutSeconds: Double = 30.0,
)
public data class ValidationGuardStatus(public val state: String, public val conforms: Boolean?, public val blocking: Long)
public class SparklesValidation internal constructor(private val owner: DatasetGraphSparkles) {
    public fun guard(): SparklesValidationGuard = SparklesValidationGuard(owner)
    @JvmOverloads public fun shacl(shapes: String, format: String = "text/turtle"): ShaclReport = SparklesOperation().use { shacl(shapes, format, it) }
    public fun shacl(shapes: String, format: String, operation: SparklesOperation): ShaclReport {
        owner.checkOpen(); require(!owner.isPinned()) { "validation operates on the live dataset" }
        return ffi { owner.handle.ffi.validateShacl(shapes, format, operation.native) }.let { r -> ShaclReport(r.conforms, r.results.map { ShaclResult(NodeFactoryExtra.parseNode(it.focusNode), it.path?.let(NodeFactoryExtra::parseNode), it.value?.let(NodeFactoryExtra::parseNode), NodeFactoryExtra.parseNode(it.sourceShape), it.severity, it.messages) }, r.turtle) }
    }
    public fun shex(schema: String, shapeMap: String): ShexReport = SparklesOperation().use { shex(schema, shapeMap, it) }
    public fun shex(schema: String, shapeMap: String, operation: SparklesOperation): ShexReport {
        owner.checkOpen(); require(!owner.isPinned()) { "validation operates on the live dataset" }
        return ffi { owner.handle.ffi.validateShex(schema, shapeMap, operation.native) }.let { r -> ShexReport(r.conforms, r.results.map { ShexResult(NodeFactoryExtra.parseNode(it.node), it.shape, it.conformant, it.reason) }, r.warnings, r.millis.toLong()) }
    }
}
public class SparklesValidationGuard internal constructor(private val owner: DatasetGraphSparkles) {
    public fun status(): ValidationGuardStatus? { owner.checkOpen(); return ffi { owner.handle.ffi.guardStatus() }?.let { ValidationGuardStatus(it.status, it.conforms, it.blocking.toLong()) } }
    public fun set(options: ValidationGuardOptions): ValidationGuardStatus {
        owner.checkNoTxn("validation.guard.set"); require(options.reportLimit >= 0); require(options.timeoutSeconds.isFinite() && options.timeoutSeconds > 0)
        return ffi { owner.handle.ffi.guardSetShacl(GuardSettings(options.mode.name.lowercase(), options.shapes, options.format, options.includeInferred, options.reportLimit.toUInt(), options.timeoutSeconds)) }.let { ValidationGuardStatus(it.status, it.conforms, it.blocking.toLong()) }
    }
    public fun reset() { owner.checkNoTxn("validation.guard.reset"); ffi { owner.handle.ffi.guardReset() } }
}
public data class BackupInfo(public val name: String, public val repository: String, public val datasetId: String, public val commit: Long, public val quads: Long, public val logicalBytes: Long, public val addedBytes: Long, public val note: String?)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.BackupInfo.toInfo(): BackupInfo = BackupInfo(name, repository, datasetId, commit.toLong(), quads.toLong(), logicalBytes.toLong(), addedBytes.toLong(), note)
public class SparklesBackupRepository internal constructor(internal val native: FfiBackupRepository) : AutoCloseable {
    public companion object {
        @JvmStatic @JvmOverloads public fun open(url: String, initialize: Boolean = false): SparklesBackupRepository { io.github.kclejeune.sparkles.jena.internal.NativeLoader.load(); return SparklesBackupRepository(ffi { FfiBackupRepository.open(url, initialize) }) }
    }
    public fun list(): List<BackupInfo> = ffi { native.list() }.map { it.toInfo() }
    public fun test(): RepositoryTest = ffi { native.test() }.let { RepositoryTest(it.ok, it.conditionalWrites) }
    @JvmOverloads public fun verify(names: List<String> = emptyList(), level: VerifyLevel = VerifyLevel.EXISTS): VerifyReport = SparklesOperation().use { verify(names, level, it) }
    public fun verify(names: List<String>, level: VerifyLevel, operation: SparklesOperation): VerifyReport = ffi { native.verify(names, level.name.lowercase(), operation.native) }.toReport()
    @JvmOverloads public fun gc(dryRun: Boolean = true, graceSeconds: Long = 86400): GcReport = SparklesOperation().use { gc(dryRun, graceSeconds, it) }
    public fun gc(dryRun: Boolean, graceSeconds: Long, operation: SparklesOperation): GcReport {
        require(graceSeconds >= 0)
        return ffi { native.gc(dryRun, graceSeconds.toULong(), operation.native) }.let { GcReport(it.dryRun, it.candidates.toLong(), it.deleted.toLong(), it.deletedBytes.toLong(), it.keptYoung.toLong(), it.storedBytesAfter.toLong(), it.millis.toLong()) }
    }
    public fun locks(): List<RepositoryLock> = ffi { native.locks() }.map { RepositoryLock(it.id, it.kind, it.operation, it.host, it.pid.toLong(), it.created, it.stale) }
    public fun breakLock(id: String): Boolean = ffi { native.breakLock(id) }
    override fun close(): Unit = native.close()
}
public class SparklesBackups internal constructor(private val owner: DatasetGraphSparkles, private val repository: SparklesBackupRepository) {
    public fun get(name: String): BackupInfo? { owner.checkOpen(); return ffi { owner.handle.ffi.backupsGet(repository.native, name) }?.toInfo() }
    @JvmOverloads public fun verify(name: String, level: VerifyLevel = VerifyLevel.EXISTS): VerifyReport = SparklesOperation().use { verify(name, level, it) }
    public fun verify(name: String, level: VerifyLevel, operation: SparklesOperation): VerifyReport { owner.checkCapture("backups.verify"); return ffi { owner.handle.ffi.backupsVerify(repository.native, name, level.name.lowercase(), operation.native) }.toReport() }
    public fun list(): List<BackupInfo> { owner.checkOpen(); return ffi { owner.handle.ffi.backupsList(repository.native) }.map { it.toInfo() } }
    @JvmOverloads public fun create(name: String, note: String? = null): BackupInfo = SparklesOperation().use { create(name, note, it) }
    public fun create(name: String, note: String?, operation: SparklesOperation): BackupInfo { owner.checkCapture("backups.create"); return ffi { owner.handle.ffi.backupsCreate(repository.native, name, note, operation.native) }.toInfo() }
    public fun delete(name: String): Boolean { owner.checkOpen(); return ffi { owner.handle.ffi.backupsDelete(repository.native, name) } }
}
public enum class VerifyLevel { EXISTS, DATA, RESTORE }
public data class BackupVerification(public val name: String, public val status: String, public val missing: List<String>, public val corrupt: List<String>)
public data class VerifyReport(public val level: VerifyLevel, public val status: String, public val backups: List<BackupVerification>, public val millis: Long)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.VerifyInfo.toReport(): VerifyReport = VerifyReport(VerifyLevel.valueOf(level.uppercase()), status, backups.map { BackupVerification(it.name, it.status, it.missing, it.corrupt) }, millis.toLong())
public data class RepositoryTest(public val ok: Boolean, public val conditionalWrites: Boolean)
public data class GcReport(public val dryRun: Boolean, public val candidates: Long, public val deleted: Long, public val deletedBytes: Long, public val keptYoung: Long, public val storedBytesAfter: Long, public val millis: Long)
public data class RepositoryLock(public val id: String, public val kind: String, public val operation: String, public val host: String, public val pid: Long, public val created: String, public val stale: Boolean)
