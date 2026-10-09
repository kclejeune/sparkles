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
            val publishUrl = providers.gradleProperty("sparkles.publishUrl")
            repositories { maven {
                name = "release"
                url = uri(publishUrl.orElse(rootProject.layout.projectDirectory.dir("build/maven-staging").asFile.toURI().toString()).get())
                // Without sparkles.publishUrl the artifacts go to a local directory, which
                // takes no credentials.
                if (publishUrl.isPresent) {
                    credentials { username = providers.environmentVariable("MAVEN_USERNAME").orNull; password = providers.environmentVariable("MAVEN_PASSWORD").orNull }
                }
            } }
            publications.withType<MavenPublication>().configureEach {
                pom {
                    name = project.name
                    description = project.description
                    url = "https://github.com/kclejeune/sparkles"
                    licenses { license { name = "Apache License, Version 2.0"; url = "https://www.apache.org/licenses/LICENSE-2.0.txt" } }
                    scm {
                        url = "https://github.com/kclejeune/sparkles"
                        connection = "scm:git:https://github.com/kclejeune/sparkles.git"
                        developerConnection = "scm:git:ssh://git@github.com/kclejeune/sparkles.git"
                    }
                    developers { developer { id = "kclejeune"; name = "Kennan LeJeune"; url = "https://github.com/kclejeune" } }
                }
            }
        }
        // Maven Central requires sources and javadoc jars beside every jar. The natives and the
        // Fuseki bundle have no sources of their own, so theirs hold a README that points to
        // the repository, which Central accepts.
        if (project.name != "sparkles-jena") {
            val placeholder = layout.buildDirectory.file("central-placeholder/README.md")
            val writePlaceholder = tasks.register("centralPlaceholder") {
                val text = "This artifact has no separate sources or API documentation. " +
                    "See https://github.com/kclejeune/sparkles and the sparkles-jena artifact.\n"
                outputs.file(placeholder)
                doLast { placeholder.get().asFile.apply { parentFile.mkdirs(); writeText(text) } }
            }
            val placeholderJars = listOf("sources", "javadoc").map { kind ->
                tasks.register<Jar>("${kind}PlaceholderJar") {
                    archiveClassifier = kind
                    from(writePlaceholder)
                }
            }
            extensions.configure<PublishingExtension> {
                publications.withType<MavenPublication>().configureEach { placeholderJars.forEach { artifact(it) } }
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
