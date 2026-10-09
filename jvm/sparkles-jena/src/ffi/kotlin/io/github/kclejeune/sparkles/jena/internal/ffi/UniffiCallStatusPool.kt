package io.github.kclejeune.sparkles.jena.internal.ffi

/**
 * The call status records of UniFFI's call helper, reused on each thread.
 *
 * The generated helper makes a new `UniffiRustCallStatus` for every native call. That is a
 * JNA `Structure`, so each call allocated two blocks of native memory (the record and its
 * error buffer), registered both with JNA's cleaner and with its table of live memory, and
 * left them to the garbage collector. For a call that does little work in Rust, that
 * bookkeeping and the collector's work on it cost more than the call itself.
 *
 * The build rewrites the helper to take a record from here and to give it back once the
 * call's status has been checked (the `ffiBindings` task in build.gradle.kts). A call made
 * while another is being prepared on the same thread, as when lowering an argument
 * allocates a Rust buffer, takes a record of its own, so a record is never shared by two
 * calls in flight. A record is reset before each use: its code says success and its error
 * buffer is empty, as a new record's are.
 */
internal object UniffiCallStatusPool {
    /** Records kept per thread; more are made when calls nest deeper, and then dropped. */
    private const val KEEP = 8

    private val free: ThreadLocal<ArrayDeque<UniffiRustCallStatus>> =
        ThreadLocal.withInitial { ArrayDeque(KEEP) }

    fun acquire(): UniffiRustCallStatus {
        val status = free.get().removeLastOrNull() ?: return UniffiRustCallStatus()
        status.code = UNIFFI_CALL_SUCCESS
        val buf = status.error_buf
        buf.capacity = 0
        buf.len = 0
        buf.data = null
        return status
    }

    fun release(status: UniffiRustCallStatus) {
        val records = free.get()
        if (records.size < KEEP) records.addLast(status)
    }
}
