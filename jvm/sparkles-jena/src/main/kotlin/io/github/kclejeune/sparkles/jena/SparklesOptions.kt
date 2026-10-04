package io.github.kclejeune.sparkles.jena

import java.util.Objects

/** Where a query runs when it uses something that only Java can evaluate (P04 §3.5). */
public enum class SparklesFallback {
    /** in Sparkles, or in ARQ when the query needs Java */
    AUTO,

    /** in Sparkles; a query that needs Java fails with `QueryExecException` */
    NEVER,

    /** in ARQ over `find()`, always (for comparisons) */
    ALWAYS,
}

/** How blank node labels made by Jena map to stored blank nodes (P04 §3.2). */
public enum class BlankNodeLabels {
    /**
     * A label written in a committed transaction names its node in later transactions,
     * for as long as the dataset is open. It costs about 150 bytes of memory per label.
     */
    DATASET,

    /** Labels are scoped to the transaction that writes them, as in Sparkles' own API. */
    TRANSACTION,
}

/** Where SERVICE and remote `LOAD` may connect. */
public enum class SparklesOutbound {
    /** anywhere the process can reach, loopback and private addresses included, as in Jena */
    OPEN,

    /** the server's rules: public addresses only */
    SERVER,
}

/**
 * The settings of a dataset, built with [builder]. [DEFAULT] holds the defaults.
 */
public class SparklesOptions private constructor(b: Builder) {
    /** Queries see the union of the named graphs as their default graph (TDB2's `unionDefaultGraph`). */
    public val unionDefaultGraph: Boolean = b.unionDefaultGraph
    public val fallback: SparklesFallback = b.fallback
    public val blankNodeLabels: BlankNodeLabels = b.blankNodeLabels

    /** Writes outside a transaction commit one by one, rather than throwing. */
    public val autocommit: Boolean = b.autocommit
    public val readOnly: Boolean = b.readOnly
    public val outboundPolicy: SparklesOutbound = b.outboundPolicy

    /** The most terms a result's term table holds before it restarts (P04 §5.2). */
    public val termCacheSize: Int = b.termCacheSize

    /** A builder that starts from these options. */
    public fun toBuilder(): Builder = Builder()
        .unionDefaultGraph(unionDefaultGraph)
        .fallback(fallback)
        .blankNodeLabels(blankNodeLabels)
        .autocommit(autocommit)
        .readOnly(readOnly)
        .outboundPolicy(outboundPolicy)
        .termCacheSize(termCacheSize)

    override fun equals(other: Any?): Boolean = other is SparklesOptions &&
        unionDefaultGraph == other.unionDefaultGraph && fallback == other.fallback &&
        blankNodeLabels == other.blankNodeLabels && autocommit == other.autocommit &&
        readOnly == other.readOnly && outboundPolicy == other.outboundPolicy &&
        termCacheSize == other.termCacheSize

    override fun hashCode(): Int = Objects.hash(
        unionDefaultGraph, fallback, blankNodeLabels, autocommit, readOnly, outboundPolicy, termCacheSize,
    )

    override fun toString(): String = "SparklesOptions(unionDefaultGraph=$unionDefaultGraph, fallback=$fallback, " +
        "blankNodeLabels=$blankNodeLabels, autocommit=$autocommit, readOnly=$readOnly, " +
        "outboundPolicy=$outboundPolicy, termCacheSize=$termCacheSize)"

    /** Builds [SparklesOptions]. */
    public class Builder internal constructor() {
        internal var unionDefaultGraph = false
        internal var fallback = SparklesFallback.AUTO
        internal var blankNodeLabels = BlankNodeLabels.DATASET
        internal var autocommit = false
        internal var readOnly = false
        internal var outboundPolicy = SparklesOutbound.OPEN
        internal var termCacheSize = 262_144

        public fun unionDefaultGraph(value: Boolean): Builder = apply { unionDefaultGraph = value }
        public fun fallback(value: SparklesFallback): Builder = apply { fallback = value }
        public fun blankNodeLabels(value: BlankNodeLabels): Builder = apply { blankNodeLabels = value }
        public fun autocommit(value: Boolean): Builder = apply { autocommit = value }
        public fun readOnly(value: Boolean): Builder = apply { readOnly = value }
        public fun outboundPolicy(value: SparklesOutbound): Builder = apply { outboundPolicy = value }
        public fun termCacheSize(value: Int): Builder = apply {
            require(value > 0) { "termCacheSize must be positive" }
            termCacheSize = value
        }

        public fun build(): SparklesOptions = SparklesOptions(this)
    }

    public companion object {
        @JvmField
        public val DEFAULT: SparklesOptions = Builder().build()

        @JvmStatic
        public fun builder(): Builder = Builder()
    }
}
