//! Opt-in local measurement. No providers, executable launches or owner paths.
use super::*;

fn measured(address: SocketAddr, token: &str, path: &str, case: &str, run: usize) -> WireReply {
    let start = Instant::now();
    let mut stream = connect(address, true).unwrap();
    stream.sock.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").unwrap();
    stream.flush().unwrap();
    let reply = read_reply(stream).unwrap();
    let wall_ms=start.elapsed().as_secs_f64()*1000.;
    let parse=Instant::now();let value=reply.json();let parse_ms=parse.elapsed().as_secs_f64()*1000.;
    let serialize=Instant::now();let bytes=serde_json::to_vec(&value).unwrap();let serialize_ms=serialize.elapsed().as_secs_f64()*1000.;
    assert_eq!(bytes.len(),reply.body.len());
    println!("RESOURCE_PERF {}", serde_json::json!({"case":case,"run":run,"status":reply.status,"wall_ms":wall_ms,"json_parse_ms":parse_ms,"json_value_serialize_ms":serialize_ms,"bytes":reply.body.len()}));
    if case == "status_saturated" || case == "full_saturated" {
        assert!(matches!(reply.status,200|503));
        if reply.status==503 { assert_eq!(value["error"]["code"],"temporarily_unavailable"); }
    } else { assert_eq!(reply.status, 200); }
    reply
}

#[test]
#[ignore = "bounded loopback 400-resource performance measurement"]
fn inventory_endpoint_400() {
    let mut f = Fixture::new("minecraft", 16, 4096, 60);
    let (device,key) = enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
    let token = management_session(&f,&device,&key);
    let mods = f.temp.path().join("game/mods");
    std::fs::create_dir_all(&mods).unwrap();
    // 350 enabled + 30 disabled Fabric JARs, 20 resource packs. Small payloads
    // isolate admission/declaration overhead; not representative JAR throughput.
    for i in 0..400 {
        let mut zip=zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let (dir,name,metadata)=if i<380 {
            (&mods,format!("resource-{i}.jar{}",if i>=350 {".disabled"}else{""}),"fabric.mod.json")
        }else{
            let dir=f.temp.path().join("game/resourcepacks");
            std::fs::create_dir_all(&dir).unwrap();
            // Keep this owned directory alive outside the tuple.
            zip.start_file("pack.mcmeta",zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(br#"{"pack":{"pack_format":34,"description":"fixture"}}"#).unwrap();
            std::fs::write(dir.join(format!("resource-{i}.zip")),zip.finish().unwrap().into_inner()).unwrap();
            continue;
        };
        zip.start_file(metadata,zip::write::SimpleFileOptions::default()).unwrap();
        write!(zip,"{{\"id\":\"resource_{i}\",\"name\":\"Resource {i}\",\"version\":\"1\"}}").unwrap();
        std::fs::write(dir.join(name),zip.finish().unwrap().into_inner()).unwrap();
    }
    for run in 0..3 {
        for (case,path) in [("summary","/api/v3/manage/content?view=summary"),("full","/api/v3/manage/content")] {
            let reply=measured(f.address,&token,path,case,run);
            assert_eq!(reply.json()["data"]["items"].as_array().unwrap().len(),400);
        }
        for _ in 0..3 { measured(f.address,&token,"/api/v3/manage/server/status","status_idle",run); }
        let address=f.address;
        std::thread::scope(|scope| {
            let task=scope.spawn(|| measured(address,&token,"/api/v3/manage/content","full_concurrent",run));
            // Barrier-free overlap, reported as such: polling starts 50 ms after submission.
            std::thread::sleep(Duration::from_millis(50));
            for _ in 0..5 { measured(f.address,&token,"/api/v3/manage/server/status","status_loaded",run); }
            for _ in 0..3 { measured(f.address,&token,"/api/v3/manage/operations?limit=1","jobs_loaded",run); }
            assert_eq!(task.join().unwrap().status,200);
        });
    }
    let address=f.address;
    std::thread::scope(|scope| {
        let a=scope.spawn(|| measured(address,&token,"/api/v3/manage/content","full_saturated",0));
        let b=scope.spawn(|| measured(address,&token,"/api/v3/manage/content","full_saturated",1));
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..3 { measured(address,&token,"/api/v3/manage/server/status","status_saturated",0); }
        a.join().unwrap();b.join().unwrap();
    });
    std::fs::rename(mods.join("resource-0.jar"),mods.join("renamed.jar")).unwrap();
    std::fs::remove_file(mods.join("resource-1.jar")).unwrap();
    let reply=measured(f.address,&token,"/api/v3/manage/content","invalidation",0).json();
    let rows=reply["data"]["items"].as_array().unwrap();
    assert_eq!(rows.len(),399);
    assert!(rows.iter().any(|r|r["filename"]=="renamed.jar"));
    assert!(!rows.iter().any(|r|r["filename"]=="resource-1.jar"));
    f.finish();
    let document=std::fs::read(f.temp.path().join("host.toml")).unwrap();
    let config=HostConfiguration::parse_toml(&document,f.temp.path(),Path::new("host.toml")).unwrap();
    let ListenerDescription::Tls(listener)=config.listener() else {panic!("TLS")};
    let listener=listener.on_ephemeral_port();
    let host=Host::open(config,f.clock.clone()).unwrap();
    let transport=Transport::start(host.network_handle(),&listener).unwrap();
    f.address=transport.local_addr();f.host=Some(host);f.transport=Some(transport);
    let token=management_session(&f,&device,&key);
    let reply=measured(f.address,&token,"/api/v3/manage/content","reopen",0);
    assert_eq!(reply.json()["data"]["items"].as_array().unwrap().len(),399);
}

