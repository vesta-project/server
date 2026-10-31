//! Legacy archive budgets are review thresholds, never authority to bypass safety.
use super::*;
use std::io::{Read, Seek};

fn warning(rule: &str, item: &str, actual: u64, limit: u64) -> InstallWarning {
    InstallWarning {
        code: "archive_limit_warning".into(),
        rule: rule.into(),
        item: item.into(),
        detail: format!("{actual} > {limit}"),
    }
}
fn exceeded(warnings: &mut Vec<InstallWarning>, rule: &str, item: &str, actual: u64, limit: u64) {
    if actual > limit {
        warnings.push(warning(rule, item, actual, limit));
    }
}
pub(crate) fn inspect(
    reader: impl Read + Seek,
    compressed: u64,
    item: &str,
) -> Result<Vec<InstallWarning>> {
    let mut zip =
        zip::ZipArchive::new(reader).map_err(|_| super::super::super::PackError::InvalidArchive)?;
    let mut warnings = Vec::new();
    exceeded(
        &mut warnings,
        "legacy_archive_bytes",
        item,
        compressed,
        super::super::super::MAX_ARTIFACT_BYTES as u64,
    );
    exceeded(
        &mut warnings,
        "legacy_archive_entries",
        item,
        zip.len() as u64,
        super::super::super::MAX_VESTA_ENTRIES as u64,
    );
    let mut total = 0u64;
    let mut metadata = 0u64;
    for index in 0..zip.len() {
        let file = zip
            .by_index(index)
            .map_err(|_| super::super::super::PackError::InvalidArchive)?;
        if file.is_dir() {
            continue;
        }
        total = total
            .checked_add(file.size())
            .ok_or(super::super::super::PackError::TooLarge)?;
        let member = format!("{item} / {}", file.name());
        exceeded(
            &mut warnings,
            "legacy_member_bytes",
            &member,
            file.size(),
            super::super::super::MAX_PINNED_FILE_BYTES as u64,
        );
        exceeded(
            &mut warnings,
            "catalogue_file_bytes",
            &member,
            file.size(),
            vesta_protocol::pack::catalogue::MAX_FILE_SIZE,
        );
        if matches!(
            file.name(),
            "modrinth.index.json" | "manifest.json" | vesta_protocol::pack::INDEX_NAME
        ) {
            metadata = metadata
                .checked_add(file.size())
                .ok_or(super::super::super::PackError::TooLarge)?;
        }
    }
    exceeded(
        &mut warnings,
        "legacy_blob_bytes",
        item,
        total,
        MAX_BLOBS as u64,
    );
    exceeded(
        &mut warnings,
        "catalogue_total_bytes",
        item,
        total,
        vesta_protocol::pack::catalogue::MAX_TOTAL_SIZE,
    );
    exceeded(
        &mut warnings,
        "catalogue_metadata_bytes",
        item,
        metadata,
        vesta_protocol::pack::catalogue::MAX_JSON_BYTES as u64,
    );
    Ok(warnings)
}
pub(super) fn payload_warnings(payload: &Payload, item: &str) -> Vec<InstallWarning> {
    let mut warnings = Vec::new();
    exceeded(
        &mut warnings,
        "legacy_member_bytes",
        item,
        payload.size,
        super::super::super::MAX_PINNED_FILE_BYTES as u64,
    );
    exceeded(
        &mut warnings,
        "catalogue_file_bytes",
        item,
        payload.size,
        vesta_protocol::pack::catalogue::MAX_FILE_SIZE,
    );
    warnings
}
pub(super) fn manifest_warnings(
    manifest: &PackManifest,
    total: u64,
    blobs: &BTreeMap<String, Payload>,
) -> Vec<InstallWarning> {
    let mut warnings = Vec::new();
    exceeded(
        &mut warnings,
        "catalogue_file_count",
        "pack",
        (manifest.entries.len() + manifest.override_files.len()) as u64,
        MAX_ENTRIES as u64,
    );
    exceeded(
        &mut warnings,
        "catalogue_metadata_bytes",
        "pack",
        serde_json::to_vec(manifest).map_or(0, |bytes| bytes.len()) as u64,
        vesta_protocol::pack::catalogue::MAX_JSON_BYTES as u64,
    );
    exceeded(
        &mut warnings,
        "catalogue_total_bytes",
        "pack",
        total,
        vesta_protocol::pack::catalogue::MAX_TOTAL_SIZE,
    );
    for file in &manifest.override_files {
        if let Some(payload) = file.sha256.as_ref().and_then(|hash| blobs.get(hash)) {
            exceeded(
                &mut warnings,
                "legacy_member_bytes",
                &file.path,
                payload.size,
                super::super::super::MAX_PINNED_FILE_BYTES as u64,
            );
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    #[test]
    fn thresholds_are_inclusive_and_have_reviewable_identity() {
        let mut warnings = Vec::new();
        exceeded(&mut warnings, "legacy_member_bytes", "config/data", 16, 16);
        assert!(warnings.is_empty());
        exceeded(&mut warnings, "legacy_member_bytes", "config/data", 17, 16);
        assert_eq!(warnings[0].code, "archive_limit_warning");
        assert_eq!(warnings[0].rule, "legacy_member_bytes");
        assert_eq!(warnings[0].item, "config/data");
    }
    #[test]
    fn oversized_member_reaches_review_instead_of_refusal() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            "config/large.dat",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&vec![
            0;
            super::super::super::super::MAX_PINNED_FILE_BYTES + 1
        ])
        .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let warnings = inspect(
            Cursor::new(&bytes),
            bytes.len() as u64,
            "https://example.com/pack.zip",
        )
        .unwrap();
        assert!(warnings.iter().any(|w| w.rule == "legacy_member_bytes"
            && w.item == "https://example.com/pack.zip / config/large.dat"));
    }
}
