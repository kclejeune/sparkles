package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.NativeLoader
import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.ffi.checkIri as nativeCheckIri
import io.github.kclejeune.sparkles.jena.internal.ffi.checkLangtag as nativeCheckLangtag
import io.github.kclejeune.sparkles.jena.internal.ffi.checkData as nativeCheckData
import io.github.kclejeune.sparkles.jena.internal.ffi.convertGeometries as nativeConvertGeometries
import io.github.kclejeune.sparkles.jena.internal.ffi.previewSchedule as nativePreviewSchedule
import io.github.kclejeune.sparkles.jena.internal.ffi.formatDocument
import io.github.kclejeune.sparkles.jena.internal.ffi.lintDocument
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.atlas.json.JsonArray

/** Engine syntax checks and document helpers, independent of any dataset. */
public object SparklesHelpers {
    @JvmStatic public fun checkIri(iri: String): JsonObject { NativeLoader.load(); return document(ffi { nativeCheckIri(iri) }).asObject }
    @JvmStatic public fun checkLanguageTag(tag: String): JsonObject { NativeLoader.load(); return document(ffi { nativeCheckLangtag(tag) }).asObject }
    /** JSON null means valid RDF; an object carries the syntax diagnostic. */
    @JvmStatic @JvmOverloads public fun checkData(text: String, format: String, baseIri: String? = null): org.apache.jena.atlas.json.JsonValue { NativeLoader.load(); return document(ffi { nativeCheckData(text, format, baseIri) }) }
    /** Each literal has either a geometry or an error; one bad literal does not fail the batch. */
    @JvmStatic public fun convertGeometries(items: JsonArray): JsonArray { NativeLoader.load(); return document(ffi { nativeConvertGeometries(items.bytes()) }).asArray }
    @JvmStatic @JvmOverloads public fun previewSchedule(schedule: String, timezone: String = "UTC", count: Int = 5, after: String? = null): List<String> { NativeLoader.load(); require(count in 0..1000); return ffi { nativePreviewSchedule(schedule, timezone, count.toUInt(), after) } }
    @JvmStatic @JvmOverloads public fun format(text: String, language: String, options: JsonObject = JsonObject()): JsonObject = SparklesOperation().use { format(text, language, options, it) }
    @JvmStatic public fun format(text: String, language: String, options: JsonObject, operation: SparklesOperation): JsonObject = document(ffi { formatDocument(text, language, options.bytes(), operation.native) }).asObject
    @JvmStatic @JvmOverloads public fun lint(text: String, language: String, levels: Map<String, String> = emptyMap()): JsonObject = SparklesOperation().use { lint(text, language, levels, it) }
    @JvmStatic public fun lint(text: String, language: String, levels: Map<String, String>, operation: SparklesOperation): JsonObject = document(ffi { lintDocument(text, language, levels, operation.native) }).asObject
}

public data class TextSearchOptions @JvmOverloads public constructor(public val predicates: List<String> = emptyList(), public val language: String? = null, public val graph: String? = null, public val limit: Int = 20, public val highlight: Boolean = true)
public data class GeoFeaturesOptions @JvmOverloads public constructor(public val bbox: List<Double>, public val graph: String? = null, public val predicate: String? = null, public val limit: Int = 5000, public val tolerance: Double? = null)
public data class RecallOptions @JvmOverloads public constructor(public val samples: Int = 100, public val k: Int = 10, public val ef: Int? = null)
public data class EmbeddingEnvironment @JvmOverloads public constructor(public val enabled: Boolean = true, public val allowPrivate: Boolean = false, public val secrets: Map<String, String> = emptyMap())
public data class DiagnosticsOptions @JvmOverloads public constructor(public val checks: List<String> = emptyList(), public val limit: Int = 100, public val includeInferences: Boolean = false, public val graphs: List<String> = emptyList(), public val closure: String = "subclass")
