plugins {
    java
}

val modId: String = rootProject.property("mod.id") as String
val modVersion: String = rootProject.property("mod.version") as String
val mcVersion: String = project.property("deps.minecraft") as String
val releaseId: String = project.property("release.id") as String

version = "$modVersion+$releaseId"
base.archivesName = modId

repositories {
    mavenCentral()
    maven("https://hub.spigotmc.org/nexus/content/repositories/snapshots/") { name = "Spigot" }
}

dependencies {
    compileOnly("org.spigotmc:spigot-api:$mcVersion-R0.1-SNAPSHOT")
    implementation(project(":core"))
}

sourceSets {
    main {
        java {
            exclude("com/vesta/loader/platform/FabricEntrypoint.java")
            exclude("com/vesta/loader/platform/NeoForgeEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperEntrypoint.java")
            exclude("com/vesta/loader/platform/QuiltEntrypoint.java")
        }
    }
}

tasks {
    processResources {
        exclude("**/fabric.mod.json", "**/quilt.mod.json", "**/neoforge.mods.toml", "**/paper-plugin.yml")
        filesMatching("plugin.yml") {
            expand("version" to modVersion)
        }
    }

    jar {
        archiveFileName.set("$modId-$modVersion+$releaseId.jar")
        from(configurations.runtimeClasspath.get().filter { it.name.startsWith("vesta-") }.map { zipTree(it) })
    }
}

java {
    withSourcesJar()
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}
