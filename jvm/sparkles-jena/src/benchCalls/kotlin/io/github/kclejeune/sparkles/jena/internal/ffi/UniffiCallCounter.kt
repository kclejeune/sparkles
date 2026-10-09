package io.github.kclejeune.sparkles.jena.internal.ffi

import java.util.concurrent.atomic.LongAdder

/**
 * The number of calls into the native library, for the binding comparison's native call
 * counts. Only the counting copy of the generated bindings (the `callCountingBindings`
 * task) increments it, once in each call helper and once in each hand-written JNI call,
 * and only benchmark processes that count calls put that copy on their classpath.
 */
public object UniffiCallCounter {
    /** Every native call, through UniFFI or JNI. */
    @JvmField
    public val calls: LongAdder = LongAdder()

    /** The calls of [calls] that went through the hand-written JNI entry points. */
    @JvmField
    public val jniCalls: LongAdder = LongAdder()
}
