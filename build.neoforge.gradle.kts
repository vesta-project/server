plugins {
    id("net.neoforged.moddev")
}

val modId: String = rootProject.property("mod.id") as String
val modVersion: String = rootProject.property("mod.version") as String
val mcVersion: String = project.property("deps.minecraft") as String
val releaseId: String = project.property("release.id") as String
val mcRangeNeoForge: String = project.property("mc.range.neoforge") as String

version = "$modVersion+$releaseId"
base.archivesName = modId

neoForge {
    version = project.property("deps.neoforge") as String

    runs {
        register("client") {
            client()
            gameDirectory = file("run")
        }
    }

    mods {
        register(modId) {
            sourceSet(sourceSets["main"])
        }
    }
}

repositories {
    mavenCentral()
    maven("https://maven.neoforged.net/releases/") { name = "NeoForged" }
}

dependencies {
    implementation(project(":core"))
    jarJar(project(":core"))
}

sourceSets {
    main {
        java {
            exclude("com/vesta/loader/platform/BukkitEntrypoint.java")
            exclude("com/vesta/loader/platform/BukkitVestaMessenger.java")
            exclude("com/vesta/loader/platform/FabricEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperEntrypoint.java")
            exclude("com/vesta/loader/platform/PaperVestaMessenger.java")
            exclude("com/vesta/loader/platform/QuiltEntrypoint.java")
        }
    }
}

tasks {
    processResources {
        exclude("**/fabric.mod.json", "**/quilt.mod.json", "**/plugin.yml", "**/paper-plugin.yml")
        filesMatching("META-INF/neoforge.mods.toml") {
            expand(
                "version" to modVersion,
                "mc_range_neoforge" to mcRangeNeoForge,
            )
        }
    }

    jar {
        archiveFileName.set("$modId-$modVersion+$releaseId.jar")
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
