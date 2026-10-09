import java.security.MessageDigest

plugins { `java-library`; `maven-publish`; signing }
description = "Sparkles native libraries with verified platform resources"
java { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
val repoRoot = rootDir.parentFile
fun hostPlatform(): String {
    val os = System.getProperty("os.name").lowercase()
    val arch = when (val a = System.getProperty("os.arch").lowercase()) { "amd64", "x86_64" -> "x86_64"; "arm64", "aarch64" -> "aarch64"; else -> a }
    return "${when { os.startsWith("linux") -> "linux"; os.startsWith("mac") -> "macos"; os.startsWith("windows") -> "windows"; else -> os }}-$arch"
}
abstract class PackNatives : DefaultTask() {
    @get:InputFiles abstract val libraries: ConfigurableFileCollection
    @get:Input abstract val platform: Property<String>
    @get:Input abstract val multiPlatform: Property<Boolean>
    @get:OutputDirectory abstract val output: DirectoryProperty
    @TaskAction fun pack() {
        val root = output.get().asFile
        root.deleteRecursively()
        for (lib in libraries.files.sortedBy { it.path }) {
            val p = if (multiPlatform.get()) lib.parentFile.name else platform.get()
            require(p in setOf("linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-aarch64", "windows-x86_64")) { "unsupported native platform $p" }
            val dir = root.resolve("io/github/kclejeune/sparkles/native/$p").also { it.mkdirs() }
            val target = dir.resolve(lib.name)
            require(!target.exists()) { "duplicate native $p/${lib.name}" }
            lib.copyTo(target)
            val digest = MessageDigest.getInstance("SHA-256").digest(target.readBytes())
            dir.resolve("${lib.name}.sha256").writeText(digest.joinToString("") { "%02x".format(it) } + "\n")
        }
    }
}
val nativeDir = providers.gradleProperty("sparkles.nativeDir")
val nativeProfile = providers.environmentVariable("JVM_PROFILE").orElse("release").map { if (it == "dev") "debug" else it }
val nativeLibraryName = when {
    hostPlatform().startsWith("macos-") -> "libsparkles_ffi.dylib"
    hostPlatform().startsWith("windows-") -> "sparkles_ffi.dll"
    else -> "libsparkles_ffi.so"
}
val nativeLibrary = providers.gradleProperty("sparkles.nativeLib").orElse(nativeProfile.map { repoRoot.resolve("target/$it/$nativeLibraryName").path })
val packNative = tasks.register<PackNatives>("packNative") {
    platform = providers.gradleProperty("sparkles.nativePlatform").orElse(hostPlatform())
    multiPlatform = nativeDir.isPresent
    libraries.from(if (nativeDir.isPresent) fileTree(rootProject.file(nativeDir.get())) { include("**/*.so", "**/*.dylib", "**/*.dll") }
        else rootProject.file(nativeLibrary.get()))
    output = layout.buildDirectory.dir("natives")
}
sourceSets.main { resources.srcDir(packNative) }
tasks.jar {
    from(repoRoot.resolve("LICENSE")) { into("META-INF") }
    from(rootDir.resolve("sparkles-jena/THIRD_PARTY_LICENSES.md")) { into("META-INF") }
    manifest { attributes("Automatic-Module-Name" to "io.github.kclejeune.sparkles.natives", "Implementation-Version" to project.version) }
}
// The platforms the release workflow builds; the loader also knows macos-x86_64 and windows-x86_64.
val platforms = listOf("linux-x86_64", "linux-aarch64", "macos-aarch64")
val classifiers = platforms.map { p -> tasks.register<Jar>("${p.replace('-', '_')}Jar") {
    archiveClassifier = p
    from(packNative) { include("io/github/kclejeune/sparkles/native/$p/**") }
    from(repoRoot.resolve("LICENSE")) { into("META-INF") }
    from(rootDir.resolve("sparkles-jena/THIRD_PARTY_LICENSES.md")) { into("META-INF") }
    onlyIf { packNative.get().output.get().asFile.resolve("io/github/kclejeune/sparkles/native/$p").exists() }
} }
publishing { publications { create<MavenPublication>("maven") { from(components["java"]); classifiers.forEach { artifact(it) } } } }
