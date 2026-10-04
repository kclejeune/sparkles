package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.BlankNodeLabels
import io.github.kclejeune.sparkles.jena.SparklesOptions
import io.github.kclejeune.sparkles.jena.internal.ffi.BlankNodeMode
import io.github.kclejeune.sparkles.jena.internal.ffi.DatasetOptions
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiDataset
import java.nio.file.Files
import java.nio.file.Path

/**
 * The open native datasets of this JVM by canonical path, so that two opens of one
 * directory share a handle, as TDB2's `StoreConnection` shares a location.
 */
internal object Registry {
    private val open = HashMap<String, Handle>()

    private fun nativeOptions(o: SparklesOptions) = DatasetOptions(
        if (o.blankNodeLabels == BlankNodeLabels.DATASET) BlankNodeMode.DATASET else BlankNodeMode.TRANSACTION,
        o.readOnly,
        o.termCacheSize.toUInt(),
    )

    @Synchronized
    fun open(path: Path, options: SparklesOptions): Handle {
        NativeLoader.load()
        Files.createDirectories(path)
        val key = path.toRealPath().toString()
        open[key]?.let {
            it.refs.incrementAndGet()
            return it
        }
        val ffi = ffi { FfiDataset.open(key, nativeOptions(options)) }
        val h = Handle(key, ffi, options)
        open[key] = h
        return h
    }

    fun memory(options: SparklesOptions): Handle {
        NativeLoader.load()
        return Handle(null, FfiDataset.memory(nativeOptions(options)), options)
    }

    @Synchronized
    fun release(h: Handle) {
        if (h.refs.decrementAndGet() > 0) return
        h.key?.let { open.remove(it) }
        h.shutdown()
    }
}
