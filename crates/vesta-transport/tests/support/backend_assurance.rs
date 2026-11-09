//! Bounded local assurance: owned fixture roots, loopback TLS and no provider IO.
use super::*;
use vesta_control::identity::BuiltinRole;

fn roles(f: &Fixture, token: &str) -> serde_json::Value {
    let response=f.request("GET","/api/v3/manage/roles",b"",Some(token));
    assert_eq!(response.status,200);response.json()["data"].clone()
}

fn reject_sensitive(f: &Fixture, token: &str, other_token: &str) {
    for path in ["roles","users","devices","software/jobs"] {
        let response=f.request("GET",&format!("/api/v3/manage/{path}"),b"",Some(token));
        management_reply_error(&response,401,"management_session_invalid");
        let body=String::from_utf8_lossy(&response.body);assert!(!body.contains(token));assert!(!body.contains(other_token));
    }
    let response=management_mutation(f,token,"roles","assurance-foreign-role",&serde_json::json!({"name":"Must not exist","rank":1,"capabilities":[]}));
    management_reply_error(&response,401,"management_session_invalid");assert!(!String::from_utf8_lossy(&response.body).contains(token));
}

#[test]
fn assurance_cross_server_tokens_nonce_replay_and_revocation_have_no_role_effects() {
    use base64::{Engine,engine::general_purpose::URL_SAFE_NO_PAD};
    let a=Fixture::new("minecraft",8,4096,3);let b=Fixture::new("minecraft",8,4096,3);
    assert_ne!(a.host().database().server_id(),b.host().database().server_id());
    assert!(a.address.ip().is_loopback()&&b.address.ip().is_loopback());
    let (device_a,key_a)=enroll_management(&a,BuiltinRole::Owner);let token_a=management_session(&a,&device_a,&key_a);
    let (device_b,key_b)=enroll_management(&b,BuiltinRole::Owner);let token_b=management_session(&b,&device_b,&key_b);
    let before_a=roles(&a,&token_a);let before_b=roles(&b,&token_b);
    reject_sensitive(&a,&token_b,&token_a);reject_sensitive(&b,&token_a,&token_b);
    assert_eq!(roles(&a,&token_a),before_a);assert_eq!(roles(&b,&token_b),before_b);
    let principal_b:String=b.host().database().read(|c|c.query_row("SELECT principal_id FROM devices WHERE id=?",[&device_b],|r|r.get(0)).map_err(Into::into)).unwrap();
    management_reply_error(&a.request("GET",&format!("/api/v3/manage/users/{principal_b}"),b"",Some(&token_a)),404,"resource_not_found");
    let challenge=a.request("POST","/api/v3/auth/session/challenge",&serde_json::to_vec(&serde_json::json!({"device_id":device_a})).unwrap(),None);assert_eq!(challenge.status,200);
    let c=challenge.json();let signature=URL_SAFE_NO_PAD.encode(key_a.sign(&vesta_control::identity::management::session_message(c["server_id"].as_str().unwrap(),c["nonce"].as_str().unwrap())).as_ref());
    let proof=serde_json::to_vec(&serde_json::json!({"device_id":device_a,"nonce":c["nonce"],"signature":signature})).unwrap();
    b.request("POST","/api/v3/auth/session",&proof,None).error(401,"management_session_invalid");
    assert_eq!(a.request("POST","/api/v3/auth/session",&proof,None).status,200);
    a.request("POST","/api/v3/auth/session",&proof,None).error(401,"management_session_invalid");
    a.host().management().revoke_local(&device_a).unwrap();reject_sensitive(&a,&token_a,&token_b);
    // Read the durable role state without using the intentionally revoked principal.
    let names:Vec<String>=a.host().database().read(|c|{let mut q=c.prepare("SELECT name FROM roles")?;Ok(q.query_map([],|r|r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?)}).unwrap();
    assert!(!names.iter().any(|name|name=="Must not exist"));assert_eq!(roles(&b,&token_b),before_b);
}

#[test]
fn assurance_account_unlink_invalidates_retained_session_and_new_challenge() {
    let f=Fixture::new("minecraft",8,4096,3);let (device,key)=enroll_management(&f,BuiltinRole::Owner);let token=management_session(&f,&device,&key);
    let before=roles(&f,&token);assert!(!before.is_null());
    f.host().database().write(|c|{c.execute("DELETE FROM user_minecraft_accounts WHERE principal_id=(SELECT principal_id FROM devices WHERE id=?)",[&device])?;Ok(())}).unwrap();
    reject_sensitive(&f,&token,"not-a-credential");
    f.request("POST","/api/v3/auth/session/challenge",&serde_json::to_vec(&serde_json::json!({"device_id":device})).unwrap(),None).error(401,"management_session_invalid");
    let count:i64=f.host().database().read(|c|c.query_row("SELECT count(*) FROM roles WHERE name='Must not exist'",[],|r|r.get(0)).map_err(Into::into)).unwrap();assert_eq!(count,0);
}
