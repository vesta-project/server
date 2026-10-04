package com.vesta.wrapper.operations;

import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.authorization.Capability;
import com.vesta.wrapper.identity.IdentityRecords.RemoteActor;
import com.vesta.wrapper.identity.IdentityService;
import com.vesta.wrapper.identity.LocalAuthority;
import com.vesta.wrapper.state.EventLog;
import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateDirectory;
import com.vesta.wrapper.state.StateValues;

import java.nio.file.Path;
import java.time.Clock;
import java.time.Duration;
import java.time.Instant;
import java.time.ZoneId;
import java.time.ZoneOffset;
import java.util.Optional;
import java.util.UUID;

/** Real SQLite/identity fixture; its local proof verifier is never used by production startup. */
public final class CanonicalOperationFixture implements AutoCloseable {
    public final MutableClock clock = new MutableClock();
    public final StateDirectory directory;
    public final StateDatabase db;
    public final AuditLog audit;
    public final IdentityService identities;
    public final OperationService operations;
    public final ConfirmationService confirmations;
    public final RemoteActor owner;

    private record Proof(LocalAuthority.Attestation attestation) implements LocalAuthority.Proof { }

    public CanonicalOperationFixture(Path state) {
        directory = new StateDirectory(state);
        db = StateDatabase.open(directory, clock);
        audit = new AuditLog(db);
        identities = new IdentityService(db, audit, localAuthority());
        operations = new OperationService(db, audit, new EventLog(db));
        confirmations = new ConfirmationService(db, audit, operations);
        owner = bootstrap(identities, clock);
    }

    public static LocalAuthority localAuthority() {
        return (proof, purpose) -> proof instanceof Proof p ? Optional.of(p.attestation()) : Optional.empty();
    }

    public static RemoteActor bootstrap(IdentityService identities, Clock clock) {
        var proof = new Proof(new LocalAuthority.Attestation(UUID.randomUUID().toString(),
                LocalAuthority.Purpose.OWNER_BOOTSTRAP, "fixture-os-user", clock.instant(), clock.instant().plusSeconds(60)));
        var code = identities.issueOwnerBootstrapCode(proof, null);
        var paired = identities.redeemOwnerCode(code.code(), "Fixture owner", "Fixture device", StateValues.sha256Hex("fixture-owner-key"));
        return new RemoteActor(paired.principalId(), paired.deviceId());
    }

    public void deny(Capability capability) {
        db.write(c -> {
            try (var ps = c.prepareStatement("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'','deny','fixture',?)")) {
                ps.setString(1, UUID.randomUUID().toString()); ps.setString(2, owner.principalId());
                ps.setString(3, capability.id()); ps.setLong(4, clock.millis()); ps.executeUpdate();
            }
            return null;
        });
    }

    public int count(String table) {
        return db.read(c -> { try (var statement = c.createStatement(); var row = statement.executeQuery("SELECT count(*) FROM " + table)) {
            row.next(); return row.getInt(1);
        } });
    }

    public void auditFailure(boolean enabled) {
        db.write(c -> { try (var statement = c.createStatement()) {
            statement.execute(enabled
                    ? "CREATE TRIGGER audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'injected'); END"
                    : "DROP TRIGGER audit_failure");
        } return null; });
    }

    @Override public void close() { db.close(); }

    public static final class MutableClock extends Clock {
        private volatile Instant now = Instant.parse("2026-10-05T12:00:00Z");
        public void advance(Duration duration) { now = now.plus(duration); }
        @Override public ZoneId getZone() { return ZoneOffset.UTC; }
        @Override public Clock withZone(ZoneId zone) { return Clock.fixed(now, zone); }
        @Override public Instant instant() { return now; }
    }
}
