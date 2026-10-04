package com.vesta.wrapper.config;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class DurableConfigLoaderTest {
    @TempDir Path root;

    @Test void explicitDurableOptInAndOutsideStateDirectoryAreParsed() throws Exception {
        Path state = root.resolve("private-state"); Path file = root.resolve("wrapper.toml");
        Files.writeString(file, "[state]\nenabled = true\ndirectory = '" + state + "'\n");
        var config = ConfigLoader.load(file);
        assertTrue(config.durableRuntimeEnabled); assertEquals(state.toAbsolutePath().normalize(), config.stateDirectory);
    }

    @Test void legacyDefaultIsExplicitAndStateLocationIsStableForConfiguredIdentity() {
        var config = ConfigLoader.load(root.resolve("missing.toml"));
        assertFalse(config.durableRuntimeEnabled);
        assertEquals(Path.of(System.getProperty("user.home"), ".vesta", "state", config.serverId.toString()).toAbsolutePath().normalize(), config.stateDirectory);
        UUID id = UUID.randomUUID();
        var first = new WrapperConfig.Builder().serverId(id).build(); var second = new WrapperConfig.Builder().serverId(id).build();
        assertEquals(first.stateDirectory, second.stateDirectory);
    }

    @ParameterizedTest @ValueSource(strings = {
            "[state\nenabled=true", "state=true", "[state]\nenabled='true'", "[state]\nenabled=1",
            "[state]\ndirectory=true", "[state]\ndirectory=[]", "[state]\ndirectory=''", "[state]\nunknown=true",
            "[state]\n'fixture-private-secret'=true",
            "[state]\nenabled=true\n[server]\nhttp-port='not-a-port'"
    })
    void malformedExistingConfigurationCannotDowngradeToLegacyDefaults(String text) throws Exception {
        Path file = root.resolve("wrapper.toml"); Files.writeString(file, text + "\n# fixture-private-secret\n");
        var failure = assertThrows(ConfigLoader.ConfigException.class, () -> ConfigLoader.load(file));
        assertNull(failure.getCause()); assertFalse(failure.getMessage().contains("fixture-private-secret"));
        assertFalse(failure.getMessage().contains("not-a-port"));
    }

    @Test void existingDirectoryIsNotAMissingConfiguration() throws Exception {
        Path directory = Files.createDirectories(root.resolve("config-directory"));
        assertThrows(ConfigLoader.ConfigException.class, () -> ConfigLoader.load(directory));
    }
}
