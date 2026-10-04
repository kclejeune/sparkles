// A Java 17 program and tests written only against sparkles-jena's public API (P04 §7). It
// compiles with -Xlint:all -Werror, so raw types, unchecked conversions and deprecations
// fail the build, and it needs no Kotlin of its own.

plugins {
    java
    application
}

java {
    toolchain.languageVersion = JavaLanguageVersion.of(17)
}

dependencies {
    implementation(project(":sparkles-jena"))
    runtimeOnly(libs.slf4j.simple)

    testImplementation(platform(libs.junit.bom))
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.platform.launcher)
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 17
    options.compilerArgs.addAll(listOf("-Xlint:all", "-Werror"))
}

application {
    mainClass = "sample.SparklesSample"
    applicationDefaultJvmArgs = listOf("-Dorg.slf4j.simpleLogger.defaultLogLevel=warn")
}

tasks.test {
    useJUnitPlatform()
    systemProperty("org.slf4j.simpleLogger.defaultLogLevel", "warn")
}
