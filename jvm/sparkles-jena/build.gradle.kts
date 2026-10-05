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

// The generated bindings compile on their own: they are public Kotlin that explicit API mode
// would reject, and their warnings are not ours. Their classes go into this jar.
val ffi: SourceSet = sourceSets.create("ffi") {
    kotlin.srcDir(bindingsDir)
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
