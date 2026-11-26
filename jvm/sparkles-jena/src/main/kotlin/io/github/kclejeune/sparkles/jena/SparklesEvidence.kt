package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.Registry
import io.github.kclejeune.sparkles.jena.internal.ffi
import org.apache.jena.atlas.json.JsonArray
import org.apache.jena.atlas.json.JsonObject

/**
 * Evidence settings, durable holds, reference resolution, proposals, reviews, withdrawals,
 * identity resolution and dependent assessment lookups (spec C25), from
 * `DatasetGraphSparkles.evidence()`.
 */
public class SparklesEvidence internal constructor(private val owner: DatasetGraphSparkles) {
    private fun checkWrite(what: String) {
        owner.checkNoTxn(what)
        Registry.checkNoTransactionsForDataset(owner.handle.ownerDatasetId)
    }

    private fun checkLiveRead(what: String) {
        owner.checkOpen()
        require(!owner.isPinned()) { "$what runs on the live dataset" }
    }

    /** Read the dataset's recorded evidence settings and currently protected or detached role graphs. */
    public fun settings(): JsonObject {
        owner.checkOpen()
        return document(ffi { owner.handle.ffi.evidenceSettings() }).asObject
    }

    /** Record complete evidence settings and protect every graph of their roles. */
    public fun configure(settings: JsonObject): JsonObject =
        SparklesOperation().use { configure(settings, it) }

    /** Record complete evidence settings with an explicit [operation] for cancellation or timeout. */
    public fun configure(settings: JsonObject, operation: SparklesOperation): JsonObject {
        checkWrite("evidence.configure")
        return document(ffi { owner.handle.ffi.evidenceConfigure(settings.bytes(), operation.native) }).asObject
    }

    /** Record complete evidence settings from a raw UTF-8 JSON string. */
    public fun configure(settingsJson: String): JsonObject =
        SparklesOperation().use { configure(settingsJson, it) }

    /** Record complete evidence settings from a raw UTF-8 JSON string with an explicit [operation]. */
    public fun configure(settingsJson: String, operation: SparklesOperation): JsonObject {
        checkWrite("evidence.configure")
        return document(
            ffi {
                owner.handle.ffi.evidenceConfigure(
                    settingsJson.toByteArray(Charsets.UTF_8),
                    operation.native,
                )
            },
        ).asObject
    }

    /** Stop protecting [graph], a graph of the evidence settings' roles, so ordinary writes can change it. */
    public fun detach(graph: String): JsonObject {
        checkWrite("evidence.detach")
        return document(ffi { owner.handle.ffi.evidenceDetach(graph) }).asObject
    }

    /** List durable evidence source holds, optionally filtered by branch and paginated by id. */
    @JvmOverloads
    public fun holds(
        onBranch: String? = null,
        after: String? = null,
        limit: Int? = null,
    ): JsonObject {
        owner.checkOpen()
        require(limit == null || limit > 0) { "limit must be positive" }
        return document(ffi { owner.handle.ffi.evidenceHolds(onBranch, after, limit?.toUInt()) }).asObject
    }

    /** Acquire a durable evidence hold over [commits], each `{ "commit": <seq>, "branch"?, "branchId"? }`. */
    @JvmOverloads
    public fun acquireHold(commits: JsonArray, id: String? = null): JsonObject {
        val req = JsonObject().also { j ->
            id?.let { j.put("id", it) }
            j.put("commits", commits)
        }
        return acquireHold(req)
    }

    /** Acquire a durable evidence hold from a request object `{ "id"?, "commits": [...] }`. */
    public fun acquireHold(request: JsonObject): JsonObject {
        checkWrite("evidence.acquireHold")
        return document(ffi { owner.handle.ffi.evidenceAcquireHold(request.bytes()) }).asObject
    }

    /** Release the durable evidence hold [id], returning whether it existed. */
    public fun releaseHold(id: String): Boolean {
        checkWrite("evidence.releaseHold")
        return ffi { owner.handle.ffi.evidenceReleaseHold(id) }
    }

    /** Verify stored-fact and record references in order under the 64 MiB preparation allowance. */
    @JvmOverloads
    public fun resolve(references: JsonArray, branch: String? = null): JsonObject =
        SparklesOperation().use { resolve(references, branch, it) }

    /** Verify stored-fact and record references with an explicit [operation]. */
    public fun resolve(
        references: JsonArray,
        branch: String?,
        operation: SparklesOperation,
    ): JsonObject {
        val req = JsonObject().also { it.put("references", references) }
        return resolve(req, branch, operation)
    }

    /** Verify stored-fact and record references from a request object `{ "references": [...] }`. */
    @JvmOverloads
    public fun resolve(request: JsonObject, branch: String? = null): JsonObject =
        SparklesOperation().use { resolve(request, branch, it) }

    /** Verify stored-fact and record references from a request object with an explicit [operation]. */
    public fun resolve(
        request: JsonObject,
        branch: String?,
        operation: SparklesOperation,
    ): JsonObject {
        checkLiveRead("evidence.resolve")
        return document(
            ffi { owner.handle.ffi.evidenceResolve(branch, request.bytes(), operation.native) },
        ).asObject
    }

    /** Publish an authenticated batch of source records, references, claims, links, assessments or identities. */
    @JvmOverloads
    public fun propose(
        proposal: JsonObject,
        branch: String? = null,
        principal: String? = null,
        author: String? = null,
    ): JsonObject = SparklesOperation().use { propose(proposal, branch, principal, author, it) }

    /** Publish an authenticated evidence proposal batch with an explicit [operation]. */
    public fun propose(
        proposal: JsonObject,
        branch: String?,
        principal: String?,
        author: String?,
        operation: SparklesOperation,
    ): JsonObject {
        checkWrite("evidence.propose")
        return document(
            ffi {
                owner.handle.ffi.evidencePropose(
                    branch,
                    principal,
                    author,
                    proposal.bytes(),
                    operation.native,
                )
            },
        ).asObject
    }

    /** Append immutable review decisions under a named review or matching policy. */
    @JvmOverloads
    public fun review(
        review: JsonObject,
        branch: String? = null,
        principal: String? = null,
        author: String? = null,
    ): JsonObject = SparklesOperation().use { review(review, branch, principal, author, it) }

    /** Append immutable review decisions with an explicit [operation]. */
    public fun review(
        review: JsonObject,
        branch: String?,
        principal: String?,
        author: String?,
        operation: SparklesOperation,
    ): JsonObject {
        checkWrite("evidence.review")
        return document(
            ffi {
                owner.handle.ffi.evidenceReview(
                    branch,
                    principal,
                    author,
                    review.bytes(),
                    operation.native,
                )
            },
        ).asObject
    }

    /** Withdraw one or more claim occurrences, updating dependent projections. */
    @JvmOverloads
    public fun withdraw(
        withdrawal: JsonObject,
        branch: String? = null,
        principal: String? = null,
        author: String? = null,
    ): JsonObject = SparklesOperation().use { withdraw(withdrawal, branch, principal, author, it) }

    /** Withdraw one or more claim occurrences with an explicit [operation]. */
    public fun withdraw(
        withdrawal: JsonObject,
        branch: String?,
        principal: String?,
        author: String?,
        operation: SparklesOperation,
    ): JsonObject {
        checkWrite("evidence.withdraw")
        return document(
            ffi {
                owner.handle.ffi.evidenceWithdraw(
                    branch,
                    principal,
                    author,
                    withdrawal.bytes(),
                    operation.native,
                )
            },
        ).asObject
    }

    /** Resolve [entities] through the current resolution map of matching policy [policy]. */
    @JvmOverloads
    public fun resolveEntities(
        policy: String,
        entities: List<String>,
        branch: String? = null,
    ): JsonObject = SparklesOperation().use { resolveEntities(policy, entities, branch, it) }

    /** Resolve [entities] through a matching policy's resolution map with an explicit [operation]. */
    public fun resolveEntities(
        policy: String,
        entities: List<String>,
        branch: String?,
        operation: SparklesOperation,
    ): JsonObject {
        checkLiveRead("evidence.resolveEntities")
        return document(
            ffi { owner.handle.ffi.evidenceResolveEntities(branch, policy, entities, operation.native) },
        ).asObject
    }

    /** Find authenticated assessments that depend on any of [changed]. */
    @JvmOverloads
    public fun findDependents(
        changed: List<String>,
        transitive: Boolean = false,
        branch: String? = null,
    ): JsonObject = SparklesOperation().use { findDependents(changed, transitive, branch, it) }

    /** Find authenticated dependent assessments with an explicit [operation]. */
    public fun findDependents(
        changed: List<String>,
        transitive: Boolean,
        branch: String?,
        operation: SparklesOperation,
    ): JsonObject {
        checkLiveRead("evidence.findDependents")
        return document(
            ffi {
                owner.handle.ffi.evidenceFindDependents(
                    branch,
                    changed,
                    transitive,
                    operation.native,
                )
            },
        ).asObject
    }
}
