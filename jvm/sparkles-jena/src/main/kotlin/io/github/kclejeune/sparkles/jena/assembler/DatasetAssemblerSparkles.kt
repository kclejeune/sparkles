package io.github.kclejeune.sparkles.jena.assembler

import io.github.kclejeune.sparkles.jena.BlankNodeLabels
import io.github.kclejeune.sparkles.jena.SparklesDatasets
import io.github.kclejeune.sparkles.jena.SparklesFallback
import io.github.kclejeune.sparkles.jena.SparklesOptions
import org.apache.jena.assembler.Assembler
import org.apache.jena.rdf.model.Resource
import org.apache.jena.rdf.model.ResourceFactory
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.core.assembler.AssemblerUtils
import org.apache.jena.sparql.core.assembler.DatasetAssembler

/** The assembler vocabulary for a Sparkles dataset in Fuseki or embedded Jena. */
public object VocabSparkles {
    public const val NS: String = "urn:x-sparkles:assembler#"
    @JvmField public val DATASET: Resource = ResourceFactory.createResource(NS + "DatasetSparkles")
    internal fun register() { AssemblerUtils.registerDataset(DATASET, DatasetAssemblerSparkles()) }
}

/** Opens a native Sparkles dataset from a Jena assembler resource. */
public class DatasetAssemblerSparkles : DatasetAssembler() {
    override fun createDataset(assembler: Assembler, root: Resource): DatasetGraph {
        fun value(name: String): String? {
            val statements = root.listProperties(ResourceFactory.createProperty(VocabSparkles.NS + name)).toList()
            require(statements.size <= 1) { "sparkles:$name must have at most one value" }
            return statements.firstOrNull()?.`object`?.asLiteral()?.string
        }
        fun flag(name: String, fallback: Boolean = false): Boolean = value(name)?.let {
            when (it.lowercase()) { "true", "1" -> true; "false", "0" -> false; else -> error("invalid sparkles:$name boolean") }
        } ?: fallback
        val opts = SparklesOptions.builder()
            .unionDefaultGraph(flag("unionDefaultGraph"))
            .autocommit(flag("autocommit"))
            .readOnly(flag("readOnly"))
            .fallback(value("fallback")?.let { SparklesFallback.valueOf(it.uppercase()) } ?: SparklesFallback.AUTO)
            .blankNodeLabels(value("blankNodeLabels")?.let { BlankNodeLabels.valueOf(it.uppercase()) } ?: BlankNodeLabels.DATASET)
            .build()
        val location = value("location")
        val memory = flag("memory")
        require(memory != (location != null)) { "choose exactly one of sparkles:memory true or sparkles:location" }
        val ds = if (memory) SparklesDatasets.memory(opts) else SparklesDatasets.open(java.nio.file.Path.of(location!!), opts)
        try { AssemblerUtils.mergeContext(root, ds.context); return ds }
        catch (e: Throwable) { ds.close(); throw e }
    }
}
