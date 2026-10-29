package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.engine.unionDefaultGraph as datasetUnionDefaultGraph
import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.ffi.AlgorithmOpts
import org.apache.jena.atlas.json.JsonArray
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.sparql.exec.QueryExec

/** The graph view and query budgets for algorithm statistics and writes. */
public data class AlgorithmOptions @JvmOverloads public constructor(
    public val unionDefaultGraph: Boolean? = null,
    public val defaultGraph: List<String> = emptyList(),
    public val namedGraphs: List<String> = emptyList(),
    public val includeInferred: Boolean = false,
    public val maxRows: Long? = null,
    public val maxMemoryBytes: Long? = null,
    public val maxRowsProduced: Long? = null,
    public val noCache: Boolean = false,
) {
    internal fun native(owner: DatasetGraphSparkles): AlgorithmOpts {
        require((maxRows ?: 0) >= 0 && (maxMemoryBytes ?: 0) >= 0 && (maxRowsProduced ?: 0) >= 0) { "budgets must not be negative" }
        return AlgorithmOpts(unionDefaultGraph ?: datasetUnionDefaultGraph(owner, owner.context), defaultGraph, namedGraphs, includeInferred, maxRows?.toULong(), maxMemoryBytes?.toULong(), maxRowsProduced?.toULong(), noCache)
    }
}

/** Named projections retained by their dataset. */
public class SparklesProjections internal constructor(private val owner: DatasetGraphSparkles) {
    public fun list(): JsonArray { owner.checkOpen(); return document(ffi { owner.handle.ffi.algorithmProjectionsList() }).asArray }
    public fun get(name: String): JsonObject? { owner.checkOpen(); return ffi { owner.handle.ffi.algorithmProjectionsGet(name) }?.let { document(it).asObject } }
    @JvmOverloads public fun put(name: String, definition: JsonObject, author: String? = null, ifRevision: Long? = null): JsonObject {
        owner.checkNoTxn("algorithms.projections.put"); require(ifRevision == null || ifRevision >= 0)
        return document(ffi { owner.handle.ffi.algorithmProjectionsPut(name, definition.bytes(), author, ifRevision?.toULong()) }).asObject
    }
    @JvmOverloads public fun delete(name: String, ifRevision: Long? = null): Boolean {
        owner.checkNoTxn("algorithms.projections.delete"); require(ifRevision == null || ifRevision >= 0)
        return ffi { owner.handle.ffi.algorithmProjectionsDelete(name, ifRevision?.toULong()) }
    }
}

/** Graph algorithms over the dataset or its retained view. */
public class SparklesAlgorithms internal constructor(private val owner: DatasetGraphSparkles) {
    public fun projections(): SparklesProjections = SparklesProjections(owner)
    @JvmOverloads public fun queryText(request: JsonObject, summary: Boolean = false): String {
        owner.checkOpen(); return ffi { owner.handle.ffi.algorithmQueryText(request.bytes(), summary) }
    }
    /** The caller owns and closes the returned query execution. */
    public fun run(request: JsonObject): QueryExec = QueryExec.dataset(owner).query(queryText(request)).build()
    @JvmOverloads public fun stats(request: JsonObject, options: AlgorithmOptions = AlgorithmOptions()): JsonObject = SparklesOperation().use { stats(request, options, it) }
    public fun stats(request: JsonObject, options: AlgorithmOptions, operation: SparklesOperation): JsonObject {
        owner.checkOpen(); return document(owner.source().algorithmStats(request.bytes(), options.native(owner), operation.native)).asObject
    }
    /** A write uses batched commits; a later failure can leave earlier batches committed. */
    @JvmOverloads public fun runWrite(request: JsonObject, options: AlgorithmOptions = AlgorithmOptions(), author: String? = null): JsonObject = SparklesOperation().use { runWrite(request, options, author, it) }
    public fun runWrite(request: JsonObject, options: AlgorithmOptions, author: String?, operation: SparklesOperation): JsonObject {
        owner.checkNoTxn("algorithms.runWrite")
        return document(ffi { owner.handle.ffi.algorithmRunWrite(request.bytes(), options.native(owner), author, operation.native) }).asObject
    }
}
