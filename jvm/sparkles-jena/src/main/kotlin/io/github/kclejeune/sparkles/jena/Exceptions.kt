package io.github.kclejeune.sparkles.jena

import org.apache.jena.query.QueryExecException
import org.apache.jena.query.QueryParseException
import org.apache.jena.riot.RiotException
import org.apache.jena.shared.JenaException
import org.apache.jena.sparql.JenaTransactionException

/**
 * Implemented by every exception that Sparkles raises, alongside the Jena exception class
 * that Jena raises in the same situation, so a caller can catch Jena's type or ask whether
 * Sparkles raised it.
 */
public interface SparklesError {
    /** The engine's name for the kind of error, such as `SparqlSyntax` or `Conflict`. */
    public val kind: String

    /** The engine's message. */
    public val engineMessage: String
}

/** A SPARQL syntax error that Sparkles found in a query or update. */
public class SparklesQueryParseException(
    override val kind: String,
    override val engineMessage: String,
    line: Int,
    column: Int,
) : QueryParseException(engineMessage, line, column), SparklesError

/** RDF data that Sparkles could not parse. */
public class SparklesRiotException(
    override val kind: String,
    override val engineMessage: String,
) : RiotException(engineMessage), SparklesError

/** A request went past one of its budgets (rows, memory, rows produced). */
public class SparklesBudgetExceededException(
    override val kind: String,
    override val engineMessage: String,
    /** which budget: `rows`, `memory`, `rows-produced`, … */
    public val budget: String?,
    public val limit: Long?,
    public val requested: Long?,
) : QueryExecException(engineMessage), SparklesError

/** Something this build of Sparkles does not support. */
public class SparklesUnsupportedException(
    override val kind: String,
    override val engineMessage: String,
) : QueryExecException(engineMessage), SparklesError

/** An invalid argument or request. */
public class SparklesInvalidException(
    override val kind: String,
    override val engineMessage: String,
) : JenaException(engineMessage), SparklesError

/** A write that conflicts with another, or a transaction that cannot go on. */
public class SparklesTransactionException(
    override val kind: String,
    override val engineMessage: String,
) : JenaTransactionException(engineMessage), SparklesError

/** A write that a write guard rejected; nothing was written. */
public class SparklesWriteRejectedException(
    override val kind: String,
    override val engineMessage: String,
) : JenaTransactionException(engineMessage), SparklesError

/** A commit or other named thing that does not exist. */
public class SparklesNotFoundException(
    override val kind: String,
    override val engineMessage: String,
) : JenaException(engineMessage), SparklesError

/** An operation that the dataset's settings do not permit. */
public class SparklesNotPermittedException(
    override val kind: String,
    override val engineMessage: String,
) : JenaException(engineMessage), SparklesError

/** A failure of the storage: I/O, corruption, a full disk. */
public open class SparklesStorageException(
    override val kind: String,
    override val engineMessage: String,
) : JenaException(engineMessage), SparklesError

/** The database directory is open in another process. */
public class SparklesDatasetLockedException(
    kind: String,
    engineMessage: String,
) : SparklesStorageException(kind, engineMessage)

/** A failure inside the native library, such as a panic. */
public class SparklesInternalException(
    override val kind: String,
    override val engineMessage: String,
) : JenaException(engineMessage), SparklesError
