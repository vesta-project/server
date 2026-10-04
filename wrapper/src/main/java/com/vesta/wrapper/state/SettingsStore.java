package com.vesta.wrapper.state;

import com.vesta.wrapper.audit.AuditActor;
import com.vesta.wrapper.audit.AuditLog;
import com.vesta.wrapper.audit.AuditRecord;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;

/**
 * Desired versus effective configuration with explicit apply modes. Not for secrets: values are
 * stored in plain text in the state database. Each change is audited atomically.
 */
public final class SettingsStore {
    public static final int MAX_VALUE_CHARS = 4_096;

    public enum ApplyMode {
        IMMEDIATE, ON_RESTART, MANUAL;

        public String wireName() {
            return name().toLowerCase(Locale.ROOT);
        }

        public static ApplyMode fromWire(String value) {
            return valueOf(value.toUpperCase(Locale.ROOT));
        }
    }

    public record Setting(String key, String desiredValue, long desiredRevision, ApplyMode applyMode,
                          String effectiveValue, long effectiveRevision, Instant updatedAt) {
        public boolean pending() {
            return effectiveRevision != desiredRevision;
        }
    }

    private final StateDatabase db;
    private final AuditLog audit;

    public SettingsStore(StateDatabase db, AuditLog audit) {
        this.db = db;
        this.audit = audit;
    }

    public Setting setDesired(AuditActor actor, String key, String value, ApplyMode mode) {
        StateValues.requireText("setting key", key, 128);
        if (value == null || value.length() > MAX_VALUE_CHARS) {
            throw new StateException("invalid_request", "setting value missing or too large");
        }
        return db.write(c -> {
            long now = db.clock().millis();
            try (PreparedStatement ps = c.prepareStatement(
                    "INSERT INTO settings (server_id, key, desired_value, desired_revision, apply_mode, updated_at) "
                            + "VALUES (?,?,?,1,?,?) ON CONFLICT (server_id, key) DO UPDATE SET "
                            + "desired_value = excluded.desired_value, desired_revision = settings.desired_revision + 1, "
                            + "apply_mode = excluded.apply_mode, updated_at = excluded.updated_at")) {
                ps.setString(1, db.serverId());
                ps.setString(2, key);
                ps.setString(3, value);
                ps.setString(4, mode.wireName());
                ps.setLong(5, now);
                ps.executeUpdate();
            }
            Setting s = load(c, key).orElseThrow();
            audit.append(c, new AuditRecord(actor, "settings.set_desired", "setting:" + key, "ok", null,
                    Map.of("revision", Long.toString(s.desiredRevision()), "applyMode", mode.wireName())));
            return s;
        });
    }

    /** Records that {@code revision} of the desired value is now effective. */
    public Setting markEffective(AuditActor actor, String key, long revision) {
        return db.write(c -> {
            Setting s = load(c, key).orElseThrow(() -> new StateException("not_found", "Unknown setting"));
            if (revision != s.desiredRevision()) {
                throw new StateException("stale_revision", "Revision is not the current desired revision");
            }
            try (PreparedStatement ps = c.prepareStatement(
                    "UPDATE settings SET effective_value = desired_value, effective_revision = desired_revision, "
                            + "updated_at = ? WHERE server_id = ? AND key = ? AND desired_revision = ?")) {
                ps.setLong(1, db.clock().millis());
                ps.setString(2, db.serverId());
                ps.setString(3, key);
                ps.setLong(4, revision);
                ps.executeUpdate();
            }
            audit.append(c, new AuditRecord(actor, "settings.mark_effective", "setting:" + key, "ok", null,
                    Map.of("revision", Long.toString(revision))));
            return load(c, key).orElseThrow();
        });
    }

    public Optional<Setting> get(String key) {
        return db.read(c -> load(c, key));
    }

    public List<Setting> pending() {
        return db.read(c -> {
            List<Setting> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT key FROM settings WHERE server_id = ? AND desired_revision <> effective_revision ORDER BY key")) {
                ps.setString(1, db.serverId());
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(load(c, rs.getString(1)).orElseThrow());
                    }
                }
            }
            return List.copyOf(out);
        });
    }

    private Optional<Setting> load(Connection c, String key) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT key, desired_value, desired_revision, apply_mode, effective_value, effective_revision, updated_at "
                        + "FROM settings WHERE server_id = ? AND key = ?")) {
            ps.setString(1, db.serverId());
            ps.setString(2, key);
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next()) {
                    return Optional.empty();
                }
                return Optional.of(new Setting(rs.getString(1), rs.getString(2), rs.getLong(3),
                        ApplyMode.fromWire(rs.getString(4)), rs.getString(5), rs.getLong(6),
                        Instant.ofEpochMilli(rs.getLong(7))));
            }
        }
    }
}
