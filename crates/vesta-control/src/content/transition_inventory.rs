//! Explicit live metadata-only inventory for the bounded real-transition batch.
//! No artifact downloads, owner state, external executables or caller URLs.
use super::*;
use serde_json::{Value, json};

fn public_version(value: &Value, provider: Provider) -> Value {
    match provider {
        Provider::Modrinth => json!({
            "id":value["id"], "project_id":value["project_id"],
            "version_number":value["version_number"], "date":value["date_published"],
            "game_versions":value["game_versions"], "loaders":value["loaders"],
            "files":value["files"].as_array().map(|files| files.iter().map(|f|json!({
                "filename":f["filename"], "size":f["size"], "primary":f["primary"], "hashes":f["hashes"]
            })).collect::<Vec<_>>())
        }),
        Provider::Curseforge => json!({
            "id":value["id"], "project_id":value["modId"],
            "display":value["displayName"], "filename":value["fileName"],
            "date":value["fileDate"], "game_versions":value["gameVersions"],
            "is_server_pack":value["isServerPack"], "server_pack_file_id":value["serverPackFileId"],
            "size":value["fileLength"], "hashes":value["hashes"]
        }),
    }
}

#[test]
#[ignore = "explicit bounded public provider metadata; no artifact installation"]
fn published_transition_versions() {
    let out = std::path::PathBuf::from(
        std::env::var_os("VESTA_TRANSITION_OUTPUT").expect("explicit owned output"),
    );
    assert!(out.is_dir());
    let client = UreqProviderClient::configured()
        .with_deadline(Instant::now() + Duration::from_secs(90))
        .unwrap();
    let provider = std::env::var("VESTA_TRANSITION_PROVIDER").unwrap();
    let mut observations = Vec::new();
    let started = Instant::now();
    if provider == "modrinth" {
        for id in ["T9GJwPoJ", "4SKGla61"] {
            let v = client
                .pack_document(Provider::Modrinth, "Jkb29YJU", Some(id), false)
                .unwrap();
            assert_eq!(v["project_id"], "Jkb29YJU");
            assert_eq!(v["id"], id);
            observations.push(public_version(&v, Provider::Modrinth));
        }
    } else {
        assert_eq!(provider, "curseforge");
        let v = client
            .pack_document(Provider::Curseforge, "285109", None, true)
            .unwrap();
        let versions = v.as_array().unwrap();
        assert!(versions.len() <= 50);
        for version in versions {
            assert_eq!(version["modId"], 285109);
            observations.push(public_version(version, Provider::Curseforge));
        }
    }
    let result = json!({"provider":provider,"elapsed_ms":started.elapsed().as_secs_f64()*1000.,"metadata_only":true,"versions":observations});
    std::fs::write(
        out.join(format!("{provider}-versions.json")),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!(
        "TRANSITION_METADATA provider={} versions={} elapsed_ms={:.3}",
        provider,
        result["versions"].as_array().unwrap().len(),
        started.elapsed().as_secs_f64() * 1000.
    );
}
