plugins {
    id("dev.kikugie.stonecutter")
    id("fabric-loom") version "1.11-SNAPSHOT" apply false
    id("net.fabricmc.fabric-loom") version "1.15.5" apply false
    id("org.quiltmc.loom") version "1.15.1" apply false
    id("net.neoforged.moddev") version "2.0.141" apply false
}

stonecutter active "fabric-1.21"

stonecutter parameters {
    val meta = node.metadata
    constants.match(meta.project.substringBefore('-'), "fabric", "quilt", "neoforge", "bukkit", "paper")
    filters.include("**/*.fsh", "**/*.vsh")
}

tasks.register("testServerConnections") {
    group = "verification"
    description = "Runs the deterministic Vesta core and wrapper communication suites."
    dependsOn(":core:test", ":wrapper:test")
}
