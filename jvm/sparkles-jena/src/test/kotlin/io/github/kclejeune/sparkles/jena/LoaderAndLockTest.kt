package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.NativeLoader
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File
import java.nio.file.Path
import java.util.concurrent.TimeUnit

/** Opens a database directory and prints the simple name of what happened (for A3). */
object LockProbe {
    @JvmStatic
    fun main(args: Array<String>) {
        try {
            SparklesDatasets.open(Path.of(args[0])).close()
            println("opened")
        } catch (e: Exception) {
            println(e.javaClass.simpleName)
        }
    }
}

class LoaderAndLockTest {
    @Test
    fun platform_names() {
        assertEquals("linux-x86_64", NativeLoader.platform("Linux", "amd64"))
        assertEquals("linux-aarch64", NativeLoader.platform("Linux", "aarch64"))
        assertEquals("macos-aarch64", NativeLoader.platform("Mac OS X", "aarch64"))
        assertEquals("macos-x86_64", NativeLoader.platform("Mac OS X", "x86_64"))
        assertEquals("windows-x86_64", NativeLoader.platform("Windows 11", "amd64"))
        assertNull(NativeLoader.platform("SunOS", "sparcv9"))
        assertNull(NativeLoader.platform("FreeBSD", "amd64"))
        assertEquals("sparkles_ffi.dll", NativeLoader.libraryName("windows-x86_64"))
        assertEquals("libsparkles_ffi.dylib", NativeLoader.libraryName("macos-aarch64"))
        assertEquals("libsparkles_ffi.so", NativeLoader.libraryName("linux-x86_64"))
    }

    private fun sha256(bytes: ByteArray): String =
        java.security.MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    @Test
    fun extraction_uses_a_fresh_private_directory_and_checks_the_written_bytes(@TempDir base: Path) {
        val bytes = "not really a library".toByteArray()
        val expected = sha256(bytes)
        // A directory planted at a predictable name is never reused.
        val planted = java.nio.file.Files.createDirectories(base.resolve("sparkles-native-planted"))
        java.nio.file.Files.write(planted.resolve("libsparkles_ffi.so"), "evil".toByteArray())

        val first = NativeLoader.extractTo(base, "libsparkles_ffi.so", bytes.inputStream(), expected)
        val second = NativeLoader.extractTo(base, "libsparkles_ffi.so", bytes.inputStream(), expected)
        assertNotEquals(first.parent, second.parent)
        assertNotEquals(planted, first.parent)
        assertArrayEquals(bytes, java.nio.file.Files.readAllBytes(first))
        if (java.nio.file.Files.getFileStore(base).supportsFileAttributeView("posix")) {
            val perms = java.nio.file.attribute.PosixFilePermissions.toString(java.nio.file.Files.getPosixFilePermissions(first.parent))
            assertEquals("rwx------", perms)
            val filePerms = java.nio.file.attribute.PosixFilePermissions.toString(java.nio.file.Files.getPosixFilePermissions(first))
            assertEquals("rwx------", filePerms)
        }

        val before = java.nio.file.Files.list(base).use { it.count() }
        assertThrows(UnsatisfiedLinkError::class.java) {
            NativeLoader.extractTo(base, "libsparkles_ffi.so", "tampered".toByteArray().inputStream(), expected)
        }
        assertEquals(before, java.nio.file.Files.list(base).use { it.count() })
    }

    private fun probe(dir: Path): String {
        val java = File(System.getProperty("java.home"), "bin/java").path
        val p = ProcessBuilder(
            java,
            "-cp",
            System.getProperty("java.class.path"),
            "-Dorg.slf4j.simpleLogger.defaultLogLevel=error",
            LockProbe::class.java.name,
            dir.toString(),
        ).redirectErrorStream(true).start()
        assertTrue(p.waitFor(120, TimeUnit.SECONDS))
        return p.inputStream.bufferedReader().readLines().lastOrNull() ?: ""
    }

    /** A3: a directory open in one JVM is locked against another. */
    @Test
    fun a3_a_second_jvm_cannot_open_the_directory(@TempDir dir: Path) {
        val db = dir.resolve("db")
        SparklesDatasets.open(db).use {
            assertEquals(SparklesDatasetLockedException::class.java.simpleName, probe(db))
        }
        assertEquals("opened", probe(db))
    }
}
