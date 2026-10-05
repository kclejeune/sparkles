package io.github.kclejeune.sparkles.jena
import org.apache.jena.query.ReadWrite
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.update.UpdateAction
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path

class CatalogAndBranchesTest {
    @Test fun catalog_path_and_main_aliases_retain_one_owner(@TempDir directory: Path) {
        SparklesCatalog.open(directory).use { catalog ->
            val ds = catalog.create("wiki")
            val path = catalog.info("wiki")!!.path!!
            SparklesDatasets.open(path).use { byPath ->
                ds.branch("main").use { main ->
                    ds.begin(ReadWrite.WRITE)
                    assertTrue(byPath.isInTransaction()); assertTrue(main.isInTransaction())
                    assertThrows(JenaTransactionException::class.java) { byPath.begin(ReadWrite.WRITE) }
                    ds.abort(); ds.end()
                    ds.close()
                    SparklesDatasets.open(path).use { reopened ->
                        byPath.begin(ReadWrite.WRITE)
                        assertTrue(reopened.isInTransaction())
                        byPath.abort(); byPath.end()
                    }
                    UpdateAction.parseExecute("INSERT DATA { <urn:example:a> <urn:example:p> 1 }", main)
                }
            }
        }
        SparklesDatasets.open(directory.resolve("databases/wiki")).close()
    }
    @Test fun aliases_share_identity_and_transaction_ownership() {
        SparklesCatalog.memory().use { catalog ->
            val original = catalog.create("wiki")
            val alias = catalog.get("wiki")!!
            assertEquals(original.datasetId(), alias.datasetId())
            original.begin(ReadWrite.WRITE)
            assertThrows(JenaTransactionException::class.java) { alias.begin(ReadWrite.WRITE) }
            original.abort(); original.end()
            val renamed = catalog.rename("wiki", "knowledge")
            assertNull(catalog.get("wiki")); assertEquals(original.datasetId(), renamed.datasetId())
            assertEquals(renamed.datasetId(), catalog.getById(original.datasetId())!!.datasetId())
            val oldId = original.datasetId()
            assertTrue(catalog.delete("knowledge"))
            val replacement = catalog.create("knowledge")
            assertNotEquals(oldId, replacement.datasetId())
            assertEquals(replacement.datasetId(), catalog.get("knowledge")!!.datasetId())
            catalog.close()
            assertTrue(original.isClosed()); assertTrue(alias.isClosed()); assertTrue(replacement.isClosed())
            assertThrows(SparklesInvalidException::class.java) { catalog.list() }
        }
    }
    @Test fun catalog_clone_reservation_and_registered_repository(@TempDir directory: Path) {
        SparklesCatalog.open(directory.resolve("catalog")).use { catalog ->
            val ds = catalog.create("source")
            UpdateAction.parseExecute("INSERT DATA { <urn:example:a> <urn:example:p> 1 }", ds)
            catalog.reserve("reserved", ReservationKind.CLONE, "test").use {
                assertThrows(RuntimeException::class.java) { catalog.create("reserved") }
            }
            catalog.create("reserved").close()
            val copy = catalog.cloneDataset("source", "copy", true)
            assertNotEquals(ds.datasetId(), copy.datasetId())
            QueryExec.dataset(copy).query("ASK { <urn:example:a> <urn:example:p> 1 }").build().use { assertTrue(it.ask()) }
            val repositories = catalog.repositories()
            val url = directory.resolve("repo").toUri().toString()
            SparklesBackupRepository.open(url, true).close()
            repositories.add("local", url)
            assertNotNull(repositories.get("local")); assertEquals(1, repositories.list().size)
            repositories.open("local").use { repo ->
                ds.backups(repo).create("saved")
                val restored = catalog.restore(repo, "saved", "restored")
                QueryExec.dataset(restored).query("ASK { <urn:example:a> <urn:example:p> 1 }").build().use { assertTrue(it.ask()) }
            }
            assertTrue(repositories.remove("local"))
            catalog.close(); assertThrows(SparklesInvalidException::class.java) { repositories.list() }
        }
    }
    @Test fun persistent_catalog_reopens_renamed_dataset(@TempDir directory: Path) {
        var id: String? = null
        SparklesCatalog.open(directory).use { catalog ->
            val ds = catalog.create("before"); id = ds.datasetId()
            UpdateAction.parseExecute("INSERT DATA { <urn:example:a> <urn:example:p> 1 }", ds)
            assertThrows(SparklesTransactionException::class.java) { catalog.rename("before", "after") }
            ds.close()
            catalog.rename("before", "after")
        }
        SparklesCatalog.open(directory).use { catalog ->
            assertNull(catalog.info("before")); assertEquals(id, catalog.info("after")!!.id)
            val ds = catalog.get("after")!!
            QueryExec.dataset(ds).query("ASK { <urn:example:a> <urn:example:p> 1 }").build().use { assertTrue(it.ask()) }
        }
    }
    @Test fun branches_isolate_writes_and_preview_refuses_owned_writer() {
        SparklesDatasets.memory().use { ds ->
            UpdateAction.parseExecute("INSERT DATA { <urn:example:a> <urn:example:p> 1 }", ds)
            val branches = ds.branches(); val info = branches.create("dev")
            ds.branch("dev").use { dev ->
                assertEquals(info.id, dev.datasetId())
                UpdateAction.parseExecute("INSERT DATA { <urn:example:b> <urn:example:p> 2 }", dev)
                QueryExec.dataset(ds).query("ASK { <urn:example:b> ?p ?o }").build().use { assertFalse(it.ask()) }
                val preview = branches.previewMerge("dev"); assertEquals(1L, preview.inserted)
                assertTrue(branches.merge("dev").merged)
                QueryExec.dataset(ds).query("ASK { <urn:example:b> ?p ?o }").build().use { assertTrue(it.ask()) }
                assertEquals(info.id, branches.rename("dev", "work").id)
                branches.note("work", "reviewed"); assertEquals("reviewed", branches.get("work").note)
                branches.protect("work", true); assertTrue(branches.get("work").protected)
                branches.protect("work", false); branches.delete("work")
            }
            assertEquals(listOf("main"), branches.list().map { it.name })
        }
    }
}
