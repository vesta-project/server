//! Pure retained-byte fixtures. No filesystem, network, provider or game execution.
use super::*;
use crate::config::HostConfiguration;
use std::{collections::BTreeMap, path::Path};
use vesta_protocol::pack::catalogue::{
    ArchiveFormat, ArchivePack, Manifest, ManifestMetadata, PackFile, PlatformPack, RelativePath,
};

const ABC: &[u8] = b"abc";
const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const EMPTY_SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
const MODRINTH_URL: &str = "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar";
const CURSEFORGE_URL: &str = "https://edge.forgecdn.net/files/1234/567/owned.jar";

// Hand-authored host schema. Parsing is pure; these lexical paths are never opened.
const HOST: &str = r#"schema-version = 1
[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game']
launch-definition = 'private/launch.toml'
[pack]
name = 'Golden Build'
version = 'v1'
revision = 7
minimum-revision = 3
minecraft-version = '1.21.1'
description = 'private-description-not-projected'
[pack.loader]
kind = 'fabric'
version = '0.16.10'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'disabled'
"#;
fn host(text: &str) -> PackMetadata {
    let base = if cfg!(windows) {
        Path::new("C:/owned-vesta-build-fixture")
    } else {
        Path::new("/owned-vesta-build-fixture")
    };
    HostConfiguration::parse_toml(text.as_bytes(), base, Path::new("host.toml"))
        .unwrap()
        .pack()
        .clone()
}
fn metadata() -> PackMetadata {
    host(HOST)
}
fn catalogue_metadata() -> ManifestMetadata {
    ManifestMetadata::new("Golden Build", "v1", 7, 3).unwrap()
}
fn descriptor(path: &str, body: &[u8], source: FileSource) -> PackFile {
    PackFile::new(
        path,
        &hex::encode(DigestSha256::digest(body)),
        body.len() as u64,
        source,
    )
    .unwrap()
}
fn snapshot(entries: Vec<(PackFile, Vec<u8>)>) -> PreparedSnapshot {
    let mut files = Vec::new();
    let mut bodies = BTreeMap::new();
    let mut total = 0;
    for (file, bytes) in entries {
        total += bytes.len();
        assert!(
            bodies
                .insert(file.path().clone(), bytes.into_boxed_slice())
                .is_none()
        );
        files.push(file);
    }
    PreparedSnapshot {
        missing_directories: Vec::new(),
        private_files: Vec::new(),
        manifest: Manifest::new(catalogue_metadata(), PackContent::Custom(files)).unwrap(),
        bodies,
        total,
    }
}
#[test]
fn trusted_join_descriptor_is_in_the_player_index_but_not_file_entries() {
    let text = format!(
        "{HOST}\n[pack.join]\naddress = 'play.example.org'\nport = 25565\ndescription = 'Player server'\n"
    );
    let pack = build_vesta(
        &embedded_snapshot(),
        &host(&text),
        VestaBuildLimits::default(),
    )
    .unwrap();
    let index: serde_json::Value = serde_json::from_slice(index_bytes(&pack)).unwrap();
    assert_eq!(
        index["join"],
        serde_json::json!({"address":"play.example.org", "port":25565, "minecraft_version":"1.21.1", "description":"Player server"})
    );
    let typed: vesta_protocol::pack::model::VestaIndexV1 =
        serde_json::from_slice(index_bytes(&pack)).unwrap();
    assert_eq!(typed.join.as_ref().unwrap().address(), "play.example.org");
    assert_eq!(typed.files.len(), 1);
    let ordinary = build_vesta(
        &embedded_snapshot(),
        &metadata(),
        VestaBuildLimits::default(),
    )
    .unwrap();
    let index: serde_json::Value = serde_json::from_slice(index_bytes(&ordinary)).unwrap();
    assert!(index.get("join").is_none());
}

fn embedded_snapshot() -> PreparedSnapshot {
    snapshot(vec![(
        descriptor("config/owned.txt", ABC, FileSource::embedded()),
        ABC.to_vec(),
    )])
}
fn reference_snapshot(
    platform: Platform,
    project: &str,
    version: &str,
    url: Option<&str>,
    body: &[u8],
) -> PreparedSnapshot {
    snapshot(vec![(
        descriptor(
            "mods/provider.jar",
            body,
            FileSource::platform(platform, project, version, url).unwrap(),
        ),
        body.to_vec(),
    )])
}
fn assert_error<T>(result: Result<T, VestaBuildError>, expected: VestaBuildError) {
    let error = result
        .err()
        .expect("the build must refuse without an artifact");
    assert_eq!(error, expected);
    assert_eq!(error.to_string(), "Vesta pack build failed");
    assert!(error.source().is_none());
}

// Independent stored-ZIP inspector, deliberately not zip::ZipArchive. Checks
// local and central headers, footer, CRCs, ordering, timestamp, permissions and
// exact extents so an archive-writer/reader shared mistake cannot mask a failure.
struct Member<'a> {
    name: &'a str,
    body: &'a [u8],
    offset: usize,
    crc: u32,
}
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn crc32(body: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in body {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}
fn inspect(bytes: &[u8]) -> Vec<Member<'_>> {
    assert!(bytes.len() >= 22);
    let footer = bytes.len() - 22;
    assert_eq!(u32_at(bytes, footer), 0x0605_4b50);
    assert_eq!(u16_at(bytes, footer + 4), 0); // Single disk.
    assert_eq!(u16_at(bytes, footer + 6), 0);
    let count = usize::from(u16_at(bytes, footer + 8));
    assert_eq!(usize::from(u16_at(bytes, footer + 10)), count);
    assert_eq!(u16_at(bytes, footer + 20), 0); // No archive comment.
    let central = u32_at(bytes, footer + 16) as usize;
    assert_eq!(central + u32_at(bytes, footer + 12) as usize, footer);
    let mut members = Vec::new();
    let mut offset = 0;
    while offset < central {
        assert_eq!(u32_at(bytes, offset), 0x0403_4b50);
        assert_eq!(u16_at(bytes, offset + 4), 10); // Stored, non-ZIP64.
        let flags = u16_at(bytes, offset + 6);
        assert_eq!(flags & !0x800, 0); // Only the UTF-8 filename flag is permitted.
        assert_eq!(u16_at(bytes, offset + 8), 0); // Stored compression.
        assert_eq!(u16_at(bytes, offset + 10), 0); // 00:00:00.
        assert_eq!(u16_at(bytes, offset + 12), 0x21); // 1980-01-01.
        let crc = u32_at(bytes, offset + 14);
        let size = u32_at(bytes, offset + 18) as usize;
        assert_eq!(u32_at(bytes, offset + 22) as usize, size);
        let name_size = usize::from(u16_at(bytes, offset + 26));
        assert_eq!(u16_at(bytes, offset + 28), 0); // No local extra fields.
        let body_start = offset + 30 + name_size;
        let name = std::str::from_utf8(&bytes[offset + 30..body_start]).unwrap();
        assert_eq!(flags, if name.is_ascii() { 0 } else { 0x800 });
        assert!(!name.is_empty() && !name.ends_with('/'));
        let body = &bytes[body_start..body_start + size];
        assert_eq!(crc, crc32(body));
        members.push(Member {
            name,
            body,
            offset,
            crc,
        });
        offset = body_start + size;
    }
    assert_eq!(offset, central);
    assert_eq!(members.len(), count);
    for member in &members {
        assert_eq!(u32_at(bytes, offset), 0x0201_4b50);
        assert_eq!(u16_at(bytes, offset + 4) >> 8, 3); // Fixed Unix creator.
        assert_eq!(u16_at(bytes, offset + 6), 10);
        assert_eq!(
            u16_at(bytes, offset + 8),
            if member.name.is_ascii() { 0 } else { 0x800 }
        );
        assert_eq!(u16_at(bytes, offset + 10), 0);
        assert_eq!(u16_at(bytes, offset + 12), 0);
        assert_eq!(u16_at(bytes, offset + 14), 0x21);
        assert_eq!(u32_at(bytes, offset + 16), member.crc);
        assert_eq!(u32_at(bytes, offset + 20) as usize, member.body.len());
        assert_eq!(u32_at(bytes, offset + 24) as usize, member.body.len());
        let name_size = usize::from(u16_at(bytes, offset + 28));
        assert_eq!(u16_at(bytes, offset + 30), 0); // No central extra fields.
        assert_eq!(u16_at(bytes, offset + 32), 0); // No member comment.
        assert_eq!(u16_at(bytes, offset + 34), 0);
        assert_eq!(u16_at(bytes, offset + 36), 0);
        assert_eq!(u32_at(bytes, offset + 38), 0o100644 << 16);
        assert_eq!(u32_at(bytes, offset + 42) as usize, member.offset);
        assert_eq!(
            &bytes[offset + 46..offset + 46 + name_size],
            member.name.as_bytes()
        );
        offset += 46 + name_size;
    }
    assert_eq!(offset, footer); // No ZIP64 footer, trailing data or hidden entries.
    members
}
fn index_bytes(pack: &BuiltVestaPack) -> &[u8] {
    let members = inspect(pack.bytes());
    assert_eq!(members.last().unwrap().name, "vesta.index.json");
    members.last().unwrap().body
}

// This is hand-authored compact JSON, not project_index/serde serialization.
const MIXED_INDEX: &str = concat!(
    "{\"formatVersion\":1,\"name\":\"Golden Build\",\"version\":\"v1\",\"revision\":7,",
    "\"minimumRevision\":3,\"minecraftVersion\":\"1.21.1\",\"modloaderType\":\"fabric\",",
    "\"modloaderVersion\":\"0.16.10\",\"dependencies\":{\"minecraft\":\"1.21.1\",",
    "\"fabric-loader\":\"0.16.10\"},\"files\":[",
    "{\"path\":\"config/server.txt\",\"size\":3,\"hashes\":{\"sha256\":",
    "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"},",
    "\"env\":{\"client\":\"required\",\"server\":\"required\"},\"source\":{\"type\":\"embedded\"}},",
    "{\"path\":\"mods/curseforge.jar\",\"size\":3,\"hashes\":{\"sha256\":",
    "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\",",
    "\"sha1\":\"a9993e364706816aba3e25717850c26c9cd0d89d\"},",
    "\"env\":{\"client\":\"required\",\"server\":\"required\"},\"source\":{\"type\":\"curseforge\",",
    "\"platform\":\"curseforge\",\"project_id\":\"123456\",\"version_id\":\"1234567\"}},",
    "{\"path\":\"mods/embed.jar\",\"size\":0,\"hashes\":{\"sha256\":",
    "\"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\",",
    "\"sha1\":\"da39a3ee5e6b4b0d3255bfef95601890afd80709\"},",
    "\"env\":{\"client\":\"required\",\"server\":\"required\"},\"source\":{\"type\":\"embedded\"}},",
    "{\"path\":\"mods/modrinth.jar\",\"size\":3,\"hashes\":{\"sha256\":",
    "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\",",
    "\"sha1\":\"a9993e364706816aba3e25717850c26c9cd0d89d\"},",
    "\"env\":{\"client\":\"required\",\"server\":\"required\"},\"source\":{\"type\":\"modrinth\",",
    "\"platform\":\"modrinth\",\"project_id\":\"Abcd1234\",\"version_id\":\"Efgh5678\",",
    "\"url\":\"https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar\"}}]}"
);

#[test]
fn independent_mixed_index_and_zip_structure_are_exact_and_repeatable() {
    let prepared = snapshot(vec![
        (
            descriptor(
                "mods/modrinth.jar",
                ABC,
                FileSource::platform(
                    Platform::Modrinth,
                    "Abcd1234",
                    "Efgh5678",
                    Some(MODRINTH_URL),
                )
                .unwrap(),
            ),
            ABC.to_vec(),
        ),
        (
            PackFile::new("mods/embed.jar", EMPTY_SHA256, 0, FileSource::embedded())
                .unwrap()
                .with_sha1(EMPTY_SHA1)
                .unwrap(),
            Vec::new(),
        ),
        (
            descriptor(
                "mods/curseforge.jar",
                ABC,
                FileSource::platform(Platform::Curseforge, "123456", "1234567", None).unwrap(),
            ),
            ABC.to_vec(),
        ),
        (
            descriptor(
                "config/server.txt",
                ABC,
                FileSource::server(Some("https://owned.invalid/server-description")).unwrap(),
            ),
            ABC.to_vec(),
        ),
    ]);
    let pack = build_vesta(&prepared, &metadata(), VestaBuildLimits::default()).unwrap();
    assert_eq!(index_bytes(&pack), MIXED_INDEX.as_bytes());
    let members = inspect(pack.bytes());
    assert_eq!(
        members.iter().map(|member| member.name).collect::<Vec<_>>(),
        [
            "overrides/config/server.txt",
            "overrides/mods/embed.jar",
            "vesta.index.json"
        ]
    );
    assert_eq!(members[0].body, ABC);
    assert_eq!(members[0].crc, 0x3524_41c2); // Independent known CRC vector.
    assert_eq!(members[1].body, b"");
    assert_eq!(members[1].crc, 0);
    assert_eq!(pack.format(), "vesta");
    assert_eq!(pack.size(), pack.bytes().len() as u64);
    assert_eq!(
        pack.sha256().as_bytes().as_slice(),
        &DigestSha256::digest(pack.bytes())[..]
    );
    let repeated = build_vesta(&prepared, &metadata(), VestaBuildLimits::default()).unwrap();
    assert_eq!(pack.bytes(), repeated.bytes());
    assert_eq!(pack.sha256(), repeated.sha256());
}

#[test]
fn nonportable_names_cannot_become_descriptors_for_owned_byte_building() {
    // Catalogue and filesystem authorization remain deliberately ASCII-only.
    // Do not widen their admission to make a ZIP filename fixture pass.
    for path in [
        "config/café.txt",
        "config/config file.txt",
        "config/CON.txt",
        "../escaped.txt",
        "/root.txt",
        "C:/native.txt",
    ] {
        assert_eq!(
            PackFile::new(path, ABC_SHA256, 3, FileSource::embedded()).err(),
            Some(vesta_protocol::pack::catalogue::CatalogueError::InvalidPath)
        );
    }
}

#[test]
fn checked_zip_minimum_overhead_and_real_finalization_failure_are_bounded() {
    assert_eq!(minimum_stored_member_bytes(9, 5).unwrap(), 99);
    for (name, body) in [(usize::MAX, 0), (0, usize::MAX), (usize::MAX / 2, 0)] {
        assert_error(
            minimum_stored_member_bytes(name, body),
            VestaBuildError::TooLarge,
        );
    }
    // Deliberately bypass the public builder's preflight to exercise a real
    // writer's footer failure against its actual bounded sink. No artifact escapes.
    let mut sink = BoundedBuffer::new(120); // 22 + 99 minus one byte.
    let written = (|| -> zip::result::ZipResult<()> {
        let mut writer = ZipWriter::new(&mut sink);
        writer.start_file(
            "owned.txt",
            SimpleFileOptions::DEFAULT
                .compression_method(CompressionMethod::Stored)
                .last_modified_time(DateTime::DEFAULT)
                .unix_permissions(0o644)
                .system(System::Unix),
        )?;
        writer.write_all(b"owned")?;
        writer.finish().map(|_| ())
    })();
    assert!(written.is_err());
    assert!(sink.bytes.len() <= 120);
    assert_error(sink.finish(), VestaBuildError::TooLarge);
}

#[test]
fn empty_pack_has_only_an_independently_authored_index_without_loader_fields() {
    let host = host(&HOST.replace("[pack.loader]\nkind = 'fabric'\nversion = '0.16.10'\n", ""));
    let pack = build_vesta(&snapshot(Vec::new()), &host, VestaBuildLimits::default()).unwrap();
    assert_eq!(inspect(pack.bytes()).len(), 1);
    assert_eq!(
        index_bytes(&pack),
        br#"{"formatVersion":1,"name":"Golden Build","version":"v1","revision":7,"minimumRevision":3,"minecraftVersion":"1.21.1","dependencies":{"minecraft":"1.21.1"},"files":[]}"#
    );
}

#[test]
fn minecraft_and_each_loader_dependency_come_only_from_trusted_host_metadata() {
    for (kind, dependency) in [
        ("fabric", "fabric-loader"),
        ("quilt", "quilt-loader"),
        ("forge", "forge"),
        ("neoforge", "neoforge"),
    ] {
        let host = host(
            &HOST
                .replace("kind = 'fabric'", &format!("kind = '{kind}'"))
                .replace(
                    "minecraft-version = '1.21.1'",
                    "minecraft-version = '1.20.4'",
                ),
        );
        let pack = build_vesta(&embedded_snapshot(), &host, VestaBuildLimits::default()).unwrap();
        let index: serde_json::Value = serde_json::from_slice(index_bytes(&pack)).unwrap();
        assert_eq!(index["minecraftVersion"], "1.20.4");
        assert_eq!(index["dependencies"]["minecraft"], "1.20.4");
        assert_eq!(index["modloaderType"], kind);
        assert_eq!(index["modloaderVersion"], "0.16.10");
        assert_eq!(index["dependencies"][dependency], "0.16.10");
        assert_eq!(index["dependencies"].as_object().unwrap().len(), 2);
        assert!(
            !std::str::from_utf8(index_bytes(&pack))
                .unwrap()
                .contains("private-description")
        );
    }
}

#[test]
fn all_four_snapshot_metadata_fields_must_match_exactly() {
    for observed in [
        ManifestMetadata::new("golden Build", "v1", 7, 3).unwrap(),
        ManifestMetadata::new("Golden Build", "V1", 7, 3).unwrap(),
        ManifestMetadata::new("Golden Build", "v1", 8, 3).unwrap(),
        ManifestMetadata::new("Golden Build", "v1", 7, 2).unwrap(),
    ] {
        let prepared = PreparedSnapshot {
            missing_directories: Vec::new(),
            private_files: Vec::new(),
            manifest: Manifest::new(observed, PackContent::Custom(Vec::new())).unwrap(),
            bodies: BTreeMap::new(),
            total: 0,
        };
        assert_error(
            build_vesta(&prepared, &metadata(), VestaBuildLimits::default()),
            VestaBuildError::MetadataMismatch,
        );
    }
}

#[test]
fn whole_platform_and_archive_descriptions_are_not_prepared_custom_snapshots() {
    for content in [
        PackContent::Platform(PlatformPack::new(Platform::Curseforge, "1", "2", None).unwrap()),
        PackContent::Archive(
            ArchivePack::new("owned.zip", EMPTY_SHA256, 0, ArchiveFormat::Zip, None).unwrap(),
        ),
    ] {
        let prepared = PreparedSnapshot {
            missing_directories: Vec::new(),
            private_files: Vec::new(),
            manifest: Manifest::new(catalogue_metadata(), content).unwrap(),
            bodies: BTreeMap::new(),
            total: 0,
        };
        assert_error(
            build_vesta(&prepared, &metadata(), VestaBuildLimits::default()),
            VestaBuildError::InvalidSnapshot,
        );
    }
}

#[test]
fn exact_body_membership_accounting_size_and_sha256_are_required() {
    let mut missing = embedded_snapshot();
    missing.bodies.clear();
    let mut extra = embedded_snapshot();
    extra.bodies.insert(
        RelativePath::parse("config/unlisted.txt").unwrap(),
        b"unlisted".to_vec().into_boxed_slice(),
    );
    let mut substituted = embedded_snapshot();
    let bytes = substituted.bodies.pop_first().unwrap().1;
    substituted
        .bodies
        .insert(RelativePath::parse("config/unlisted.txt").unwrap(), bytes);
    let mut wrong_total = embedded_snapshot();
    wrong_total.total += 1;
    let mut corrupted = embedded_snapshot();
    corrupted
        .bodies
        .get_mut(&RelativePath::parse("config/owned.txt").unwrap())
        .unwrap()[2] = b'd';
    let wrong_size = snapshot(vec![(
        PackFile::new("config/owned.txt", ABC_SHA256, 4, FileSource::embedded()).unwrap(),
        ABC.to_vec(),
    )]);
    let wrong_hash = snapshot(vec![(
        PackFile::new("config/owned.txt", EMPTY_SHA256, 3, FileSource::embedded()).unwrap(),
        ABC.to_vec(),
    )]);
    for prepared in [
        missing,
        extra,
        substituted,
        wrong_total,
        corrupted,
        wrong_size,
        wrong_hash,
    ] {
        assert_error(
            build_vesta(&prepared, &metadata(), VestaBuildLimits::default()),
            VestaBuildError::InvalidSnapshot,
        );
    }
}

#[test]
fn optional_sha1_is_verified_and_references_receive_computed_retained_byte_sha1() {
    let correct = snapshot(vec![(
        PackFile::new("mods/owned.jar", ABC_SHA256, 3, FileSource::embedded())
            .unwrap()
            .with_sha1(ABC_SHA1)
            .unwrap(),
        ABC.to_vec(),
    )]);
    let built = build_vesta(&correct, &metadata(), VestaBuildLimits::default()).unwrap();
    let index: serde_json::Value = serde_json::from_slice(index_bytes(&built)).unwrap();
    assert_eq!(index["files"][0]["hashes"]["sha1"], ABC_SHA1);
    let wrong = snapshot(vec![(
        PackFile::new("mods/owned.jar", ABC_SHA256, 3, FileSource::embedded())
            .unwrap()
            .with_sha1(&"00".repeat(20))
            .unwrap(),
        ABC.to_vec(),
    )]);
    assert_error(
        build_vesta(&wrong, &metadata(), VestaBuildLimits::default()),
        VestaBuildError::InvalidSnapshot,
    );
    let provider = reference_snapshot(Platform::Curseforge, "1", "2", None, ABC);
    let built = build_vesta(&provider, &metadata(), VestaBuildLimits::default()).unwrap();
    assert_eq!(inspect(built.bytes()).len(), 1);
    let index: serde_json::Value = serde_json::from_slice(index_bytes(&built)).unwrap();
    assert_eq!(index["files"][0]["hashes"]["sha1"], ABC_SHA1);
    assert_eq!(index["files"][0]["hashes"]["sha256"], ABC_SHA256);
    assert!(index["files"][0]["source"].get("url").is_none());
    let mut corrupted = provider;
    corrupted.bodies.values_mut().next().unwrap()[0] = b'z';
    assert_error(
        build_vesta(&corrupted, &metadata(), VestaBuildLimits::default()),
        VestaBuildError::InvalidSnapshot,
    );
}

#[test]
fn provider_ids_and_positive_sizes_are_admitted_not_merely_projected() {
    for (platform, project, version, url) in [
        (Platform::Modrinth, "short", "Efgh5678", Some(MODRINTH_URL)),
        (
            Platform::Modrinth,
            "Abcd1234",
            "bad_slug",
            Some(MODRINTH_URL),
        ),
        (
            Platform::Modrinth,
            "Abcd1234",
            "Efgh56789",
            Some(MODRINTH_URL),
        ),
        (Platform::Modrinth, "Abcd1234", "Efgh5678", None),
        (Platform::Curseforge, "0", "1234567", None),
        (Platform::Curseforge, "01", "1234567", None),
        (Platform::Curseforge, "4294967296", "1234567", None),
        (Platform::Curseforge, "123456", "0", None),
        (Platform::Curseforge, "123456", "01234567", None),
        (Platform::Curseforge, "123456", "not_a_number", None),
    ] {
        assert_error(
            build_vesta(
                &reference_snapshot(platform, project, version, url, ABC),
                &metadata(),
                VestaBuildLimits::default(),
            ),
            VestaBuildError::InvalidReference,
        );
    }
    for (platform, project, version, url) in [
        (
            Platform::Modrinth,
            "Abcd1234",
            "Efgh5678",
            Some(MODRINTH_URL),
        ),
        (Platform::Curseforge, "1", "2", None),
    ] {
        assert_error(
            build_vesta(
                &reference_snapshot(platform, project, version, url, b""),
                &metadata(),
                VestaBuildLimits::default(),
            ),
            VestaBuildError::InvalidReference,
        );
    }
}

#[test]
fn exact_provider_url_families_and_default_https_port_are_supported() {
    for (platform, project, version, raw) in [
        (Platform::Modrinth, "Abcd1234", "Efgh5678", MODRINTH_URL),
        (
            Platform::Modrinth,
            "Abcd1234",
            "Efgh5678",
            "https://cdn.modrinth.com:443/data/Abcd1234/versions/Efgh5678/owned.jar",
        ),
        (Platform::Curseforge, "123456", "1234567", CURSEFORGE_URL),
        (
            Platform::Curseforge,
            "123456",
            "1234567",
            "https://mediafilez.forgecdn.net/files/1234/567/owned.jar",
        ),
        (
            Platform::Curseforge,
            "4294967295",
            "4294967295",
            "https://edge.forgecdn.net/files/4294967/295/owned.jar",
        ),
        (
            Platform::Curseforge,
            "1",
            "1",
            "https://edge.forgecdn.net/files/0/1/owned.jar",
        ),
    ] {
        let built = build_vesta(
            &reference_snapshot(platform, project, version, Some(raw), ABC),
            &metadata(),
            VestaBuildLimits::new(MAX_VESTA_INDEX_BYTES, MAX_VESTA_ARCHIVE_BYTES, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(inspect(built.bytes()).len(), 1); // Never embed a provider body.
        let index: serde_json::Value = serde_json::from_slice(index_bytes(&built)).unwrap();
        assert_eq!(index["files"][0]["source"]["url"], raw); // No rewriting.
    }
}

#[test]
fn dangerous_raw_url_spelling_and_normalization_are_refused_before_projection() {
    let invalid = [
        "http://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://private-secret.invalid/data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://cdn.modrinth.com.evil.invalid/data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://user@cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://@cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://cdn.modrinth.com:444/data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar?",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar#",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/nested/owned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/./owned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/wrong/../owned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Wrong123/owned.jar",
        "https://cdn.modrinth.com//data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/%6fwned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/%2e%2e/owned.jar",
        "https://cdn.modrinth.com\\data\\Abcd1234\\versions\\Efgh5678\\owned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/ownéd.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned file.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned\t.jar",
        " https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar",
        "https://cdn.modrinth.com/data/Abcd1234/versions/Efgh5678/owned.jar\n",
        "not-a-url",
    ];
    for raw in invalid {
        assert_error(
            validate_reference_url(raw, Platform::Modrinth, "Abcd1234", "Efgh5678"),
            VestaBuildError::InvalidReference,
        );
        // Some spelling is already rejected by the immutable catalogue. Every
        // representable descriptive source must also fail this builder boundary.
        if let Ok(source) =
            FileSource::platform(Platform::Modrinth, "Abcd1234", "Efgh5678", Some(raw))
        {
            let prepared = snapshot(vec![(
                descriptor("mods/provider.jar", ABC, source),
                ABC.to_vec(),
            )]);
            assert_error(
                build_vesta(&prepared, &metadata(), VestaBuildLimits::default()),
                VestaBuildError::InvalidReference,
            );
        }
    }
    assert_error(
        validate_reference_url(
            &format!("https://cdn.modrinth.com/{}", "a".repeat(2_048)),
            Platform::Modrinth,
            "Abcd1234",
            "Efgh5678",
        ),
        VestaBuildError::InvalidReference,
    );
    for raw in [
        "https://edge.forgecdn.net/files/01234/567/owned.jar",
        "https://edge.forgecdn.net/files/1234/0567/owned.jar",
        "https://edge.forgecdn.net/files/1234/568/owned.jar",
        "https://edge.forgecdn.net/files/1234/567/nested/owned.jar",
        "https://edge.forgecdn.net/files/1234/567/",
        "https://edge.forgecdn.net/files/1234/567/wrong/../owned.jar",
        "https://media.forgecdn.net/files/1234/567/owned.jar",
    ] {
        assert_error(
            build_vesta(
                &reference_snapshot(Platform::Curseforge, "123456", "1234567", Some(raw), ABC),
                &metadata(),
                VestaBuildLimits::default(),
            ),
            VestaBuildError::InvalidReference,
        );
    }
}

#[test]
fn output_byte_limits_are_inclusive_and_finalization_cannot_return_partial_artifacts() {
    let prepared = embedded_snapshot();
    let baseline = build_vesta(&prepared, &metadata(), VestaBuildLimits::default()).unwrap();
    let index_size = index_bytes(&baseline).len();
    let archive_size = baseline.bytes().len();
    let exact = VestaBuildLimits::new(index_size, archive_size, 2).unwrap();
    let built = build_vesta(&prepared, &metadata(), exact).unwrap();
    assert_eq!(built.bytes(), baseline.bytes());
    for limits in [
        VestaBuildLimits::new(index_size - 1, archive_size, 2).unwrap(),
        VestaBuildLimits::new(index_size, archive_size - 1, 2).unwrap(),
        VestaBuildLimits::new(index_size, 1, 2).unwrap(),
        VestaBuildLimits::new(index_size, archive_size, 1).unwrap(),
    ] {
        assert_error(
            build_vesta(&prepared, &metadata(), limits),
            VestaBuildError::TooLarge,
        );
    }
    // Reference bodies still undergo integrity checks, but consume no ZIP entry.
    assert!(
        build_vesta(
            &reference_snapshot(Platform::Curseforge, "1", "2", None, ABC),
            &metadata(),
            VestaBuildLimits::new(MAX_VESTA_INDEX_BYTES, MAX_VESTA_ARCHIVE_BYTES, 1).unwrap(),
        )
        .is_ok()
    );
}

#[test]
fn zero_and_over_ceiling_limits_are_rejected_without_work() {
    for (index, archive, entries) in [
        (0, MAX_VESTA_ARCHIVE_BYTES, MAX_VESTA_ENTRIES),
        (MAX_VESTA_INDEX_BYTES, 0, MAX_VESTA_ENTRIES),
        (MAX_VESTA_INDEX_BYTES, MAX_VESTA_ARCHIVE_BYTES, 0),
        (
            MAX_VESTA_INDEX_BYTES + 1,
            MAX_VESTA_ARCHIVE_BYTES,
            MAX_VESTA_ENTRIES,
        ),
        (
            MAX_VESTA_INDEX_BYTES,
            MAX_VESTA_ARCHIVE_BYTES + 1,
            MAX_VESTA_ENTRIES,
        ),
        (
            MAX_VESTA_INDEX_BYTES,
            MAX_VESTA_ARCHIVE_BYTES,
            MAX_VESTA_ENTRIES + 1,
        ),
    ] {
        assert_error(
            VestaBuildLimits::new(index, archive, entries),
            VestaBuildError::InvalidLimit,
        );
    }
    assert_eq!(VestaBuildLimits::default(), VestaBuildLimits::production());
    assert_eq!(
        VestaBuildLimits::new(
            MAX_VESTA_INDEX_BYTES,
            MAX_VESTA_ARCHIVE_BYTES,
            MAX_VESTA_ENTRIES
        )
        .unwrap(),
        VestaBuildLimits::production()
    );
    assert!(VestaBuildLimits::new(1, 1, 1).is_ok());
}

#[test]
fn maximum_snapshot_file_count_and_archive_entry_count_are_inclusive() {
    let prepared = snapshot(
        (0..MAX_FILES)
            .rev()
            .map(|i| {
                (
                    PackFile::new(
                        &format!("mods/f{i:04}.jar"),
                        EMPTY_SHA256,
                        0,
                        FileSource::embedded(),
                    )
                    .unwrap(),
                    Vec::new(),
                )
            })
            .collect(),
    );
    let built = build_vesta(&prepared, &metadata(), VestaBuildLimits::default()).unwrap();
    let members = inspect(built.bytes());
    assert_eq!(members.len(), 4_097);
    assert_eq!(members.first().unwrap().name, "overrides/mods/f0000.jar");
    assert_eq!(members[MAX_FILES - 1].name, "overrides/mods/f4095.jar");
    assert_eq!(members.last().unwrap().name, "vesta.index.json");
    assert_error(
        build_vesta(
            &prepared,
            &metadata(),
            VestaBuildLimits::new(MAX_VESTA_INDEX_BYTES, MAX_VESTA_ARCHIVE_BYTES, MAX_FILES)
                .unwrap(),
        ),
        VestaBuildError::TooLarge,
    );
}

#[test]
fn retained_file_and_total_byte_ceilings_are_inclusive_even_for_unembedded_references() {
    let body = vec![b'x'; MAX_PINNED_FILE_BYTES];
    let file_hash = hex::encode(DigestSha256::digest(&body));
    let mut prepared = snapshot(
        (0..4)
            .map(|i| {
                (
                    PackFile::new(
                        &format!("mods/large{i}.jar"),
                        &file_hash,
                        body.len() as u64,
                        FileSource::platform(Platform::Curseforge, "1", "2", None).unwrap(),
                    )
                    .unwrap(),
                    body.clone(),
                )
            })
            .collect(),
    );
    drop(body);
    assert_eq!(prepared.total, MAX_PINNED_BYTES);
    assert!(build_vesta(&prepared, &metadata(), VestaBuildLimits::default()).is_ok());
    let PackContent::Custom(files) = prepared.manifest.content() else {
        panic!("the fixture is custom")
    };
    let mut files = files.clone();
    files.push(descriptor(
        "mods/over-total.jar",
        b"x",
        FileSource::platform(Platform::Curseforge, "1", "2", None).unwrap(),
    ));
    prepared.manifest = Manifest::new(catalogue_metadata(), PackContent::Custom(files)).unwrap();
    prepared.bodies.insert(
        RelativePath::parse("mods/over-total.jar").unwrap(),
        b"x".to_vec().into_boxed_slice(),
    );
    prepared.total += 1;
    assert_error(
        build_vesta(&prepared, &metadata(), VestaBuildLimits::default()),
        VestaBuildError::InvalidSnapshot,
    );
    drop(prepared);
    let body = vec![b'x'; MAX_PINNED_FILE_BYTES + 1];
    let oversized = snapshot(vec![(
        descriptor(
            "mods/oversized.jar",
            &body,
            FileSource::platform(Platform::Curseforge, "1", "2", None).unwrap(),
        ),
        body,
    )]);
    assert_error(
        build_vesta(&oversized, &metadata(), VestaBuildLimits::default()),
        VestaBuildError::InvalidSnapshot,
    );
}

#[test]
fn bounded_buffer_backpatches_without_holes_or_growth_and_latches_first_failure() {
    let mut buffer = BoundedBuffer::new(4);
    buffer.write_all(b"abcd").unwrap();
    assert_eq!(buffer.seek(SeekFrom::Start(1)).unwrap(), 1);
    buffer.write_all(b"XY").unwrap();
    assert_eq!(buffer.seek(SeekFrom::End(-1)).unwrap(), 3);
    assert_eq!(buffer.seek(SeekFrom::Current(-3)).unwrap(), 0);
    assert_eq!(buffer.finish().unwrap(), b"aXYd");
    for from in [SeekFrom::Start(5), SeekFrom::Start(u64::MAX)] {
        let mut buffer = BoundedBuffer::new(4);
        assert!(buffer.seek(from).is_err());
        assert_eq!(buffer.failure, Some(VestaBuildError::TooLarge));
        assert!(buffer.write_all(b"a").is_err());
        assert!(buffer.seek(SeekFrom::Start(0)).is_err());
        assert!(buffer.flush().is_err());
        assert_error(buffer.finish(), VestaBuildError::TooLarge);
    }
    for from in [
        SeekFrom::Start(1),
        SeekFrom::End(-1),
        SeekFrom::Current(i64::MIN),
    ] {
        let mut buffer = BoundedBuffer::new(4);
        assert!(buffer.seek(from).is_err());
        assert_error(buffer.finish(), VestaBuildError::Unavailable);
    }
    let mut buffer = BoundedBuffer::new(4);
    buffer.write_all(b"abcd").unwrap();
    assert!(buffer.write_all(b"e").is_err());
    assert_eq!(buffer.bytes, b"abcd"); // Failure performs no partial append.
    assert_error(buffer.finish(), VestaBuildError::TooLarge);
}

#[test]
fn build_errors_and_artifact_debug_never_render_input_text_or_digest() {
    let prepared = snapshot(vec![(
        descriptor(
            "config/private-secret.txt",
            b"private-body-secret",
            FileSource::embedded(),
        ),
        b"private-body-secret".to_vec(),
    )]);
    let built = build_vesta(&prepared, &metadata(), VestaBuildLimits::default()).unwrap();
    let rendered = format!("{built:?} {:?}", built.sha256());
    for secret in [
        "private-secret",
        "private-body-secret",
        "Golden Build",
        "vesta.index.json",
        built.sha256().to_hex().as_str(),
    ] {
        assert!(!rendered.contains(secret));
    }
    for error in [
        VestaBuildError::InvalidLimit,
        VestaBuildError::MetadataMismatch,
        VestaBuildError::InvalidSnapshot,
        VestaBuildError::InvalidReference,
        VestaBuildError::TooLarge,
        VestaBuildError::Unavailable,
    ] {
        let rendered = format!("{error:?} {error}");
        assert!(rendered.contains(error.code()));
        assert!(
            error
                .code()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_')
        );
        for secret in [
            "private-secret",
            "private-body-secret",
            MODRINTH_URL,
            ABC_SHA256,
        ] {
            assert!(!rendered.contains(secret));
        }
    }
}
