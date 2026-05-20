plugins {
    `java-library`
}

java {
    withSourcesJar()
}

tasks.withType<JavaCompile>().configureEach {
    options.encoding = "UTF-8"
    options.release.set((project.property("core.java.target") as String).toInt())
}
