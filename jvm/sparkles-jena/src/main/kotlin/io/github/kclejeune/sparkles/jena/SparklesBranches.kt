package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.Registry
import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.toReceipt
import io.github.kclejeune.sparkles.jena.internal.ffi.MergeSettings

public data class BranchInfo(public val name: String, public val id: String, public val head: Long?, public val upstream: String?, public val ahead: Long, public val behind: Long, public val protected: Boolean, public val note: String?, public val linked: Boolean, public val broken: Boolean)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.BranchInfo.toInfo(): BranchInfo = BranchInfo(name, id, head?.toLong(), upstream, ahead.toLong(), behind.toLong(), protected, note, linked, broken)
public enum class ConflictScope { CELL, SUBJECT, QUAD }
public enum class ConflictResolution { OURS, THEIRS, BASE, UNION }
public data class MergeOptions @JvmOverloads public constructor(public val ffOnly: Boolean = false, public val squash: Boolean = false, public val replay: Boolean = false, public val scope: ConflictScope = ConflictScope.CELL, public val onConflict: ConflictResolution? = null, public val expectSource: Long? = null, public val expectTarget: Long? = null, public val includeInferences: Boolean = false, public val message: String? = null)
private fun MergeOptions.native(): MergeSettings {
    require(expectSource == null || expectSource >= 0); require(expectTarget == null || expectTarget >= 0)
    return MergeSettings(ffOnly, squash, replay, scope.name.lowercase(), onConflict?.name?.lowercase(), expectSource?.toULong(), expectTarget?.toULong(), includeInferences, message)
}
public data class ConflictCell(public val graph: String?, public val subject: String, public val predicate: String?, public val base: List<String>, public val ours: List<String>, public val theirs: List<String>)
public data class MergeReport(public val merged: Boolean, public val upToDate: Boolean, public val fastForward: Boolean, public val squashed: Boolean, public val inserted: Long, public val deleted: Long, public val conflictsFound: Long, public val conflictsResolved: Long, public val conflicts: List<ConflictCell>, public val truncated: Boolean, public val receipt: CommitReceipt?)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.MergeInfo.toReport(): MergeReport = MergeReport(merged, upToDate, fastForward, squashed, inserted.toLong(), deleted.toLong(), conflictsFound.toLong(), conflictsResolved.toLong(), conflicts.map { ConflictCell(it.graph, it.subject, it.predicate, it.base, it.ours, it.theirs) }, truncated, receipt?.toReceipt())
public class SparklesBranches internal constructor(private val owner: DatasetGraphSparkles) {
    @JvmOverloads public fun commitGraph(branches: List<String>? = null, before: String? = null, limit: Int = 100): org.apache.jena.atlas.json.JsonObject {
        require(limit >= 0); owner.checkCapture("branches.commitGraph"); Registry.checkNoTransactionsForDataset(owner.handle.ownerDatasetId)
        return document(ffi { owner.handle.ffi.commitGraph(branches, before, limit.toUInt()) }).asObject
    }
    public fun list(): List<BranchInfo> { owner.checkCapture("branches.list"); Registry.checkNoTransactionsForDataset(owner.handle.ownerDatasetId); return ffi { owner.handle.ffi.branchesList() }.map { it.toInfo() } }
    public fun get(name: String): BranchInfo { capture(listOf(name)); return ffi { owner.handle.ffi.branchesGet(name) }.toInfo() }
    @JvmOverloads public fun create(name: String, from: String = "main", at: String = "head", protected: Boolean = false, note: String? = null): BranchInfo {
        capture(listOf(from)); owner.checkNoTxn("branches.create")
        return ffi { owner.handle.ffi.branchesCreate(name, from, at, protected, note) }.toInfo()
    }
    @JvmOverloads public fun delete(name: String, force: Boolean = false, reparent: Boolean = false) { capture(listOf(name)); owner.checkNoTxn("branches.delete"); ffi { owner.handle.ffi.branchesDelete(name, force, reparent) } }
    public fun rename(name: String, to: String): BranchInfo { capture(listOf(name)); owner.checkNoTxn("branches.rename"); return ffi { owner.handle.ffi.branchesRename(name, to) }.toInfo() }
    public fun protect(name: String, on: Boolean): BranchInfo { capture(listOf(name)); owner.checkNoTxn("branches.protect"); return ffi { owner.handle.ffi.branchesProtect(name, on) }.toInfo() }
    public fun note(name: String, note: String?): BranchInfo { owner.checkNoTxn("branches.note"); return ffi { owner.handle.ffi.branchesNote(name, note) }.toInfo() }
    public fun settings(): List<String> { owner.checkOpen(); return ffi { owner.handle.ffi.branchesSettings() } }
    public fun setSettings(predicates: List<String>): List<String> { owner.checkNoTxn("branches.settings"); return ffi { owner.handle.ffi.branchesSetSettings(predicates) } }
    private fun capture(names: List<String>) { owner.checkCapture("branches.capture"); Registry.checkNoTransactions(ffi { owner.handle.ffi.branchesIds(names) }) }
    @JvmOverloads public fun merge(source: String, target: String = "main", options: MergeOptions = MergeOptions()): MergeReport = SparklesOperation().use { merge(source, target, options, false, it) }
    @JvmOverloads public fun previewMerge(source: String, target: String = "main", options: MergeOptions = MergeOptions()): MergeReport = SparklesOperation().use { merge(source, target, options, true, it) }
    public fun merge(source: String, target: String, options: MergeOptions, preview: Boolean, operation: SparklesOperation): MergeReport { capture(listOf(source, target)); if (!preview) owner.checkNoTxn("branches.merge"); return ffi { owner.handle.ffi.branchesMerge(source, target, options.native(), preview, operation.native) }.toReport() }
    @JvmOverloads public fun revert(branch: String, commit: Long, options: MergeOptions = MergeOptions()): MergeReport = SparklesOperation().use { revert(branch, commit, options, false, it) }
    @JvmOverloads public fun previewRevert(branch: String, commit: Long, options: MergeOptions = MergeOptions()): MergeReport = SparklesOperation().use { revert(branch, commit, options, true, it) }
    public fun revert(branch: String, commit: Long, options: MergeOptions, preview: Boolean, operation: SparklesOperation): MergeReport { require(commit >= 0); capture(listOf(branch)); if (!preview) owner.checkNoTxn("branches.revert"); return ffi { owner.handle.ffi.branchesRevert(branch, commit.toULong(), options.native(), preview, operation.native) }.toReport() }
    @JvmOverloads public fun cherryPick(source: String, commit: Long, target: String = "main", options: MergeOptions = MergeOptions()): MergeReport = SparklesOperation().use { cherryPick(source, commit, target, options, false, it) }
    @JvmOverloads public fun previewCherryPick(source: String, commit: Long, target: String = "main", options: MergeOptions = MergeOptions()): MergeReport = SparklesOperation().use { cherryPick(source, commit, target, options, true, it) }
    public fun cherryPick(source: String, commit: Long, target: String, options: MergeOptions, preview: Boolean, operation: SparklesOperation): MergeReport { require(commit >= 0); capture(listOf(source, target)); if (!preview) owner.checkNoTxn("branches.cherryPick"); return ffi { owner.handle.ffi.branchesCherryPick(source, commit.toULong(), target, options.native(), preview, operation.native) }.toReport() }
}
