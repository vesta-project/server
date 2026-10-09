use super::*;

#[test]
fn test_valid_file_with_multiple_entries_and_extras() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Player1",
            "level": 1,
            "extra": "ignored"
        },
        {
            "uuid": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "name": "Player2"
        }
    ]"#;

    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    assert_eq!(whitelist.len(), 2);
    assert!(!whitelist.is_empty());
}

#[test]
fn test_duplicate_uuids_collapse() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Player1"
        },
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Player2"
        }
    ]"#;

    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    assert_eq!(whitelist.len(), 1);
}

#[test]
fn test_contains_true() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Player1"
        }
    ]"#;

    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    let uuid = MinecraftUuid::parse("550e8400-e29b-41d4-a716-446655440000").expect("parse uuid");
    assert!(whitelist.contains(uuid));
}

#[test]
fn test_contains_false() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Player1"
        }
    ]"#;

    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    let uuid = MinecraftUuid::parse("6ba7b810-9dad-11d1-80b4-00c04fd430c8").expect("parse uuid");
    assert!(!whitelist.contains(uuid));
}

#[test]
fn test_empty_array() {
    let json = "[]";
    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    assert_eq!(whitelist.len(), 0);
    assert!(whitelist.is_empty());
}

#[test]
fn test_non_array_top_level_object() {
    let json = r#"{"uuid": "550e8400-e29b-41d4-a716-446655440000"}"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_non_array_top_level_string() {
    let json = r#""not an array""#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_element_not_an_object() {
    let json = r#"[
        "not an object"
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_element_is_array() {
    let json = r#"[
        []
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_missing_uuid_field() {
    let json = r#"[
        {
            "name": "Player1"
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_uuid_field_not_string() {
    let json = r#"[
        {
            "uuid": 123,
            "name": "Player1"
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_uuid_field_null() {
    let json = r#"[
        {
            "uuid": null,
            "name": "Player1"
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_invalid_uuid_format() {
    let json = r#"[
        {
            "uuid": "not-a-valid-uuid",
            "name": "Player1"
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_nil_uuid() {
    let json = r#"[
        {
            "uuid": "00000000-0000-0000-0000-000000000000",
            "name": "Player1"
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_non_canonical_uuid_wrong_format() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716",
            "name": "Player1"
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_non_string_name_boolean() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": true
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_non_string_name_number() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": 42
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_non_string_name_object() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": {}
        }
    ]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_invalid_utf8() {
    let bytes = vec![0xFF, 0xFE, 0xFD];
    let result = Whitelist::parse(&bytes);
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_invalid_json_syntax() {
    let json = r#"[{"uuid": "550e8400-e29b-41d4-a716-446655440000",]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_oversized_bytes() {
    let bytes = vec![b'x'; MAX_WHITELIST_BYTES + 1];
    let result = Whitelist::parse(&bytes);
    assert_eq!(result, Err(WhitelistError::TooLarge));
}

#[test]
fn test_max_bytes_boundary() {
    let bytes = vec![b'x'; MAX_WHITELIST_BYTES];
    let result = Whitelist::parse(&bytes);
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_too_many_entries() {
    let mut json = String::from("[");
    for i in 0..=MAX_WHITELIST_ENTRIES {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"uuid": "550e8400-e29b-41d4-a716-{:012x}"}}"#,
            i
        ));
    }
    json.push(']');

    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::TooLarge));
}

#[test]
fn test_max_entries_boundary() {
    let mut json = String::from("[");
    for i in 0..MAX_WHITELIST_ENTRIES {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"uuid": "550e8400-e29b-41d4-a716-{:012x}"}}"#,
            i + 1
        ));
    }
    json.push(']');

    let result = Whitelist::parse(json.as_bytes());
    assert!(result.is_ok());
    let whitelist = result.unwrap();
    assert_eq!(whitelist.len(), MAX_WHITELIST_ENTRIES);
}

#[test]
fn test_debug_output_no_uuids() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Player1"
        },
        {
            "uuid": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "name": "Player2"
        }
    ]"#;

    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    let debug_output = format!("{:?}", whitelist);

    assert!(debug_output.contains("Whitelist"));
    assert!(debug_output.contains("entries: 2"));
    assert!(!debug_output.contains("550e8400"));
    assert!(!debug_output.contains("6ba7b810"));
}

#[test]
fn test_error_code_too_large() {
    let error = WhitelistError::TooLarge;
    assert_eq!(error.code(), "whitelist_too_large");
}

#[test]
fn test_error_code_malformed() {
    let error = WhitelistError::Malformed;
    assert_eq!(error.code(), "whitelist_malformed");
}

#[test]
fn test_error_display_format() {
    let error = WhitelistError::Malformed;
    assert_eq!(error.to_string(), "whitelist_malformed");
    assert_eq!(WhitelistError::TooLarge.to_string(), "whitelist_too_large");
}

#[test]
fn test_default_whitelist_empty() {
    let whitelist = Whitelist::default();
    assert!(whitelist.is_empty());
    assert_eq!(whitelist.len(), 0);
}

#[test]
fn test_whitelist_clone_equality() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000"
        }
    ]"#;

    let whitelist1 = Whitelist::parse(json.as_bytes()).expect("parse failed");
    let whitelist2 = whitelist1.clone();

    assert_eq!(whitelist1, whitelist2);
}

#[test]
fn test_leading_utf8_bom() {
    let mut json = Vec::new();
    json.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    json.extend_from_slice(b"[]");

    let result = Whitelist::parse(&json);
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_uuid_case_insensitive_parsing() {
    let json_lower = r#"[{"uuid": "550e8400-e29b-41d4-a716-446655440000"}]"#;
    let json_upper = r#"[{"uuid": "550E8400-E29B-41D4-A716-446655440000"}]"#;

    let wl_lower = Whitelist::parse(json_lower.as_bytes()).expect("parse failed");
    let wl_upper = Whitelist::parse(json_upper.as_bytes()).expect("parse failed");

    let uuid_lower =
        MinecraftUuid::parse("550e8400-e29b-41d4-a716-446655440000").expect("parse uuid");
    let uuid_upper =
        MinecraftUuid::parse("550E8400-E29B-41D4-A716-446655440000").expect("parse uuid");

    assert!(wl_lower.contains(uuid_lower));
    assert!(wl_upper.contains(uuid_upper));
}

#[test]
fn test_optional_name_field_missing() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000"
        }
    ]"#;

    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    assert_eq!(whitelist.len(), 1);
}

#[test]
fn test_name_field_empty_string() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": ""
        }
    ]"#;

    let whitelist = Whitelist::parse(json.as_bytes()).expect("parse failed");
    assert_eq!(whitelist.len(), 1);
}

#[test]
fn test_multiple_errors_fail_closed() {
    let json = r#"[
        {
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Player1"
        },
        {
            "uuid": "invalid-uuid",
            "name": "Player2"
        }
    ]"#;

    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}

#[test]
fn test_empty_object_missing_uuid() {
    let json = r#"[{}]"#;
    let result = Whitelist::parse(json.as_bytes());
    assert_eq!(result, Err(WhitelistError::Malformed));
}
