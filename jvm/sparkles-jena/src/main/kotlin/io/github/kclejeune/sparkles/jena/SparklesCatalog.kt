package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.NativeLoader
import io.github.kclejeune.sparkles.jena.internal.Registry
import io.github.kclejeune.sparkles.jena.internal.ffi
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiCatalog
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiDataset
import java.nio.file.Path
import org.apache.jena.atlas.json.JsonArray
import org.apache.jena.atlas.json.JsonObject

public data class DatasetInfo(public val name: String, public val id: String, public val memory: Boolean, public val path: Path?, public val attached: Boolean, public val reservedBy: String?)
private fun io.github.kclejeune.sparkles.jena.internal.ffi.DatasetInfo.toInfo(): DatasetInfo = DatasetInfo(name, id, memory, path?.let(Path::of), attached, reservedBy)

/** A registry of dataset aliases. Every returned dataset belongs to this catalog's lifecycle. */
public class SparklesCatalog private constructor(private val native: FfiCatalog, private val options: SparklesOptions, private val memory: Boolean) : AutoCloseable {
    private val datasets = LinkedHashSet<DatasetGraphSparkles>()
    @Volatile private var closed = false
    public companion object {
        @JvmStatic @JvmOverloads public fun open(path: Path, options: SparklesOptions = SparklesOptions.builder().build()): SparklesCatalog {
            NativeLoader.load()
            return SparklesCatalog(ffi { FfiCatalog.open(path.toString(), Registry.nativeOptions(options)) }, options, false)
        }
        @JvmStatic public fun inspect(path: Path): List<DatasetInfo> { NativeLoader.load(); return ffi { io.github.kclejeune.sparkles.jena.internal.ffi.catalogInspect(path.toString()) }.map { it.toInfo() } }
        @JvmStatic @JvmOverloads public fun memory(options: SparklesOptions = SparklesOptions.builder().build()): SparklesCatalog {
            NativeLoader.load()
            return SparklesCatalog(ffi { FfiCatalog.memory(Registry.nativeOptions(options)) }, options, true)
        }
    }
    internal fun checkOpen() { if (closed) throw SparklesInvalidException("Closed", "the catalog is closed") }
    @Synchronized private fun wrap(ds: FfiDataset): DatasetGraphSparkles = DatasetGraphSparkles(Registry.fromNative(ds, options), options).also { datasets.add(it) }
    public fun list(): List<DatasetInfo> { checkOpen(); return ffi { native.list() }.map { it.toInfo() } }
    public fun info(name: String): DatasetInfo? { checkOpen(); return ffi { native.info(name) }?.toInfo() }
    public fun get(name: String): DatasetGraphSparkles? { checkOpen(); return ffi { native.get(name) }?.let(::wrap) }
    public fun getById(id: String): DatasetGraphSparkles? { checkOpen(); return ffi { native.getById(java.util.UUID.fromString(id).toString()) }?.let(::wrap) }
    @JvmOverloads public fun create(name: String, inMemory: Boolean = memory): DatasetGraphSparkles { checkOpen(); return wrap(ffi { native.create(name, inMemory) }) }
    @JvmOverloads public fun attach(name: String, path: Path? = null): DatasetGraphSparkles { checkOpen(); return wrap(ffi { native.attach(name, path?.toString()) }) }
    public fun delete(name: String): Boolean { checkOpen(); return ffi { native.delete(name) } }
    /** Persistent datasets must have all handles closed before their directory can move. */
    public fun rename(from: String, to: String): DatasetGraphSparkles { checkOpen(); return wrap(ffi { native.rename(from, to) }) }
    public fun backupFiles(): Map<String, Path> { checkOpen(); return ffi { native.backupFiles() }.associate { it.name to Path.of(it.path) } }
    public fun reserve(name: String, kind: ReservationKind, holder: String): CatalogReservation { checkOpen(); return CatalogReservation(ffi { native.reserve(name, kind.name.lowercase(), holder) }) }
    private fun checkCapture(name: String) { checkOpen(); info(name)?.let { Registry.checkNoTransactionsForDataset(it.id) } }
    @JvmOverloads public fun cloneDataset(source: String, target: String, inMemory: Boolean? = null): DatasetGraphSparkles = SparklesOperation().use { cloneDataset(source, target, inMemory, it) }
    public fun cloneDataset(source: String, target: String, inMemory: Boolean?, operation: SparklesOperation): DatasetGraphSparkles { checkCapture(source); return wrap(ffi { native.cloneDataset(source, target, inMemory, operation.native) }) }
    /** Restore creates a persistent dataset and requires a catalog directory. */
    public fun restore(repository: SparklesBackupRepository, backup: String, target: String): DatasetGraphSparkles = SparklesOperation().use { restore(repository, backup, target, it) }
    public fun restore(repository: SparklesBackupRepository, backup: String, target: String, operation: SparklesOperation): DatasetGraphSparkles { checkOpen(); return wrap(ffi { native.restore(repository.native, backup, target, operation.native) }) }
    public fun repositories(): CatalogRepositories { checkOpen(); return CatalogRepositories(this, native) }
    public fun runPolicy(config: JsonObject): JsonObject = SparklesOperation().use { runPolicy(config, it) }
    public fun runPolicy(config: JsonObject, operation: SparklesOperation): JsonObject { checkOpen(); list().forEach { Registry.checkNoTransactionsForDataset(it.id) }; return document(ffi { native.runPolicy(config.bytes(), operation.native) }).asObject }
    @JvmOverloads public fun applyRetention(config: JsonObject, dryRun: Boolean = true): JsonObject { checkOpen(); return document(ffi { native.applyRetention(config.bytes(), dryRun) }).asObject }
    @Synchronized override fun close() { if (!closed) { closed = true; datasets.toList().forEach { it.close() }; datasets.clear(); native.close() } }
}

public enum class ReservationKind { CLONE, RESTORE }
/** A catalog reservation is released deterministically when closed. */
public class CatalogReservation internal constructor(private val native: io.github.kclejeune.sparkles.jena.internal.ffi.FfiReservation) : AutoCloseable {
    private var closed = false
    @Synchronized override fun close() { if (!closed) { closed = true; native.release(); native.close() } }
}
public class CatalogRepositories internal constructor(private val owner: SparklesCatalog, private val native: FfiCatalog) {
    public fun list(): JsonArray { owner.checkOpen(); return document(ffi { native.repositoriesList() }).asArray }
    public fun get(name: String): JsonObject? { owner.checkOpen(); val value = document(ffi { native.repositoriesGet(name) }); return if (value.isNull) null else value.asObject }
    public fun add(name: String, url: String): JsonObject { owner.checkOpen(); return document(ffi { native.repositoriesPut(name, url, false) }).asObject }
    public fun update(name: String, url: String): JsonObject { owner.checkOpen(); return document(ffi { native.repositoriesPut(name, url, true) }).asObject }
    public fun remove(name: String): Boolean { owner.checkOpen(); return ffi { native.repositoriesRemove(name) } }
    public fun open(name: String): SparklesBackupRepository { owner.checkOpen(); return SparklesBackupRepository(ffi { native.repositoriesOpen(name) }) }
    public fun withFixed(configurations: JsonArray) { owner.checkOpen(); ffi { native.repositoriesFixed(configurations.bytes()) } }
}
