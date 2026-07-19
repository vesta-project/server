plugins {
    java
}

tasks.register("packageDist") {
    group = "distribution"
    description = "Build wrapper zip + installer scripts into build/dist (run scripts/package-dist.*)"
    dependsOn(":wrapper:installDist")
    doLast {
        val isWindows = System.getProperty("os.name").lowercase().contains("windows")
        val script = if (isWindows) {
            project.file("scripts/package-dist.ps1")
        } else {
            project.file("scripts/package-dist.sh")
        }
        if (isWindows) {
            exec {
                commandLine(
                    "powershell",
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    script.absolutePath
                )
            }
        } else {
            exec {
                commandLine("bash", script.absolutePath)
            }
        }
    }
}
