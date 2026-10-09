import org.jetbrains.kotlin.gradle.dsl.JvmDefaultMode
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    `java-library`
    `maven-publish`
    signing
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.dokka)
}

description = "Apache Jena's DatasetGraph, transactions and query engines backed by the Sparkles engine"

// The native library and its generated Kotlin bindings are built outside Gradle (`mise run
// jvm:build`, or the Nix package). These properties name them; the defaults are where the
// mise tasks put them.
val repoRoot: File = rootDir.parentFile
val bindingsDir: Provider<String> = providers.gradleProperty("sparkles.bindings").orElse(repoRoot.resolve("target/jvm/uniffi").path)

java {
    toolchain.languageVersion = JavaLanguageVersion.of(providers.gradleProperty("sparkles.javaVersion").orElse("17").get().toInt())
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
    withSourcesJar()
}
tasks.withType<JavaCompile>().configureEach { options.release = 17 }

// The generated bindings, with UniFFI's call helper changed to reuse its call status
// records (src/ffi/kotlin/.../UniffiCallStatusPool.kt says why). The build fails if the
// helper is not the one this expects, as after a UniFFI upgrade.
val ffiBindings = tasks.register<Sync>("ffiBindings") {
    from(bindingsDir)
    into(layout.buildDirectory.dir("generated/uniffi"))
    doLast {
        val helper = listOf(
            "    var status = UniffiRustCallStatus()",
            "    val return_value = callback(status)",
            "    uniffiCheckCallStatus(errorHandler, status)",
            "    return return_value",
        ).joinToString("\n")
        val pooled = listOf(
            "    val status = UniffiCallStatusPool.acquire()",
            "    try {",
            "        val return_value = callback(status)",
            "        uniffiCheckCallStatus(errorHandler, status)",
            "        return return_value",
            "    } finally {",
            "        UniffiCallStatusPool.release(status)",
            "    }",
        ).joinToString("\n")
        val files = destinationDir.walkTopDown().filter { it.isFile && it.name.endsWith(".kt") }.toList()
        val changed = files.filter { it.readText().contains(helper) }
        check(changed.size == 1) { "expected UniFFI's call helper in one generated file, found it in ${changed.size}" }
        val file = changed.single()
        var text = file.readText().replace(helper, pooled)

        // The hand-written JNI calls (src/ffi/kotlin/.../SparklesJni.kt) borrow an object's
        // handle under its call counter, as UniFFI's own `callWithHandle` does, but without
        // the handle clone that is a native call of its own.
        val callWithHandle = "    internal inline fun <R> callWithHandle(block: (handle: Long) -> R): R {\n"
        val borrow = listOf(
            "    internal inline fun <R> uniffiBorrowHandle(block: (handle: Long) -> R): R {",
            "        do {",
            "            val c = this.callCounter.get()",
            "            if (c == 0L) {",
            "                throw IllegalStateException(\"\${this.javaClass.simpleName} object has already been destroyed\")",
            "            }",
            "            if (c == Long.MAX_VALUE) {",
            "                throw IllegalStateException(\"\${this.javaClass.simpleName} call counter would overflow\")",
            "            }",
            "        } while (! this.callCounter.compareAndSet(c, c + 1L))",
            "        try {",
            "            return block(this.handle)",
            "        } finally {",
            "            if (this.callCounter.decrementAndGet() == 0L) {",
            "                cleanable?.clean()",
            "            }",
            "        }",
            "    }",
            "",
        ).joinToString("\n", postfix = "\n")
        val objects = text.split(callWithHandle).size - 1
        check(objects >= 4) { "expected UniFFI's callWithHandle in every generated object class, found it $objects times" }
        text = text.replace(callWithHandle, borrow + callWithHandle)

        // The objects that the JNI calls make are freed through JNI too, unless they are off.
        for ((ffiName, hook) in listOf("ffireadtxn" to "freeReadTxn", "ffiquery" to "freeQuery", "fficursor" to "freeCursor")) {
            val free = listOf(
                "            uniffiRustCall { status ->",
                "                UniffiLib.uniffi_sparkles_ffi_fn_free_$ffiName(handle, status)",
                "            }",
            ).joinToString("\n")
            check(text.split(free).size == 2) { "expected one clean action for $ffiName in the generated bindings" }
            text = text.replace(free, "            if (!SparklesJni.$hook(handle)) " + free.trimStart())
        }
        file.writeText(text)
    }
}

// The generated bindings compile on their own: they are public Kotlin that explicit API mode
// would reject, and their warnings are not ours. Their classes go into this jar.
val ffi: SourceSet = sourceSets.create("ffi") {
    kotlin.srcDir(ffiBindings)
}

kotlin {
    jvmToolchain(providers.gradleProperty("sparkles.javaVersion").orElse("17").get().toInt())
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
if (providers.gradleProperty("sparkles.jenaVersion").orNull?.startsWith("6.") == true) {
    // Jena 6 removed jena-core's legacy JUnit 3 AbstractTestGraph fixture. The ARQ
    // dataset/graph/transaction suites and our round-trip tests still run on this line.
    kotlin.sourceSets.named("test") { kotlin.exclude("**/contract/TestGraphSparkles.kt") }
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
    runtimeOnly(project(":sparkles-jena-natives"))
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
    testImplementation(libs.jena.rdfconnection)
    testImplementation(libs.jena.fuseki.main)
    testRuntimeOnly(libs.slf4j.simple)
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
    // the project's license, and the notices of the crates the native library links
    from(repoRoot.resolve("LICENSE")) { into("META-INF") }
    from(layout.projectDirectory.file("THIRD_PARTY_LICENSES.md")) { into("META-INF") }
    manifest {
        attributes(
            "Implementation-Title" to "sparkles-jena",
            "Automatic-Module-Name" to "io.github.kclejeune.sparkles.jena",
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

tasks.withType<Test>().configureEach {
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

// The suite again with the hand-written JNI calls off (SparklesJni), so that both paths
// of every call they cover give the same answers and errors.
val testUniffi = tasks.register<Test>("testUniffi") {
    description = "Runs the tests with -Dsparkles.jni=false, on UniFFI's calls alone"
    group = "verification"
    testClassesDirs = sourceSets.test.get().output.classesDirs
    classpath = sourceSets.test.get().runtimeClasspath
    systemProperty("sparkles.jni", "false")
    shouldRunAfter(tasks.test)
}
tasks.check { dependsOn(testUniffi) }

// The performance check of P04 §5.4, not part of `check`:
// ./gradlew :sparkles-jena:perfCheck -Pdata=DATA.nt -Pqueries=QUERIES.tsv [-Piterations=10] [-Ptdb2=false]
tasks.register<JavaExec>("perfCheck") {
    description = "Times queries through Jena on Sparkles, through the native library alone, and on TDB2"
    classpath = sourceSets.test.get().runtimeClasspath
    mainClass = "io.github.kclejeune.sparkles.jena.PerfCheck"
    maxHeapSize = "8g"
    jvmArgs("-Dorg.slf4j.simpleLogger.defaultLogLevel=warn", "-Xss64m")
    args(
        providers.gradleProperty("data").orElse("data.nt").get(),
        providers.gradleProperty("queries").orElse("queries.tsv").get(),
        providers.gradleProperty("iterations").orElse("10").get(),
        if (providers.gradleProperty("tdb2").orElse("true").get() == "false") "notdb" else "tdb",
    )
}

// The binding comparison of scripts/bench-bindings/bench.py runs
// io.github.kclejeune.sparkles.jena.bench.BindingsBench (a test class, so not shipped) in
// plain JVMs. `bindingsBenchClasspath` writes the classpaths it uses: the test runtime
// classpath, and the same with a copy of the generated bindings that counts native calls
// placed first. The counting copy increments a counter in UniFFI's call helper, which every
// generated call goes through, and in each hand-written JNI call of SparklesJni, and
// compiles with the module name of the `ffi` source set.
val countingBindingsDir = layout.buildDirectory.dir("bindings-bench/counting-src")
val callCountingBindings = tasks.register<Sync>("callCountingBindings") {
    from(ffiBindings)
    from("src/ffi/kotlin")
    into(countingBindingsDir)
    val marker = "    val status = UniffiCallStatusPool.acquire()"
    val jniMarker = "// counted native call"
    filter { line ->
        when {
            line == marker -> "    UniffiCallCounter.calls.increment()\n$line"
            line.trim() == jniMarker ->
                line.replace(jniMarker, "UniffiCallCounter.calls.increment(); UniffiCallCounter.jniCalls.increment()")
            else -> line
        }
    }
}
val benchCalls: SourceSet = sourceSets.create("benchCalls") {
    kotlin.srcDir(callCountingBindings)
    compileClasspath = ffi.compileClasspath
}
tasks.named<KotlinCompile>("compileBenchCallsKotlin") {
    compilerOptions {
        moduleName.set("sparkles-jena_ffi")
        allWarningsAsErrors = false
        suppressWarnings = true
    }
}
tasks.register("bindingsBenchClasspath") {
    description = "Compiles the binding comparison and writes its classpaths to build/bindings-bench"
    val runtime = sourceSets.test.get().runtimeClasspath
    val counting: FileCollection = files(benchCalls.output)
    dependsOn(runtime, counting)
    val dir = layout.buildDirectory.dir("bindings-bench")
    outputs.dir(dir)
    doLast {
        val d = dir.get().asFile
        d.resolve("classpath.txt").writeText(runtime.files.joinToString(File.pathSeparator) + "\n")
        d.resolve("classpath-calls.txt").writeText((counting.files + runtime.files).joinToString(File.pathSeparator) + "\n")
    }
}

publishing { publications { create<MavenPublication>("maven") { from(components["java"]) } } }

configurations.configureEach { resolutionStrategy.eachDependency { if (requested.group == "org.apache.jena" && providers.gradleProperty("sparkles.jenaVersion").isPresent) useVersion(providers.gradleProperty("sparkles.jenaVersion").get()) } }

// HTML documentation is also carried as the standard Maven documentation artifact.
dokka {
    dokkaPublications.html { outputDirectory.set(layout.buildDirectory.dir("dokka/html")) }
    dokkaSourceSets.configureEach {
        if (name != "main") suppress.set(true)
        perPackageOption { matchingRegex.set(".*\\.internal.*"); suppress.set(true) }
        enableJdkDocumentationLink.set(false)
        enableKotlinStdLibDocumentationLink.set(false)
    }
}
val documentationJar = tasks.register<Jar>("documentationJar") {
    dependsOn(tasks.named("dokkaGeneratePublicationHtml"))
    archiveClassifier.set("javadoc")
    from(layout.buildDirectory.dir("dokka/html"))
}
tasks.assemble { dependsOn(documentationJar) }
publishing.publications.named<MavenPublication>("maven") { artifact(documentationJar) }
