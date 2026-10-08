use std::error::Error;
use vesta_protocol::discovery::*;

const SERVER_ID: &str = "12345678-1234-5678-9abc-123456789abc";

fn manifest(version: &str, identity: &str, features: &str) -> String {
    format!(
        r#"{{"protocol":"{PROTOCOL}","version":{version},"server_id":{identity},"features":{features}}}"#
    )
}

fn error(input: &str, expected: ErrorCode) {
    let actual = decode_manifest(input.as_bytes()).unwrap_err();
    assert_eq!(actual.code(), expected);
    assert_eq!(
        actual.to_string(),
        format!("Server feature protocol failed: {}", expected.wire())
    );
    assert!(actual.source().is_none());
}

#[test]
fn deferred_validation_precedence_matches_java_not_serde_scalar_rules() {
    // Java accepts a lone surrogate string until UUID validation. Its unknown
    // selected-version error takes precedence; malformed nested features do not.
    error(
        &manifest("2", r#""\ud800""#, "[]"),
        ErrorCode::UnsupportedProtocol,
    );
    error(
        &manifest("1", r#""\ud800""#, "[]"),
        ErrorCode::InvalidMessage,
    );
    error(
        &manifest(
            "2",
            r#""\ud800""#,
            r#"[{"id":"bad..id","support":"supported","reason":null}]"#,
        ),
        ErrorCode::InvalidMessage,
    );
    let unsupported = manifest("2", r#""\ud800""#, "[]");
    error(&format!("{unsupported} []"), ErrorCode::InvalidMessage);
    // String limits apply while reading and win over the deferred version check.
    error(
        &manifest("2", &format!("\"{}\"", "😀".repeat(33)), "[]"),
        ErrorCode::InvalidMessage,
    );
}

#[test]
fn every_public_entry_point_revalidates_invalid_request_literals() {
    for request in [
        DiscoveryRequest { versions: vec![] },
        DiscoveryRequest {
            versions: vec![1, 0],
        },
        DiscoveryRequest {
            versions: vec![1, 1],
        },
        DiscoveryRequest {
            versions: (1..=9).collect(),
        },
    ] {
        assert_eq!(
            negotiate(&request).unwrap_err().code(),
            ErrorCode::InvalidMessage
        );
        assert_eq!(
            encode_request(&request).unwrap_err().code(),
            ErrorCode::InvalidMessage
        );
        let valid = Manifest::new(VERSION, SERVER_ID.into(), vec![]).unwrap();
        assert_eq!(
            check_response(&request, SERVER_ID, &valid)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidMessage
        );
    }
}

#[test]
fn unexpected_deep_containers_and_raw_controls_fail_without_recursion_or_echo() {
    let nesting = format!("{}0{}", "[".repeat(4_000), "]".repeat(4_000));
    error(
        &manifest("1", &format!("\"{SERVER_ID}\""), &nesting),
        ErrorCode::InvalidMessage,
    );
    for control in 0u8..=31 {
        let malicious = manifest(
            "2",
            &format!("\"before{}after\"", char::from(control)),
            "[]",
        );
        error(&malicious, ErrorCode::InvalidMessage);
    }
    // Escaped JSON control is syntactically valid and remains deferred until UUID validation.
    error(
        &manifest("2", r#""before\u0000after""#, "[]"),
        ErrorCode::UnsupportedProtocol,
    );
}

#[test]
fn oversized_malformed_utf8_reports_size_before_parser_failure() {
    let bytes = vec![0xff; MAX_BYTES + 1];
    assert_eq!(
        decode_request(&bytes).unwrap_err().code(),
        ErrorCode::MessageTooLarge
    );
    assert_eq!(
        decode_manifest(&bytes).unwrap_err().code(),
        ErrorCode::MessageTooLarge
    );
    assert_eq!(
        decode_failure(&bytes).unwrap_err().code(),
        ErrorCode::MessageTooLarge
    );
}
