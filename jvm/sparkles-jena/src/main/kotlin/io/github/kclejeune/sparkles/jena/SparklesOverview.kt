package io.github.kclejeune.sparkles.jena
import io.github.kclejeune.sparkles.jena.internal.ffi.GroupsOpts
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.atlas.json.JsonString
import org.apache.jena.atlas.json.JsonValue

/** What a graph overview groups: the instances of a class, or the members of a group that an earlier answer named. */
public sealed class GroupsWithin {
    /** The instances of the class with this IRI. */
    public data class OfClass(public val iri: String) : GroupsWithin()

    /** The members of the group with this id, from the `groups` of an earlier answer. */
    public data class OfGroup(public val id: String) : GroupsWithin()

    internal fun value(): JsonObject = JsonObject().also {
        when (this) {
            is OfClass -> it.put("class", iri)
            is OfGroup -> it.put("group", id)
        }
    }
}

/** How a graph overview splits what it groups. */
public sealed class GroupsBy {
    /** Members when there are few of them, else the communities that F14 wrote, else the best property. */
    public object Auto : GroupsBy()

    /** Each instance on its own. */
    public object Members : GroupsBy()

    /** The communities that F14 wrote to this predicate. */
    public data class Community(public val predicate: String) : GroupsBy()

    /** The values of this predicate, in 2 to 48 buckets when they are numbers or dates, 12 when [buckets] is null. */
    public data class Property @JvmOverloads public constructor(public val predicate: String, public val buckets: Int? = null) : GroupsBy()

    internal fun value(): JsonValue = when (this) {
        Auto -> JsonString("auto")
        Members -> JsonString("members")
        is Community -> JsonObject().also { it.put("community", predicate) }
        is Property -> JsonObject().also { j -> j.put("property", predicate); buckets?.let { j.put("buckets", it.toLong()) } }
    }
}

/**
 * A request for the overview of a graph. [limit] is 1 to 5,000 and defaults to 5,000. [at] is a
 * commit, time or named snapshot as `commit:N`, `time:<RFC 3339>` or `snapshot:<name>`, and the
 * default is the head. A dataset pinned with `at` reads its own commit and rejects a request that names a point. The native library checks the
 * request and throws [SparklesInvalidException] for one that is malformed.
 */
public data class GroupsRequest @JvmOverloads public constructor(
    public val within: GroupsWithin,
    public val by: GroupsBy = GroupsBy.Auto,
    public val limit: Int? = null,
    public val at: String? = null,
) {
    internal fun value(): JsonObject = JsonObject().also { j ->
        j.put("within", within.value())
        j.put("by", by.value())
        limit?.let { j.put("limit", it.toLong()) }
        at?.let { j.put("at", it) }
    }
}

/**
 * The view and the budgets of an overview, which are those of a query. A null or empty
 * [defaultGraph] or [namedGraphs] leaves the dataset's own view. The timeout and the cancellation
 * belong to the [SparklesOperation] of the call, whose timeout bounds all of the overview's
 * queries together.
 */
public data class GroupsOptions @JvmOverloads public constructor(
    public val defaultGraph: List<String>? = null,
    public val namedGraphs: List<String>? = null,
    public val includeInferred: Boolean = false,
    public val maxRows: Long? = null,
    public val maxMemoryBytes: Long? = null,
    public val maxRowsProduced: Long? = null,
    public val noCache: Boolean = false,
) {
    internal fun native(union: Boolean): GroupsOpts {
        require((maxRows ?: 0) >= 0 && (maxMemoryBytes ?: 0) >= 0 && (maxRowsProduced ?: 0) >= 0) { "budgets must not be negative" }
        return GroupsOpts(
            unionDefaultGraph = union,
            defaultGraph = defaultGraph.orEmpty(),
            namedGraphs = namedGraphs.orEmpty(),
            includeInferred = includeInferred,
            maxRows = maxRows?.toULong(),
            maxMemoryBytes = maxMemoryBytes?.toULong(),
            maxRowsProduced = maxRowsProduced?.toULong(),
            noCache = noCache,
        )
    }
}

/** The class overview that Explore draws, from `DatasetGraphSparkles.graph()`. */
public class SparklesGraphOverview internal constructor(private val owner: DatasetGraphSparkles) {
    /**
     * The groups of a class or a group, and the edges between them, as the object that API.md
     * describes under "Graph overview", with the options of a query.
     */
    @JvmOverloads public fun groups(request: GroupsRequest, options: GroupsOptions = GroupsOptions()): JsonObject =
        SparklesOperation().use { groups(request, options, it) }

    /**
     * As above, with an [operation] that cancels the call and whose timeout is one deadline for
     * all of the queries of the overview.
     */
    public fun groups(request: GroupsRequest, options: GroupsOptions, operation: SparklesOperation): JsonObject {
        owner.checkOpen()
        return document(owner.overview(request.value(), options.native(owner.options.unionDefaultGraph), operation)).asObject
    }
}
