package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.internal.ffi.SparklesJni
import io.github.kclejeune.sparkles.jena.internal.ffi.ffiVersion
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.channels.Channels
import java.nio.file.StandardOpenOption
import java.nio.file.attribute.PosixFileAttributeView
import java.nio.file.attribute.PosixFilePermissions
import java.io.InputStream
import java.security.DigestInputStream
import java.security.MessageDigest
import java.util.Locale
import java.util.Properties

/**
 * Finds the native library and makes the generated bindings load it (P04 §6.3).
 *
 * The system property `sparkles.native.path` names a library file to use as is.
 * Otherwise the library for this platform is extracted from the jar into a new
 * directory under `sparkles.native.dir` or `java.io.tmpdir` that only the current user
 * can write. Its SHA-256 is checked while it is written, and the copy is deleted when the
 * JVM exits.
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
        // The hand-written JNI calls load the same library file, unless sparkles.jni=false.
        jni = SparklesJni.ENABLED
        loaded = true
    }

    /** Whether the hottest reads go through the hand-written JNI calls (SparklesJni). */
    @Volatile
    var jni = false
        private set

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
        return stream.use { extractTo(Path.of(base), name, it, expected) }.toFile()
    }

    /** Whether the file system supports POSIX permissions (not Windows). */
    private fun isPosix(dir: Path): Boolean =
        Files.getFileStore(dir).supportsFileAttributeView(PosixFileAttributeView::class.java)

    /**
     * Copy `input` into a new directory under `base` that only this user can write, and
     * return the copy. The SHA-256 is computed from the bytes as they are written, and no
     * other user can replace the file afterwards, so the file that is loaded is the file
     * that was checked. The directory is never shared with another JVM or reused, which
     * closes the window a predictable shared path would leave for a local attacker.
     */
    internal fun extractTo(base: Path, name: String, input: InputStream, expected: String): Path {
        Files.createDirectories(base)
        val posix = isPosix(base)
        if (!posix) cleanStale(base)
        val dir = if (posix) {
            Files.createTempDirectory(base, EXTRACT_PREFIX, PosixFilePermissions.asFileAttribute(PRIVATE))
        } else {
            Files.createTempDirectory(base, EXTRACT_PREFIX)
        }
        val target = dir.resolve(name)
        try {
            val md = MessageDigest.getInstance("SHA-256")
            val attrs = if (posix) arrayOf(PosixFilePermissions.asFileAttribute(PRIVATE)) else emptyArray()
            Files.newByteChannel(target, setOf(StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE), *attrs).use { ch ->
                val out = Channels.newOutputStream(ch)
                DigestInputStream(input, md).copyTo(out, 1 shl 16)
                out.flush()
            }
            if (hex(md.digest()) != expected) {
                throw UnsatisfiedLinkError("the Sparkles native library in the jar does not match its SHA-256")
            }
        } catch (e: Throwable) {
            Files.deleteIfExists(target)
            Files.deleteIfExists(dir)
            throw e
        }
        // A loaded library can be unlinked on POSIX systems but not on Windows, where
        // cleanStale removes copies that no running JVM holds open.
        target.toFile().deleteOnExit()
        dir.toFile().deleteOnExit()
        return target
    }

    /** Delete earlier extractions of this user that no process still has loaded (Windows). */
    private fun cleanStale(base: Path) {
        val me = System.getProperty("user.name")
        try {
            Files.newDirectoryStream(base, "$EXTRACT_PREFIX*").use { dirs ->
                for (d in dirs) {
                    try {
                        if (!Files.isDirectory(d, java.nio.file.LinkOption.NOFOLLOW_LINKS)) continue
                        if (!Files.getOwner(d).name.substringAfterLast('\\').equals(me, ignoreCase = true)) continue
                        Files.list(d).use { files -> files.forEach { Files.deleteIfExists(it) } }
                        Files.deleteIfExists(d)
                    } catch (_: IOException) {
                        // in use by another JVM, or not ours
                    } catch (_: UnsupportedOperationException) {
                    }
                }
            }
        } catch (_: IOException) {
        }
    }

    private const val EXTRACT_PREFIX = "sparkles-native-"
    private val PRIVATE = PosixFilePermissions.fromString("rwx------")

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }
}
