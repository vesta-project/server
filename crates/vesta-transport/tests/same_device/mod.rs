//! Bounded opt-in loopback measurements. No provider requests or runtime launches.
use super::*;
use sha2::{Digest, Sha256};

struct FirstRead<R> { inner: R, start: Instant, first_ms: Option<f64> }
impl<R: Read> Read for FirstRead<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n=self.inner.read(bytes)?;
        if n>0 && self.first_ms.is_none() { self.first_ms=Some(self.start.elapsed().as_secs_f64()*1000.); }
        Ok(n)
    }
}
fn measured(address: SocketAddr, token: &str, path: &str, dataset: &str, state: &str, run: usize) -> WireReply {
    let start=Instant::now();
    let mut stream=connect(address,true).unwrap();
    let connect_ms=start.elapsed().as_secs_f64()*1000.;
    stream.sock.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    write!(stream,"GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").unwrap();
    stream.flush().unwrap();
    let mut reader=FirstRead {inner:stream,start,first_ms:None};
    let reply=read_reply(&mut reader).unwrap();
    let wall_ms=start.elapsed().as_secs_f64()*1000.;
    let parse=Instant::now(); let value=reply.json(); let parse_ms=parse.elapsed().as_secs_f64()*1000.;
    assert_eq!(reply.status,200,"{}",String::from_utf8_lossy(&reply.body));
    let probe=reply.headers.get("x-vesta-latency-probe").map(|s|serde_json::from_str::<serde_json::Value>(s).unwrap());
    println!("SAME_DEVICE {}",serde_json::json!({"dataset":dataset,"state":state,"run":run,"route":path,"wall_ms":wall_ms,"connect_tls_ms":connect_ms,"ttfb_ms":reader.first_ms,"parse_ms":parse_ms,"bytes":reply.body.len(),"rows":value["data"]["items"].as_array().map(Vec::len),"queue_ns":reply.headers.get("x-vesta-queue-ns"),"probe":probe}));
    reply
}
fn reopen(f: &mut Fixture) {
    f.finish();
    let document=std::fs::read(f.temp.path().join("host.toml")).unwrap();
    let config=HostConfiguration::parse_toml(&document,f.temp.path(),Path::new("host.toml")).unwrap();
    let ListenerDescription::Tls(listener)=config.listener() else {panic!("TLS")};
    let listener=listener.on_ephemeral_port();
    let host=Host::open(config,f.clock.clone()).unwrap();
    let transport=Transport::start(host.network_handle(),&listener).unwrap();
    f.address=transport.local_addr();f.host=Some(host);f.transport=Some(transport);
}
fn populate(f: &Fixture, dataset: &str) -> usize {
    let mods=f.temp.path().join("game/mods");std::fs::create_dir_all(&mods).unwrap();
    if dataset=="atm400" {
        // Explicit audit-owned retained input; never mutate it or use owner services.
        let base=std::path::PathBuf::from(std::env::var_os("VESTA_LATENCY_RETAINED_ROOT").expect("explicit retained audit root"));
        assert!(std::fs::read_to_string(base.join("game/server.properties")).unwrap().contains("motd=installation-audit-preserved"));
        let receipt:serde_json::Value=serde_json::from_slice(&std::fs::read(std::env::var_os("VESTA_LATENCY_RECEIPT").unwrap()).unwrap()).unwrap();
        let files=receipt["files"].as_object().unwrap();
        let mut paths=files.iter().filter(|(p,_)|p.starts_with("mods/")&&p.ends_with(".jar")).collect::<Vec<_>>();paths.sort_by_key(|(p,_)|*p);
        assert!(paths.len()>=400);let mut total=0u64;
        for (path,hash) in paths.into_iter().take(400) {
            let source=base.join("game").join(path);let meta=std::fs::symlink_metadata(&source).unwrap();assert!(meta.is_file()&&!meta.file_type().is_symlink());
            total+=meta.len();assert!(total<1_500_000_000);
            let bytes=std::fs::read(source).unwrap();assert_eq!(hex::encode(Sha256::digest(&bytes)),hash.as_str().unwrap());
            std::fs::write(f.temp.path().join("game").join(path),bytes).unwrap();
        }
        println!("SAME_DEVICE_INPUT {}",serde_json::json!({"dataset":dataset,"bytes":total,"files":400,"sha256_verified":true}));400
    } else {
        let count=if dataset=="synthetic100" {100}else{assert_eq!(dataset,"synthetic1600");1600};
        for i in 0..count {
            let mut zip=zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));zip.start_file("fabric.mod.json",zip::write::SimpleFileOptions::default()).unwrap();
            write!(zip,"{{\"id\":\"resource_{i}\",\"name\":\"Resource {i}\",\"version\":\"1\"}}").unwrap();
            std::fs::write(mods.join(format!("resource-{i}.jar{}",if i%20==0 {".disabled"}else{""})),zip.finish().unwrap().into_inner()).unwrap();
        }count
    }
}
#[test]
#[ignore = "bounded same-device latency matrix; explicit fixture inputs required"]
fn endpoint_matrix() {
    let dataset=std::env::var("VESTA_LATENCY_DATASET").expect("explicit dataset");
    let mut f=Fixture::new("minecraft",16,4096,120);
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
    let mut token=management_session(&f,&device,&key);
    let count=populate(&f,&dataset);
    for run in 0..6 {
        let reply=measured(f.address,&token,"/api/v3/manage/content",&dataset,if run==0 {"application_first"}else{"warm"},run);
        assert_eq!(reply.json()["data"]["items"].as_array().unwrap().len(),count);
    }
    measured(f.address,&token,"/api/v3/manage/content?view=summary",&dataset,"summary",0);
    for run in 0..3 {reopen(&mut f);token=management_session(&f,&device,&key);measured(f.address,&token,"/api/v3/manage/content",&dataset,"service_reopen",run);}
    if dataset=="atm400" {
        for run in 0..5 {measured(f.address,&token,"/api/v3/manage/server/status",&dataset,"poll_quiet",run);measured(f.address,&token,"/api/v3/manage/operations?limit=1",&dataset,"poll_quiet",run);}
        let address=f.address;
        std::thread::scope(|scope| {
            let job=scope.spawn(||measured(address,&token,"/api/v3/manage/content",&dataset,"loaded",0));
            // Explicit 2 Hz bounded polling, at most ten pairs, no public calls.
            for run in 0..10 {if job.is_finished(){break;}measured(f.address,&token,"/api/v3/manage/server/status",&dataset,"poll_loaded",run);measured(f.address,&token,"/api/v3/manage/operations?limit=1",&dataset,"poll_loaded",run);std::thread::sleep(Duration::from_millis(500));}
            job.join().unwrap();
        });
    }
}
