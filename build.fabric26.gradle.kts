plugins {
    id("net.fabricmc.fabric-loom")
}

val modId: String = rootProject.property("mod.id") as String
val modVersion: String = rootProject.property("mod.version") as String
val mcVersion: String = project.property("deps.minecraft") as String
val fabricLoader: String = rootProject.property("deps.fabric-loader") as String

version = "$modVersion+$mcVersion-fabric"
base.archivesName = modId

loom {
    runs {
        named("client") {
            configName = "Fabric $mcVersion Client"
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
            exclude("com/vesta/loader/platform/NeoForgeEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperEntrypoint.java")
            exclude("com/vesta/loader/platform/QuiltEntrypoint.java")
        }
    }
}

tasks {
    processResources {
        exclude("**/quilt.mod.json", "**/neoforge.mods.toml", "**/plugin.yml", "**/paper-plugin.yml")
        filesMatching("fabric.mod.json") {
            expand(
                "version" to modVersion,
                "minecraft" to mcVersion
            )
        }
    }
}

java {
    withSourcesJar()
    toolchain.languageVersion.set(JavaLanguageVersion.of(25))
}
