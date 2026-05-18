plugins {
    java
}

val modId: String = rootProject.property("mod.id") as String
val modVersion: String = rootProject.property("mod.version") as String
val mcVersion: String = project.property("deps.minecraft") as String

version = "$modVersion+$mcVersion-bukkit"
base.archivesName = modId

repositories {
    mavenCentral()
    maven("https://hub.spigotmc.org/nexus/content/repositories/snapshots/") { name = "Spigot" }
}

dependencies {
    compileOnly("org.spigotmc:spigot-api:$mcVersion-R0.1-SNAPSHOT")
    implementation(project(":core"))
}

tasks {
    processResources {
        exclude("**/fabric.mod.json", "**/quilt.mod.json", "**/neoforge.mods.toml", "**/paper-plugin.yml")
        filesMatching("plugin.yml") {
            expand(
                "version" to modVersion,
                "minecraft" to mcVersion
            )
        }
    }

    jar {
        archiveFileName.set("$modId-$modVersion+$mcVersion-bukkit.jar")
        from(configurations.runtimeClasspath.get().filter { it.name.startsWith("vesta-") }.map { zipTree(it) })
    }
}

java {
    withSourcesJar()
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}
