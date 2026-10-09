package io.github.kclejeune.sparkles.jena.internal.ffi

import java.nio.ByteBuffer

/**
 * Hand-written JNI calls for the reads that small requests make most often (P04 §5.4):
 * beginning a read transaction, `contains`, `find` and its cursor's later batches, and
 * preparing and executing a query, each on the head snapshot or in a read transaction,
 * and freeing the read transactions, queries and cursors these make.
 *
 * A UniFFI call through JNA costs a few microseconds of dispatch, status records and
 * buffer structures, and a method call also clones the object's handle in a call of its
 * own. For these reads that was most of their time. The entry points live in the same
 * native library (`crates/sparkles-ffi/src/jni_calls.rs`), call the same Rust methods as
 * the UniFFI exports and pass the same bytes, so only the transport differs. They work on
 * the objects UniFFI made: a call borrows the object's handle under the object's UniFFI
 * call counter (`uniffiBorrowHandle`, which the `ffiBindings` task adds to the generated
 * classes), so a concurrent `close` frees the object only after the call, as with UniFFI,
 * and objects they return are wrapped in the generated classes.
 *
 * Errors arrive as the same `FfiException` and `InternalException` that UniFFI throws.
 * Each function falls back to its UniFFI call when its group is off. All are off when the
 * system property `sparkles.jni` is `false`, or when the library could not be loaded for
 * JNI (as when another class loader in the JVM has loaded it already). The property can
 * also list the groups to turn on, of `read`, `contains`, `find` and `query`, which the
 * benchmark uses to measure each call on its own.
 */
public object SparklesJni {
    /** The entry points' version, which `jni_calls.rs` must return. */
    private const val ABI_VERSION = 2

    private val groups: Set<String> = System.getProperty("sparkles.jni")?.trim()?.lowercase().let { p ->
        when (p) {
            null, "", "true" -> setOf("read", "contains", "find", "query")
            "false" -> emptySet()
            else -> p.split(',').map { it.trim() }.toSet()
        }
    }

    /** Whether any call goes through JNI. Each flag is read once, so the JIT can fold the checks. */
    @JvmField
    public val ENABLED: Boolean = groups.isNotEmpty() && load()

    /** Beginning a read transaction, and freeing one. */
    @JvmField
    public val READ: Boolean = ENABLED && "read" in groups

    /** `contains`. */
    @JvmField
    public val CONTAINS: Boolean = ENABLED && "contains" in groups

    /** `find` with its first batch, a cursor's later batches, and freeing the cursor. */
    @JvmField
    public val FIND: Boolean = ENABLED && "find" in groups

    /** Preparing and executing a query, and freeing it. */
    @JvmField
    public val QUERY: Boolean = ENABLED && "query" in groups

    private fun load(): Boolean {
        // the library UniFFI loaded, which NativeLoader names before the first call
        val path = System.getProperty("uniffi.component.sparkles_ffi.libraryOverride")?.takeIf { it.isNotBlank() }
            ?: return false
        return try {
            System.load(path)
            abiVersion() == ABI_VERSION
        } catch (_: UnsatisfiedLinkError) {
            false
        } catch (_: SecurityException) {
            false
        }
    }

    private inline fun <R> jni(block: () -> R): R = try {
        block()
    } catch (f: SparklesJniFailure) {
        throw f.toException()
    }

    public fun beginRead(ds: FfiDataset): FfiReadTxn {
        if (!READ) return ds.beginRead()
        // counted native call
        val h = ds.uniffiBorrowHandle { jni { beginRead(it) } }
        return FfiReadTxn(UniffiWithHandle, h)
    }

    /** Whether the dataset applies RDFS on read, through JNI when the `find` group is on. */
    public fun rdfsEnabled(ds: FfiDataset): Boolean {
        if (!FIND) return ds.rdfsEnabled()
        // counted native call
        return ds.uniffiBorrowHandle { jni { rdfsEnabledNative(it) } }
    }

    public fun contains(ds: FfiDataset, pattern: ByteArray): Boolean {
        if (!CONTAINS) return ds.contains(pattern)
        // counted native call
        return ds.uniffiBorrowHandle { jni { containsHead(it, pattern) } }
    }

    public fun contains(t: FfiReadTxn, pattern: ByteArray): Boolean {
        if (!CONTAINS) return t.contains(pattern)
        // counted native call
        return t.uniffiBorrowHandle { jni { containsRead(it, pattern) } }
    }

    public fun find(ds: FfiDataset, pattern: ByteArray, firstRows: Int): FindResult {
        if (!FIND) return ds.find(pattern, firstRows.toUInt())
        val out = LongArray(1)
        // counted native call
        val batch = ds.uniffiBorrowHandle { jni { findHead(it, pattern, firstRows, out) } }
        return findResult(batch, out[0])
    }

    public fun find(t: FfiReadTxn, pattern: ByteArray, firstRows: Int): FindResult {
        if (!FIND) return t.find(pattern, firstRows.toUInt())
        val out = LongArray(1)
        // counted native call
        val batch = t.uniffiBorrowHandle { jni { findRead(it, pattern, firstRows, out) } }
        return findResult(batch, out[0])
    }

    private fun findResult(batch: ByteArray, cursor: Long): FindResult =
        FindResult(batch, if (cursor == 0L) null else FfiCursor(UniffiWithHandle, cursor))

    public fun nextBatch(c: FfiCursor, maxRows: Int): Batch {
        if (!FIND) return c.nextBatch(maxRows.toUInt())
        val out = LongArray(1)
        // counted native call
        val batch = c.uniffiBorrowHandle { jni { cursorNext(it, maxRows, out) } }
        return Batch(batch, out[0] != 0L)
    }

    public fun prepareQuery(ds: FfiDataset, text: String, opts: QueryOpts): FfiQuery {
        if (!QUERY) return ds.prepareQuery(text, opts)
        val t = text.encodeToByteArray()
        val o = encode(opts)
        // counted native call
        val h = ds.uniffiBorrowHandle { jni { prepareHead(it, t, o) } }
        return FfiQuery(UniffiWithHandle, h)
    }

    public fun prepareQuery(txn: FfiReadTxn, text: String, opts: QueryOpts): FfiQuery {
        if (!QUERY) return txn.prepareQuery(text, opts)
        val t = text.encodeToByteArray()
        val o = encode(opts)
        // counted native call
        val h = txn.uniffiBorrowHandle { jni { prepareRead(it, t, o) } }
        return FfiQuery(UniffiWithHandle, h)
    }

    public fun execute(q: FfiQuery, firstRows: Int): Execution {
        if (!QUERY) return q.execute(firstRows.toUInt())
        // counted native call
        val bytes = q.uniffiBorrowHandle { jni { execute(it, firstRows) } }
        return FfiConverterTypeExecution.read(ByteBuffer.wrap(bytes))
    }

    /** The options in UniFFI's serialization of the record, which the Rust side reads back. */
    private fun encode(opts: QueryOpts): ByteArray {
        val buf = ByteBuffer.allocate(FfiConverterTypeQueryOpts.allocationSize(opts).toInt())
        FfiConverterTypeQueryOpts.write(opts, buf)
        return buf.array().copyOf(buf.position())
    }

    // The generated classes' clean actions call these (the `ffiBindings` task), and make
    // UniFFI's call when they return false.

    @JvmStatic
    internal fun freeReadTxn(handle: Long): Boolean {
        if (!READ) return false
        // counted native call
        jni { freeReadTxnNative(handle) }
        return true
    }

    @JvmStatic
    internal fun freeQuery(handle: Long): Boolean {
        if (!QUERY) return false
        // counted native call
        jni { freeQueryNative(handle) }
        return true
    }

    @JvmStatic
    internal fun freeCursor(handle: Long): Boolean {
        if (!FIND) return false
        // counted native call
        jni { freeCursorNative(handle) }
        return true
    }

    /**
     * The failure paths, for the tests: `kind` 1 throws UniFFI's exception for an error, 2
     * panics in Rust and throws its `InternalException`, and others return `kind`.
     */
    public fun selfTest(kind: Int): Int {
        check(ENABLED) { "the JNI calls are off" }
        return jni { selfTestNative(kind) }
    }

    @JvmStatic private external fun selfTestNative(kind: Int): Int
    @JvmStatic private external fun abiVersion(): Int
    @JvmStatic private external fun beginRead(dataset: Long): Long
    @JvmStatic private external fun rdfsEnabledNative(dataset: Long): Boolean
    @JvmStatic private external fun containsHead(dataset: Long, pattern: ByteArray): Boolean
    @JvmStatic private external fun containsRead(txn: Long, pattern: ByteArray): Boolean
    @JvmStatic private external fun findHead(dataset: Long, pattern: ByteArray, firstRows: Int, out: LongArray): ByteArray
    @JvmStatic private external fun findRead(txn: Long, pattern: ByteArray, firstRows: Int, out: LongArray): ByteArray
    @JvmStatic private external fun cursorNext(cursor: Long, maxRows: Int, out: LongArray): ByteArray
    @JvmStatic private external fun prepareHead(dataset: Long, text: ByteArray, options: ByteArray): Long
    @JvmStatic private external fun prepareRead(txn: Long, text: ByteArray, options: ByteArray): Long
    @JvmStatic private external fun execute(query: Long, firstRows: Int): ByteArray

    @JvmStatic private external fun freeReadTxnNative(txn: Long)
    @JvmStatic private external fun freeQueryNative(query: Long)
    @JvmStatic private external fun freeCursorNative(cursor: Long)
}

/**
 * What the JNI entry points throw: an `FfiError` in UniFFI's serialization (`kind` 0), or
 * a panic's message in UTF-8 (`kind` 1). It is replaced at once by the exception UniFFI
 * throws for the same failure, so it carries no stack trace.
 */
internal class SparklesJniFailure(private val kind: Int, private val payload: ByteArray) :
    RuntimeException(null, null, false, false) {
    fun toException(): Exception = when (kind) {
        0 -> FfiConverterTypeFfiError.read(ByteBuffer.wrap(payload))
        else -> InternalException(payload.decodeToString())
    }
}
