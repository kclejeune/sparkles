// The JVM bindings (spec docs/specs/P04-jvm-bindings.md). `mise run jvm:build` builds the
// native library with cargo, generates its Kotlin bindings with the crate's own
// uniffi-bindgen, and runs this build with their paths (see sparkles-jena/build.gradle.kts).

pluginManagement {
    repositories {
        gradlePluginPortal()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositoriesMode = RepositoriesMode.FAIL_ON_PROJECT_REPOS
    repositories {
        mavenCentral()
    }
}

rootProject.name = "sparkles-jvm"

include("sparkles-jena")
include("sample-java")
