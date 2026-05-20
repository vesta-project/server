plugins {
    id("org.quiltmc.loom")
}

val modId: String = rootProject.property("mod.id") as String
val modVersion: String = rootProject.property("mod.version") as String
val mcVersion: String = project.property("deps.minecraft") as String
val quiltLoader: String = rootProject.property("deps.quilt-loader") as String

version = "$modVersion+$mcVersion-quilt"
base.archivesName = modId

loom {
    runs {
        named("client") {
            configName = "Quilt $mcVersion Client"
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

    implementation(project(":core"))
    include(project(":core"))
}

sourceSets {
    main {
        java {
            exclude("com/vesta/loader/platform/BukkitEntrypoint.java")
            exclude("com/vesta/loader/platform/FabricEntrypoint.java")
            exclude("com/vesta/loader/platform/NeoForgeEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperEntrypoint.java")
        }
    }
}

tasks {
    processResources {
        exclude("**/fabric.mod.json", "**/neoforge.mods.toml", "**/plugin.yml", "**/paper-plugin.yml")
        filesMatching("quilt.mod.json") {
            expand(
                "version" to modVersion,
                "minecraft" to mcVersion
            )
        }
    }

    remapJar {
        archiveFileName.set("$modId-$modVersion+$mcVersion-quilt.jar")
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
