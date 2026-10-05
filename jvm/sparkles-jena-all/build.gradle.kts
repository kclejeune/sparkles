import java.util.zip.ZipFile

plugins { `java-library`; `maven-publish`; signing }
description = "Sparkles Fuseki bundle (Jena and SLF4J supplied by the host)"
dependencies { implementation(project(":sparkles-jena")); implementation(project(":sparkles-jena-natives")) }

/** Stage legal notices without resolving other projects during jar task configuration. */
abstract class DependencyLegalNotices : DefaultTask() {
    @get:InputFiles abstract val libraries: ConfigurableFileCollection
    @get:InputFile abstract val apacheLicense: RegularFileProperty
    @get:InputFile abstract val attributions: RegularFileProperty
    @get:OutputDirectory abstract val output: DirectoryProperty
    @TaskAction fun stage() {
        val root = output.get().asFile
        root.deleteRecursively()
        val legal = root.resolve("META-INF/licenses").also { it.mkdirs() }
        for (artifact in libraries.files.sortedBy { it.name }) {
            val destination = legal.resolve(artifact.name.removeSuffix(".jar"))
            ZipFile(artifact).use { archive ->
                for (entry in archive.entries()) {
                    if (entry.isDirectory) continue
                    val name = entry.name.substringAfterLast('/').uppercase()
                    if (name.endsWith(".CLASS")) continue
                    if (name.contains("LICENSE") || name.startsWith("NOTICE") || name.startsWith("COPYING") || name.startsWith("COPYRIGHT") || name == "AL2.0" || name == "LGPL2.1" || entry.name.startsWith("META-INF/licenses/")) {
                        val target = destination.resolve(entry.name)
                        target.parentFile.mkdirs()
                        archive.getInputStream(entry).use { input -> target.outputStream().use { sink -> input.copyTo(sink) } }
                    }
                }
            }
            if (artifact.name.startsWith("kotlin-stdlib-") || artifact.name.startsWith("jspecify-")) {
                destination.mkdirs()
                apacheLicense.get().asFile.copyTo(destination.resolve("LICENSE"), overwrite = true)
            }
        }
        attributions.get().asFile.copyTo(legal.resolve("ATTRIBUTIONS.txt"))
    }
}
val bundledJars = provider { configurations.runtimeClasspath.get().filter { it.name.startsWith("sparkles-jena-") || it.name.startsWith("kotlin-stdlib-") || it.name.startsWith("jna-") || it.name.startsWith("jspecify-") } }
val legalNotices = tasks.register<DependencyLegalNotices>("dependencyLegalNotices") {
    dependsOn(":sparkles-jena:jar", ":sparkles-jena-natives:jar")
    libraries.from(bundledJars)
    apacheLicense.set(layout.projectDirectory.file("licenses/APACHE-2.0.txt"))
    attributions.set(layout.projectDirectory.file("licenses/ATTRIBUTIONS.txt"))
    output.set(layout.buildDirectory.dir("dependency-legal-notices"))
}
val bundle = tasks.named<Jar>("jar") {
    archiveBaseName = "sparkles-jena-all"
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    dependsOn(":sparkles-jena:jar", ":sparkles-jena-natives:jar")
    from(bundledJars.map { jars -> jars.map { zipTree(it) } })
    from(legalNotices)
    exclude("META-INF/*.SF", "META-INF/*.RSA", "META-INF/*.DSA", "module-info.class", "META-INF/versions/**/module-info.class")
    manifest { attributes("Implementation-Title" to "sparkles-jena-all", "Implementation-Version" to project.version, "Enable-Native-Access" to "ALL-UNNAMED") }
}
tasks.assemble { dependsOn(bundle) }
publishing { publications { create<MavenPublication>("maven") { artifact(bundle) } } }
