plugins {
    id("fabric-loom")
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
    mappings(loom.officialMojangMappings())
    modImplementation("net.fabricmc:fabric-loader:$fabricLoader")

    implementation(project(":core"))
    include(project(":core"))
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

    remapJar {
        archiveFileName.set("$modId-$modVersion+$mcVersion-fabric.jar")
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
