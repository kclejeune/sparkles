package io.github.kclejeune.sparkles.jena

import org.apache.jena.sparql.engine.binding.Binding
import org.apache.jena.sparql.exec.QueryExecDatasetBuilder

// Jena 6 removed this builder method. Keep the Jena 5 API test and benchmark without
// replacing initial bindings with substitutions, whose semantics differ.
internal object LegacyInitialBinding {
    private val method = try {
        QueryExecDatasetBuilder::class.java.getMethod("initialBinding", Binding::class.java)
    } catch (_: NoSuchMethodException) {
        null
    }

    val supported: Boolean get() = method != null

    fun requireSupported() {
        check(supported) { "This Jena version has no initialBinding API; sparql-initial-binding requires Jena 5" }
    }

    fun apply(builder: QueryExecDatasetBuilder, binding: Binding): QueryExecDatasetBuilder {
        requireSupported()
        method!!.invoke(builder, binding)
        return builder
    }
}
