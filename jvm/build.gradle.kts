plugins { base }
allprojects {
    group = providers.gradleProperty("group").get()
    version = providers.gradleProperty("version").get()
    dependencyLocking { lockAllConfigurations() }
    // The locked release graph uses the catalog's Jena line. Compatibility jobs replace
    // that line deliberately and must resolve its corresponding transitive graph.
    configurations.configureEach {
        if (providers.gradleProperty("sparkles.jenaVersion").isPresent) {
            resolutionStrategy.deactivateDependencyLocking()
        }
        if (isCanBeResolved) {
            attributes.attribute(org.gradle.api.attributes.java.TargetJvmVersion.TARGET_JVM_VERSION_ATTRIBUTE,
                providers.gradleProperty("sparkles.javaVersion").orElse("17").get().toInt())
        }
    }
}
subprojects {
    plugins.withId("maven-publish") {
        extensions.configure<PublishingExtension> {
            repositories { maven {
                name = "release"
                url = uri(providers.gradleProperty("sparkles.publishUrl").orElse(rootProject.layout.projectDirectory.dir("build/maven-staging").asFile.toURI().toString()).get())
                credentials { username = providers.environmentVariable("MAVEN_USERNAME").orNull; password = providers.environmentVariable("MAVEN_PASSWORD").orNull }
            } }
            publications.withType<MavenPublication>().configureEach {
                pom {
                    name = project.name
                    description = project.description
                    url = "https://github.com/kclejeune/sparkles"
                    licenses { license { name = "Apache License, Version 2.0"; url = "https://www.apache.org/licenses/LICENSE-2.0.txt" } }
                    scm { url = "https://github.com/kclejeune/sparkles"; connection = "scm:git:https://github.com/kclejeune/sparkles.git" }
                    developers { developer { id = "kclejeune"; name = "Kyle Clejeune" } }
                }
            }
        }
        tasks.withType<PublishToMavenRepository>().configureEach { doFirst { require(providers.environmentVariable("SPARKLES_PUBLISH").orNull == "true") { "publishing requires SPARKLES_PUBLISH=true" } } }
    }
    plugins.withId("signing") {
        extensions.configure<SigningExtension> {
            val key = providers.environmentVariable("MAVEN_SIGNING_KEY").orNull
            if (key != null) {
                useInMemoryPgpKeys(key, providers.environmentVariable("MAVEN_SIGNING_PASSWORD").orNull)
                sign(extensions.getByType<PublishingExtension>().publications)
            }
        }
    }
}
