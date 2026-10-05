package com.vesta.wrapper.runtime;

import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.config.WrapperConfig;
import com.vesta.wrapper.console.ConsoleBuffer;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.launch.LaunchSpec;
import com.vesta.wrapper.launch.LaunchSpec.StopMode;
import com.vesta.wrapper.operations.CanonicalOperationFixture;
import com.vesta.wrapper.operations.Operation;
import com.vesta.wrapper.operations.OperationException;
import com.vesta.wrapper.operations.OperationRunner;
import com.vesta.wrapper.operations.OperationState;
import com.vesta.wrapper.operations.lifecycle.LifecycleBindings;
import com.vesta.wrapper.protocol.ServerFeatureCodec;
import com.vesta.wrapper.protocol.ServerFeatureProtocol;
import com.vesta.wrapper.protocol.ServerFeatureProtocol.*;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.EnumSource;
import org.junit.jupiter.params.provider.MethodSource;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import java.util.stream.Stream;

import static com.vesta.wrapper.protocol.ServerFeatureProtocol.FeatureId.*;
import static com.vesta.wrapper.runtime.FakeGameFixtures.await;
import static org.junit.jupiter.api.Assertions.*;

/** Discovery is observational; reports (including forged ones) never become identity or authority. */
class AgentRuntimeFeatureDiscoveryTest {
    private static final UUID LEGACY_ID = UUID.fromString("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
    private static final DiscoveryRequest OFFER = new DiscoveryRequest(List.of(2,1));
    private static final OperationRunner.Settings SETTINGS = new OperationRunner.Settings(1,Duration.ofMillis(10),Duration.ofMillis(100));
    @TempDir Path root;
    private final CanonicalOperationFixture.MutableClock clock = new CanonicalOperationFixture.MutableClock();
    private AgentRuntime runtime;
    private MinecraftServerProcess game;
    private WrapperConfig config;
    private RemoteActor owner;

    private void open(LaunchSpec spec, boolean bootstrap) throws Exception {
        Path work = Files.createDirectories(root.resolve("game"));
        config = new WrapperConfig.Builder().serverId(LEGACY_ID).scanRoot(work).minecraftWorkingDirectory(work).minecraftLaunch(spec)
                .durableRuntimeEnabled(true).stateDirectory(root.resolve("state")).build();
        game = new MinecraftServerProcess(config,new ConsoleBuffer(),Duration.ofSeconds(5));
        runtime = AgentRuntime.open(config,game,clock,CanonicalOperationFixture.localAuthority(),SETTINGS);
        if (bootstrap) owner = CanonicalOperationFixture.bootstrap(runtime.identities(),clock);
    }

    private int count(String table) {
        return runtime.database().read(c -> { try (var s = c.createStatement(); var r = s.executeQuery("SELECT count(*) FROM "+table)) {
            r.next(); return r.getInt(1);
        } });
    }
    private List<Integer> counts() {
        var out = new ArrayList<Integer>();
        for (String table : List.of("principals","devices","role_assignments","principal_overrides","operations","confirmations","events","audit_events","resource_locks","settings")) out.add(count(table));
        return out;
    }
    private Manifest discover() { return runtime.featureDiscovery().discover(OFFER); }
    private LaunchSpec missingJar() { return LaunchSpec.javaJar("unused-java",List.of("-Dsecret=PRIVATE_RUNTIME_ARG"),Path.of("PRIVATE_ARTIFACT.jar"),List.of("PRIVATE_GAME_ARG")); }
    private Manifest forgedAllSupported() {
        var entries = new ArrayList<Feature>();
        for (FeatureId id : FeatureId.values()) entries.add(Feature.supported(id));
        entries.add(new Feature("security.manage",Support.SUPPORTED,null));
        var forged = ServerFeatureCodec.decodeManifest(ServerFeatureCodec.encodeManifest(new Manifest(1,runtime.database().serverId(),entries)));
        ServerFeatureProtocol.checkResponse(OFFER,runtime.database().serverId(),forged);
        return forged;
    }
    private void submitDenied(RemoteActor actor, String type) {
        String input = type.equals(LifecycleBindings.START) ? "{}" : "{\"expected_generation\":\"gen_"+"A".repeat(22)+"\"}";
        OperationException e = assertThrows(OperationException.class, () -> runtime.submissions().submit(actor,"https",type,input,null));
        assertEquals(OperationException.AUTHORIZATION_DENIED,e.getCode());
    }
    private void replayDenied(RemoteActor actor) {
        StateException e = assertThrows(StateException.class, () -> runtime.operationEvents().currentCheckpoint(actor));
        assertEquals("forbidden",e.getCode());
    }

    @AfterEach void cleanup() throws Exception {
        if (runtime != null) { runtime.close(); await(runtime::databaseClosed); assertTrue(runtime.runner().fullyStopped()); }
        if (game != null) { game.kill(); await(() -> !game.isRunning()); }
        FakeGameFixtures.deleteTreeWithRetry(root);
    }

    private record Shape(String name, LaunchSpec spec, boolean configured, boolean stop) { }
    static Stream<Shape> shapes() {
        return Stream.of(
                new Shape("none",null,false,false),
                new Shape("jar",LaunchSpec.javaJar("unused-java",List.of("-Dsecret=PRIVATE_RUNTIME_ARG"),Path.of("PRIVATE_ARTIFACT.jar"),List.of("PRIVATE_GAME_ARG")),true,true),
                new Shape("argfiles",LaunchSpec.javaArgFiles("unused-java",List.of(),List.of(Path.of("PRIVATE_ARGFILE")),List.of()),true,true),
                new Shape("native console",LaunchSpec.nativeExecutable(Path.of("PRIVATE_EXECUTABLE"),List.of(),StopMode.CONSOLE_STOP),true,true),
                new Shape("native unsupported",LaunchSpec.nativeExecutable(Path.of("PRIVATE_EXECUTABLE"),List.of(),StopMode.UNSUPPORTED),true,false),
                new Shape("jar unsupported",LaunchSpec.javaJar("unused-java",List.of(),Path.of("PRIVATE_ARTIFACT.jar"),List.of()).withStopMode(StopMode.UNSUPPORTED),true,false));
    }

    @ParameterizedTest @MethodSource("shapes") void reportsOnlyConfiguredMechanismsWithoutPreflightEffectsReadinessOrContinuity(Shape shape) throws Exception {
        open(shape.spec(),false);
        List<Integer> before = counts();
        Manifest report = discover();
        assertEquals(runtime.database().serverId(),report.serverId()); assertNotEquals(LEGACY_ID.toString(),report.serverId());
        assertEquals(shape.configured(),report.supports(GAME_START)); assertEquals(shape.configured(),report.supports(GAME_FORCE_KILL));
        assertEquals(shape.stop(),report.supports(GAME_STOP)); assertTrue(report.supports(TASKS_REPLAY));
        assertFalse(report.supports(GAME_READINESS)); assertFalse(report.supports(SUPERVISOR_CONTINUITY));
        for (Feature f : report.features()) {
            if (f.support() == Support.SUPPORTED) assertNull(f.reason());
            else if (f.id().equals(GAME_READINESS.id()) || f.id().equals(SUPERVISOR_CONTINUITY.id())) assertEquals("not_implemented",f.reason());
            else assertEquals(shape.configured() ? "unsupported_stop" : "not_configured",f.reason());
        }
        String wire = new String(ServerFeatureCodec.encodeManifest(report),StandardCharsets.UTF_8);
        assertFalse(wire.contains("PRIVATE_")); assertFalse(wire.contains(root.toString()));
        for (int i=0;i<20;i++) assertEquals(report,discover());
        assertEquals(before,counts()); assertEquals(0,count("principals")); assertEquals(0,count("operations"));
        assertFalse(game.isRunning()); assertNull(game.currentGenerationId()); assertFalse(Files.exists(root.resolve("game/starts.txt")));
        assertEquals(Manifest.class,report.getClass()); assertEquals(6,report.features().size());
    }

    @Test void noCommonVersionIsFixedNoEffectFailure() throws Exception {
        open(missingJar(),false);
        List<Integer> before=counts();
        ProtocolException e=assertThrows(ProtocolException.class, () -> runtime.featureDiscovery().discover(new DiscoveryRequest(List.of(2))));
        assertEquals(ErrorCode.UNSUPPORTED_PROTOCOL,e.code()); assertNull(e.getCause());
        assertEquals(before,counts()); assertFalse(game.isRunning());
    }

    @Test void canonicalIdentitySurvivesReopenWithoutCreatingAnOwnerOrLaunching() throws Exception {
        open(missingJar(),false); Manifest original=discover();
        runtime.close(); await(runtime::databaseClosed);
        game=new MinecraftServerProcess(config,new ConsoleBuffer());
        runtime=AgentRuntime.open(config,game,clock,CanonicalOperationFixture.localAuthority(),SETTINGS);
        assertEquals(original,discover()); assertEquals(0,count("principals")); assertFalse(game.isRunning());
    }

    @Test void correctUuidAndForgedSupportedFeaturesCannotAuthenticateOrRegisterAnAction() throws Exception {
        open(missingJar(),false); List<Integer> before=counts();
        Manifest forged=forgedAllSupported(); assertTrue(forged.supports(GAME_START)); assertTrue(forged.supports(GAME_READINESS));
        assertEquals(before,counts());
        RemoteActor unrecognized=new RemoteActor(UUID.randomUUID().toString(),UUID.randomUUID().toString());
        for (String type : List.of(LifecycleBindings.START,LifecycleBindings.STOP,LifecycleBindings.FORCE_KILL)) submitDenied(unrecognized,type);
        replayDenied(unrecognized); submitDenied(null,LifecycleBindings.START); replayDenied(null);
        assertEquals(0,count("principals")); assertEquals(0,count("devices")); assertEquals(0,count("operations"));
        assertTrue(runtime.bindings().find("security.manage").isEmpty());
        assertTrue(runtime.bindings().find("supervisor.continuity").isEmpty());
        assertFalse(game.isRunning()); assertFalse(discover().supports(GAME_READINESS));
    }

    @ParameterizedTest @EnumSource(value=Capability.class,names={"LIFECYCLE_START","LIFECYCLE_STOP","LIFECYCLE_FORCE_KILL","TASKS_VIEW"})
    void supportAndPreviouslyAllowedPolicyCannotOverrideANewExactDeny(Capability capability) throws Exception {
        open(missingJar(),true); Manifest original=discover(); forgedAllSupported();
        assertTrue(runtime.authorization().resolve(owner.principalId(),owner.deviceId()).check(capability,"").allowed());
        runtime.database().write(c -> { try (var ps=c.prepareStatement("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'','deny','fixture',?)")) {
            ps.setString(1,UUID.randomUUID().toString()); ps.setString(2,owner.principalId()); ps.setString(3,capability.id()); ps.setLong(4,clock.millis()); ps.executeUpdate();
        } return null; });
        assertEquals(original,discover());
        assertFalse(runtime.authorization().resolve(owner.principalId(),owner.deviceId()).check(capability,"").allowed());
        switch (capability) {
            case LIFECYCLE_START -> submitDenied(owner,LifecycleBindings.START);
            case LIFECYCLE_STOP -> submitDenied(owner,LifecycleBindings.STOP);
            case LIFECYCLE_FORCE_KILL -> submitDenied(owner,LifecycleBindings.FORCE_KILL);
            case TASKS_VIEW -> replayDenied(owner);
            default -> fail("Unexpected fixture capability");
        }
        assertEquals(0,count("operations")); assertFalse(game.isRunning());
    }

    @Test void revokedDeviceCannotUseAStillSupportedStartOrReplay() throws Exception {
        open(missingJar(),true); Manifest report=discover();
        assertTrue(runtime.operationEvents().currentCheckpoint(owner).complete());
        runtime.identities().revokeDevice(owner,owner.deviceId());
        assertEquals(report,discover()); forgedAllSupported();
        submitDenied(owner,LifecycleBindings.START); replayDenied(owner);
        assertEquals(0,count("operations")); assertFalse(game.isRunning());
    }

    @Test void supportedKillStillRequiresExactConfirmationAndReportingNeverStartsOrStopsTheOwnedGame() throws Exception {
        Path work=Files.createDirectories(root.resolve("game")); Path jar=FakeGameFixtures.jar(work);
        open(LaunchSpec.javaJar(FakeGameFixtures.javaExecutable().toString(),List.of(),jar,List.of("graceful")),true);
        Manifest report=discover(); assertTrue(report.supports(GAME_FORCE_KILL)); assertFalse(game.isRunning());
        Operation start=runtime.submissions().submit(owner,"https",LifecycleBindings.START,"{}",null).operation();
        await(() -> runtime.operations().get(start.id()).orElseThrow().state().terminal());
        assertEquals(OperationState.SUCCEEDED,runtime.operations().get(start.id()).orElseThrow().state());
        await(() -> game.consoleBuffer().snapshot(0,100).lines.stream().anyMatch(l -> l.text.equals("FAKE_READY")));
        String generation=game.currentGenerationId();
        Operation kill=runtime.submissions().submit(owner,"https",LifecycleBindings.FORCE_KILL,"{\"expected_generation\":\""+generation+"\"}",null).operation();
        assertEquals(OperationState.AWAITING_CONFIRMATION,kill.state());
        List<Integer> before=counts(); assertEquals(report,discover()); assertEquals(before,counts()); assertEquals(generation,game.currentGenerationId());
        assertTrue(game.isRunning()); assertEquals(OperationState.AWAITING_CONFIRMATION,runtime.operations().get(kill.id()).orElseThrow().state());
        var token=runtime.confirmations().issue(owner,"https",kill.id(),Duration.ofMinutes(1));
        runtime.confirmations().confirm(owner,"https",kill.id(),token.token()); runtime.runner().wake();
        await(() -> runtime.operations().get(kill.id()).orElseThrow().state().terminal());
        assertEquals(OperationState.SUCCEEDED,runtime.operations().get(kill.id()).orElseThrow().state()); assertFalse(game.isRunning());
        assertTrue(runtime.audit().verifyChain().valid());
    }
}
