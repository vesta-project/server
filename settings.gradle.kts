pluginManagement {
    repositories {
        mavenCentral()
        gradlePluginPortal()
        maven("https://maven.fabricmc.net/") { name = "Fabric" }
        maven("https://maven.neoforged.net/releases/") { name = "NeoForged" }
        maven("https://maven.minecraftforge.net/") { name = "Forge" }
        maven("https://maven.quiltmc.org/repository/release/") { name = "Quilt" }
        maven("https://repo.papermc.io/repository/maven-public/") { name = "Paper" }
        maven("https://hub.spigotmc.org/nexus/repository/snapshots/") { name = "Spigot" }
    }
}

buildscript {
    repositories {
        mavenCentral()
    }
    dependencies {
        classpath("org.tomlj:tomlj:1.1.1")
    }
}

import org.tomlj.Toml
import org.tomlj.TomlTable
import java.io.File

plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "0.9.0"
    id("dev.kikugie.stonecutter") version "0.7"
}

data class ReleaseLine(
    val id: String,
    val compileMc: String,
    val buildscript: String,
    val gradleProperties: String,
)

fun loadReleaseMatrix(matrixFile: File): List<ReleaseLine> {
    val result = Toml.parse(matrixFile.toPath())
    result.errors().forEach { error ->
        throw IllegalStateException("Invalid ${matrixFile.name}: $error")
    }
    val releases = result.getArray("release")
        ?: throw IllegalStateException("versions-matrix.toml must contain [[release]] entries")
    return releases.toList().map { entry ->
        val release = entry as TomlTable
        val id = release.getString("id") ?: error("release.id")
        val compileMc = release.getString("compile_mc") ?: error("release.compile_mc")
        val buildscript = release.getString("buildscript") ?: error("release.buildscript")
        val mcRangeMin = release.getString("mc_range_min") ?: error("release.mc_range_min")
        val mcRangeMax = release.getString("mc_range_max")
        val loader = release.getString("loader") ?: error("release.loader")
        val neoforge = release.getString("neoforge")
        val fabricRange = if (mcRangeMax != null) ">=$mcRangeMin <=$mcRangeMax" else ">=$mcRangeMin"
        val neoforgeRange = if (mcRangeMax != null) "[$mcRangeMin,$mcRangeMax]" else "[$mcRangeMin,)"
        val props = buildString {
            appendLine("deps.minecraft=$compileMc")
            neoforge?.let { appendLine("deps.neoforge=$it") }
            appendLine("release.id=$id")
            appendLine("release.loader=$loader")
            appendLine("mc.range.min=$mcRangeMin")
            mcRangeMax?.let { appendLine("mc.range.max=$it") }
            appendLine("mc.range.fabric=$fabricRange")
            appendLine("mc.range.quilt=$fabricRange")
            appendLine("mc.range.neoforge=$neoforgeRange")
        }
        ReleaseLine(id, compileMc, buildscript, props)
    }
}

fun materializeVersionDirs(rootDir: File, releases: List<ReleaseLine>) {
    val versionsRoot = File(rootDir, "versions")
    versionsRoot.mkdirs()
    releases.forEach { release ->
        val versionDir = File(versionsRoot, release.id)
        versionDir.mkdirs()
        File(versionDir, "gradle.properties").writeText(release.gradleProperties)
    }
}

rootProject.name = "vesta-server-sync"

include(":core")
include(":wrapper")

val releaseLines = loadReleaseMatrix(file("versions-matrix.toml"))
materializeVersionDirs(rootDir, releaseLines)

stonecutter {
    create(rootProject) {
        releaseLines.forEach { release ->
            vers(release.id, release.compileMc).buildscript = release.buildscript
        }
        vcsVersion = "fabric-1.21"
    }
}
