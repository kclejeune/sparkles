import java.security.MessageDigest
import org.jetbrains.kotlin.gradle.dsl.JvmDefaultMode
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    `java-library`
    alias(libs.plugins.kotlin.jvm)
}

description = "Apache Jena's DatasetGraph, transactions and query engines backed by the Sparkles engine"

// The native library and its generated Kotlin bindings are built outside Gradle (`mise run
// jvm:build`, or the Nix package). These properties name them; the defaults are where the
// mise tasks put them.
val repoRoot: File = rootDir.parentFile
val nativeLib: Provider<RegularFile> =
    providers.gradleProperty("sparkles.nativeLib")
        .map { layout.projectDirectory.file(File(it).absolutePath) }
        .orElse(layout.projectDirectory.file(repoRoot.resolve("target/release/libsparkles_ffi.so").path))
val bindingsDir: Provider<String> =
    providers.gradleProperty("sparkles.bindings")
        .orElse(repoRoot.resolve("target/jvm/uniffi").path)
val nativePlatform: Provider<String> =
    providers.gradleProperty("sparkles.nativePlatform").orElse(hostPlatform())

fun hostPlatform(): String {
    val os = System.getProperty("os.name").lowercase()
    val arch = when (val a = System.getProperty("os.arch").lowercase()) {
        "amd64", "x86_64" -> "x86_64"
        "aarch64", "arm64" -> "aarch64"
        else -> a
    }
    val name = when {
        os.startsWith("linux") -> "linux"
        os.startsWith("mac") || os.startsWith("darwin") -> "macos"
        os.startsWith("windows") -> "windows"
        else -> os
    }
    return "$name-$arch"
}

java {
    toolchain.languageVersion = JavaLanguageVersion.of(17)
    withSourcesJar()
}

// The generated bindings compile on their own: they are public Kotlin that explicit API mode
// would reject, and their warnings are not ours. Their classes go into this jar.
val ffi: SourceSet = sourceSets.create("ffi") {
    kotlin.srcDir(bindingsDir)
}

kotlin {
    jvmToolchain(17)
    explicitApi()
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
        jvmDefault = JvmDefaultMode.NO_COMPATIBILITY
        // deprecations of Jena 5.6 are removals in Jena 6
        allWarningsAsErrors = true
    }
}

tasks.named<KotlinCompile>("compileTestKotlin") {
    compilerOptions.allWarningsAsErrors = false
}

tasks.named<KotlinCompile>("compileFfiKotlin") {
    compilerOptions {
        allWarningsAsErrors = false
        suppressWarnings = true
    }
}

dependencies {
    "ffiImplementation"(libs.jna)
    api(libs.jena.arq)
    api(files(ffi.output.classesDirs).builtBy(tasks.named("compileFfiKotlin")))
    implementation(libs.jna)
    implementation(libs.jspecify)
    implementation(libs.slf4j.api)
    // the TDB2 import; checked for at the call
    compileOnly(libs.jena.tdb2)

    testImplementation(platform(libs.junit.bom))
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.platform.launcher)
    testImplementation(libs.junit4)
    testRuntimeOnly(libs.junit.vintage)
    testImplementation(variantOf(libs.jena.arq) { classifier("tests") })
    testImplementation(variantOf(libs.jena.core) { classifier("tests") })
    testImplementation(variantOf(libs.jena.base) { classifier("tests") })
    testImplementation(libs.jena.tdb2)
    testRuntimeOnly(libs.slf4j.simple)
}

/** Copies the native library into the resource layout the loader reads, with its SHA-256. */
abstract class PackNative : DefaultTask() {
    @get:InputFile
    abstract val library: RegularFileProperty

    @get:Input
    abstract val platform: Property<String>

    @get:OutputDirectory
    abstract val output: DirectoryProperty

    @TaskAction
    fun pack() {
        val lib = library.get().asFile
        val dir = output.get().asFile.resolve("io/github/kclejeune/sparkles/native/${platform.get()}")
        output.get().asFile.deleteRecursively()
        dir.mkdirs()
        val target = dir.resolve(lib.name)
        lib.copyTo(target, overwrite = true)
        val digest = MessageDigest.getInstance("SHA-256").digest(target.readBytes())
        dir.resolve("${lib.name}.sha256").writeText(digest.joinToString("") { "%02x".format(it) } + "\n")
    }
}

val packNative = tasks.register<PackNative>("packNative") {
    library = nativeLib
    platform = nativePlatform
    output = layout.buildDirectory.dir("natives")
}

sourceSets.main {
    resources.srcDir(packNative)
}

tasks.processResources {
    val v = project.version.toString()
    inputs.property("version", v)
    filesMatching("**/sparkles-jena.properties") {
        expand("version" to v)
    }
}

tasks.jar {
    from(ffi.output)
    manifest {
        attributes(
            "Implementation-Title" to "sparkles-jena",
            "Implementation-Version" to project.version,
            "Enable-Native-Access" to "ALL-UNNAMED",
        )
    }
}

tasks.named<Jar>("sourcesJar") {
    from(ffi.allSource)
    // the native library is a resource, not a source
    exclude("io/github/kclejeune/sparkles/native/**")
}

tasks.test {
    useJUnitPlatform()
    maxHeapSize = "2g"
    jvmArgs("-XX:+EnableDynamicAgentLoading")
    systemProperty("org.slf4j.simpleLogger.defaultLogLevel", "warn")
    testLogging {
        events("failed", "skipped")
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
        showStandardStreams = false
    }
}
