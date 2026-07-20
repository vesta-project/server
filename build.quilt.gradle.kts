plugins {
    id("org.quiltmc.loom")
}

val modId: String = rootProject.property("mod.id") as String
val modVersion: String = rootProject.property("mod.version") as String
val mcVersion: String = project.property("deps.minecraft") as String
val releaseId: String = project.property("release.id") as String
val mcRangeQuilt: String = project.property("mc.range.quilt") as String
val quiltLoader: String = rootProject.property("deps.quilt-loader") as String

version = "$modVersion+$releaseId"
base.archivesName = modId

loom {
    runs {
        named("client") {
            configName = "Quilt $releaseId Client"
        }
    }
}

repositories {
    mavenCentral()
    maven("https://maven.quiltmc.org/repository/release/") { name = "Quilt" }
}

dependencies {
    minecraft("com.mojang:minecraft:$mcVersion")
    mappings(loom.officialMojangMappings())
    modImplementation("org.quiltmc:quilt-loader:$quiltLoader")
    modImplementation("net.fabricmc.fabric-api:fabric-api:${project.findProperty("deps.fabric-api") ?: "0.102.1+1.21.1"}")

    implementation(project(":core"))
    include(project(":core"))
}

sourceSets {
    main {
        java {
            exclude("com/vesta/loader/platform/BukkitEntrypoint.java")
            exclude("com/vesta/loader/platform/BukkitVestaMessenger.java")
            exclude("com/vesta/loader/platform/FabricEntrypoint.java")
            exclude("com/vesta/loader/platform/NeoForgeEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperVestaMessenger.java")
        }
    }
}

tasks {
    processResources {
        exclude("**/fabric.mod.json", "**/neoforge.mods.toml", "**/plugin.yml", "**/paper-plugin.yml")
        filesMatching("quilt.mod.json") {
            expand(
                "version" to modVersion,
                "mc_range_quilt" to mcRangeQuilt,
            )
        }
    }

    remapJar {
        archiveFileName.set("$modId-$modVersion+$releaseId.jar")
    }
}

java {
    withSourcesJar()
    val javaCompat = if (stonecutter.eval(stonecutter.current.version, ">=1.20.5")) {
        JavaVersion.VERSION_21
    } else {
        JavaVersion.VERSION_17
    }
    sourceCompatibility = javaCompat
    targetCompatibility = javaCompat
}
