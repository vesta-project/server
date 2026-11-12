//! Fixed-origin public catalog. Listing metadata selects candidates, never trust.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub tag: String,
    pub nightly: bool,
    pub published_at: String,
}
#[derive(Deserialize)]
struct Record {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
}
pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag.as_bytes()[0].is_ascii_alphanumeric()
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        && !tag.starts_with("installer-")
}
fn parse_page(bytes: &[u8]) -> Result<(Vec<Published>, usize), String> {
    let records: Vec<Record> =
        serde_json::from_slice(bytes).map_err(|_| "invalid release catalog")?;
    let count = records.len();
    if count > 10 {
        return Err("release catalog page exceeds entry limit".into());
    }
    Ok((
        records
            .into_iter()
            .filter_map(|record| {
                let nightly = record.tag_name.starts_with("nightly-");
                if record.draft || !valid_tag(&record.tag_name) || record.prerelease != nightly {
                    return None;
                }
                let published_at = record.published_at?;
                if published_at.len() != 20
                    || !published_at.bytes().enumerate().all(|(i, b)| match i {
                        4 | 7 => b == b'-',
                        10 => b == b'T',
                        13 | 16 => b == b':',
                        19 => b == b'Z',
                        _ => b.is_ascii_digit(),
                    })
                {
                    return None;
                }
                Some(Published {
                    tag: record.tag_name,
                    nightly,
                    published_at,
                })
            })
            .collect(),
        count,
    ))
}
pub fn catalog() -> Result<Vec<Published>, String> {
    let agent = http_agent();
    let mut releases = Vec::new();
    for page in 1..=10 {
        let bytes = fetch_bounded(
            &agent,
            &format!(
                "https://api.github.com/repos/vesta-project/server/releases?per_page=10&page={page}"
            ),
            4 * 1024 * 1024,
            "release catalog unavailable",
        )?;
        let (records, count) = parse_page(&bytes)?;
        releases.extend(records);
        if count < 10 {
            break;
        }
    }
    releases.sort_by(|a, b| {
        b.published_at
            .cmp(&a.published_at)
            .then_with(|| b.tag.cmp(&a.tag))
    });
    releases.dedup_by(|a, b| a.tag == b.tag);
    Ok(releases)
}
pub fn latest(releases: &[Published], nightly: bool) -> Result<&str, String> {
    releases
        .iter()
        .filter(|release| release.nightly == nightly)
        .max_by(|a, b| {
            a.published_at
                .cmp(&b.published_at)
                .then_with(|| a.tag.cmp(&b.tag))
        })
        .map(|release| release.tag.as_str())
        .ok_or_else(|| {
            if nightly {
                "No published nightly is available."
            } else {
                "No stable release has been published yet. Choose Nightly or an exact tag."
            }
            .into()
        })
}
pub fn load(tag: &str, release_key: &str) -> Result<Release, String> {
    if !valid_tag(tag) {
        return Err("invalid server release tag".into());
    }
    let key = decode_hex_exact(release_key, 32, "invalid release key")?;
    let target = crate::target()?;
    let prefix = format!("{target}-");
    let base = format!("https://github.com/vesta-project/server/releases/download/{tag}");
    let agent = http_agent();
    let bytes = fetch_bounded(
        &agent,
        &format!("{base}/{prefix}manifest.json"),
        MAX_MANIFEST,
        "release manifest unavailable",
    )?;
    let sig = fetch_bounded(
        &agent,
        &format!("{base}/{prefix}manifest.sig"),
        MAX_SIGNATURE,
        "release signature unavailable",
    )?;
    let hash = Sha256::digest(&bytes);
    // The catalog supplies no hashes/keys/URLs. Authenticate the manifest with
    // the independently trusted key before admitting any artifacts or effects.
    verified_release(
        ArtifactSource::Remote(base, prefix.clone()),
        &prefix,
        bytes,
        sig,
        &hash,
        &key,
        Some(tag),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn verified_selection_binds_version_signature_and_server_artifact_contract() {
        let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let target = crate::target().unwrap();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let manifest = Manifest {
            schema: 1,
            version: "v1.0".into(),
            target: target.into(),
            files: ["vesta-setup", "vesta-agent", "vesta-supervisor", "vesta"]
                .map(|name| Artifact {
                    name: format!("{name}{suffix}"),
                    sha256: "a".repeat(64),
                    size: 1,
                })
                .to_vec(),
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let signature = base64::engine::general_purpose::STANDARD
            .encode(key.sign(&bytes).as_ref())
            .into_bytes();
        let hash = Sha256::digest(&bytes);
        let prefix = format!("{target}-");
        let source = ArtifactSource::Remote(
            "https://github.com/vesta-project/server/releases/download/v1.0".into(),
            prefix.clone(),
        );
        let release = verified_release(
            source.clone(),
            &prefix,
            bytes.clone(),
            signature.clone(),
            &hash,
            key.public_key().as_ref(),
            Some("v1.0"),
        )
        .unwrap();
        assert_eq!(release.manifest.version, "v1.0");
        assert!(
            verified_release(
                source.clone(),
                &prefix,
                bytes.clone(),
                signature.clone(),
                &hash,
                key.public_key().as_ref(),
                Some("v2.0")
            )
            .is_err()
        );
        assert!(
            verified_release(
                source.clone(),
                &prefix,
                bytes.clone(),
                signature.clone(),
                &hash,
                &[0; 32],
                Some("v1.0")
            )
            .is_err()
        );
        let mut changed = bytes.clone();
        changed.push(b' ');
        assert!(
            verified_release(
                source,
                &prefix,
                changed.clone(),
                signature,
                &Sha256::digest(&changed),
                key.public_key().as_ref(),
                Some("v1.0")
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "bounded public GitHub metadata smoke; no artifact installation"]
    fn public_catalog_and_manifest_smoke() {
        assert_eq!(
            std::env::var("VESTA_PUBLIC_RELEASE_SMOKE").as_deref(),
            Ok("1")
        );
        let entries = catalog().unwrap();
        let tag = latest(&entries, true).unwrap();
        let pin = include_str!("../../../../scripts/bootstrap/release-public-key.txt").trim();
        let release = load(tag, pin).unwrap();
        assert_eq!(release.manifest.version, tag);
        assert_eq!(release.manifest.target, crate::target().unwrap());
        assert!(matches!(release.manifest.files.len(), 3 | 4));
        eprintln!(
            "public catalog/manifest verified: {tag}, {} components; no artifacts installed",
            release.manifest.files.len()
        );
    }
    #[test]
    fn catalog_ignores_installer_drafts_unpublished_and_wrong_channel_candidates() {
        let bytes = br#"[{"tag_name":"installer-20261112-abc","draft":false,"prerelease":true,"published_at":"2026-11-12T00:00:00Z"},{"tag_name":"v1.0","draft":false,"prerelease":false,"published_at":"2026-11-12T00:00:00Z"},{"tag_name":"nightly-20261112-abc","draft":false,"prerelease":true,"published_at":"2026-11-12T00:00:00Z"},{"tag_name":"v2.0","draft":true,"prerelease":false,"published_at":null}]"#;
        let (records, count) = parse_page(bytes).unwrap();
        assert_eq!(count, 4);
        assert_eq!(records.len(), 2);
        assert_eq!(latest(&records, false).unwrap(), "v1.0");
        assert_eq!(latest(&records, true).unwrap(), "nightly-20261112-abc");
        assert!(latest(&[], false).is_err());
        for tag in [
            "../main",
            "x?token=bad",
            "installer-abc",
            "",
            "/root",
            "x#fragment",
        ] {
            assert!(!valid_tag(tag));
        }
    }
}
