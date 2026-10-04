package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.Registry
import io.github.kclejeune.sparkles.jena.internal.Tdb2Import
import org.apache.jena.query.Dataset
import org.apache.jena.query.DatasetFactory
import org.apache.jena.sys.JenaSystem
import java.nio.file.Path

/** Opens Sparkles datasets (P04 §4.1). */
public object SparklesDatasets {
    /** A persistent dataset in `path`, created if the directory is empty. */
    @JvmStatic
    public fun open(path: Path): DatasetGraphSparkles = open(path, SparklesOptions.DEFAULT)

    /** A persistent dataset in `path`, created if the directory is empty. */
    @JvmStatic
    public fun open(path: String): DatasetGraphSparkles = open(Path.of(path), SparklesOptions.DEFAULT)

    /**
     * A persistent dataset in `path`, created if the directory is empty. Two opens of one
     * directory in this JVM share the native dataset; another process that has it open
     * makes this throw [SparklesDatasetLockedException].
     */
    @JvmStatic
    public fun open(path: Path, options: SparklesOptions): DatasetGraphSparkles {
        JenaSystem.init()
        return DatasetGraphSparkles(Registry.open(path, options), options)
    }

    /** A new, empty in-memory dataset. */
    @JvmStatic
    public fun memory(): DatasetGraphSparkles = memory(SparklesOptions.DEFAULT)

    /** A new, empty in-memory dataset. */
    @JvmStatic
    public fun memory(options: SparklesOptions): DatasetGraphSparkles {
        JenaSystem.init()
        return DatasetGraphSparkles(Registry.memory(options), options)
    }

    /** [open], wrapped as a Jena `Dataset`. */
    @JvmStatic
    public fun openDataset(path: Path): Dataset = DatasetFactory.wrap(open(path))

    /** [open], wrapped as a Jena `Dataset`. */
    @JvmStatic
    public fun openDataset(path: Path, options: SparklesOptions): Dataset = DatasetFactory.wrap(open(path, options))

    /**
     * Copy a TDB2 database into a Sparkles database in one commit (P04 §4.6). It reads the
     * TDB2 database with Jena's own code, so it needs `org.apache.jena:jena-tdb2` on the
     * classpath. The Sparkles database must be empty.
     */
    @JvmStatic
    public fun importTdb2(tdbDir: Path, sparklesDir: Path): ImportReport =
        importTdb2(tdbDir, sparklesDir, ImportOptions.DEFAULT)

    /** [importTdb2], adding to a Sparkles database that has data when `options` say so. */
    @JvmStatic
    public fun importTdb2(tdbDir: Path, sparklesDir: Path, options: ImportOptions): ImportReport {
        try {
            Class.forName("org.apache.jena.tdb2.DatabaseMgr")
        } catch (e: ClassNotFoundException) {
            throw IllegalStateException(
                "importTdb2 reads the TDB2 database with Jena's TDB2: add org.apache.jena:jena-tdb2 to the classpath",
                e,
            )
        }
        return Tdb2Import.run(tdbDir, sparklesDir, options)
    }
}
