use super::*;
use crate::progress::{ProgressReporter, SilentProgressReporter};
use serde_json::{json, Value};
use sha1::{Digest as _, Sha1};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use tempfile::TempDir;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

mod execution;
mod catalogue_cache;
mod buildtools;
mod execution_workspace;
mod installation;
mod loaders;
mod plans;
mod providers;
mod runtime;
mod transfer;

const BODY: &[u8] = b"owned inert server artifact fixture\n";
const ACK: Acknowledgments = Acknowledgments {
    legacy_checksum: true,
    warnings: true,
};

#[derive(Default)]
struct Reporter {
    cancel: AtomicBool,
    pause: AtomicBool,
    dry: bool,
    cancel_at: Option<u64>,
    cancel_on_done: bool,
    updates: Mutex<Vec<u64>>,
}
impl ProgressReporter for Reporter {
    fn start_step(&self, _: &str, _: Option<u32>) {}
    fn update_bytes(&self, n: u64, _: Option<u64>) {
        self.updates.lock().unwrap().push(n);
        if self.cancel_at.is_some_and(|v| n >= v) {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }
    fn done(&self, _: bool, _: Option<&str>) {
        if self.cancel_on_done {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn is_paused(&self) -> bool {
        self.pause.load(Ordering::SeqCst)
    }
    fn is_dry_run(&self) -> bool {
        self.dry
    }
}

struct Fixture {
    root: TempDir,
    server: MockServer,
    installer: ServerInstaller,
    software: Software,
    artifact_path: String,
}
impl Fixture {
    async fn new(software: Software) -> Self {
        let server = MockServer::start().await;
        let root = tempfile::tempdir().unwrap();
        let mut installer = ServerInstaller::new(
            InstallRoots::new(root.path(), &[]).unwrap(),
            TransportConfig::direct(),
        )
        .unwrap();
        installer.http.fixture_origin = Some(url::Url::parse(&server.uri()).unwrap());
        installer.fixture_java = Some(25);
        installer.fixture_space = Some((root.path().to_path_buf(), 1 << 40));
        installer.coordinator = install::Coordinator::new();
        installer.limits.metadata_bytes = 8192;
        installer.limits.artifact_bytes = 128;
        installer.limits.total_bytes = 65536;
        installer.limits.idle_ms = 1000;
        installer.limits.total_ms = 5000;
        let artifact_path = mount_provider(&server, software, BODY).await;
        Self {
            root,
            server,
            installer,
            software,
            artifact_path,
        }
    }
    async fn plan(&self) -> InstallPlan {
        self.installer
            .plan(
                self.software,
                &Selection::default(),
                &std::env::current_exe().unwrap(),
                &SilentProgressReporter,
            )
            .await
            .unwrap()
    }
    async fn plan_result(&self) -> Result<InstallPlan> {
        self.installer
            .plan(
                self.software,
                &Selection::default(),
                &std::env::current_exe().unwrap(),
                &SilentProgressReporter,
            )
            .await
    }
    async fn artifact(&self) {
        mount(
            &self.server,
            &self.artifact_path,
            ResponseTemplate::new(200).set_body_bytes(BODY),
        )
        .await;
    }
}
async fn mount(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(server)
        .await;
}
async fn mount_json(server: &MockServer, route: &str, v: Value) {
    mount(server, route, ResponseTemplate::new(200).set_body_json(v)).await;
}
fn digest(algorithm: DigestAlgorithm, bytes: &[u8]) -> String {
    match algorithm {
        DigestAlgorithm::Sha256 => model::sha256(bytes),
        DigestAlgorithm::Sha1 => format!("{:x}", Sha1::digest(bytes)),
        DigestAlgorithm::Md5 => format!("{:x}", md5::Md5::digest(bytes)),
    }
}
async fn mount_provider(server: &MockServer, software: Software, bytes: &[u8]) -> String {
    match software {
        Software::Vanilla => {
            let hash = digest(DigestAlgorithm::Sha1, bytes);
            let artifact_path = format!("/v1/objects/{}/server.jar", hash);
            let detail = json!({"id":"1.21.4","javaVersion":{"majorVersion":21},"downloads":{
                "server":{"url":format!("https://piston-data.mojang.com{}",artifact_path),"sha1":hash,"size":bytes.len()},
                "client":{"url":"https://untrusted.invalid/client.jar","sha1":"bad","size":999},
                "server_mappings":{"url":"https://untrusted.invalid/map.txt","sha1":"bad","size":999}}});
            let encoded = serde_json::to_vec(&detail).unwrap();
            mount(
                server,
                "/minecraft/v0/versions/1.21.4.json",
                ResponseTemplate::new(200).set_body_bytes(encoded.clone()),
            )
            .await;
            mount_json(server, "/minecraft/v0/manifest.json", json!({"latest":{"release":"1.21.4"},"versions":[{
                "id":"1.21.4","type":"release","url":"https://launcher-meta.modrinth.com/minecraft/v0/versions/1.21.4.json",
                "sha1":digest(DigestAlgorithm::Sha1,&encoded), "originalSha1":"0000000000000000000000000000000000000000"}]})).await;
            artifact_path
        }
        Software::Paper | Software::Folia => {
            let hash = model::sha256(bytes);
            let name = format!("{}-1.21.4-7.jar", software.id());
            let artifact_path = format!("/v1/objects/{}/{}", hash, name);
            let base = format!("/v3/projects/{}", software.id());
            mount_json(server, &base, json!({"project":{"id":software.id(),"name":"display"},"versions":{"1.21":["1.21.4"]}})).await;
            mount_json(server, &(base.clone()+"/versions/1.21.4"), json!({"version":{"id":"1.21.4","java":{"version":{"minimum":21},
                "flags":{"recommended":["-javaagent:untrusted.jar"]}},"support":{"status":"SUPPORTED"}},"builds":[7,6]})).await;
            mount_json(server, &(base+"/versions/1.21.4/builds"), json!([{"id":7,"channel":"STABLE","downloads":{"server:default":{
                "name":name,"size":bytes.len(),"checksums":{"sha256":hash},"url":format!("https://fill-data.papermc.io{}", artifact_path)}}}])).await;
            artifact_path
        }
        Software::Purpur => {
            mount_json(
                server,
                "/v2/purpur",
                json!({"project":"purpur","versions":["1.21.4"]}),
            )
            .await;
            mount_json(server, "/v2/purpur/1.21.4", json!({"project":"purpur","version":"1.21.4","builds":{"latest":"7","all":["6","7"]}})).await;
            mount_json(server, "/v2/purpur/1.21.4/7", json!({"project":"purpur","version":"1.21.4","build":"7","result":"SUCCESS","md5":digest(DigestAlgorithm::Md5,bytes)})).await;
            "/v2/purpur/1.21.4/7/download".into()
        }
        _ => panic!("fixture only for implemented providers"),
    }
}
fn entries(path: &Path) -> Vec<String> {
    let mut values = Vec::new();
    fn walk(root: &Path, path: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(path).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, out);
            }
        }
    }
    walk(path, path, &mut values);
    values.sort();
    values
}
