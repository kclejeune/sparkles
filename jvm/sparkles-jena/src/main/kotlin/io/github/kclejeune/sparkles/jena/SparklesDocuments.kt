package io.github.kclejeune.sparkles.jena
import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.ffi.QueryChange
import io.github.kclejeune.sparkles.jena.internal.ffi.SchemaRequest
import org.apache.jena.atlas.json.JSON
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.atlas.json.JsonArray
import org.apache.jena.atlas.json.JsonValue
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.sparql.util.NodeFactoryExtra

internal fun document(bytes: ByteArray): JsonValue = JSON.parseAny(bytes.toString(Charsets.UTF_8))
internal fun JsonValue.bytes(): ByteArray = toString().toByteArray(Charsets.UTF_8)
public data class DefinitionChange @JvmOverloads public constructor(public val author: String? = null, public val message: String? = null, public val ifVersion: Long? = null)
private fun DefinitionChange.native(): QueryChange { require(ifVersion == null || ifVersion >= 0); return QueryChange(author, message, ifVersion?.toULong()) }
public data class SavedQueryDefinition @JvmOverloads public constructor(public val query: String, public val description: String? = null, public val parameters: JsonObject = JsonObject(), public val results: String? = null, public val mcp: Boolean = true) {
    internal fun value(): JsonObject = JsonObject().also { j -> j.put("query", query); description?.let { j.put("description", it) }; j.put("parameters", parameters); results?.let { j.put("results", it) }; j.put("mcp", mcp) }
}
public class SparklesQueries internal constructor(private val owner: DatasetGraphSparkles) {
    /** Pairs of name and full current definition, including its version metadata. */
    public fun list(): JsonArray { owner.checkOpen(); return document(ffi { owner.handle.ffi.queriesList() }).asArray }
    @JvmOverloads public fun get(name: String, version: Long? = null): JsonObject? { owner.checkOpen(); require(version == null || version >= 0); return ffi { owner.handle.ffi.queriesGet(name, version?.toULong()) }?.let { document(it).asObject } }
    public fun versions(name: String): JsonArray? { owner.checkOpen(); return ffi { owner.handle.ffi.queriesVersions(name) }?.let { document(it).asArray } }
    @JvmOverloads public fun put(name: String, definition: SavedQueryDefinition, change: DefinitionChange = DefinitionChange()): JsonObject { owner.checkNoTxn("queries.put"); return document(ffi { owner.handle.ffi.queriesPut(name, definition.value().bytes(), change.native()) }).asObject }
    @JvmOverloads public fun delete(name: String, ifVersion: Long? = null): Boolean { owner.checkNoTxn("queries.delete"); require(ifVersion == null || ifVersion >= 0); return ffi { owner.handle.ffi.queriesDelete(name, ifVersion?.toULong()) } }
    /** The caller owns and closes the returned query execution, as with Jena's query API. */
    @JvmOverloads public fun run(name: String, parameters: JsonObject = JsonObject(), version: Long? = null): QueryExec {
        owner.checkOpen(); require(version == null || version >= 0)
        val bound = ffi { owner.handle.ffi.queriesBind(name, version?.toULong(), parameters.bytes()) }
        val builder = QueryExec.dataset(owner).query(bound.query)
        bound.bindings.forEach { (variable, term) -> builder.substitution(variable, NodeFactoryExtra.parseNode(term)) }
        return builder.build()
    }
}
public data class SchemaOptions @JvmOverloads public constructor(public val at: String? = null, public val limit: Int = 1000, public val cursor: String? = null)
private fun SchemaOptions.native(): SchemaRequest { require(limit >= 0); return SchemaRequest(at, limit.toUInt(), cursor) }
public class SparklesSchema internal constructor(private val owner: DatasetGraphSparkles) {
    @JvmOverloads public fun report(options: SchemaOptions = SchemaOptions()): JsonObject = SparklesOperation().use { report(options, null, it) }
    @JvmOverloads public fun classes(options: SchemaOptions = SchemaOptions()): JsonObject = SparklesOperation().use { report(options, "classes", it) }
    @JvmOverloads public fun predicates(options: SchemaOptions = SchemaOptions()): JsonObject = SparklesOperation().use { report(options, "predicates", it) }
    public fun report(options: SchemaOptions, list: String?, operation: SparklesOperation): JsonObject { owner.checkOpen(); return document(ffi { owner.handle.ffi.schemaReport(options.native(), list, operation.native) }).asObject }
    @JvmOverloads public fun diff(from: String, options: SchemaOptions = SchemaOptions()): JsonObject = SparklesOperation().use { diff(from, options, it) }
    public fun diff(from: String, options: SchemaOptions, operation: SparklesOperation): JsonObject { owner.checkOpen(); return document(ffi { owner.handle.ffi.schemaDiff(from, options.native(), operation.native) }).asObject }
    @JvmOverloads public fun profiles(classes: List<String> = emptyList(), at: String? = null): JsonObject = SparklesOperation().use { profiles(classes, at, it) }
    public fun profiles(classes: List<String>, at: String?, operation: SparklesOperation): JsonObject { owner.checkOpen(); return document(ffi { owner.handle.ffi.schemaProfiles(at, classes, operation.native) }).asObject }
    @JvmOverloads public fun draftShapes(classes: List<String> = emptyList(), support: Double = 1.0, at: String? = null): JsonObject = SparklesOperation().use { draftShapes(classes, support, at, it) }
    public fun draftShapes(classes: List<String>, support: Double, at: String?, operation: SparklesOperation): JsonObject { owner.checkOpen(); require(support.isFinite() && support > 0 && support <= 1); return document(ffi { owner.handle.ffi.schemaDraft(at, classes, support, operation.native) }).asObject }
    @JvmOverloads public fun constraints(shapes: List<String> = emptyList(), at: String? = null): JsonObject { owner.checkOpen(); return document(ffi { owner.handle.ffi.schemaConstraints(at, shapes) }).asObject }
}
public data class GraphQlConfig @JvmOverloads public constructor(public val sdl: String, public val dataGraph: JsonValue = JSON.parseAny("\"default\""), public val reasoning: Boolean? = null, public val introspection: Boolean = true, public val persistedOnly: Boolean = false, public val limits: JsonObject = JsonObject()) {
    internal fun value(): JsonObject = JsonObject().also { j -> j.put("sdl", sdl); j.put("dataGraph", dataGraph); reasoning?.let { j.put("reasoning", it) }; j.put("introspection", introspection); j.put("persistedOnly", persistedOnly); j.put("limits", limits) }
}
public class SparklesGraphQl internal constructor(private val owner: DatasetGraphSparkles) {
    @JvmOverloads public fun get(version: Long? = null): JsonObject? { owner.checkOpen(); require(version == null || version >= 0); return ffi { owner.handle.ffi.graphqlGet(version?.toULong()) }?.let { document(it).asObject } }
    public fun versions(): JsonArray { owner.checkOpen(); return document(ffi { owner.handle.ffi.graphqlVersions() }).asArray }
    @JvmOverloads public fun reset(ifVersion: Long? = null): Boolean { owner.checkNoTxn("graphql.reset"); require(ifVersion == null || ifVersion >= 0); return ffi { owner.handle.ffi.graphqlReset(ifVersion?.toULong()) } }
    @JvmOverloads public fun put(config: GraphQlConfig, change: DefinitionChange = DefinitionChange()): JsonObject { owner.checkNoTxn("graphql.put"); return document(ffi { owner.handle.ffi.graphqlPut(config.value().bytes(), change.native()) }).asObject }
    public fun sdl(): String? { owner.checkOpen(); return ffi { owner.handle.ffi.graphqlSdl() } }
    @JvmOverloads public fun execute(query: String, variables: JsonObject = JsonObject(), operationName: String? = null, at: String? = null): JsonObject = SparklesOperation().use { execute(query, variables, operationName, at, it) }
    public fun execute(query: String, variables: JsonObject, operationName: String?, at: String?, operation: SparklesOperation): JsonObject { owner.checkOpen(); require(!owner.isPinned()) { "use the at argument for historical GraphQL requests" }; return document(ffi { owner.handle.ffi.graphqlExecute(query, operationName, variables.bytes(), at, operation.native) }).asObject }
    @JvmOverloads public fun draft(source: String? = null): JsonObject { owner.checkOpen(); return document(ffi { owner.handle.ffi.graphqlDraft(source) }).asObject }
}
