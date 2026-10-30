//! Download-only Fabric/Quilt bundles. Fixtures are inert bytes served by wiremock; nothing executes.
use super::super::{
    bundle,
    providers::loaders::{maven_path, sidecar_digest},
};
use super::*;
use std::collections::HashMap;

#[tokio::test]
async fn managed_fabric_and_quilt_use_absolute_classpaths_for_preserved_game_directory() {
    for software in [Software::Fabric, Software::Quilt] {
        let libs = if software == Software::Fabric {
            fabric_libs()
        } else {
            quilt_libs()
        };
        let f = loader_fixture(software, &libs).await;
        let p = plan_with(&f, software, &selection()).await.unwrap();
        install(&f, &p).await.unwrap();
        let game = tempfile::tempdir().unwrap();
        std::fs::write(game.path().join("server.properties"), b"motd=existing\n").unwrap();
        let launch = f.installer.managed_launch(&p, game.path()).await.unwrap();
        assert_eq!(
            launch.working_directory,
            dunce::canonicalize(game.path()).unwrap()
        );
        assert_eq!(launch.args.len(), 5);
        assert!(
            launch.args[0].ends_with(
                &p.destination()
                    .join("software/vanilla.jar")
                    .to_string_lossy()
                    .to_string()
            )
        );
        assert_eq!(launch.args[1], "-cp");
        assert_eq!(launch.args[3], bundle::server_main(software).unwrap());
        assert_eq!(launch.args[4], "nogui");
        let cp: Vec<_> = launch.args[2]
            .split(if cfg!(windows) { ';' } else { ':' })
            .collect();
        assert_eq!(cp.len(), libs.len());
        assert!(cp.iter().all(|s| std::path::Path::new(s).is_absolute()));
        assert_eq!(launch.required_files.len(), libs.len() + 1);
        assert_eq!(launch.artifacts.len(), libs.len() + 1);
        crate::verify_managed_artifacts(&launch.artifacts)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(game.path().join("server.properties")).unwrap(),
            b"motd=existing\n"
        );
        // The original fresh-install receipt/relative argfile remains verifiable.
        f.installer.verify(&p).await.unwrap();
        std::fs::write(&launch.required_files[1], b"tampered loader").unwrap();
        assert!(
            crate::verify_managed_artifacts(&launch.artifacts)
                .await
                .is_err()
        );
    }
}

type InstallPlanMutation = Box<dyn Fn(&mut InstallPlan)>;

const GAME: &str = "1.21.4";
const TOKEN: &str = "${modrinth.gameVersion}";
const MODRINTH_MAVEN: &str = "https://launcher-meta.modrinth.com/maven/";
const FABRIC_MAVEN: &str = "https://maven.fabricmc.net/";
const QUILT_MAVEN: &str = "https://maven.quiltmc.org/repository/release/";

#[derive(Clone)]
struct Lib {
    /// Spelling in the Modrinth profile, possibly containing the game-version token.
    modrinth: String,
    coord: String,
    quilt_repo: bool,
    body: Vec<u8>,
    /// Bytes actually served for the artifact download.
    served: Vec<u8>,
    official_sha: bool,
    official_size: bool,
}
fn lib(modrinth: &str, quilt_repo: bool, body: &[u8], sha: bool, size: bool) -> Lib {
    Lib {
        modrinth: modrinth.into(),
        coord: modrinth.replace(TOKEN, GAME),
        quilt_repo,
        body: body.to_vec(),
        served: body.to_vec(),
        official_sha: sha,
        official_size: size,
    }
}
/// Independent Maven layout, deliberately not the production helper.
fn mpath(coord: &str) -> String {
    let p: Vec<_> = coord.split(':').collect();
    format!(
        "{}/{}/{}/{}-{}.jar",
        p[0].replace('.', "/"),
        p[1],
        p[2],
        p[1],
        p[2]
    )
}
fn repo(l: &Lib) -> &'static str {
    if l.quilt_repo {
        QUILT_MAVEN
    } else {
        FABRIC_MAVEN
    }
}
fn route(l: &Lib) -> String {
    format!(
        "{}{}",
        if l.quilt_repo {
            "/repository/release/"
        } else {
            "/"
        },
        mpath(&l.coord)
    )
}
fn loader_of(s: Software) -> &'static str {
    if s == Software::Fabric {
        "0.16.10"
    } else {
        "0.28.1"
    }
}
fn fabric_libs() -> Vec<Lib> {
    vec![
        lib("org.ow2.asm:asm:9.7.1", false, b"asm fixture", true, true),
        lib(
            "net.fabricmc:sponge-mixin:0.15.4+mixin.0.8.7",
            false,
            b"mixin fixture bytes",
            true,
            false,
        ),
        lib(
            &format!("net.fabricmc:intermediary:{TOKEN}"),
            false,
            b"intermediary fixture",
            false,
            false,
        ),
        lib(
            "net.fabricmc:fabric-loader:0.16.10",
            false,
            b"fabric loader fixture",
            false,
            false,
        ),
    ]
}
fn quilt_libs() -> Vec<Lib> {
    vec![
        lib(
            "net.fabricmc:sponge-mixin:0.15.4+mixin.0.8.7",
            false,
            b"mixin fixture bytes",
            false,
            false,
        ),
        lib(
            &format!("org.quiltmc:hashed:{TOKEN}"),
            true,
            b"hashed fixture",
            false,
            false,
        ),
        lib(
            &format!("net.fabricmc:intermediary:{TOKEN}"),
            false,
            b"intermediary fixture",
            false,
            false,
        ),
        lib(
            "org.quiltmc:quilt-loader:0.28.1",
            true,
            b"quilt loader fixture",
            false,
            false,
        ),
    ]
}

struct Docs {
    manifest: Value,
    client: Value,
    server: Value,
    info: Option<Value>,
    sidecars: HashMap<String, Option<Vec<u8>>>,
    heads: HashMap<String, Option<Vec<u8>>>,
}

#[tokio::test]
async fn historical_fabric_labels_do_not_poison_selectable_versions() {
    let libs = fabric_libs();
    let f = Fixture::new(Software::Vanilla).await;
    let mut d = docs(Software::Fabric, &libs);
    d.manifest["gameVersions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"1.14 Pre-Release 1","loaders":[]}));
    mount_docs(&f, Software::Fabric, &libs, &d).await;
    let versions = f
        .installer
        .versions(Software::Fabric, &SilentProgressReporter)
        .await
        .unwrap();
    assert_eq!(versions, ["1.21.3", GAME]);
}

#[tokio::test]
async fn quilt_uses_selected_game_group_and_numeric_releases() {
    let libs = quilt_libs();
    let f = Fixture::new(Software::Vanilla).await;
    let mut d = docs(Software::Quilt, &libs);
    for loader in d.manifest["versionGroups"][0]["loaders"]
        .as_array_mut()
        .unwrap()
    {
        loader["stable"] = json!(false);
    }
    d.manifest["versionGroups"].as_array_mut().unwrap().push(json!({"id":"v1","loaders":[{"id":"0.29.0","stable":false,"url":"https://launcher-meta.modrinth.com/quilt/v1/version-group/v1/loader-version/0.29.0"}]}));
    d.manifest["gameVersions"][0]["versionGroup"] = json!("v1");
    mount_docs(&f, Software::Quilt, &libs, &d).await;
    let old = f
        .installer
        .releases(Software::Quilt, "1.21.3", &SilentProgressReporter)
        .await
        .unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].id, "0.29.0");
    let current = f
        .installer
        .releases(Software::Quilt, GAME, &SilentProgressReporter)
        .await
        .unwrap();
    assert!(current.iter().any(|v| v.id == loader_of(Software::Quilt)));
    assert!(current.iter().all(|v| !v.id.contains("beta")));
}
fn docs(s: Software, libs: &[Lib]) -> Docs {
    let loader = loader_of(s);
    let profile = |id: &str| {
        if s == Software::Quilt {
            return format!(
                "https://launcher-meta.modrinth.com/quilt/v1/version-group/v2/loader-version/{id}"
            );
        }
        format!(
            "https://launcher-meta.modrinth.com/{}/v0/versions/{}.json",
            s.id(),
            id
        )
    };
    let mut manifest = json!({"gameVersions":[
        {"id":"1.21.3","loaders":[]},
        {"id":GAME,"loaders":[]},
        {"id":TOKEN,"loaders":[
            {"id":"0.99.0-beta","url":profile("0.99.0-beta"),"stable":false},
            {"id":loader,"url":profile(loader),"stable":true},
            {"id":"0.1.0","url":profile("0.1.0"),"stable":true}
        ]}
    ]});
    if s == Software::Quilt {
        let choices = manifest["gameVersions"][2]["loaders"].take();
        manifest["gameVersions"].as_array_mut().unwrap().pop();
        for game in manifest["gameVersions"].as_array_mut().unwrap() {
            game["versionGroup"] = json!("v2");
        }
        manifest["versionGroups"] = json!([{ "id": "v2", "loaders": choices }]);
    }
    let client = json!({
        "mainClass": bundle::client_main(s).unwrap(),
        "arguments": {"game": ["--modrinth-supplied"]},
        "libraries": libs.iter().map(|l| json!({
            "name": l.modrinth, "url": MODRINTH_MAVEN,
            "include_in_classpath": true, "downloadable": true
        })).collect::<Vec<_>>()
    });
    // Official order deliberately differs: membership is checked, Modrinth order is used.
    let server = json!({
        "mainClass": bundle::server_main(s).unwrap(),
        "launcherMainClass": "metadata.must.not.Launch",
        "arguments": {"game": ["--metadata-arg"], "jvm": ["-javaagent:evil.jar"]},
        "libraries": libs.iter().rev().map(|l| {
            let mut v = json!({"name": l.coord, "url": repo(l)});
            if s == Software::Fabric && l.official_sha {
                v["sha256"] = json!(model::sha256(&l.body));
            }
            if s == Software::Fabric && l.official_size {
                v["size"] = json!(l.body.len());
            }
            v
        }).collect::<Vec<_>>()
    });
    let info = (s == Software::Quilt).then(|| {
        let pin = |prefix: &str| {
            let l = libs.iter().find(|l| l.coord.starts_with(prefix)).unwrap();
            json!({
                "maven": l.coord, "version": l.coord.rsplit(':').next().unwrap(),
                "file_size": l.body.len(),
                "hashes": {"sha1": "0".repeat(40), "sha256": model::sha256(&l.body), "sha512": "0".repeat(128)}
            })
        };
        json!({
            "loader": pin("org.quiltmc:quilt-loader:"),
            "hashed": pin("org.quiltmc:hashed:"),
            "intermediary": {"maven": format!("net.fabricmc:intermediary:{GAME}"), "version": GAME}
        })
    });
    let mut sidecars = HashMap::new();
    let mut heads = HashMap::new();
    for l in libs {
        let name = mpath(&l.coord).rsplit('/').next().unwrap().to_string();
        sidecars.insert(
            route(l),
            Some(format!("{}  {}\n", model::sha256(&l.body), name).into_bytes()),
        );
        heads.insert(route(l), Some(l.body.clone()));
    }
    Docs {
        manifest,
        client,
        server,
        info,
        sidecars,
        heads,
    }
}
async fn mount_with(server: &MockServer, m: &str, route: &str, body: Vec<u8>) {
    Mock::given(method(m))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
}
async fn mount_docs(f: &Fixture, s: Software, libs: &[Lib], d: &Docs) {
    let loader = loader_of(s);
    let id = s.id();
    let raw = |v: &Value| serde_json::to_vec(v).unwrap();
    mount_with(
        &f.server,
        "GET",
        &format!(
            "/{id}/{}/manifest.json",
            if s == Software::Quilt { "v1" } else { "v0" }
        ),
        raw(&d.manifest),
    )
    .await;
    mount_with(
        &f.server,
        "GET",
        &if s == Software::Quilt {
            format!("/quilt/v1/version-group/v2/loader-version/{loader}")
        } else {
            format!("/{id}/v0/versions/{loader}.json")
        },
        raw(&d.client),
    )
    .await;
    let meta = if s == Software::Fabric { "/v2" } else { "/v3" };
    mount_with(
        &f.server,
        "GET",
        &format!("{meta}/versions/loader/{GAME}/{loader}/server/json"),
        raw(&d.server),
    )
    .await;
    if let Some(info) = &d.info {
        mount_with(
            &f.server,
            "GET",
            &format!("/v3/versions/loader/{GAME}/{loader}"),
            raw(info),
        )
        .await;
    }
    for l in libs {
        let r = route(l);
        mount_with(&f.server, "GET", &r, l.served.clone()).await;
        if let Some(Some(sidecar)) = d.sidecars.get(&r) {
            mount_with(&f.server, "GET", &format!("{r}.sha256"), sidecar.clone()).await;
        }
        if let Some(Some(head)) = d.heads.get(&r) {
            mount_with(&f.server, "HEAD", &r, head.clone()).await;
        }
    }
}
fn selection() -> Selection {
    Selection {
        version: Some(GAME.into()),
        ..Selection::default()
    }
}
async fn plan_with(f: &Fixture, s: Software, sel: &Selection) -> Result<InstallPlan> {
    f.installer
        .plan(
            s,
            sel,
            &std::env::current_exe().unwrap(),
            &SilentProgressReporter,
        )
        .await
}
/// Mounted fixture with Vanilla metadata and artifact plus the selected loader.
async fn loader_fixture(s: Software, libs: &[Lib]) -> Fixture {
    let f = Fixture::new(Software::Vanilla).await;
    f.artifact().await;
    mount_docs(&f, s, libs, &docs(s, libs)).await;
    f
}
async fn artifact_gets(f: &Fixture, libs: &[Lib]) -> usize {
    let routes: Vec<_> = libs.iter().map(route).collect();
    f.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| {
            r.method == "GET"
                && (routes.contains(&r.url.path().to_string()) || r.url.path() == f.artifact_path)
        })
        .count()
}
async fn install(f: &Fixture, p: &InstallPlan) -> Result<InstallOutcome> {
    f.installer
        .install(p, Approval::review(p, ACK)?, &SilentProgressReporter)
        .await
}
fn staging_is_clean(f: &Fixture) {
    let path = f.root.path().join(".staging");
    if path.exists() {
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
    }
}
fn expected_args(s: Software, libs: &[Lib]) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp: Vec<_> = libs
        .iter()
        .map(|l| format!("../software/libraries/{}", mpath(&l.coord)))
        .collect();
    format!(
        "-D{}.gameJarPath=../software/vanilla.jar\n-cp\n{}\n{}\nnogui\n",
        if s == Software::Fabric {
            "fabric"
        } else {
            "loader"
        },
        cp.join(sep),
        bundle::server_main(s).unwrap()
    )
}

#[test]
fn maven_coordinates_accept_only_the_narrow_three_part_grammar() {
    assert_eq!(
        maven_path("net.fabricmc:sponge-mixin:0.15.4+mixin.0.8.7").unwrap(),
        "net/fabricmc/sponge-mixin/0.15.4+mixin.0.8.7/sponge-mixin-0.15.4+mixin.0.8.7.jar"
    );
    assert_eq!(
        maven_path("org.ow2.asm:asm:9.7.1").unwrap(),
        mpath("org.ow2.asm:asm:9.7.1")
    );
    for refused in [
        "net.fabricmc:fabric-loader",
        "a:b:1:natives-windows",
        "a:b:1@zip",
        "a:b:[1.0,2.0)",
        &format!("net.fabricmc:intermediary:{TOKEN}"),
        "a:b:..",
        "a:b:../1",
        "a/b:c:1",
        "a\\b:c:1",
        "a:b c:1",
        "a:b:1\n",
        "a:b:1%2e",
        "a:é:1",
        "a+b:c:1",
        "a:b+c:1",
        ".a:b:1",
        "a.:b:1",
        "a:CON:1",
        "a:b:",
        "::",
        &format!("a:b:{}", "1".repeat(65)),
    ] {
        assert!(maven_path(refused).is_err(), "{refused}");
    }
}
#[test]
fn sidecar_digests_are_strict_sha256_text() {
    let hex = model::sha256(b"x");
    for good in [
        hex.clone(),
        format!("{hex}\n"),
        format!("{hex}  file.jar\n"),
        hex.to_uppercase(),
    ] {
        assert_eq!(
            sidecar_digest(good.as_bytes()).unwrap().hex(),
            hex,
            "{good:?}"
        );
    }
    for bad in [
        String::new(),
        hex[..63].to_string(),
        format!("{hex}0"),
        format!("{hex}x file.jar"),
        format!("{}g", &hex[..63]),
        format!("{hex}  a b.jar"),
        format!("{hex} {}", "a".repeat(200)),
        "<!doctype html><html>not found</html>".repeat(3),
    ] {
        assert!(sidecar_digest(bad.as_bytes()).is_err(), "{bad:?}");
    }
    assert!(sidecar_digest(format!("{hex} é").as_bytes()).is_err());
}

#[tokio::test]
async fn fabric_plan_uses_modrinth_order_official_pins_and_compiled_launch() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    assert_eq!(p.schema, 2);
    assert!(p.artifact.is_none());
    let b = p.bundle.as_ref().unwrap();
    assert_eq!(
        b.loader_version, "0.16.10",
        "first stable, not first listed"
    );
    let reviewed = p.artifacts();
    assert_eq!(reviewed.len(), libs.len() + 1);
    assert_eq!(reviewed[0].destination, "software/vanilla.jar");
    assert_eq!(reviewed[0].family, Some(SourceFamily::MojangObjects));
    assert_eq!(reviewed[0].digest.algorithm(), DigestAlgorithm::Sha1);
    for (r, l) in reviewed[1..].iter().zip(&libs) {
        assert_eq!(
            r.destination,
            format!("software/libraries/{}", mpath(&l.coord))
        );
        assert_eq!(r.url, format!("{}{}", FABRIC_MAVEN, mpath(&l.coord)));
        assert_eq!(r.family, Some(SourceFamily::FabricMaven));
        assert_eq!(r.digest.algorithm(), DigestAlgorithm::Sha256);
        assert_eq!(r.digest.hex(), model::sha256(&l.body));
        assert_eq!(r.size, Some(l.body.len() as u64));
    }
    assert_eq!(b.classpath, (1..=libs.len()).collect::<Vec<_>>());
    let args = expected_args(Software::Fabric, &libs);
    assert_eq!(b.generated.path, "software/launch.args");
    assert_eq!(b.generated.size, args.len() as u64);
    assert_eq!(b.generated.sha256, model::sha256(args.as_bytes()));
    let r = p.launch_recipe();
    assert_eq!(r.kind, "java-argfile");
    assert_eq!(r.artifact, None);
    assert_eq!(
        r.argfile,
        Some(p.destination().join("software/launch.args"))
    );
    // `nogui` is the final argfile token after the compiled main class; no trailing recipe args.
    assert!(r.args.is_empty());
    assert!(args.ends_with(
        "KnotServer
nogui
"
    ));
    assert_eq!(r.working_directory, p.destination().join("game"));
    let wire = serde_json::to_string(&p).unwrap();
    for forbidden in [
        "javaagent",
        "metadata.must.not.Launch",
        "--metadata-arg",
        "--modrinth-supplied",
        "KnotClient",
        "launcher-meta.modrinth.com/maven",
        "eula=true",
    ] {
        assert!(!wire.contains(forbidden), "{forbidden}");
    }
    assert_eq!(
        artifact_gets(&f, &libs).await,
        0,
        "planning never downloads"
    );
    assert!(entries(f.root.path()).is_empty());
}

#[tokio::test]
async fn fabric_install_publishes_every_file_with_receipt_last_and_verify_rehashes() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    let InstallOutcome::Installed(r) = install(&f, &p).await.unwrap() else {
        panic!("expected installed bundle");
    };
    let root = p.destination();
    assert_eq!(
        std::fs::read(root.join("software/vanilla.jar")).unwrap(),
        BODY
    );
    for l in &libs {
        assert_eq!(
            std::fs::read(root.join("software/libraries").join(mpath(&l.coord))).unwrap(),
            l.body
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("software/launch.args")).unwrap(),
        expected_args(Software::Fabric, &libs)
    );
    assert!(root.join("control/receipt.json").is_file());
    assert_eq!(std::fs::read_dir(root.join("game")).unwrap().count(), 0);
    assert_eq!(r.eula, "not_accepted");
    assert_eq!(r.plan_hash, p.hash());
    let b = r.bundle.as_ref().unwrap();
    assert_eq!(b.artifacts.len(), libs.len() + 1);
    for (record, l) in b.artifacts[1..].iter().zip(&libs) {
        assert_eq!(record.observed_sha256, model::sha256(&l.body));
        assert_eq!(record.published_digest.hex(), model::sha256(&l.body));
        assert_eq!(record.actual_size, l.body.len() as u64);
    }
    staging_is_clean(&f);
    let verified = f.installer.verify(&p).await.unwrap();
    assert_eq!(verified.plan_hash, p.hash());
    assert_eq!(
        install(&f, &p).await.unwrap_err(),
        InstallError::DestinationExists
    );
}

#[tokio::test]
async fn bundle_verification_requires_original_pins_despite_a_consistent_rewritten_receipt() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    install(&f, &p).await.unwrap();
    let root = p.destination();
    let target = root.join("software/libraries").join(mpath(&libs[2].coord));
    let tampered = vec![b'x'; libs[2].body.len()];
    std::fs::write(&target, &tampered).unwrap();
    assert!(f.installer.verify(&p).await.is_err());
    // A receipt rewritten to describe the tampered bytes still cannot replace the original pin.
    let marker = root.join("control/receipt.json");
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    let records = receipt["artifacts"].as_array_mut().unwrap();
    let record = records
        .iter_mut()
        .find(|r| {
            r["path"]
                .as_str()
                .unwrap()
                .ends_with(&mpath(&libs[2].coord))
        })
        .unwrap();
    record["observed_sha256"] = json!(model::sha256(&tampered));
    std::fs::write(&marker, serde_json::to_vec(&receipt).unwrap()).unwrap();
    assert!(f.installer.verify(&p).await.is_err());
    // An undeclared file inside the software tree is refused without deleting it.
    std::fs::write(&target, &libs[2].body).unwrap();
    receipt = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    let records = receipt["artifacts"].as_array_mut().unwrap();
    for r in records.iter_mut() {
        if r["path"]
            .as_str()
            .unwrap()
            .ends_with(&mpath(&libs[2].coord))
        {
            r["observed_sha256"] = json!(model::sha256(&libs[2].body));
        }
    }
    std::fs::write(&marker, serde_json::to_vec(&receipt).unwrap()).unwrap();
    f.installer.verify(&p).await.unwrap();
    let extra = root.join("software/libraries/extra.jar");
    std::fs::write(&extra, b"undeclared").unwrap();
    assert!(f.installer.verify(&p).await.is_err());
    assert_eq!(std::fs::read(extra).unwrap(), b"undeclared");
}

#[tokio::test]
async fn bundle_receipt_decoder_rejects_unknown_duplicate_missing_and_oversized_records() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    install(&f, &p).await.unwrap();
    let marker = p.destination().join("control/receipt.json");
    let original = std::fs::read(&marker).unwrap();
    let base: Value = serde_json::from_slice(&original).unwrap();
    let mut cases: Vec<Vec<u8>> = Vec::new();
    let mut unknown = base.clone();
    unknown["unexpected"] = json!(true);
    cases.push(serde_json::to_vec(&unknown).unwrap());
    let mut missing = base.clone();
    missing["artifacts"].as_array_mut().unwrap().pop();
    cases.push(serde_json::to_vec(&missing).unwrap());
    let mut duplicate = base.clone();
    let first = duplicate["artifacts"][1].clone();
    duplicate["artifacts"].as_array_mut().unwrap()[2] = first;
    cases.push(serde_json::to_vec(&duplicate).unwrap());
    let mut reordered = base.clone();
    reordered["artifacts"].as_array_mut().unwrap().swap(1, 2);
    cases.push(serde_json::to_vec(&reordered).unwrap());
    let mut eula = base.clone();
    eula["eula"] = json!("accepted");
    cases.push(serde_json::to_vec(&eula).unwrap());
    let mut kind = base.clone();
    kind["launch"]["kind"] = json!("java-jar");
    cases.push(serde_json::to_vec(&kind).unwrap());
    let mut trailing = original.clone();
    trailing.extend_from_slice(b" {}");
    cases.push(trailing);
    let mut oversized = original.clone();
    oversized.extend(std::iter::repeat_n(b' ', bundle::RECEIPT_BYTES));
    cases.push(oversized);
    for data in cases {
        std::fs::write(&marker, &data).unwrap();
        assert!(f.installer.verify(&p).await.is_err());
    }
    std::fs::write(&marker, &original).unwrap();
    f.installer.verify(&p).await.unwrap();
}

#[tokio::test]
async fn every_partial_publication_boundary_retains_the_bundle_without_a_receipt() {
    let libs = fabric_libs();
    let mut f = loader_fixture(Software::Fabric, &libs).await;
    let links = libs.len() + 2; // vanilla + libraries + generated argfile
    for index in 0..links {
        f.installer.fail_after_bundle_link = Some(index);
        let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
        assert_eq!(
            install(&f, &p).await.unwrap_err(),
            InstallError::PublishFailed
        );
        let root = p.destination();
        assert!(root.is_dir(), "partial bundle retained at {index}");
        assert!(!root.join("control/receipt.json").exists());
        let published: Vec<_> = p
            .artifacts()
            .iter()
            .map(|a| root.join(&a.destination))
            .chain(std::iter::once(root.join("software/launch.args")))
            .map(|path| path.exists())
            .collect();
        assert_eq!(
            published,
            (0..links).map(|i| i <= index).collect::<Vec<_>>(),
            "links published through boundary {index}"
        );
        assert!(f.installer.verify(&p).await.is_err());
        staging_is_clean(&f);
        f.installer.fail_after_bundle_link = None;
        assert_eq!(
            install(&f, &p).await.unwrap_err(),
            InstallError::DestinationExists,
            "incomplete bundle is never adopted or overwritten"
        );
    }
}

#[tokio::test]
async fn changed_downloaded_library_refuses_before_any_publication() {
    let mut libs = fabric_libs();
    libs[3].served = vec![b'z'; libs[3].body.len()];
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    assert_eq!(
        install(&f, &p).await.unwrap_err(),
        InstallError::ArtifactChanged
    );
    assert!(!p.destination().exists());
    assert!(
        !f.root.path().join("instances").exists()
            || entries(&f.root.path().join("instances")).is_empty()
    );
    staging_is_clean(&f);
}

#[tokio::test]
async fn shared_install_byte_budget_covers_every_artifact() {
    let libs = fabric_libs();
    let mut f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    // Each file fits individually, but planning bytes plus all artifacts exceed the one budget.
    let needed = p.transferred + p.artifacts().iter().map(|a| a.size.unwrap()).sum::<u64>();
    f.installer.limits.total_bytes = needed - 1;
    let mut p = p;
    p.limits.total_bytes = needed - 1;
    assert_eq!(
        install(&f, &p).await.unwrap_err(),
        InstallError::LimitExceeded
    );
    assert!(!p.destination().exists());
    staging_is_clean(&f);
}

#[tokio::test]
async fn bundle_plan_binds_every_artifact_field_and_requires_legacy_acknowledgement() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    assert!(
        p.hash() != {
            let mut v = p.clone();
            v.schema = 1;
            v.hash()
        }
    );
    let changes: Vec<InstallPlanMutation> = vec![
        Box::new(|v| v.bundle.as_mut().unwrap().loader_version.push('1')),
        Box::new(|v| v.bundle.as_mut().unwrap().artifacts[2].pin.url.push('a')),
        Box::new(|v| v.bundle.as_mut().unwrap().artifacts[2].pin.size = Some(1)),
        Box::new(|v| v.bundle.as_mut().unwrap().artifacts[3].pin.digest.hex = "0".repeat(64)),
        Box::new(|v| {
            v.bundle.as_mut().unwrap().artifacts[4]
                .destination
                .push('x')
        }),
        Box::new(|v| v.bundle.as_mut().unwrap().artifacts[1].family = SourceFamily::QuiltMaven),
        Box::new(|v| v.bundle.as_mut().unwrap().classpath.swap(0, 1)),
        Box::new(|v| v.bundle.as_mut().unwrap().generated.sha256 = "0".repeat(64)),
        Box::new(|v| v.bundle.as_mut().unwrap().classpath_separator = "|".into()),
    ];
    for change in changes {
        let mut changed = p.clone();
        change(&mut changed);
        assert_ne!(changed.hash(), p.hash());
        let approval = Approval::review(&p, ACK).unwrap();
        assert_eq!(
            f.installer
                .install(&changed, approval, &SilentProgressReporter)
                .await
                .unwrap_err(),
            InstallError::ApprovalMismatch
        );
    }
    // The bundle's Vanilla pin is SHA-1, so legacy acknowledgement is required for the bundle.
    assert_eq!(
        Approval::review(
            &p,
            Acknowledgments {
                legacy_checksum: false,
                warnings: true
            }
        )
        .unwrap_err(),
        InstallError::ApprovalRequired
    );
    assert!(entries(f.root.path()).is_empty());
}

#[tokio::test]
async fn quilt_plan_requires_agreeing_meta_and_maven_pins_and_uses_loader_property() {
    let libs = quilt_libs();
    let f = loader_fixture(Software::Quilt, &libs).await;
    let p = plan_with(&f, Software::Quilt, &selection()).await.unwrap();
    let b = p.bundle.as_ref().unwrap();
    assert_eq!(b.loader_version, "0.28.1");
    for (a, l) in p.artifacts()[1..].iter().zip(&libs) {
        assert_eq!(a.url, format!("{}{}", repo(l), mpath(&l.coord)));
        assert_eq!(
            a.family,
            Some(if l.quilt_repo {
                SourceFamily::QuiltMaven
            } else {
                SourceFamily::FabricMaven
            })
        );
        assert_eq!(a.digest.hex(), model::sha256(&l.body));
    }
    let InstallOutcome::Installed(_) = install(&f, &p).await.unwrap() else {
        panic!("expected installed bundle");
    };
    assert_eq!(
        std::fs::read_to_string(p.destination().join("software/launch.args")).unwrap(),
        expected_args(Software::Quilt, &libs)
    );
    f.installer.verify(&p).await.unwrap();
}

#[tokio::test]
async fn quilt_meta_and_maven_disagreement_fails_closed_before_artifact_downloads() {
    for target in ["org.quiltmc:quilt-loader:", "org.quiltmc:hashed:"] {
        let libs = quilt_libs();
        let f = Fixture::new(Software::Vanilla).await;
        f.artifact().await;
        let mut d = docs(Software::Quilt, &libs);
        let key = if target.contains("loader") {
            "loader"
        } else {
            "hashed"
        };
        d.info.as_mut().unwrap()[key]["hashes"]["sha256"] = json!(model::sha256(b"different"));
        mount_docs(&f, Software::Quilt, &libs, &d).await;
        assert_eq!(
            plan_with(&f, Software::Quilt, &selection())
                .await
                .unwrap_err(),
            InstallError::IntegrityConflict
        );
        assert_eq!(artifact_gets(&f, &libs).await, 0);
        assert!(entries(f.root.path()).is_empty());
    }
}

#[tokio::test]
async fn metadata_cannot_change_membership_main_class_or_repository_policy() {
    type Tweak = Box<dyn Fn(&mut Docs)>;
    let fabric: Vec<(&str, InstallError, Tweak)> = vec![
        (
            "client main",
            InstallError::InvalidMetadata,
            Box::new(|d| d.client["mainClass"] = json!("evil.Main")),
        ),
        (
            "server main",
            InstallError::InvalidMetadata,
            Box::new(|d| {
                d.server["mainClass"] = json!("net.fabricmc.loader.impl.launch.knot.KnotClient")
            }),
        ),
        (
            "extra client library",
            InstallError::InvalidMetadata,
            Box::new(|d| {
                d.client["libraries"].as_array_mut().unwrap().push(json!({
                    "name":"org.example:extra:1.0","url":MODRINTH_MAVEN,
                    "include_in_classpath":true,"downloadable":true}))
            }),
        ),
        (
            "extra official library",
            InstallError::InvalidMetadata,
            Box::new(|d| {
                d.server["libraries"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name":"org.example:extra:1.0","url":FABRIC_MAVEN}))
            }),
        ),
        (
            "duplicate client library",
            InstallError::InvalidMetadata,
            Box::new(|d| {
                let first = d.client["libraries"][0].clone();
                d.client["libraries"].as_array_mut().unwrap().push(first)
            }),
        ),
        (
            "client library mirror",
            InstallError::InvalidMetadata,
            Box::new(|d| d.client["libraries"][0]["url"] = json!("https://evil.example/maven/")),
        ),
        (
            "not on classpath",
            InstallError::InvalidMetadata,
            Box::new(|d| d.client["libraries"][1]["include_in_classpath"] = json!(false)),
        ),
        (
            "not downloadable",
            InstallError::InvalidMetadata,
            Box::new(|d| d.client["libraries"][1]["downloadable"] = json!(false)),
        ),
        (
            "unlisted repository",
            InstallError::SourceRefused,
            Box::new(|d| {
                d.server["libraries"][0]["url"] = json!("https://repo1.maven.org/maven2/")
            }),
        ),
        (
            "quilt repository for fabric",
            InstallError::SourceRefused,
            Box::new(|d| d.server["libraries"][0]["url"] = json!(QUILT_MAVEN)),
        ),
        (
            "classifier",
            InstallError::InvalidMetadata,
            Box::new(|d| {
                d.client["libraries"][0]["name"] = json!("org.ow2.asm:asm:9.7.1:natives");
                d.server["libraries"][3]["name"] = json!("org.ow2.asm:asm:9.7.1:natives");
            }),
        ),
        (
            "catalogue profile url",
            InstallError::InvalidMetadata,
            Box::new(|d| {
                d.manifest["gameVersions"][2]["loaders"][1]["url"] =
                    json!("https://launcher-meta.modrinth.com/fabric/v0/versions/other.json")
            }),
        ),
    ];
    for (name, expected, tweak) in fabric {
        let libs = fabric_libs();
        let f = Fixture::new(Software::Vanilla).await;
        f.artifact().await;
        let mut d = docs(Software::Fabric, &libs);
        tweak(&mut d);
        mount_docs(&f, Software::Fabric, &libs, &d).await;
        assert_eq!(
            plan_with(&f, Software::Fabric, &selection())
                .await
                .unwrap_err(),
            expected,
            "{name}"
        );
        assert_eq!(artifact_gets(&f, &libs).await, 0, "{name}");
        assert!(
            !f.server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path().starts_with("/maven/")),
            "{name}: the Modrinth mirror is never an artifact source"
        );
        assert!(entries(f.root.path()).is_empty(), "{name}");
    }
}

#[tokio::test]
async fn partial_official_evidence_must_agree_with_sidecar_and_head() {
    type Tweak = Box<dyn Fn(&mut Docs, &[Lib])>;
    let cases: Vec<(&str, Tweak)> = vec![
        (
            "official sha256 without size disagrees with sidecar",
            Box::new(|d, _| d.server["libraries"][2]["sha256"] = json!(model::sha256(b"other"))),
        ),
        (
            "official size without sha256 disagrees with HEAD",
            Box::new(|d, libs| {
                let r = &mut d.server["libraries"][1];
                r["size"] = json!(libs[2].body.len() + 1);
            }),
        ),
    ];
    for (name, tweak) in cases {
        let libs = fabric_libs();
        let f = Fixture::new(Software::Vanilla).await;
        f.artifact().await;
        let mut d = docs(Software::Fabric, &libs);
        tweak(&mut d, &libs);
        mount_docs(&f, Software::Fabric, &libs, &d).await;
        assert_eq!(
            plan_with(&f, Software::Fabric, &selection())
                .await
                .unwrap_err(),
            InstallError::IntegrityConflict,
            "{name}"
        );
        assert_eq!(artifact_gets(&f, &libs).await, 0, "{name}");
    }
}

#[tokio::test]
async fn missing_or_malformed_sidecar_and_head_evidence_refuse() {
    type Tweak = Box<dyn Fn(&mut Docs, &[Lib])>;
    let cases: Vec<(&str, Tweak)> = vec![
        (
            "missing sidecar",
            Box::new(|d, libs| {
                d.sidecars.insert(route(&libs[3]), None);
            }),
        ),
        (
            "html sidecar",
            Box::new(|d, libs| {
                d.sidecars.insert(
                    route(&libs[3]),
                    Some(b"<!doctype html><html>not found</html>".repeat(3)),
                );
            }),
        ),
        (
            "oversized sidecar",
            Box::new(|d, libs| {
                let mut v = model::sha256(&libs[3].body).into_bytes();
                v.extend(std::iter::repeat_n(b' ', 300));
                d.sidecars.insert(route(&libs[3]), Some(v));
            }),
        ),
        (
            "missing HEAD",
            Box::new(|d, libs| {
                d.heads.insert(route(&libs[3]), None);
            }),
        ),
        (
            "empty HEAD length",
            Box::new(|d, libs| {
                d.heads.insert(route(&libs[3]), Some(Vec::new()));
            }),
        ),
        (
            "HEAD above artifact ceiling",
            Box::new(|d, libs| {
                d.heads.insert(route(&libs[3]), Some(vec![0; 129]));
            }),
        ),
    ];
    for (name, tweak) in cases {
        let libs = fabric_libs();
        let f = Fixture::new(Software::Vanilla).await;
        f.artifact().await;
        let mut d = docs(Software::Fabric, &libs);
        tweak(&mut d, &libs);
        mount_docs(&f, Software::Fabric, &libs, &d).await;
        assert!(
            plan_with(&f, Software::Fabric, &selection()).await.is_err(),
            "{name}"
        );
        assert_eq!(artifact_gets(&f, &libs).await, 0, "{name}");
        assert!(entries(f.root.path()).is_empty(), "{name}");
    }
}

#[tokio::test]
async fn loader_library_count_is_capped_before_official_metadata_is_requested() {
    for (count, accepted) in [(128usize, true), (129, false)] {
        let libs: Vec<_> = (0..count)
            .map(|i| {
                lib(
                    &format!("net.fabricmc:fixture-{i}:1.0"),
                    false,
                    format!("library {i}").as_bytes(),
                    true,
                    true,
                )
            })
            .collect();
        let mut f = Fixture::new(Software::Vanilla).await;
        f.installer.limits.total_bytes = 1 << 20;
        f.installer.limits.metadata_bytes = 1 << 20;
        f.artifact().await;
        mount_docs(&f, Software::Fabric, &libs, &docs(Software::Fabric, &libs)).await;
        let result = plan_with(&f, Software::Fabric, &selection()).await;
        assert_eq!(result.is_ok(), accepted, "{count}");
        if !accepted {
            assert_eq!(result.unwrap_err(), InstallError::InvalidMetadata);
            assert!(
                !f.server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .any(|r| r.url.path().ends_with("/server/json"))
            );
        }
    }
}

#[tokio::test]
async fn loader_selection_is_validated_and_never_falls_back() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    for (sel, expected) in [
        (
            Selection {
                loader_version: Some("9.9.9".into()),
                ..selection()
            },
            InstallError::Unavailable,
        ),
        (
            Selection {
                loader_version: Some("../0.16.10".into()),
                ..selection()
            },
            InstallError::InvalidInput,
        ),
        (
            Selection {
                loader_version: Some("0.16.10 ".into()),
                ..selection()
            },
            InstallError::InvalidInput,
        ),
        (
            Selection {
                version: Some("1.20.1".into()),
                ..selection()
            },
            InstallError::Unavailable,
        ),
        (
            Selection {
                build: Some(1),
                ..selection()
            },
            InstallError::InvalidInput,
        ),
    ] {
        assert_eq!(
            plan_with(&f, Software::Fabric, &sel).await.unwrap_err(),
            expected,
            "{sel:?}"
        );
    }
    // Loader selection is meaningless for single-JAR providers and is refused, not ignored.
    let paper = Fixture::new(Software::Paper).await;
    assert_eq!(
        plan_with(
            &paper,
            Software::Paper,
            &Selection {
                loader_version: Some("0.16.10".into()),
                ..Selection::default()
            }
        )
        .await
        .unwrap_err(),
        InstallError::InvalidInput
    );
    assert!(entries(f.root.path()).is_empty());
}

#[tokio::test]
async fn loader_catalogue_lists_supported_versions_without_downloads() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let mut versions = f
        .installer
        .versions(Software::Fabric, &SilentProgressReporter)
        .await
        .unwrap();
    versions.sort();
    assert_eq!(versions, ["1.21.3", "1.21.4"]);
    assert_eq!(artifact_gets(&f, &libs).await, 0);
    assert!(entries(f.root.path()).is_empty());
}

/// Remount Vanilla metadata (manifest digest included) declaring an older Java runtime.
async fn vanilla_java(f: &Fixture, major: u32) {
    let hash = digest(DigestAlgorithm::Sha1, BODY);
    let detail = json!({"id":GAME,"javaVersion":{"majorVersion":major},"downloads":{
        "server":{"url":format!("https://piston-data.mojang.com{}", f.artifact_path),"sha1":hash,"size":BODY.len()}}});
    let encoded = serde_json::to_vec(&detail).unwrap();
    let manifest = json!({"latest":{"release":GAME},"versions":[{
        "id":GAME,"type":"release","url":format!("https://launcher-meta.modrinth.com/minecraft/v0/versions/{GAME}.json"),
        "sha1":digest(DigestAlgorithm::Sha1,&encoded),"originalSha1":"0000000000000000000000000000000000000000"}]});
    for (route, body) in [
        (format!("/minecraft/v0/versions/{GAME}.json"), encoded),
        (
            "/minecraft/v0/manifest.json".to_string(),
            serde_json::to_vec(&manifest).unwrap(),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .with_priority(1)
            .mount(&f.server)
            .await;
    }
}

#[tokio::test]
async fn argfile_bundles_require_java_nine_even_when_vanilla_declares_java_eight() {
    let libs = fabric_libs();
    let mut f = loader_fixture(Software::Fabric, &libs).await;
    vanilla_java(&f, 8).await;
    f.installer.fixture_java = Some(8);
    assert_eq!(
        plan_with(&f, Software::Fabric, &selection())
            .await
            .unwrap_err(),
        InstallError::RuntimeIncompatible
    );
    assert_eq!(artifact_gets(&f, &libs).await, 0);
    assert!(entries(f.root.path()).is_empty());
    f.installer.fixture_java = Some(9);
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    assert_eq!(p.java_requirement().major, 9);
    assert_eq!(p.java_requirement().evidence, "java-argfile launch");
    // A newer declared runtime is preserved, never lowered to the argfile floor.
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    assert_eq!(p.java_requirement().major, 21);
}

async fn redirect(f: &Fixture, m: &str, route: &str, location: &str) {
    Mock::given(method(m))
        .and(path(route))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", location))
        .with_priority(1)
        .mount(&f.server)
        .await;
}
async fn requested(f: &Fixture, route: &str) -> usize {
    f.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == route)
        .count()
}

#[tokio::test]
async fn explicitly_named_unstable_loader_requires_experimental_opt_in() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let beta = "/fabric/v0/versions/0.99.0-beta.json";
    let named = |experimental| Selection {
        loader_version: Some("0.99.0-beta".into()),
        experimental,
        ..selection()
    };
    assert_eq!(
        plan_with(&f, Software::Fabric, &named(false))
            .await
            .unwrap_err(),
        InstallError::Unavailable
    );
    assert_eq!(
        requested(&f, beta).await,
        0,
        "refused before the profile is fetched"
    );
    // With the opt-in, selection proceeds to that loader's profile (unmounted here).
    assert_ne!(
        plan_with(&f, Software::Fabric, &named(true))
            .await
            .unwrap_err(),
        InstallError::Unavailable
    );
    assert_eq!(requested(&f, beta).await, 1);
    assert!(entries(f.root.path()).is_empty());
}

#[tokio::test]
async fn pinned_fabric_release_remains_selectable_after_recommendation_moves_on() {
    let libs = fabric_libs();
    let f = Fixture::new(Software::Vanilla).await;
    f.artifact().await;
    let mut d = docs(Software::Fabric, &libs);
    d.manifest["gameVersions"][2]["loaders"][1]["stable"] = json!(false);
    mount_docs(&f, Software::Fabric, &libs, &d).await;
    let choices = f
        .installer
        .releases(Software::Fabric, GAME, &SilentProgressReporter)
        .await
        .unwrap();
    assert!(choices.iter().any(|v| v.id == loader_of(Software::Fabric)));
    assert!(!choices.iter().any(|v| v.id.contains("beta")));
    let selected = Selection {
        loader_version: Some(loader_of(Software::Fabric).into()),
        ..selection()
    };
    let plan = plan_with(&f, Software::Fabric, &selected).await.unwrap();
    assert_eq!(plan.loader_version(), Some(loader_of(Software::Fabric)));
    assert!(!plan.warnings().contains(&Warning::ExperimentalBuild));
    install(&f, &plan).await.unwrap();
    f.installer.verify(&plan).await.unwrap();
}

#[tokio::test]
async fn schema_two_plans_hash_in_their_own_domain() {
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    let domain = |prefix: &[u8]| {
        let mut bytes = prefix.to_vec();
        bytes.extend(serde_json::to_vec(&p).unwrap());
        model::sha256(&bytes)
    };
    assert_eq!(p.hash(), domain(b"vesta.server-install.plan.v2\0"));
    assert_ne!(p.hash(), domain(b"vesta.server-install.plan.v1\0"));
}

#[test]
fn bundle_receipt_decoder_bounds_records_independently_of_byte_cap() {
    let record = json!({
        "path":"software/libraries/a/b/1/b-1.jar","url":"https://maven.fabricmc.net/a/b/1/b-1.jar",
        "family":"fabric_maven","published_size":1,
        "published_digest":{"algorithm":"sha256","hex":"0".repeat(64)},
        "actual_size":1,"observed_sha256":"0".repeat(64)
    });
    let receipt = |n: usize| {
        let v = json!({
            "schema":2,"install_id":"srv_x","plan_hash":"0".repeat(64),"software":"fabric",
            "version":GAME,"loader_version":"0.16.10","artifacts":vec![record.clone(); n],
            "generated":{"path":"software/launch.args","size":1,"sha256":"0".repeat(64)},
            "java_requirement":{"major":21,"evidence":"x"},
            "launch":{"kind":"java-argfile","java":"/java","working_directory":"/game","args":[],"stop_mode":"console-stop","argfile":"/a"},
            "eula":"not_accepted","warnings":[],"completed_at":1
        });
        serde_json::to_vec(&v).unwrap()
    };
    assert!(receipt(129).len() < bundle::RECEIPT_BYTES);
    assert!(receipt(130).len() < bundle::RECEIPT_BYTES);
    serde_json::from_slice::<bundle::BundleReceipt>(&receipt(129)).unwrap();
    assert!(serde_json::from_slice::<bundle::BundleReceipt>(&receipt(130)).is_err());
}

#[tokio::test]
async fn artifact_redirects_cannot_leave_their_source_family() {
    let libs = fabric_libs();
    let loader = route(&libs[3]);
    // Planning: HEAD on a Fabric Maven library cannot redirect to the Modrinth mirror.
    let f = loader_fixture(Software::Fabric, &libs).await;
    let mirror = format!("/maven/{}", mpath(&libs[3].coord));
    redirect(
        &f,
        "HEAD",
        &loader,
        &format!("https://launcher-meta.modrinth.com{mirror}"),
    )
    .await;
    assert_eq!(
        plan_with(&f, Software::Fabric, &selection())
            .await
            .unwrap_err(),
        InstallError::SourceRefused
    );
    assert_eq!(requested(&f, &mirror).await, 0);
    // Install: a Fabric Maven download cannot redirect into Quilt Maven.
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    let quilt = format!("/repository/release/{}", mpath(&libs[3].coord));
    redirect(
        &f,
        "GET",
        &loader,
        &format!("https://maven.quiltmc.org{quilt}"),
    )
    .await;
    assert_eq!(
        install(&f, &p).await.unwrap_err(),
        InstallError::SourceRefused
    );
    assert_eq!(requested(&f, &quilt).await, 0);
    assert!(!p.destination().exists());
    staging_is_clean(&f);
    // Quilt Maven cannot leave its release repository path family.
    let qlibs = quilt_libs();
    let f = loader_fixture(Software::Quilt, &qlibs).await;
    let sidecar = format!("{}.sha256", route(&qlibs[3]));
    let snapshot = sidecar.replace("/repository/release/", "/repository/snapshot/");
    redirect(
        &f,
        "GET",
        &sidecar,
        &format!("https://maven.quiltmc.org{snapshot}"),
    )
    .await;
    assert_eq!(
        plan_with(&f, Software::Quilt, &selection())
            .await
            .unwrap_err(),
        InstallError::SourceRefused
    );
    assert_eq!(requested(&f, &snapshot).await, 0);
}

#[tokio::test]
async fn installed_bundle_files_have_one_link_and_extra_links_fail_verification() {
    use super::super::execution_workspace::file_identity_with_links;
    let libs = fabric_libs();
    let f = loader_fixture(Software::Fabric, &libs).await;
    let p = plan_with(&f, Software::Fabric, &selection()).await.unwrap();
    install(&f, &p).await.unwrap();
    let root = p.destination();
    let files: Vec<_> = p
        .artifacts()
        .iter()
        .map(|a| root.join(&a.destination))
        .chain(std::iter::once(root.join("software/launch.args")))
        .collect();
    for file in &files {
        file_identity_with_links(&std::fs::File::open(file).unwrap(), 1)
            .unwrap_or_else(|_| panic!("{file:?} has exactly one link"));
    }
    // An alias outside the software tree escapes the allowlist scan; the link count catches it.
    let alias = f.root.path().join("alias.jar");
    std::fs::hard_link(&files[2], &alias).unwrap();
    assert_eq!(
        f.installer.verify(&p).await.unwrap_err(),
        InstallError::ReceiptInvalid
    );
    std::fs::remove_file(&alias).unwrap();
    f.installer.verify(&p).await.unwrap();
}

#[tokio::test]
async fn loader_releases_are_stable_choices_without_artifact_downloads() {
    for software in [Software::Fabric, Software::Quilt] {
        let libs = if software == Software::Fabric {
            fabric_libs()
        } else {
            quilt_libs()
        };
        let f = loader_fixture(software, &libs).await;
        let choices = f
            .installer
            .releases(software, "1.21.4", &SilentProgressReporter)
            .await
            .unwrap();
        assert!(!choices.is_empty());
        assert!(
            choices
                .iter()
                .all(|v| v.build.is_none() && v.loader_version.as_deref() == Some(v.id.as_str()))
        );
        assert_eq!(artifact_gets(&f, &libs).await, 0);
        assert!(entries(f.root.path()).is_empty());
    }
}
