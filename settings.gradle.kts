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

plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "0.9.0"
    id("dev.kikugie.stonecutter") version "0.7"
}

rootProject.name = "vesta-server-sync"

include(":core")
include(":wrapper")

stonecutter {
    create(rootProject) {
        fun match(version: String, vararg loaders: String) = loaders
            .forEach { vers("$version-$it", version).buildscript = "build.$it.gradle.kts" }

        match("1.21.4", "fabric", "quilt", "neoforge", "paper")
        match("1.21.1", "fabric", "quilt", "neoforge", "paper")
        match("1.20.6", "fabric", "quilt", "neoforge", "paper")
        match("1.20.4", "fabric", "quilt", "bukkit")
        match("1.20.1", "fabric", "quilt", "bukkit")

        // MC 26.x uses unobfuscated jar, new Fabric Loom plugin (net.fabricmc.fabric-loom)
        vers("26.1.2-fabric", "26.1.2").buildscript = "build.fabric26.gradle.kts"
        vers("26.1.1-fabric", "26.1.1").buildscript = "build.fabric26.gradle.kts"
        vers("26.1-fabric", "26.1").buildscript = "build.fabric26.gradle.kts"

        match("26.1.2", "neoforge", "paper")
        match("26.1.1", "neoforge", "paper")
        match("26.1", "neoforge", "paper")

        vcsVersion = "1.21.4-fabric"
    }
}
