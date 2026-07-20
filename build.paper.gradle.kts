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
    maven("https://repo.papermc.io/repository/maven-public/") { name = "Paper" }
}

dependencies {
    val paperApi = if (stonecutter.eval(stonecutter.current.version, ">=26")) {
        "$mcVersion.build.+"
    } else {
        "$mcVersion-R0.1-SNAPSHOT"
    }
    compileOnly("io.papermc.paper:paper-api:$paperApi")
    implementation(project(":core"))
}

sourceSets {
    main {
        java {
            exclude("com/vesta/loader/platform/BukkitEntrypoint.java")
            exclude("com/vesta/loader/platform/BukkitVestaMessenger.java")
            exclude("com/vesta/loader/platform/FabricEntrypoint.java")
            exclude("com/vesta/loader/platform/ModdedVestaMessenger.java")
            exclude("com/vesta/loader/platform/NeoForgeEntrypoint.java")
            exclude("com/vesta/loader/platform/QuiltEntrypoint.java")
        }
    }
}

tasks {
    processResources {
        exclude("**/fabric.mod.json", "**/quilt.mod.json", "**/neoforge.mods.toml", "**/plugin.yml")
        filesMatching("paper-plugin.yml") {
            expand("version" to modVersion)
        }
    }

    jar {
        archiveFileName.set("$modId-$modVersion+$releaseId.jar")
        // Shade the Vesta core project so classes like LinkCodeService ship inside
        // the plugin. gson/guava/slf4j are provided by Paper at runtime.
        from(configurations.runtimeClasspath.get()
            .filter { it.name.startsWith("vesta-") || it.name.startsWith("core") }
            .map { zipTree(it) })
    }
}

java {
    withSourcesJar()
    if (stonecutter.eval(stonecutter.current.version, ">=26")) {
        toolchain.languageVersion.set(JavaLanguageVersion.of(25))
    } else {
        val javaCompat = if (stonecutter.eval(stonecutter.current.version, ">=1.20.5")) {
            JavaVersion.VERSION_21
        } else {
            JavaVersion.VERSION_17
        }
        sourceCompatibility = javaCompat
        targetCompatibility = javaCompat
    }
}
