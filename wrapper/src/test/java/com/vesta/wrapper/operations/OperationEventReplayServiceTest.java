package com.vesta.wrapper.operations;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.authorization.BuiltinRoles;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.operations.OperationEventReplayService.Page;
import com.vesta.wrapper.operations.OperationEventReplayService.Status;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateException;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;
import org.junit.jupiter.api.io.TempDir;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.*;

class OperationEventReplayServiceTest {
    private static final String MARKER = "TOP-SECRET-MARKER";
    private static final String ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    @TempDir Path root;
    private CanonicalOperationFixture fixture;
    private OperationEventReplayService replay;
    private AuditActor actor;

    @BeforeEach void open() {
        fixture = new CanonicalOperationFixture(root.resolve("state"));
        replay = new OperationEventReplayService(fixture.db, new EventLog(fixture.db), fixture.identities.authorization());
        actor = new AuditActor(fixture.owner.principalId(), fixture.owner.deviceId(), "https");
    }

    @AfterEach void close() { fixture.close(); }

    // ------------------------------------------------------------------ fixtures

    private String submit(String key) {
        return fixture.operations.submit(new OperationRequest(actor, "files.delete", "private-target-" + key,
                "{\"secret\":\"input-secret-" + key + "\"}", key, List.of("resource-" + key), false, false)).operation().id();
    }

    private void sql(String statement) {
        fixture.db.write(c -> { try (var s = c.createStatement()) { s.execute(statement); } return null; });
    }

    private long raw(String type, String operationId, String payload) {
        return fixture.db.write(c -> { try (var ps = c.prepareStatement(
                "INSERT INTO events (server_id, type, operation_id, at, payload_json) VALUES (?,?,?,?,?)")) {
            ps.setString(1, fixture.db.serverId()); ps.setString(2, type); ps.setString(3, operationId);
            ps.setLong(4, fixture.clock.millis()); ps.setString(5, payload); ps.executeUpdate();
            try (var s = c.createStatement(); var r = s.executeQuery("SELECT last_insert_rowid()")) { r.next(); return r.getLong(1); }
        } });
    }

    private long rawOperation(String state, int progress) {
        String id = UUID.randomUUID().toString();
        return raw("operation.state", id, "{\"operationId\":\"" + id + "\",\"state\":\"" + state + "\",\"progress\":" + progress + "}");
    }

    private String forge(String server, String position, String high) { return forgeRaw("v1." + server + "." + position + "." + high); }

    private static String forgeRaw(String plain) {
        return Base64.getUrlEncoder().withoutPadding().encodeToString(plain.getBytes(StandardCharsets.US_ASCII));
    }

    private static List<String> ids(Page page) { return page.events().stream().map(OperationEventReplayService.Event::eventId).toList(); }

    private List<Integer> counts() {
        List<Integer> out = new ArrayList<>();
        for (String table : new String[]{"events", "audit_events", "operations", "principals", "devices", "role_assignments",
                "settings", "confirmations", "resource_locks"}) out.add(fixture.count(table));
        return out;
    }

    private void code(String expected, Executable action) {
        StateException failure = assertThrows(StateException.class, action);
        assertEquals(expected, failure.getCode());
        assertNull(failure.getCause());
        assertFalse(failure.toString().contains(MARKER));
    }

    // ------------------------------------------------------------------ projection

    @Test void realOperationLifecycleProducesOrderedSanitizedEventsWithoutInputsResultsOrProgressText() {
        String id = submit("lifecycle");
        fixture.operations.claimNext().orElseThrow();
        fixture.operations.markRunning(id);
        fixture.operations.reportProgress(id, 40, "progress-secret-message");
        fixture.operations.succeed(id, "{\"result\":\"result-secret\"}");
        Page page = replay.openReplay(fixture.owner, null, 100);
        assertEquals(Status.OK, page.status()); assertTrue(page.complete()); assertEquals(1, page.apiVersion());
        assertEquals(List.of("operation.created", "operation.state", "operation.state", "operation.progress", "operation.state"),
                page.events().stream().map(OperationEventReplayService.Event::type).toList());
        assertEquals(List.of(OperationState.QUEUED, OperationState.VALIDATING, OperationState.RUNNING, OperationState.RUNNING,
                OperationState.SUCCEEDED), page.events().stream().map(OperationEventReplayService.Event::state).toList());
        assertEquals(40, page.events().get(3).progress());
        assertTrue(page.events().stream().allMatch(e -> e.operationId().equals(id)));
        assertEquals(List.of("1", "2", "3", "4", "5"), ids(page));
        String rendered = page.toString();
        for (String leaked : new String[]{"private-target", "input-secret", "progress-secret", "result-secret", "resource-lifecycle",
                fixture.owner.principalId(), fixture.owner.deviceId(), "files.delete"}) assertFalse(rendered.contains(leaked), leaked);
    }

    @Test void replayAndCheckpointsAreReadOnlyAndConfirmNoAuthority() {
        submit("a"); submit("b");
        List<Integer> before = counts(); long auditHead = fixture.audit.page(0, 1000).size();
        Page first = replay.openReplay(fixture.owner, null, 1);
        replay.readPage(fixture.owner, first.nextCursor(), 10); replay.currentCheckpoint(fixture.owner);
        replay.openReplay(fixture.owner, first.nextCursor(), 10);
        assertEquals(before, counts()); assertEquals(auditHead, fixture.audit.page(0, 1000).size());
        assertTrue(fixture.audit.verifyChain().valid());
    }

    @Test void emptyJournalIsAnOrdinaryCompleteWindowAndCheckpointStartsAtTheHead() {
        Page empty = replay.openReplay(fixture.owner, null, 10);
        assertEquals(Status.OK, empty.status()); assertTrue(empty.events().isEmpty()); assertTrue(empty.complete());
        assertEquals("0", empty.retentionFloor()); assertNotNull(empty.nextCursor());
        submit("later");
        assertEquals(List.of("1"), ids(replay.openReplay(fixture.owner, empty.nextCursor(), 10)));
    }

    @Test void pageEventListsAreImmutableAndRenderedMaximumPageStaysBounded() {
        sql("DELETE FROM events");
        for (int i = 0; i < 100; i++) rawOperation("awaiting_confirmation", 100);
        Page page = replay.openReplay(fixture.owner, null, 100);
        assertEquals(100, page.events().size());
        assertThrows(UnsupportedOperationException.class, () -> page.events().add(page.events().get(0)));
        assertTrue(page.toString().length() < 64 * 1024);
    }

    @Test void clockRollbackDoesNotReorderEventsBecauseOrderIsTheJournalCursor() {
        String first = submit("before"); fixture.clock.advance(Duration.ofHours(-5)); String second = submit("after");
        Page page = replay.openReplay(fixture.owner, null, 10);
        assertEquals(List.of(first, second), page.events().stream().map(OperationEventReplayService.Event::operationId).toList());
        assertTrue(page.events().get(1).at().isBefore(page.events().get(0).at()));
    }

    // ------------------------------------------------------------------ pagination and windows

    @Test void returnLimitPagesWithoutDuplicatesOrSkippedEvents() {
        for (int i = 0; i < 5; i++) submit("p" + i);
        Page one = replay.openReplay(fixture.owner, null, 2);
        assertEquals(List.of("1", "2"), ids(one)); assertFalse(one.complete());
        Page two = replay.readPage(fixture.owner, one.nextCursor(), 2);
        assertEquals(List.of("3", "4"), ids(two)); assertFalse(two.complete());
        Page three = replay.readPage(fixture.owner, two.nextCursor(), 2);
        assertEquals(List.of("5"), ids(three)); assertTrue(three.complete());
        Page again = replay.readPage(fixture.owner, three.nextCursor(), 2);
        assertTrue(again.events().isEmpty()); assertTrue(again.complete());
    }

    @Test void windowNeverChasesNewAppendsButACheckpointOpensTheNextWindow() {
        for (int i = 0; i < 3; i++) submit("w" + i);
        Page one = replay.openReplay(fixture.owner, null, 2);
        submit("late");
        Page two = replay.readPage(fixture.owner, one.nextCursor(), 10);
        assertEquals(List.of("3"), ids(two)); assertTrue(two.complete());
        assertEquals(List.of("4"), ids(replay.openReplay(fixture.owner, two.nextCursor(), 10)));
    }

    @Test void unknownKindsAreSkippedNeverExposedAndTheLimitDoesNotDropEligibleEventsAtTheTail() {
        String a = UUID.randomUUID().toString();
        raw("operation.created", a, "{\"operationId\":\"" + a + "\",\"state\":\"queued\",\"progress\":0}");
        raw("settings.changed", null, "{\"password\":\"" + MARKER + "\"}");
        rawOperation("running", 1);
        raw("audit.entry", null, "{\"password\":\"" + MARKER + "\"}");
        rawOperation("running", 2);
        Page one = replay.openReplay(fixture.owner, null, 2);
        assertEquals(List.of("1", "3"), ids(one)); assertFalse(one.complete());
        Page two = replay.readPage(fixture.owner, one.nextCursor(), 2);
        assertEquals(List.of("5"), ids(two)); assertTrue(two.complete());
        assertFalse(one.toString().contains(MARKER)); assertFalse(two.toString().contains(MARKER));
    }

    @Test void onlyUnknownKindsAdvanceToACompleteEmptyPageAndHugeUnrelatedPayloadsAreNotRequired() {
        raw("settings.changed", null, "x".repeat(2_000_000));
        raw("something.else", null, "{\"password\":\"" + MARKER + "\"}");
        Page page = replay.openReplay(fixture.owner, null, 10);
        assertTrue(page.events().isEmpty()); assertTrue(page.complete());
        assertFalse(page.toString().contains(MARKER));
        rawOperation("running", 3);
        assertEquals(List.of("3"), ids(replay.openReplay(fixture.owner, page.nextCursor(), 10)));
    }

    @Test void scanBudgetAdvancesPastMoreThanOnePageOfUnrelatedRowsWithoutClaimingCompletion() {
        fixture.db.write(c -> { try (var ps = c.prepareStatement(
                "INSERT INTO events (server_id, type, operation_id, at, payload_json) VALUES (?, 'other.kind', NULL, 0, '{}')")) {
            ps.setString(1, fixture.db.serverId());
            for (int i = 0; i < 520; i++) ps.executeUpdate();
        } return null; });
        rawOperation("running", 9);
        Page one = replay.openReplay(fixture.owner, null, 10);
        assertTrue(one.events().isEmpty()); assertFalse(one.complete());
        Page two = replay.readPage(fixture.owner, one.nextCursor(), 10);
        assertEquals(List.of("521"), ids(two)); assertTrue(two.complete());
    }

    @Test void journalHolesDoNotHideLaterEventsOrBlockCompletion() {
        rawOperation("running", 1); rawOperation("running", 2); rawOperation("running", 3);
        sql("DELETE FROM events WHERE cursor = 2");
        Page page = replay.openReplay(fixture.owner, null, 10);
        assertEquals(List.of("1", "3"), ids(page)); assertTrue(page.complete());
    }

    @Test void lostTailMakesAFutureCheckpointInvalidInsteadOfSilentlyEmpty() {
        rawOperation("running", 1); rawOperation("running", 2); rawOperation("running", 3);
        String token = replay.openReplay(fixture.owner, null, 10).nextCursor();
        sql("DELETE FROM events WHERE cursor = 3");
        code("invalid_cursor", () -> replay.openReplay(fixture.owner, token, 10));
        code("invalid_cursor", () -> replay.readPage(fixture.owner, token, 10));
    }

    @Test void concurrentAppendsNeverProduceDuplicatesGapsOrReorderingAcrossWindows() throws Exception {
        AtomicReference<Throwable> writerFailure = new AtomicReference<>();
        int total = 200;
        Thread writer = new Thread(() -> {
            try { for (int i = 0; i < total; i++) rawOperation("running", i % 100); }
            catch (Throwable t) { writerFailure.set(t); }
        });
        writer.start();
        List<Long> seen = new ArrayList<>(); String checkpoint = null;
        long deadline = System.nanoTime() + Duration.ofSeconds(30).toNanos();
        while (seen.size() < total && System.nanoTime() < deadline) {
            Page page = replay.openReplay(fixture.owner, checkpoint, 7);
            while (true) {
                for (var event : page.events()) seen.add(Long.parseLong(event.eventId()));
                checkpoint = page.nextCursor();
                if (page.complete()) break;
                page = replay.readPage(fixture.owner, page.nextCursor(), 7);
            }
        }
        writer.join(10_000);
        if (writerFailure.get() != null) throw new AssertionError(writerFailure.get());
        assertEquals(total, seen.size());
        for (int i = 0; i < seen.size(); i++) assertEquals(i + 1L, seen.get(i));
    }

    // ------------------------------------------------------------------ retention

    @Test void prunedHistoryRequiresExplicitResyncAndNeverSilentlySkips() {
        submit("a"); fixture.clock.advance(Duration.ofSeconds(10));
        submit("b"); fixture.clock.advance(Duration.ofSeconds(10)); submit("c");
        Page early = replay.openReplay(fixture.owner, null, 1);
        assertEquals(List.of("1"), ids(early));
        assertEquals(2, new EventLog(fixture.db).pruneOlderThan(fixture.clock.instant().minusSeconds(5)));
        Page resync = replay.readPage(fixture.owner, early.nextCursor(), 10);
        assertEquals(Status.RESYNC_REQUIRED, resync.status()); assertTrue(resync.events().isEmpty());
        assertNull(resync.nextCursor()); assertFalse(resync.complete()); assertEquals("2", resync.retentionFloor());
        assertEquals(Status.RESYNC_REQUIRED, replay.openReplay(fixture.owner, null, 10).status());
        Page ok = replay.openReplay(fixture.owner, forge(fixture.db.serverId(), "2", "2"), 10);
        assertEquals(List.of("3"), ids(ok)); assertEquals(Status.OK, ok.status());
    }

    @Test void entirelyPrunedJournalKeepsItsHeadAndAppendsContinueAfterIt() {
        submit("a"); submit("b");
        String stale = replay.openReplay(fixture.owner, null, 1).nextCursor();
        fixture.clock.advance(Duration.ofSeconds(10));
        assertEquals(2, new EventLog(fixture.db).pruneOlderThan(fixture.clock.instant()));
        assertEquals(Status.RESYNC_REQUIRED, replay.openReplay(fixture.owner, stale, 10).status());
        Page head = replay.currentCheckpoint(fixture.owner);
        assertTrue(head.complete()); assertEquals("2", head.retentionFloor());
        assertTrue(replay.openReplay(fixture.owner, head.nextCursor(), 10).events().isEmpty());
        code("invalid_cursor", () -> replay.openReplay(fixture.owner, forge(fixture.db.serverId(), "3", "3"), 10));
        submit("c");
        assertEquals(List.of("3"), ids(replay.openReplay(fixture.owner, head.nextCursor(), 10)));
    }

    // ------------------------------------------------------------------ cursor and limit validation

    @Test void everyMalformedForgedOrAheadCursorIsRejectedForOpenAndContinuation() {
        for (int i = 0; i < 3; i++) rawOperation("running", i);
        String server = fixture.db.serverId(); String valid = forge(server, "1", "3");
        String corrupted = null;
        for (char c : ALPHABET.toCharArray()) {
            String candidate = valid.substring(0, valid.length() - 1) + c;
            if (candidate.equals(valid)) continue;
            try {
                String again = Base64.getUrlEncoder().withoutPadding().encodeToString(Base64.getUrlDecoder().decode(candidate));
                if (!again.equals(candidate)) { corrupted = candidate; break; }
            } catch (IllegalArgumentException ignored) { }
        }
        assertNotNull(corrupted, "fixture must find a decodable non-canonical encoding");
        List<String> bad = new ArrayList<>(List.of("", "!!!", "A".repeat(257), valid + "=", corrupted,
                forgeRaw("v2." + server + ".1.3"), forge(UUID.randomUUID().toString(), "1", "3"),
                forge(server, "-1", "3"), forge(server, "01", "3"), forge(server, "a", "3"), forge(server, "1", "3.4"),
                forge(server, "9223372036854775808", "9223372036854775808"), forge(server, "4", "4"), forge(server, "3", "2"),
                forge(server, "1", "4"), forgeRaw("v1." + server + ".1"), forgeRaw("not-a-cursor")));
        for (String token : bad) {
            assertEquals("invalid_cursor", assertThrows(StateException.class, () -> replay.readPage(fixture.owner, token, 10)).getCode(), token);
            if (!token.equals(forge(server, "1", "4")) && !token.equals(forge(server, "3", "2")))
                assertEquals("invalid_cursor", assertThrows(StateException.class, () -> replay.openReplay(fixture.owner, token, 10)).getCode(), token);
        }
        code("invalid_cursor", () -> replay.readPage(fixture.owner, null, 10));
        assertEquals(List.of("2", "3"), ids(replay.readPage(fixture.owner, valid, 10)));
        // the same position with a stale/larger embedded horizon reopens cleanly because open recaptures it
        assertEquals(List.of("2", "3"), ids(replay.openReplay(fixture.owner, forge(server, "1", "1"), 10)));
    }

    @ParameterizedTest @ValueSource(ints = {-1, 0, 101, Integer.MAX_VALUE, Integer.MIN_VALUE})
    void limitsOutsideOneToOneHundredAreRejectedBeforeAnyRead(int limit) {
        submit("a");
        code("invalid_request", () -> replay.openReplay(fixture.owner, null, limit));
        code("invalid_request", () -> replay.readPage(fixture.owner, replay.currentCheckpoint(fixture.owner).nextCursor(), limit));
        assertEquals(1, replay.openReplay(fixture.owner, null, 100).events().size());
        assertEquals(1, replay.openReplay(fixture.owner, null, 1).events().size());
    }

    // ------------------------------------------------------------------ canonical authority

    @ParameterizedTest @ValueSource(strings = {"null", "unknown", "system", "malformed", "wrong-principal", "wrong-device",
            "revoked", "disabled", "denied", "expired", "assignment-revoked", "moderator", "pack-user"})
    void everyCallResolvesCurrentExactTasksViewAndCursorsGiveNoAuthority(String change) {
        submit("a"); submit("b");
        String token = replay.openReplay(fixture.owner, null, 1).nextCursor();
        RemoteActor caller = fixture.owner;
        switch (change) {
            case "null" -> caller = null;
            case "unknown" -> caller = new RemoteActor("unknown", "unknown");
            case "system" -> caller = new RemoteActor("system", "local");
            case "malformed" -> caller = new RemoteActor("\n", null);
            case "wrong-principal" -> caller = new RemoteActor(UUID.randomUUID().toString(), fixture.owner.deviceId());
            case "wrong-device" -> caller = new RemoteActor(fixture.owner.principalId(), UUID.randomUUID().toString());
            case "revoked" -> fixture.identities.revokeDevice(fixture.owner, fixture.owner.deviceId());
            case "disabled" -> sql("UPDATE principals SET status='disabled'");
            case "denied" -> fixture.deny(Capability.TASKS_VIEW);
            case "expired" -> sql("UPDATE role_assignments SET expires_at=" + fixture.clock.millis());
            case "assignment-revoked" -> sql("UPDATE role_assignments SET revoked_at=" + fixture.clock.millis());
            case "moderator" -> sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.MODERATOR + "'");
            case "pack-user" -> sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.PACK_USER + "'");
        }
        RemoteActor denied = caller; List<Integer> before = counts();
        code("forbidden", () -> replay.openReplay(denied, null, 10));
        code("forbidden", () -> replay.readPage(denied, token, 10));
        code("forbidden", () -> replay.currentCheckpoint(denied));
        // no cursor-validity oracle for an unauthorized caller
        code("forbidden", () -> replay.openReplay(denied, "not-a-cursor", 10));
        code("forbidden", () -> replay.readPage(denied, forge(fixture.db.serverId(), "99", "99"), 10));
        assertEquals(before, counts());
    }

    @ParameterizedTest @ValueSource(strings = {BuiltinRoles.VIEWER, BuiltinRoles.MAINTAINER, BuiltinRoles.ADMINISTRATOR})
    void exactCapabilityHoldersMayObserveTaskLifecycleButAnExplicitDenyWins(String role) {
        submit("a");
        sql("UPDATE role_assignments SET role_id='" + role + "'");
        assertEquals(1, replay.openReplay(fixture.owner, null, 10).events().size());
        fixture.deny(Capability.TASKS_VIEW);
        code("forbidden", () -> replay.openReplay(fixture.owner, null, 10));
    }

    @Test void revocationDowngradeAndDenyBetweenPagesStopTheNextPage() {
        for (int i = 0; i < 4; i++) submit("p" + i);
        Page first = replay.openReplay(fixture.owner, null, 1);
        sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.MODERATOR + "'");
        code("forbidden", () -> replay.readPage(fixture.owner, first.nextCursor(), 1));
        sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.VIEWER + "'");
        Page second = replay.readPage(fixture.owner, first.nextCursor(), 1);
        assertEquals(List.of("2"), ids(second));
        sql("UPDATE role_assignments SET role_id='" + BuiltinRoles.OWNER + "'"); // revocation needs the owner role
        fixture.identities.revokeDevice(fixture.owner, fixture.owner.deviceId());
        code("forbidden", () -> replay.readPage(fixture.owner, second.nextCursor(), 1));
    }

    // ------------------------------------------------------------------ strict projection and failure redaction

    @ParameterizedTest @ValueSource(strings = {
            "{\"operationId\":\"%ID%\",\"state\":\"running\"}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":1,\"leak\":\"TOP-SECRET-MARKER\"}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"state\":\"failed\",\"progress\":1}",
            "{}", "[]", "null", "\"TOP-SECRET-MARKER\"",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":1} {}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":1}x",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":50.5}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":1.0}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":1e1}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":-1}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":101}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":\"50\"}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":050}",
            "{\"operationId\":\"%ID%\",\"state\":\"running\",\"progress\":true}",
            "{\"operationId\":\"%ID%\",\"state\":\"TOP-SECRET-MARKER\",\"progress\":1}",
            "{\"operationId\":\"%ID%\",\"state\":5,\"progress\":1}",
            "{\"operationId\":null,\"state\":\"running\",\"progress\":1}",
            "{\"operationId\":\"00000000-0000-4000-8000-000000000000\",\"state\":\"running\",\"progress\":1}"})
    void malformedRecognizedEventsFailTheWholePageClosedWithoutLeakingStoredText(String template) {
        rawOperation("running", 1);
        String id = UUID.randomUUID().toString();
        raw("operation.state", id, template.replace("%ID%", id));
        rawOperation("running", 2);
        code("event_malformed", () -> replay.openReplay(fixture.owner, null, 10));
        code("event_malformed", () -> replay.readPage(fixture.owner, forge(fixture.db.serverId(), "0", "3"), 10));
    }

    @ParameterizedTest @ValueSource(strings = {"missing", "not-a-uuid", "UPPERCASE", "oversize-payload"})
    void headerAndPayloadSizeViolationsOnRecognizedKindsFailClosed(String kind) {
        String id = UUID.randomUUID().toString();
        String good = "{\"operationId\":\"" + id + "\",\"state\":\"running\",\"progress\":1}";
        switch (kind) {
            case "missing" -> raw("operation.progress", null, good);
            case "not-a-uuid" -> raw("operation.progress", "not-a-uuid", good);
            case "UPPERCASE" -> raw("operation.progress", id.toUpperCase(), good.replace(id, id.toUpperCase()));
            default -> raw("operation.progress", id, "{\"operationId\":\"" + id + "\",\"state\":\"running\",\"progress\":1,\"pad\":\""
                    + "x".repeat(EventLog.MAX_PAYLOAD_CHARS) + "\"}");
        }
        code("event_malformed", () -> replay.openReplay(fixture.owner, null, 10));
    }

    @Test void runtimeFaultsAndSqlFailuresAreRedactedToFixedCodes() {
        rawOperation("running", 1);
        sql("UPDATE meta SET value='" + MARKER + "' WHERE key='events_pruned_through'");
        StateException corrupted = assertThrows(StateException.class, () -> replay.openReplay(fixture.owner, null, 10));
        assertEquals("replay_unavailable", corrupted.getCode()); assertNull(corrupted.getCause());
        assertFalse(corrupted.toString().contains(MARKER));
        sql("UPDATE meta SET value='0' WHERE key='events_pruned_through'");
        sql("ALTER TABLE events RENAME TO " + MARKER.replace('-', '_').toLowerCase());
        StateException io = assertThrows(StateException.class, () -> replay.openReplay(fixture.owner, null, 10));
        assertEquals("state_io", io.getCode()); assertNull(io.getCause());
        assertFalse(io.toString().toLowerCase().contains("top_secret"));
    }

    @Test void closedDatabaseFailsWithAStableCodeAndNoCause() {
        fixture.db.close();
        code("state_closed", () -> replay.openReplay(fixture.owner, null, 10));
        code("state_closed", () -> replay.currentCheckpoint(fixture.owner));
    }
}
