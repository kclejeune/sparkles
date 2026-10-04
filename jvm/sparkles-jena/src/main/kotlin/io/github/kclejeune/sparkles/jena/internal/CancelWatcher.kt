package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.internal.ffi.FfiQuery
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Passes Jena's cancel signal to queries running in Sparkles. Jena's timeouts set an
 * `AtomicBoolean` in the query's context, which the native query cannot see, so one daemon
 * thread checks the signals of the running queries every 10 ms and cancels the native query
 * whose signal is set.
 */
internal object CancelWatcher {
    private val running = ConcurrentHashMap<FfiQuery, AtomicBoolean>()

    private val timer: ScheduledExecutorService by lazy {
        Executors.newSingleThreadScheduledExecutor { r ->
            Thread(r, "sparkles-cancel").apply { isDaemon = true }
        }.also {
            it.scheduleWithFixedDelay(::poll, 10, 10, TimeUnit.MILLISECONDS)
        }
    }

    private fun poll() {
        for ((q, signal) in running) {
            if (signal.get()) {
                try {
                    q.cancel()
                } catch (_: RuntimeException) {
                    // the query finished and was freed meanwhile
                }
                running.remove(q)
            }
        }
    }

    fun watch(q: FfiQuery, signal: AtomicBoolean) {
        timer
        running[q] = signal
    }

    fun unwatch(q: FfiQuery) {
        running.remove(q)
    }
}
