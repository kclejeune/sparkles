package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.SparklesBudgetExceededException
import io.github.kclejeune.sparkles.jena.SparklesDatasetLockedException
import io.github.kclejeune.sparkles.jena.SparklesInternalException
import io.github.kclejeune.sparkles.jena.SparklesInvalidException
import io.github.kclejeune.sparkles.jena.SparklesNotFoundException
import io.github.kclejeune.sparkles.jena.SparklesNotPermittedException
import io.github.kclejeune.sparkles.jena.SparklesQueryParseException
import io.github.kclejeune.sparkles.jena.SparklesRiotException
import io.github.kclejeune.sparkles.jena.SparklesStorageException
import io.github.kclejeune.sparkles.jena.SparklesTransactionException
import io.github.kclejeune.sparkles.jena.SparklesUnsupportedException
import io.github.kclejeune.sparkles.jena.SparklesWriteRejectedException
import io.github.kclejeune.sparkles.jena.internal.ffi.ErrorKind
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiException
import io.github.kclejeune.sparkles.jena.internal.ffi.InternalException
import org.apache.jena.query.QueryCancelledException
import org.apache.jena.query.QueryExecException

private val POSITION = Regex("""(\d+):(\d+)""")

/** The Jena exception for an error of the native library (P04 §4.3). */
internal fun mapError(e: FfiException.Engine): RuntimeException {
    val kind = e.kind.name.lowercase().split('_').joinToString("") { it.replaceFirstChar(Char::uppercase) }
    val msg = e.detail
    return when (e.kind) {
        ErrorKind.SPARQL_SYNTAX -> {
            val m = POSITION.find(msg)
            val line = m?.groupValues?.get(1)?.toIntOrNull() ?: -1
            val col = m?.groupValues?.get(2)?.toIntOrNull() ?: -1
            SparklesQueryParseException(kind, msg, line, col)
        }
        ErrorKind.RDF_PARSE -> SparklesRiotException(kind, msg)
        ErrorKind.TIMEOUT, ErrorKind.CANCELLED -> QueryCancelledException()
        ErrorKind.BUDGET_EXCEEDED ->
            SparklesBudgetExceededException(kind, msg, e.budget, e.limit?.toLong(), e.requested?.toLong())
        ErrorKind.UNSUPPORTED, ErrorKind.HISTORY_UNSUPPORTED -> SparklesUnsupportedException(kind, msg)
        ErrorKind.INVALID, ErrorKind.MALFORMED -> SparklesInvalidException(kind, msg)
        ErrorKind.CONFLICT, ErrorKind.WRITER_BUSY, ErrorKind.PRECONDITION_FAILED, ErrorKind.TRANSACTION_ENDED ->
            SparklesTransactionException(kind, msg)
        ErrorKind.REJECTED, ErrorKind.GUARD_MISSING -> SparklesWriteRejectedException(kind, msg)
        ErrorKind.NOT_FOUND, ErrorKind.HISTORY_GONE -> SparklesNotFoundException(kind, msg)
        ErrorKind.NOT_PERMITTED -> SparklesNotPermittedException(kind, msg)
        ErrorKind.SERVICE -> QueryExecException(msg)
        ErrorKind.CORRUPT, ErrorKind.POISONED, ErrorKind.STORAGE_FULL, ErrorKind.IO -> SparklesStorageException(kind, msg)
        ErrorKind.LOCKED -> SparklesDatasetLockedException(kind, msg)
        ErrorKind.OTHER -> SparklesInternalException(kind, msg)
    }
}

/** Run a call into the native library, mapping its errors to Jena's exceptions. */
internal inline fun <T> ffi(block: () -> T): T {
    try {
        return block()
    } catch (e: FfiException.Engine) {
        throw mapError(e)
    } catch (e: InternalException) {
        throw SparklesInternalException("Internal", e.message ?: "a failure in the native library")
    }
}

/** The kind of a native error, if `e` is one, without mapping it. */
internal fun nativeKind(e: Throwable): ErrorKind? = (e as? FfiException.Engine)?.kind
