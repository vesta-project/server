plugins {
    id("net.fabricmc.fabric-loom")
}

val modId: String = rootProject.property("mod.id") as String
val modVersion: String = rootProject.property("mod.version") as String
val mcVersion: String = project.property("deps.minecraft") as String
val releaseId: String = project.property("release.id") as String
val mcRangeFabric: String = project.property("mc.range.fabric") as String
val fabricLoader: String = rootProject.property("deps.fabric-loader") as String

version = "$modVersion+$releaseId"
base.archivesName = modId

loom {
    runs {
        named("client") {
            configName = "Fabric $releaseId Client"
        }
    }
}

repositories {
    mavenCentral()
    maven("https://maven.fabricmc.net/") { name = "Fabric" }
}

dependencies {
    minecraft("com.mojang:minecraft:$mcVersion")
    implementation("net.fabricmc:fabric-loader:$fabricLoader")

    implementation(project(":core"))
    include(project(":core"))
}

sourceSets {
    main {
        java {
            exclude("com/vesta/loader/platform/BukkitEntrypoint.java")
            exclude("com/vesta/loader/platform/BukkitVestaMessenger.java")
            exclude("com/vesta/loader/platform/NeoForgeEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperVestaMessenger.java")
            exclude("com/vesta/loader/platform/QuiltEntrypoint.java")
            exclude("com/vesta/loader/platform/FabricCommandHooks.java")
        }
    }
}

tasks {
    processResources {
        exclude("**/quilt.mod.json", "**/neoforge.mods.toml", "**/plugin.yml", "**/paper-plugin.yml")
        filesMatching("fabric.mod.json") {
            expand(
                "version" to modVersion,
                "mc_range_fabric" to mcRangeFabric,
            )
        }
    }

    jar {
        archiveFileName.set("$modId-$modVersion+$releaseId.jar")
    }
}

java {
    withSourcesJar()
    toolchain.languageVersion.set(JavaLanguageVersion.of(25))
}
