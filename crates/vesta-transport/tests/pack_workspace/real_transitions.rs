//! Explicit real-provider version transitions; never part of the default suite.
use super::*;

fn selection(case: &str) -> (&'static str, &'static str, [(&'static str, &'static str);2]) {
    match case {
        "rlcraft" => ("curseforge","285109",[("4612979","2.9.3"),("4487646","2.9.2d")]),
        "cobbleverse" => ("modrinth","Jkb29YJU",[("4SKGla61","1.7.42"),("T9GJwPoJ","1.7.41b")]),
        _ => panic!("unsupported explicitly bounded pack family"),
    }
}
fn output() -> std::path::PathBuf {
    let p=std::path::PathBuf::from(std::env::var_os("VESTA_TRANSITION_OUTPUT").expect("explicit owned output"));
    assert!(p.is_dir());p
}
fn save(name: &str, value: &serde_json::Value) {
    std::fs::write(output().join(name),serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn plan_projection(plan: &serde_json::Value) -> serde_json::Value {
    // Confirmation tokens and private configuration never enter evidence files.
    json!({"valid":plan["valid"],"deferred":plan["deferred"],
        "minecraft_version":plan["minecraft_version"],"software":plan["software"],
        "loader_version":plan["loader_version"],"software_switch_required":plan["software_switch_required"],
        "software_switch":plan["software_switch"],"files":plan["files"],
        "removed":plan["removed"],"skipped":plan["skipped"],"warnings":plan["warnings"]})
}

#[test]
#[ignore = "explicit real-provider endpoint preparation for two published versions; no apply/runtime execution"]
fn published_pack_preflight() {
    let case=std::env::var("VESTA_TRANSITION_PACK").unwrap();
    let (provider,project,versions)=selection(&case);
    let f=audit_software_fixture();
    let (device,key)=enroll_management(&f,vesta_control::identity::BuiltinRole::Owner);
    let token=management_session(&f,&device,&key);
    save(&format!("{case}-preflight-ownership.json"),&json!({"root":f.temp.path(),"listener":f.address.to_string(),"kind":"new temporary loopback fixture","no_apply":true,"no_game_launch":true}));
    let mut observations=Vec::new();
    for (id,display) in versions {
        let input=json!({"analyze":false,"source":{"source":"provider","provider":provider,"project_id":project,"version_id":id}});
        let start=Instant::now();
        let r=audit_long_post(&f,&token,"pack/install/plan",None,&input);
        assert_eq!(r.status,200,"public endpoint preflight refused status={} body={}",r.status,String::from_utf8_lossy(&r.body));
        let plan=r.json()["data"].clone();assert_eq!(plan["valid"],true);
        let item=json!({"provider":provider,"project":project,"version":id,"display":display,"route":"POST /api/v3/manage/pack/install/plan","wall_ms":start.elapsed().as_secs_f64()*1000.,"plan":plan_projection(&plan)});
        println!("REAL_TRANSITION_PREFLIGHT provider={provider} project={project} version={id} mc={} loader={} loader_version={} wall_ms={:.3}",plan["minecraft_version"],plan["software"],plan["loader_version"],start.elapsed().as_secs_f64()*1000.);
        observations.push(item);
    }
    save(&format!("{case}-preflight.json"),&json!({"new_execution":true,"apply_count":0,"versions":observations}));
}
