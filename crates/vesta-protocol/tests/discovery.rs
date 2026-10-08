use vesta_protocol::discovery::{
    DiscoveryRequest, ErrorCode, Failure, Feature, MAX_BYTES, Manifest, PROTOCOL, ProtocolError,
    Support, VERSION, check_response, decode_failure, decode_manifest, decode_request,
    encode_failure, encode_manifest, encode_request, negotiate,
};

const SERVER_ID: &str = "12345678-1234-5678-9abc-123456789abc";
const OTHER_SERVER_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

fn assert_code<T>(
    result: Result<T, ProtocolError>,
    expected: ErrorCode,
    case: &str,
) -> ProtocolError {
    match result {
        Ok(_) => panic!("{case}: expected protocol error {expected:?}"),
        Err(error) => {
            assert_eq!(error.code(), expected, "{case}");
            assert_eq!(
                error.to_string(),
                format!("Server feature protocol failed: {}", expected.wire()),
                "{case}: fixed error message"
            );
            assert!(
                std::error::Error::source(&error).is_none(),
                "{case}: error must not retain a cause"
            );
            error
        }
    }
}

fn feature_ids(manifest: &Manifest) -> Vec<&str> {
    manifest
        .features
        .iter()
        .map(|feature| feature.id.as_str())
        .collect()
}

fn roundtrip(kind: &str, input: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    match kind {
        "request" => encode_request(&decode_request(input)?),
        "manifest" => encode_manifest(&decode_manifest(input)?),
        "failure" => encode_failure(&decode_failure(input)?),
        _ => panic!("unknown trusted fixture kind {kind:?}"),
    }
}

fn corpus_error_code(wire: &str) -> ErrorCode {
    match wire {
        "invalid_message" => ErrorCode::InvalidMessage,
        "message_too_large" => ErrorCode::MessageTooLarge,
        "unsupported_protocol" => ErrorCode::UnsupportedProtocol,
        "unexpected_response" => ErrorCode::UnexpectedResponse,
        _ => panic!("unknown trusted fixture error code {wire:?}"),
    }
}

#[test]
fn shared_corpus_exercises_every_peer_case() {
    let corpus: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../protocol/v1/fixtures/corpus.json"))
            .expect("shared protocol corpus must be valid JSON");
    let rows = corpus.as_array().expect("shared corpus must be an array");

    for row in rows {
        let name = row["name"].as_str().expect("corpus case name");
        let kind = row["kind"].as_str().expect("corpus message kind");
        let mut bytes = if let Some(hex) = row.get("hex").and_then(serde_json::Value::as_str) {
            decode_hex(hex, name)
        } else {
            row["json"]
                .as_str()
                .expect("corpus JSON text")
                .as_bytes()
                .to_vec()
        };

        if let Some(pad_to) = row.get("padTo").and_then(serde_json::Value::as_u64) {
            let pad_to = usize::try_from(pad_to).expect("padTo fits usize");
            assert!(
                bytes.len() <= pad_to,
                "{name}: padding cannot truncate input"
            );
            let mut padded = vec![b' '; pad_to];
            padded[pad_to - bytes.len()..].copy_from_slice(&bytes);
            bytes = padded;
        }

        if let Some(expected_error) = row.get("error").and_then(serde_json::Value::as_str) {
            let error = assert_code(
                roundtrip(kind, &bytes),
                corpus_error_code(expected_error),
                name,
            );
            assert_eq!(
                error.code().wire(),
                expected_error,
                "{name}: fixed wire code"
            );
            // Protocol failures carry only the fixed code; diagnostics must not echo peer bytes.
            let diagnostic = format!("{error:?}");
            assert!(
                !diagnostic.contains("secret"),
                "{name}: leaked diagnostic input"
            );
            assert!(
                !diagnostic.contains("password"),
                "{name}: leaked diagnostic key"
            );
            continue;
        }

        let canonical = row["canonical"].as_str().expect("canonical fixture output");
        let encoded = roundtrip(kind, &bytes).unwrap_or_else(|error| {
            panic!(
                "{name}: valid corpus message failed with {:?}",
                error.code()
            )
        });
        assert_eq!(
            encoded.as_slice(),
            canonical.as_bytes(),
            "{name}: canonical bytes"
        );
        let second = roundtrip(kind, &encoded).unwrap_or_else(|error| {
            panic!("{name}: canonical output failed with {:?}", error.code())
        });
        assert_eq!(
            second.as_slice(),
            canonical.as_bytes(),
            "{name}: roundtrip stability"
        );

        if let Some(expected) = row.get("negotiation").and_then(serde_json::Value::as_u64) {
            let request = decode_request(&bytes)
                .unwrap_or_else(|error| panic!("{name}: decode request: {:?}", error.code()));
            let selected = negotiate(&request).unwrap_or_else(|error| {
                panic!("{name}: negotiation failed with {:?}", error.code())
            });
            let expected = u16::try_from(expected)
                .unwrap_or_else(|_| panic!("{name}: expected version does not fit u16"));
            assert_eq!(selected, expected, "{name}: negotiated version");
        }
        if let Some(expected) = row
            .get("negotiationError")
            .and_then(serde_json::Value::as_str)
        {
            let request = decode_request(&bytes)
                .unwrap_or_else(|error| panic!("{name}: decode request: {:?}", error.code()));
            assert_code(negotiate(&request), corpus_error_code(expected), name);
        }
    }
}

fn decode_hex(hex: &str, case: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "{case}: fixture hex has complete bytes");
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => panic!("{case}: fixture hex contains a non-hex digit"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

#[test]
fn independent_canonical_golden_vectors_match_fixture_bytes() {
    let request_fixture = include_bytes!("../../../protocol/v1/fixtures/request.json");
    let request = decode_request(request_fixture).expect("request golden vector decodes");
    assert_eq!(request.versions, vec![2, 1]);
    assert_eq!(
        encode_request(&request).unwrap().as_slice(),
        request_fixture
    );

    let manifest_fixture = include_bytes!("../../../protocol/v1/fixtures/manifest.json");
    let manifest = decode_manifest(manifest_fixture).expect("manifest golden vector decodes");
    assert_eq!(manifest.version, VERSION);
    assert_eq!(manifest.server_id, SERVER_ID);
    assert_eq!(
        feature_ids(&manifest),
        vec![
            "game.force_kill",
            "game.readiness",
            "game.start",
            "game.stop",
            "supervisor.continuity",
            "tasks.replay",
        ]
    );
    assert_eq!(
        encode_manifest(&manifest).unwrap().as_slice(),
        manifest_fixture
    );

    let failure_fixture = include_bytes!("../../../protocol/v1/fixtures/failure.json");
    let failure = decode_failure(failure_fixture).expect("failure golden vector decodes");
    assert_eq!(failure.error, ErrorCode::UnsupportedProtocol);
    assert_eq!(
        encode_failure(&failure).unwrap().as_slice(),
        failure_fixture
    );
}

#[test]
fn request_construction_sorts_and_negotiation_never_falls_back() {
    let request = DiscoveryRequest::new(vec![1, 65_535, 2]).unwrap();
    assert_eq!(request.versions, vec![65_535, 2, 1]);
    assert_eq!(negotiate(&request).unwrap(), VERSION);
    assert_eq!(DiscoveryRequest::new(vec![2, 1, 65_535]).unwrap(), request);
    assert_code(
        negotiate(&DiscoveryRequest::new(vec![2]).unwrap()),
        ErrorCode::UnsupportedProtocol,
        "no offered supported protocol version; no fallback",
    );
}

#[test]
fn manifest_is_sorted_and_support_queries_are_exact_and_inert() {
    let manifest = Manifest::new(
        VERSION,
        SERVER_ID.to_owned(),
        vec![
            Feature::new("game.start".to_owned(), Support::Supported, None).unwrap(),
            Feature::new("game.force_kill".to_owned(), Support::Supported, None).unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(
        feature_ids(&manifest),
        vec!["game.force_kill", "game.start"]
    );
    assert!(manifest.supports("game.start"));
    assert!(!manifest.supports("game.stop"));

    let same_content = Manifest::new(
        VERSION,
        SERVER_ID.to_owned(),
        vec![
            Feature::new("game.force_kill".to_owned(), Support::Supported, None).unwrap(),
            Feature::new("game.start".to_owned(), Support::Supported, None).unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(manifest, same_content);
    assert_eq!(manifest.version, same_content.version);
    assert_eq!(manifest.server_id, same_content.server_id);
    assert_eq!(feature_ids(&manifest), feature_ids(&same_content));

    let unfamiliar = Manifest::new(
        VERSION,
        SERVER_ID.to_owned(),
        [
            "lifecycle.start",
            "security.manage",
            "game.start.extra",
            "constructor",
        ]
        .into_iter()
        .map(|id| Feature::new(id.to_owned(), Support::Supported, None).unwrap())
        .collect(),
    )
    .unwrap();
    for known in [
        "game.start",
        "game.stop",
        "game.force_kill",
        "game.readiness",
        "tasks.replay",
        "supervisor.continuity",
    ] {
        assert!(
            !unfamiliar.supports(known),
            "unrecognized exact feature set: {known}"
        );
    }
    assert_eq!(unfamiliar.features.len(), 4);
    for unknown in [
        "lifecycle.start",
        "security.manage",
        "game.start.extra",
        "constructor",
    ] {
        assert!(
            !unfamiliar.supports(unknown),
            "unknown, permission-like, or prefix ID must stay inert: {unknown}"
        );
    }

    let unknown_reason = Feature::new(
        "game.stop".to_owned(),
        Support::Unsupported,
        Some("future_reason".to_owned()),
    )
    .unwrap();
    assert_eq!(unknown_reason.reason.as_deref(), Some("future_reason"));
    let unsupported = Manifest::new(VERSION, SERVER_ID.to_owned(), vec![unknown_reason]).unwrap();
    assert!(!unsupported.supports("game.stop"));

    let secret_feature = Feature::new(
        "secret_value".to_owned(),
        Support::Unsupported,
        Some("secret_reason".to_owned()),
    )
    .unwrap();
    let secret_manifest =
        Manifest::new(VERSION, SERVER_ID.to_owned(), vec![secret_feature.clone()]).unwrap();
    assert!(!format!("{secret_feature:?}").contains("secret"));
    assert!(!format!("{secret_manifest:?}").contains("secret"));
}

#[test]
fn supports_fails_closed_for_invalid_literals_and_queries() {
    let supported = Feature {
        id: "game.start".to_owned(),
        support: Support::Supported,
        reason: None,
    };
    let valid = Manifest {
        version: VERSION,
        server_id: SERVER_ID.to_owned(),
        features: vec![supported.clone()],
    };
    assert!(valid.supports("game.start"));
    for invalid_query in [
        "",
        "game.*",
        "game..start",
        "game.start\n",
        "future.feature",
    ] {
        assert!(
            !valid.supports(invalid_query),
            "invalid or unknown query must be false: {invalid_query:?}"
        );
    }

    let invalid_feature_id = Feature {
        id: "game.*".to_owned(),
        support: Support::Supported,
        reason: None,
    };
    let invalid_reason = Feature {
        id: "game.stop".to_owned(),
        support: Support::Unsupported,
        reason: None,
    };
    let invalid_manifests = vec![
        (
            "zero selected version",
            Manifest {
                version: 0,
                server_id: SERVER_ID.to_owned(),
                features: vec![supported.clone()],
            },
        ),
        (
            "unsupported selected version",
            Manifest {
                version: 2,
                server_id: SERVER_ID.to_owned(),
                features: vec![supported.clone()],
            },
        ),
        (
            "invalid server identity",
            Manifest {
                version: VERSION,
                server_id: "secret".to_owned(),
                features: vec![supported.clone()],
            },
        ),
        (
            "invalid public feature ID",
            Manifest {
                version: VERSION,
                server_id: SERVER_ID.to_owned(),
                features: vec![supported.clone(), invalid_feature_id],
            },
        ),
        (
            "invalid public reason/support combination",
            Manifest {
                version: VERSION,
                server_id: SERVER_ID.to_owned(),
                features: vec![supported.clone(), invalid_reason],
            },
        ),
        (
            "duplicate public feature IDs",
            Manifest {
                version: VERSION,
                server_id: SERVER_ID.to_owned(),
                features: vec![supported.clone(), supported.clone()],
            },
        ),
        (
            "too many public features",
            Manifest {
                version: VERSION,
                server_id: SERVER_ID.to_owned(),
                features: (0..65)
                    .map(|index| Feature {
                        id: format!("x_{index}"),
                        support: Support::Supported,
                        reason: None,
                    })
                    .collect(),
            },
        ),
    ];
    for (case, manifest) in invalid_manifests {
        assert!(!manifest.supports("game.start"), "{case}");
    }
}

#[test]
fn response_binding_checks_version_and_identity_not_feature_authority() {
    let empty = Manifest::new(VERSION, SERVER_ID.to_owned(), vec![]).unwrap();
    let offers_supported = DiscoveryRequest::new(vec![2, 1]).unwrap();
    assert!(check_response(&offers_supported, SERVER_ID, &empty).is_ok());
    assert_code(
        check_response(&DiscoveryRequest::new(vec![2]).unwrap(), SERVER_ID, &empty),
        ErrorCode::UnexpectedResponse,
        "response version was not offered",
    );
    assert_code(
        check_response(
            &DiscoveryRequest::new(vec![1]).unwrap(),
            OTHER_SERVER_ID,
            &empty,
        ),
        ErrorCode::UnexpectedResponse,
        "response is bound to expected server identity",
    );
    assert_code(
        check_response(&DiscoveryRequest::new(vec![1]).unwrap(), "secret", &empty),
        ErrorCode::InvalidMessage,
        "malformed expected server identity",
    );

    // Features describe capability only; neither an empty set nor inert unknown IDs grant authority.
    let no_authority_claim = Manifest::new(VERSION, SERVER_ID.to_owned(), vec![]).unwrap();
    assert!(check_response(&offers_supported, SERVER_ID, &no_authority_claim).is_ok());
    let inert = Manifest::new(
        VERSION,
        SERVER_ID.to_owned(),
        vec![Feature::new("security.manage".to_owned(), Support::Supported, None).unwrap()],
    )
    .unwrap();
    assert!(!inert.supports("security.manage"));
    assert!(check_response(&offers_supported, SERVER_ID, &inert).is_ok());
}

#[test]
fn exact_programmatic_maxima_are_accepted_without_truncation() {
    let versions = DiscoveryRequest::new(vec![1, 2, 3, 4, 5, 6, 7, 65_535]).unwrap();
    assert_eq!(versions.versions.len(), 8);
    assert!(encode_request(&versions).unwrap().len() <= MAX_BYTES);

    let features: Vec<Feature> = (0..64)
        .map(|index| {
            Feature::new(
                format!("x{index:02}{}", "a".repeat(61)),
                Support::Unsupported,
                Some("r".repeat(64)),
            )
            .unwrap()
        })
        .collect();
    let manifest = Manifest::new(VERSION, SERVER_ID.to_owned(), features).unwrap();
    assert_eq!(manifest.features.len(), 64);
    let encoded = encode_manifest(&manifest).unwrap();
    assert!(encoded.len() <= MAX_BYTES);
    assert_eq!(decode_manifest(&encoded).unwrap().features.len(), 64);
}

#[test]
fn unsupported_entries_are_not_truncated_and_encoded_vectors_are_fresh() {
    let manifest = Manifest::new(
        VERSION,
        SERVER_ID.to_owned(),
        vec![
            Feature::new(
                "game.start".to_owned(),
                Support::Unsupported,
                Some("not_configured".to_owned()),
            )
            .unwrap(),
            Feature::new(
                "game.stop".to_owned(),
                Support::Unsupported,
                Some("not_configured".to_owned()),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let mut first = encode_manifest(&manifest).unwrap();
    assert!(first.len() <= MAX_BYTES);
    let decoded = decode_manifest(&first).unwrap();
    assert_eq!(feature_ids(&decoded), vec!["game.start", "game.stop"]);
    assert!(decoded.features.iter().all(|feature| {
        feature.support == Support::Unsupported
            && feature.reason.as_deref() == Some("not_configured")
    }));

    first[0] = 0;
    let second = encode_manifest(&manifest).unwrap();
    assert_eq!(second[0], b'{');
    let decoded_again = decode_manifest(&second).unwrap();
    assert_eq!(feature_ids(&decoded_again), vec!["game.start", "game.stop"]);
    assert!(decoded_again.features.iter().all(|feature| {
        feature.support == Support::Unsupported
            && feature.reason.as_deref() == Some("not_configured")
    }));
}

#[test]
fn every_fixed_error_code_roundtrips() {
    for error in [
        ErrorCode::InvalidMessage,
        ErrorCode::MessageTooLarge,
        ErrorCode::UnsupportedProtocol,
        ErrorCode::UnexpectedResponse,
    ] {
        let encoded = encode_failure(&Failure { error }).unwrap();
        assert_eq!(decode_failure(&encoded).unwrap().error, error, "{error:?}");
    }
}

#[test]
fn constructors_reject_invalid_values_and_preserve_error_categories() {
    for (name, versions) in [
        ("empty version list", vec![]),
        ("zero version", vec![0]),
        ("duplicate versions", vec![1, 1]),
        ("too many versions", vec![1, 2, 3, 4, 5, 6, 7, 8, 9]),
    ] {
        assert_code(
            DiscoveryRequest::new(versions),
            ErrorCode::InvalidMessage,
            name,
        );
    }
    assert_code(
        Manifest::new(0, SERVER_ID.to_owned(), vec![]),
        ErrorCode::InvalidMessage,
        "zero selected version",
    );
    assert_code(
        Manifest::new(2, SERVER_ID.to_owned(), vec![]),
        ErrorCode::UnsupportedProtocol,
        "unknown selected protocol version",
    );

    for id in [
        "",
        "12345678-1234-5678-9ABC-123456789ABC",
        "secret",
        "12345678-1234-5678-9abc-123456789abc\n",
    ] {
        assert_code(
            Manifest::new(VERSION, id.to_owned(), vec![]),
            ErrorCode::InvalidMessage,
            "invalid server identity",
        );
    }
    for id in ["", "game.*", "game..start", "game.start\n", &"a".repeat(65)] {
        assert_code(
            Feature::new(id.to_owned(), Support::Supported, None),
            ErrorCode::InvalidMessage,
            "invalid feature ID",
        );
    }
    for reason in [
        "",
        "BAD",
        "bad.reason",
        "bad reason",
        "reason\n",
        &"r".repeat(65),
    ] {
        assert_code(
            Feature::new(
                "game.stop".to_owned(),
                Support::Unsupported,
                Some(reason.to_owned()),
            ),
            ErrorCode::InvalidMessage,
            "invalid unsupported reason",
        );
    }
    assert_code(
        Feature::new(
            "game.start".to_owned(),
            Support::Supported,
            Some("secret".to_owned()),
        ),
        ErrorCode::InvalidMessage,
        "supported feature must have null reason",
    );
    assert_code(
        Feature::new("game.start".to_owned(), Support::Unsupported, None),
        ErrorCode::InvalidMessage,
        "unsupported feature requires reason",
    );

    let too_many_features: Vec<Feature> = (0..65)
        .map(|index| Feature::new(format!("x_{index}"), Support::Supported, None).unwrap())
        .collect();
    assert_code(
        Manifest::new(VERSION, SERVER_ID.to_owned(), too_many_features),
        ErrorCode::InvalidMessage,
        "too many features",
    );
    let duplicate_features = vec![
        Feature::new("game.start".to_owned(), Support::Supported, None).unwrap(),
        Feature::new(
            "game.start".to_owned(),
            Support::Unsupported,
            Some("future_reason".to_owned()),
        )
        .unwrap(),
    ];
    assert_code(
        Manifest::new(VERSION, SERVER_ID.to_owned(), duplicate_features),
        ErrorCode::InvalidMessage,
        "duplicate conflicting feature IDs",
    );
}

#[test]
fn size_limit_counts_complete_wire_bytes_before_parsing() {
    let valid = encode_request(&DiscoveryRequest::new(vec![1]).unwrap()).unwrap();
    let mut exact = vec![b' '; MAX_BYTES];
    exact[MAX_BYTES - valid.len()..].copy_from_slice(&valid);
    assert_eq!(decode_request(&exact).unwrap().versions, vec![1]);
    let over = vec![b' '; MAX_BYTES + 1];
    assert_code(
        decode_request(&over),
        ErrorCode::MessageTooLarge,
        "one byte over inclusive cap",
    );

    let multibyte = format!("{}{}", " ".repeat(16_000), "é".repeat(200));
    assert!(
        multibyte.chars().count() < MAX_BYTES,
        "character count must remain below the byte cap"
    );
    assert!(multibyte.len() > MAX_BYTES);
    assert_code(
        decode_request(multibyte.as_bytes()),
        ErrorCode::MessageTooLarge,
        "multibyte UTF-8 exceeds byte cap",
    );
}

#[test]
fn trailing_malformed_content_wins_over_unsupported_manifest_version() {
    let input = format!(
        "{{\"protocol\":\"{PROTOCOL}\",\"version\":2,\"server_id\":\"{SERVER_ID}\",\"features\":[]}} {{}}"
    );
    assert_code(
        decode_manifest(input.as_bytes()),
        ErrorCode::InvalidMessage,
        "trailing content is rejected before unsupported-version handling",
    );
}

#[test]
fn codecs_reject_invalid_utf8_and_never_echo_arbitrary_input() {
    let invalid_utf8 = [0xc0, 0xaf];
    assert_code(
        decode_request(&invalid_utf8),
        ErrorCode::InvalidMessage,
        "malformed UTF-8",
    );
    let malicious =
        format!("{{\"protocol\":\"{PROTOCOL}\",\"versions\":[1],\"password\":\"SUPER_SECRET\"}}");
    let error = assert_code(
        decode_request(malicious.as_bytes()),
        ErrorCode::InvalidMessage,
        "unknown secret-bearing request member",
    );
    let diagnostic = format!("{error:?}");
    assert!(!diagnostic.contains("SUPER_SECRET"));
    assert!(!diagnostic.contains("password"));
    let safe_failure = encode_failure(&Failure {
        error: error.code(),
    })
    .unwrap();
    assert!(!String::from_utf8_lossy(&safe_failure).contains("SUPER_SECRET"));
    assert_eq!(
        decode_failure(&safe_failure).unwrap().error,
        ErrorCode::InvalidMessage
    );
}
