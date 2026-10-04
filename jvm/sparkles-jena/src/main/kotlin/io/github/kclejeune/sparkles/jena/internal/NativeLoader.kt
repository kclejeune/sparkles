package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.internal.ffi.ffiVersion
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.security.MessageDigest
import java.util.Locale
import java.util.Properties

/**
 * Finds the native library and makes the generated bindings load it (P04 §6.3).
 *
 * The system property `sparkles.native.path` names a library file to use as is.
 * Otherwise the library for this platform is extracted from the jar into
 * `<sparkles.native.dir or java.io.tmpdir>/sparkles-<version>-<hash>/`, under a
 * temporary name and renamed into place, so that JVMs share one copy and an existing
 * copy with the right hash is reused.
 */
internal object NativeLoader {
    /** The UniFFI component's name, the namespace of the generated bindings. */
    private const val COMPONENT = "sparkles_ffi"
    private const val RESOURCE_ROOT = "io/github/kclejeune/sparkles/native"

    /** The library's version, from the jar. */
    val version: String by lazy {
        val p = Properties()
        NativeLoader::class.java.getResourceAsStream("/io/github/kclejeune/sparkles/jena/sparkles-jena.properties")
            ?.use { p.load(it) }
        p.getProperty("version", "0.0.0")
    }

    @Volatile
    private var loaded = false

    @Synchronized
    fun load() {
        if (loaded) return
        val path = System.getProperty("sparkles.native.path")?.takeIf { it.isNotBlank() }?.let { File(it) }
            ?: extract()
        if (!path.isFile) {
            throw UnsatisfiedLinkError("sparkles.native.path names $path, which is not a file")
        }
        System.setProperty("uniffi.component.$COMPONENT.libraryOverride", path.absolutePath)
        val v = try {
            ffiVersion()
        } catch (e: UnsatisfiedLinkError) {
            throw UnsatisfiedLinkError("cannot load the Sparkles native library $path: ${e.message}")
        }
        if (v.encodingVersion.toInt() != ENCODING_VERSION || v.crateVersion != version) {
            throw UnsatisfiedLinkError(
                "the Sparkles native library $path is version ${v.crateVersion} (encoding " +
                    "${v.encodingVersion}), and this jar is version $version (encoding $ENCODING_VERSION)",
            )
        }
        loaded = true
    }

    /** The platform directory for an `os.name` and `os.arch`, or `null` when none is built. */
    fun platform(osName: String, osArch: String): String? {
        val os = osName.lowercase(Locale.ROOT)
        val arch = when (osArch.lowercase(Locale.ROOT)) {
            "amd64", "x86_64", "x86-64" -> "x86_64"
            "aarch64", "arm64" -> "aarch64"
            else -> return null
        }
        val name = when {
            os.startsWith("linux") -> "linux"
            os.startsWith("mac") || os.startsWith("darwin") -> "macos"
            os.startsWith("windows") -> "windows"
            else -> return null
        }
        return "$name-$arch"
    }

    fun libraryName(platform: String): String = when {
        platform.startsWith("windows") -> "sparkles_ffi.dll"
        platform.startsWith("macos") -> "libsparkles_ffi.dylib"
        else -> "libsparkles_ffi.so"
    }

    /** Whether this Linux uses musl rather than glibc. */
    private fun isMusl(): Boolean {
        val maps = File("/proc/self/maps")
        return try {
            maps.isFile && maps.readLines().any { it.contains("musl") }
        } catch (_: IOException) {
            false
        }
    }

    private fun extract(): File {
        val osName = System.getProperty("os.name")
        val osArch = System.getProperty("os.arch")
        val platform = platform(osName, osArch)
            ?: throw UnsatisfiedLinkError(
                "Sparkles has no native library for $osName on $osArch; build one from " +
                    "crates/sparkles-ffi and name it with the system property sparkles.native.path",
            )
        if (platform.startsWith("linux") && isMusl()) {
            throw UnsatisfiedLinkError(
                "Sparkles ships native libraries for glibc only, and this Linux uses musl; " +
                    "build one from crates/sparkles-ffi and name it with the system property sparkles.native.path",
            )
        }
        val name = libraryName(platform)
        val resource = "$RESOURCE_ROOT/$platform/$name"
        val loader = NativeLoader::class.java.classLoader
        val expected = loader.getResourceAsStream("$resource.sha256")?.use { String(it.readBytes()).trim() }
        val stream = loader.getResourceAsStream(resource)
        if (stream == null || expected == null) {
            stream?.close()
            throw UnsatisfiedLinkError(
                "this jar has no Sparkles native library for $platform ($resource); add the jar " +
                    "for that platform, or name a library with the system property sparkles.native.path",
            )
        }
        val base = System.getProperty("sparkles.native.dir")?.takeIf { it.isNotBlank() }
            ?: System.getProperty("java.io.tmpdir")
        val dir = Path.of(base, "sparkles-$version-${expected.take(16)}")
        val target = dir.resolve(name)
        stream.use { input ->
            if (Files.isRegularFile(target) && sha256(target) == expected) return target.toFile()
            Files.createDirectories(dir)
            val tmp = Files.createTempFile(dir, name, ".tmp")
            try {
                Files.copy(input, tmp, StandardCopyOption.REPLACE_EXISTING)
                if (sha256(tmp) != expected) {
                    throw UnsatisfiedLinkError("the Sparkles native library in the jar does not match its SHA-256")
                }
                try {
                    Files.move(tmp, target, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
                } catch (_: IOException) {
                    // another JVM put it there first, or the file is in use (Windows)
                    if (!(Files.isRegularFile(target) && sha256(target) == expected)) throw
                        UnsatisfiedLinkError("cannot place the Sparkles native library at $target")
                }
            } finally {
                Files.deleteIfExists(tmp)
            }
        }
        return target.toFile()
    }

    private fun sha256(p: Path): String {
        val md = MessageDigest.getInstance("SHA-256")
        Files.newInputStream(p).use { input ->
            val buf = ByteArray(1 shl 16)
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                md.update(buf, 0, n)
            }
        }
        return md.digest().joinToString("") { "%02x".format(it) }
    }
}
