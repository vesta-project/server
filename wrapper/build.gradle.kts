plugins {
    java
    application
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(21))
    }
    withSourcesJar()
}

tasks.withType<JavaCompile>().configureEach {
    options.encoding = "UTF-8"
    options.release.set(17)
}

application {
    mainClass.set("com.vesta.wrapper.VestaWrapper")
}

repositories {
    mavenCentral()
}

dependencies {
    implementation(project(":core"))
    implementation("com.google.code.gson:gson:2.11.0")
    implementation("org.eclipse.jetty:jetty-server:12.0.16")
    implementation("ch.qos.logback:logback-classic:1.5.15")
    implementation("org.bouncycastle:bcprov-jdk18on:1.79")
    implementation("at.favre.lib:bcrypt:0.10.2")
    implementation("com.moandjiezana.toml:toml4j:0.7.2")
}
