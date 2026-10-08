package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.engine.QueryEngineSparkles
import io.github.kclejeune.sparkles.jena.engine.UpdateEngineSparkles
import io.github.kclejeune.sparkles.jena.internal.NativeLoader
import org.apache.jena.sparql.util.Symbol
import org.apache.jena.sys.JenaSubsystemLifecycle
import org.apache.jena.sys.JenaSystem

/** Context symbols that Sparkles reads (P04 §3.8), and start-up. */
public object Sparkles {
    private const val NS = "urn:x-sparkles:symbol#"

    /** A point-in-time reference: head, commit:N, time:ISO8601 or snapshot:NAME. */
    @JvmField
    public val AT: Symbol = Symbol.create(NS + "at")

    /** Queries see the union of the named graphs as their default graph (a Boolean). */
    @JvmField
    public val UNION_DEFAULT_GRAPH: Symbol = Symbol.create(NS + "unionDefaultGraph")

    /** Merge the reasoner's `urn:x-sparkles:inferred` graph into the default graph (a Boolean). */
    @JvmField
    public val INCLUDE_INFERRED: Symbol = Symbol.create(NS + "includeInferred")

    /** The most rows of an intermediate result (a number). */
    @JvmField
    public val MAX_ROWS: Symbol = Symbol.create(NS + "maxRows")

    /** The most estimated bytes of intermediate results alive at once (a number). */
    @JvmField
    public val MAX_MEMORY_BYTES: Symbol = Symbol.create(NS + "maxMemoryBytes")

    /** The most rows all operators of a request produce together (a number). */
    @JvmField
    public val MAX_ROWS_PRODUCED: Symbol = Symbol.create(NS + "maxRowsProduced")

    /** Use a fallible engine cursor for query solutions (Boolean; default false). */
    @JvmField
    public val STREAMING_EXECUTION: Symbol = Symbol.create(NS + "streamingExecution")

    /** Reject plans that require full materialization in streaming mode (Boolean). */
    @JvmField
    public val STREAMING_STRICT: Symbol = Symbol.create(NS + "streamingStrict")

    /** Neither read nor write Sparkles' result cache (a Boolean), as for benchmarks. */
    @JvmField
    public val NO_CACHE: Symbol = Symbol.create(NS + "noCache")

    /** The fallback mode of one query: a [SparklesFallback] or its name. */
    @JvmField
    public val FALLBACK: Symbol = Symbol.create(NS + "fallback")

    internal val RESOLVED_AT: Symbol = Symbol.create(NS + "resolvedAt")

    /** The version of this library. */
    @JvmStatic
    public fun version(): String = NativeLoader.version

    /**
     * Initialize Jena and load the native library now, so that a missing or mismatched
     * library fails at start-up rather than at the first open.
     */
    @JvmStatic
    public fun init() {
        JenaSystem.init()
        NativeLoader.load()
    }
}

/**
 * Registers the query and update engines with Jena when Jena initializes (listed in
 * `META-INF/services/org.apache.jena.sys.JenaSubsystemLifecycle`). The native library is
 * loaded by the first dataset opened, not here.
 */
public class InitSparkles : JenaSubsystemLifecycle {
    override fun start() {
        io.github.kclejeune.sparkles.jena.engine.FallbackDetector.initialize()
        io.github.kclejeune.sparkles.jena.assembler.VocabSparkles.register()
        io.github.kclejeune.sparkles.jena.engine.DescribeSparkles.register()
        QueryEngineSparkles.register()
        UpdateEngineSparkles.register()
    }

    override fun stop() {}

    /** After ARQ and TDB2. */
    override fun level(): Int = 60
}
