package com.vesta.wrapper.runtime;

import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.identity.PackPasswordService.VerificationStatus;
import com.vesta.wrapper.operations.CanonicalOperationFixture;
import com.vesta.wrapper.operations.OperationRunner;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;

import static org.junit.jupiter.api.Assertions.*;

class AgentRuntimePasswordTest {
    @TempDir Path root;

    private WrapperConfig config() throws Exception {
        Path game=Files.createDirectories(root.resolve("game"));
        return new WrapperConfig.Builder().scanRoot(game).minecraftWorkingDirectory(game)
                .minecraftLaunchJar(game.resolve("never-created-game.jar"))
                .durableRuntimeEnabled(true).stateDirectory(root.resolve("state")).build();
    }

    @Test void productionCompositionExposesNoAutomaticCredentialOrOwnerAndNeverLaunchesGame() throws Exception {
        var config=config(); var game=new MinecraftServerProcess(config,new ConsoleBuffer());
        try (var runtime=AgentRuntime.open(config,game)) {
            assertNotNull(runtime.packPasswords()); assertFalse(game.isRunning()); assertEquals(4,runtime.database().schemaVersion());
            assertEquals(VerificationStatus.UNAVAILABLE,runtime.packPasswords().verify("fixture-private-password".toCharArray()).status());
            assertEquals("forbidden",assertThrows(StateException.class,() -> runtime.packPasswords().setPassword(
                    new RemoteActor("system","local"),0,"fixture-private-password".toCharArray())).getCode());
            runtime.database().read(c -> { try (var s=c.createStatement()) {
                for (String table : new String[]{"principals","pack_passwords"}) try (var row=s.executeQuery("SELECT count(*) FROM "+table)) {
                    assertTrue(row.next()); assertEquals(0,row.getInt(1));
                }
            } return null; });
            assertFalse(game.isRunning());
        }
        assertFalse(game.isRunning()); assertFalse(Files.exists(config.minecraftLaunchJar));
    }

    @Test void trustedBootstrapCompositionPersistsCredentialAcrossRuntimeRestartWithoutGameEffects() throws Exception {
        var config=config(); var game=new MinecraftServerProcess(config,new ConsoleBuffer());
        var clock=new CanonicalOperationFixture.MutableClock();
        var settings=new OperationRunner.Settings(1,Duration.ofMillis(10),Duration.ofMillis(100));
        RemoteActor owner; String id;
        try (var runtime=AgentRuntime.open(config,game,clock,CanonicalOperationFixture.localAuthority(),settings)) {
            owner=CanonicalOperationFixture.bootstrap(runtime.identities(),clock); id=runtime.database().serverId();
            assertEquals(1,runtime.packPasswords().setPassword(owner,0,"fixture-private-password".toCharArray()).revision());
            assertFalse(game.isRunning());
        }
        try (var runtime=AgentRuntime.open(config,game,clock,CanonicalOperationFixture.localAuthority(),settings)) {
            assertEquals(id,runtime.database().serverId()); assertEquals(1,runtime.packPasswords().metadata(owner).revision());
            assertTrue(runtime.packPasswords().verify("fixture-private-password".toCharArray()).matched());
            assertFalse(game.isRunning()); assertTrue(runtime.audit().verifyChain().valid());
        }
        assertFalse(Files.exists(config.minecraftLaunchJar));
    }
}
