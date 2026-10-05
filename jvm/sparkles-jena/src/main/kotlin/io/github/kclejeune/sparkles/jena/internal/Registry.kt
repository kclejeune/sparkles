package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.BlankNodeLabels
import io.github.kclejeune.sparkles.jena.SparklesOptions
import io.github.kclejeune.sparkles.jena.internal.ffi.BlankNodeMode
import io.github.kclejeune.sparkles.jena.internal.ffi.DatasetOptions
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiDataset
import java.nio.file.Files
import java.nio.file.Path

/**
 * The open native datasets of this JVM by UUID, with canonical path aliases. Catalog,
 * branch and standalone opens therefore share transaction ownership and native options.
 */
internal object Registry {
    private val open = HashMap<String, Handle>()
    private val paths = HashMap<String, Handle>()

    internal fun nativeOptions(o: SparklesOptions): DatasetOptions = DatasetOptions(
        if (o.blankNodeLabels == BlankNodeLabels.DATASET) BlankNodeMode.DATASET else BlankNodeMode.TRANSACTION,
        o.readOnly,
        o.termCacheSize.toUInt(),
    )

    @Synchronized
    fun open(path: Path, options: SparklesOptions): Handle {
        NativeLoader.load()
        Files.createDirectories(path)
        val key = path.toRealPath().toString()
        paths[key]?.let {
            require(it.options.readOnly == options.readOnly &&
                it.options.blankNodeLabels == options.blankNodeLabels &&
                it.options.termCacheSize == options.termCacheSize) {
                "the dataset is already open with incompatible native options"
            }
            it.refs.incrementAndGet()
            return it
        }
        val ffi = ffi { FfiDataset.open(key, nativeOptions(options)) }
        return fromNative(ffi, options)
    }

    fun memory(options: SparklesOptions): Handle {
        NativeLoader.load()
        return fromNative(FfiDataset.memory(nativeOptions(options)), options)
    }

    /** Catalog aliases and repeated branch opens share thread ownership by dataset UUID. */
    @Synchronized
    fun fromNative(dataset: FfiDataset, options: SparklesOptions): Handle {
        val key = "uuid:" + ffi { dataset.datasetId() }
        open[key]?.let { old ->
            try {
                require(old.options.readOnly == options.readOnly && old.options.blankNodeLabels == options.blankNodeLabels && old.options.termCacheSize == options.termCacheSize) { "the dataset is already open with incompatible native options" }
                old.refs.incrementAndGet()
                return old
            } finally { dataset.close() }
        }
        try {
            val path = ffi { dataset.directory() }?.let { Path.of(it).toRealPath().toString() }
            return Handle(path ?: key, dataset, options).also {
                open[key] = it
                if (path != null) paths[path] = it
            }
        } catch (e: Throwable) { dataset.close(); throw e }
    }

    @Synchronized
    fun checkNoTransactions(ids: Collection<String>) {
        for (id in ids) open["uuid:$id"]?.let { h ->
            h.checkNoSink()
            if (h.txns.get() != null) throw org.apache.jena.sparql.JenaTransactionException("branch capture runs outside transactions on its source and target")
        }
    }
    @Synchronized
    fun checkNoTransactionsForDataset(id: String) {
        for (handle in open.values) if (handle.ownerDatasetId == id) {
            handle.checkNoSink()
            if (handle.txns.get() != null) throw org.apache.jena.sparql.JenaTransactionException("branch capture runs outside transactions on this dataset's branches")
        }
    }

    @Synchronized
    fun release(h: Handle) {
        if (h.refs.decrementAndGet() > 0) return
        open.entries.removeIf { it.value === h }
        paths.entries.removeIf { it.value === h }
        h.shutdown()
    }
}
