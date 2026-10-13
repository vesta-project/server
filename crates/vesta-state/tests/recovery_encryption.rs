mod support;

use std::io::Cursor;
use vesta_state::export::{read_recovery_export, write_recovery_export};

const PASSPHRASE: &[u8] = b"recovery test passphrase";
const WRONG_PASSPHRASE: &[u8] = b"not the recovery passphrase";
const HEADER_LEN: usize = 48;
const SEGMENT_LEN: usize = 1024 * 1024;
const TAG_LEN: usize = 16;

fn encrypted_multisegment_export() -> Vec<u8> {
    let fixture = support::Fixture::open();
    // Seed through SQLite rather than the settings API: this exercises the export
    // stream's segmentation without relaxing normal application input bounds.
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "INSERT INTO settings \
                 (server_id,key,desired_value,desired_revision,apply_mode,effective_value,updated_at) \
                 VALUES (?1,'recovery-encryption-segment-test',?2,1,'immediate',NULL,1)",
                rusqlite::params![fixture.db.server_id(), "x".repeat(2 * SEGMENT_LEN)],
            )?;
            Ok(())
        })
        .unwrap();

    let mut output = Vec::new();
    write_recovery_export(&fixture.db, &mut output, Some(PASSPHRASE)).unwrap();
    output
}

fn assert_decrypt_failed(bytes: &[u8], passphrase: Option<&[u8]>) {
    let error = read_recovery_export(&mut Cursor::new(bytes), passphrase).unwrap_err();
    assert_eq!(error.code(), "state_export_decrypt_failed");
}

fn frames(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut offset = HEADER_LEN;
    loop {
        let start = offset;
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let is_last = bytes[offset + 4] == 1;
        offset += 5 + length + TAG_LEN;
        result.push((start, offset));
        if is_last {
            assert_eq!(offset, bytes.len());
            break;
        }
    }
    result
}

#[test]
fn recovery_export_roundtrips_encrypted_and_plaintext() {
    let fixture = support::Fixture::open();
    let mut plaintext = Vec::new();
    write_recovery_export(&fixture.db, &mut plaintext, None).unwrap();

    let plain = read_recovery_export(&mut Cursor::new(&plaintext), None).unwrap();
    assert_eq!(plain.server_id, fixture.db.server_id());
    assert_eq!(plain.schema_version, vesta_state::CURRENT_SCHEMA_VERSION);
    assert!(!plain.tables.is_empty());

    let mut encrypted = Vec::new();
    write_recovery_export(&fixture.db, &mut encrypted, Some(PASSPHRASE)).unwrap();
    assert!(encrypted.starts_with(b"VESTAEX1"));
    let recovered = read_recovery_export(&mut Cursor::new(&encrypted), Some(PASSPHRASE)).unwrap();
    assert_eq!(recovered.server_id, plain.server_id);
    assert_eq!(recovered.schema_version, plain.schema_version);
    assert_eq!(
        serde_json::to_value(recovered.tables).unwrap(),
        serde_json::to_value(plain.tables).unwrap()
    );
}

#[test]
fn encrypted_export_rejects_wrong_or_missing_passphrase() {
    let encrypted = encrypted_multisegment_export();

    let wrong =
        read_recovery_export(&mut Cursor::new(&encrypted), Some(WRONG_PASSPHRASE)).unwrap_err();
    assert_eq!(wrong.code(), "state_export_decrypt_failed");

    let missing = read_recovery_export(&mut Cursor::new(&encrypted), None).unwrap_err();
    assert_eq!(missing.code(), "state_export_passphrase_required");
}

#[test]
fn encrypted_export_rejects_header_body_final_and_truncation_tampering() {
    let encrypted = encrypted_multisegment_export();
    let segment_frames = frames(&encrypted);
    assert!(
        segment_frames.len() >= 3,
        "expected more than two encrypted frames"
    );

    let mut header_flip = encrypted.clone();
    header_flip[8] ^= 1;
    assert_decrypt_failed(&header_flip, Some(PASSPHRASE));

    let mut body_flip = encrypted.clone();
    body_flip[HEADER_LEN + 5] ^= 1;
    assert_decrypt_failed(&body_flip, Some(PASSPHRASE));

    let (last_start, last_end) = *segment_frames.last().unwrap();
    let mut final_flip = encrypted.clone();
    final_flip[last_end - 1] ^= 1;
    assert_decrypt_failed(&final_flip, Some(PASSPHRASE));

    let mut truncated_final = encrypted.clone();
    truncated_final.truncate(last_end - 1);
    assert_decrypt_failed(&truncated_final, Some(PASSPHRASE));

    // Keep each frame intact while swapping the first two ciphertext segments.
    let (first_start, first_end) = segment_frames[0];
    let (second_start, second_end) = segment_frames[1];
    assert_eq!(first_start, HEADER_LEN);
    assert_eq!(first_end - first_start, second_end - second_start);
    let mut reordered = encrypted.clone();
    reordered[first_start..second_end].rotate_left(first_end - first_start);
    assert_decrypt_failed(&reordered, Some(PASSPHRASE));

    // The final segment must actually be marked final; altering its frame flag is
    // also authenticated and must not turn it into an acceptable continuation.
    let mut final_flag_flip = encrypted;
    final_flag_flip[last_start + 4] ^= 1;
    assert_decrypt_failed(&final_flag_flip, Some(PASSPHRASE));
}
