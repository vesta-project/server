use serde::Deserialize;
use vesta_state::audit::{AuditActor, AuditRecord, ChainFields, chain_hash, chain_input};

#[derive(Deserialize)]
struct Corpus {
    vectors: Vec<Vector>,
}

#[derive(Deserialize)]
struct Vector {
    case_id: String,
    previous_case_id: Option<String>,
    prev_hash: String,
    server_id: String,
    principal_id: String,
    device_id: String,
    source: String,
    action: String,
    target: String,
    outcome: String,
    operation_id: Option<String>,
    at_ms: i64,
    metadata_input: Option<Vec<(String, Option<String>)>>,
    metadata_json: String,
    chain_input_json: String,
    sha256: String,
}

#[test]
fn audit_bytes_and_hashes_match_independent_gson_vectors() {
    let corpus: Corpus = serde_json::from_slice(include_bytes!(
        "../../../protocol/audit-v1/fixtures/vectors.json"
    ))
    .unwrap();
    assert_eq!(
        corpus.vectors.len(),
        11,
        "no frozen oracle case may disappear"
    );
    let mut prior = std::collections::HashMap::new();
    for vector in corpus.vectors {
        let actor = AuditActor::new(
            vector.principal_id.clone(),
            vector.device_id.clone(),
            vector.source.clone(),
        )
        .unwrap();
        let record = AuditRecord::new(
            actor,
            vector.action.clone(),
            vector.target.clone(),
            vector.outcome.clone(),
            vector.operation_id.clone(),
            vector.metadata_input.unwrap_or_default(),
        )
        .unwrap();
        let metadata = record.metadata_json();
        assert_eq!(
            metadata, vector.metadata_json,
            "{}: Gson metadata",
            vector.case_id
        );
        let fields = ChainFields {
            prev_hash: &vector.prev_hash,
            server_id: &vector.server_id,
            principal_id: &vector.principal_id,
            device_id: &vector.device_id,
            source: &vector.source,
            action: &vector.action,
            target: &vector.target,
            outcome: &vector.outcome,
            operation_id: vector.operation_id.as_deref(),
            at_ms: vector.at_ms,
            metadata_json: &metadata,
        };
        assert_eq!(
            chain_input(&fields),
            vector.chain_input_json,
            "{}: exact Java byte domain",
            vector.case_id
        );
        assert_eq!(
            chain_hash(&fields),
            vector.sha256,
            "{}: Java SHA-256",
            vector.case_id
        );
        if let Some(previous) = &vector.previous_case_id {
            assert_eq!(
                prior.get(previous),
                Some(&vector.prev_hash),
                "{}: frozen chain relationship",
                vector.case_id
            );
        }
        assert!(
            prior.insert(vector.case_id, vector.sha256).is_none(),
            "duplicate oracle case ID"
        );
    }
}

#[test]
fn metadata_boundaries_count_utf16_and_reject_secret_names_without_echo() {
    let make = |key: &str, value: &str| {
        AuditRecord::new(
            AuditActor::system("test".into()).unwrap(),
            "config.inspect".into(),
            "config:test".into(),
            "ok".into(),
            None,
            [(key.into(), Some(value.into()))],
        )
    };
    assert!(make("description", &"😀".repeat(128)).is_ok());
    let error = match make("description", &"😀".repeat(129)) {
        Ok(_) => panic!("257+ UTF16 units must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "state_invalid_request");
    for key in [
        "PASSword",
        "authentication_token",
        "apiKEY",
        "cookie",
        "authorization",
        "credential",
        "command",
    ] {
        let error = match make(key, "DO_NOT_DISCLOSE") {
            Ok(_) => panic!("secret-bearing metadata key accepted: {key}"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "state_invalid_request");
        assert!(!format!("{error:?} {error}").contains("DO_NOT_DISCLOSE"));
        assert!(!format!("{error:?} {error}").contains(key));
    }
    // Rust whitespace includes NBSP, Java String.isBlank does not. Preserve the
    // persisted Java text contract rather than using Rust's trim/is_whitespace.
    assert!(AuditActor::new("\u{00a0}".into(), "local".into(), "test".into()).is_ok());
    assert!(AuditActor::new("\u{2000}".into(), "local".into(), "test".into()).is_err());
    assert!(AuditActor::new("actor\u{0085}".into(), "local".into(), "test".into()).is_err());
}
