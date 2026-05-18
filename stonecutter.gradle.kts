plugins {
    id("dev.kikugie.stonecutter")
    id("fabric-loom") version "1.11-SNAPSHOT" apply false
    id("net.fabricmc.fabric-loom") version "1.15.5" apply false
    id("org.quiltmc.loom") version "1.15.1" apply false
    id("net.neoforged.moddev") version "2.0.141" apply false
}

stonecutter active "1.21.4-fabric"

stonecutter parameters {
    val meta = node.metadata
    constants.match(meta.project.substringAfterLast('-'), "fabric", "quilt", "neoforge", "bukkit", "paper")
    filters.include("**/*.fsh", "**/*.vsh")
}
