use super::*;

const UUID_A: &str = "550e8400-e29b-41d4-a716-446655440000";
const UUID_B: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";

fn props(text: &str) -> Result<ServerProperties, AdmissionDataError> {
    ServerProperties::parse(text.as_bytes())
}

fn ban_uuid(text: &str) -> MinecraftUuid {
    MinecraftUuid::parse(text).expect("parse uuid")
}

#[test]
fn server_properties_defaults_when_keys_absent() {
    let parsed = props("motd=hello\nmax-players=20\n").expect("parse");
    assert!(!parsed.whitelist_enabled());
    assert!(!parsed.whitelist_enforced());
    assert!(parsed.online_mode());
}

#[test]
fn server_properties_parses_booleans_case_insensitively() {
    let parsed =
        props("white-list=TRUE\nenforce-whitelist=True\nonline-mode=false\n").expect("parse");
    assert!(parsed.whitelist_enabled());
    assert!(parsed.whitelist_enforced());
    assert!(!parsed.online_mode());

    let parsed =
        props("white-list=false\nenforce-whitelist=FALSE\nonline-mode=tRuE\n").expect("parse");
    assert!(!parsed.whitelist_enabled());
    assert!(!parsed.whitelist_enforced());
    assert!(parsed.online_mode());
}

#[test]
fn server_properties_ignores_comments_and_blank_lines() {
    let text = "# comment\n! bang comment\n\n   # indented comment\n   \nwhite-list=true\n";
    let parsed = props(text).expect("parse");
    assert!(parsed.whitelist_enabled());
    assert!(parsed.online_mode());
}

#[test]
fn server_properties_accepts_colon_separator_and_surrounding_whitespace() {
    let parsed = props("   white-list : true   \n\tonline-mode:false\n").expect("parse");
    assert!(parsed.whitelist_enabled());
    assert!(!parsed.online_mode());
}

#[test]
fn server_properties_accepts_crlf_line_endings() {
    let parsed =
        props("white-list=true\r\nenforce-whitelist=true\r\nonline-mode=false\r\n").expect("parse");
    assert!(parsed.whitelist_enabled());
    assert!(parsed.whitelist_enforced());
    assert!(!parsed.online_mode());
}

#[test]
fn server_properties_invalid_boolean_is_malformed() {
    assert_eq!(props("online-mode=yes"), Err(AdmissionDataError::Malformed));
    assert_eq!(props("white-list=1"), Err(AdmissionDataError::Malformed));
    assert_eq!(
        props("enforce-whitelist="),
        Err(AdmissionDataError::Malformed)
    );
    assert_eq!(props("online-mode"), Err(AdmissionDataError::Malformed));
}

#[test]
fn server_properties_duplicate_key_is_malformed() {
    assert_eq!(
        props("white-list=true\nwhite-list=false\n"),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn server_properties_continuation_on_our_key_is_malformed() {
    assert_eq!(
        props("white-list=tr\\\nue\n"),
        Err(AdmissionDataError::Malformed)
    );
    assert_eq!(
        props("white\\\n-list=true\n"),
        Err(AdmissionDataError::Malformed)
    );
    assert_eq!(
        props("online-mode=false\\"),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn server_properties_continuation_on_other_key_is_skipped_with_its_lines() {
    let text = "motd=first \\\nwhite-list=true\nonline-mode=false\n";
    let parsed = props(text).expect("parse");
    assert!(!parsed.whitelist_enabled());
    assert!(!parsed.online_mode());
}

#[test]
fn server_properties_even_backslashes_are_not_a_continuation() {
    let parsed = props("motd=path\\\\\nwhite-list=true\n").expect("parse");
    assert!(parsed.whitelist_enabled());
}

#[test]
fn server_properties_escaped_separator_does_not_split_key() {
    let parsed = props("white\\=list=true\n").expect("parse");
    assert!(!parsed.whitelist_enabled());
    assert!(parsed.online_mode());
}

#[test]
fn server_properties_unicode_escape_names_our_key() {
    let parsed = props("white\\u002dlist=true\n").expect("parse");
    assert!(parsed.whitelist_enabled());
}

#[test]
fn server_properties_malformed_unicode_escape_is_malformed() {
    assert_eq!(props("motd=\\u12\n"), Err(AdmissionDataError::Malformed));
    assert_eq!(props("motd=\\uZZZZ\n"), Err(AdmissionDataError::Malformed));
}

#[test]
fn server_properties_non_utf8_is_malformed() {
    let bytes = [b'w', 0xFF, b'\n'];
    assert_eq!(
        ServerProperties::parse(&bytes),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn server_properties_leading_bom_is_malformed() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"white-list=true\n");
    assert_eq!(
        ServerProperties::parse(&bytes),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn server_properties_oversized_is_too_large() {
    let bytes = vec![b'#'; MAX_SERVER_PROPERTIES_BYTES + 1];
    assert_eq!(
        ServerProperties::parse(&bytes),
        Err(AdmissionDataError::TooLarge)
    );
}

#[test]
fn server_properties_load_valid_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("server.properties");
    std::fs::write(&path, "white-list=true\nonline-mode=false\n").unwrap();
    let parsed = ServerProperties::load(&path).expect("load valid file");
    assert!(parsed.whitelist_enabled());
    assert!(!parsed.online_mode());
}

#[test]
fn server_properties_load_missing_file_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("server.properties");
    assert_eq!(
        ServerProperties::load(&path),
        Err(AdmissionDataError::Unavailable)
    );
}

#[test]
fn server_properties_load_directory_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("directory");
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        ServerProperties::load(&path),
        Err(AdmissionDataError::Unavailable)
    );
}

#[test]
fn server_properties_load_oversized_file_is_too_large() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("server.properties");
    std::fs::write(&path, vec![b'#'; MAX_SERVER_PROPERTIES_BYTES + 1]).unwrap();
    assert_eq!(
        ServerProperties::load(&path),
        Err(AdmissionDataError::TooLarge)
    );
}

#[test]
fn ban_list_parses_entries_duplicates_and_extra_fields() {
    let json = format!(
        r#"[
            {{"uuid": "{UUID_A}", "name": "Player1", "created": "2020-01-01", "source": "Console", "expires": "forever", "reason": "x", "level": 3}},
            {{"uuid": "{UUID_A}", "name": "Player1Again"}},
            {{"uuid": "{UUID_B}"}}
        ]"#
    );
    let list = BanList::parse(json.as_bytes()).expect("parse");
    assert_eq!(list.len(), 2);
    assert!(!list.is_empty());
    assert!(list.contains(ban_uuid(UUID_A)));
    assert!(list.contains(ban_uuid(UUID_B)));
    assert!(!list.contains(ban_uuid("00000000-0000-0000-0000-000000000001")));
}

#[test]
fn ban_list_expired_entry_is_still_banned() {
    let json = format!(r#"[{{"uuid": "{UUID_A}", "expires": "2000-01-01 00:00:00 +0000"}}]"#);
    let list = BanList::parse(json.as_bytes()).expect("parse");
    assert!(list.contains(ban_uuid(UUID_A)));
}

#[test]
fn ban_list_empty_array_is_empty() {
    let list = BanList::parse(b"[]").expect("parse");
    assert!(list.is_empty());
    assert_eq!(list.len(), 0);
}

#[test]
fn ban_list_non_array_is_malformed() {
    assert_eq!(
        BanList::parse(format!(r#"{{"uuid": "{UUID_A}"}}"#).as_bytes()),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn ban_list_non_object_element_is_malformed() {
    assert_eq!(
        BanList::parse(br#"["not an object"]"#),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn ban_list_bad_uuid_is_malformed() {
    assert_eq!(
        BanList::parse(br#"[{"uuid": "not-a-uuid"}]"#),
        Err(AdmissionDataError::Malformed)
    );
    assert_eq!(
        BanList::parse(br#"[{"uuid": "00000000-0000-0000-0000-000000000000"}]"#),
        Err(AdmissionDataError::Malformed)
    );
    assert_eq!(
        BanList::parse(br#"[{"name": "Player1"}]"#),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn ban_list_non_string_optional_field_is_malformed() {
    for field in OPTIONAL_BAN_STRING_FIELDS {
        let json = format!(r#"[{{"uuid": "{UUID_A}", "{field}": 5}}]"#);
        assert_eq!(
            BanList::parse(json.as_bytes()),
            Err(AdmissionDataError::Malformed),
            "field {field}"
        );
    }
    let json = format!(r#"[{{"uuid": "{UUID_A}", "reason": null}}]"#);
    assert_eq!(
        BanList::parse(json.as_bytes()),
        Err(AdmissionDataError::Malformed)
    );
}

#[test]
fn ban_list_too_many_entries_is_too_large() {
    let mut json = String::from("[");
    for i in 0..=MAX_BAN_ENTRIES {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"uuid": "550e8400-e29b-41d4-a716-{:012x}"}}"#,
            i
        ));
    }
    json.push(']');
    assert_eq!(
        BanList::parse(json.as_bytes()),
        Err(AdmissionDataError::TooLarge)
    );
}

#[test]
fn ban_list_oversized_is_too_large() {
    let bytes = vec![b' '; MAX_BAN_LIST_BYTES + 1];
    assert_eq!(BanList::parse(&bytes), Err(AdmissionDataError::TooLarge));
}

#[test]
fn ban_list_non_utf8_and_bom_are_malformed() {
    assert_eq!(
        BanList::parse(&[0xFF, 0xFE, 0xFD]),
        Err(AdmissionDataError::Malformed)
    );
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"[]");
    assert_eq!(BanList::parse(&bytes), Err(AdmissionDataError::Malformed));
}

#[test]
fn ban_list_load_valid_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("banned-players.json");
    std::fs::write(&path, format!(r#"[{{"uuid": "{UUID_A}"}}]"#)).unwrap();
    let list = BanList::load(&path).expect("load valid file");
    assert!(list.contains(ban_uuid(UUID_A)));
}

#[test]
fn ban_list_load_missing_file_is_empty() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("banned-players.json");
    let list = BanList::load(&path).expect("missing file means no bans");
    assert!(list.is_empty());
}

#[test]
fn ban_list_load_directory_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("directory");
    std::fs::create_dir(&path).unwrap();
    assert_eq!(BanList::load(&path), Err(AdmissionDataError::Unavailable));
}

#[test]
fn ban_list_load_malformed_file_is_malformed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("banned-players.json");
    std::fs::write(&path, b"not json {").unwrap();
    assert_eq!(BanList::load(&path), Err(AdmissionDataError::Malformed));
}

#[test]
fn ban_list_debug_redacts_uuids_and_names() {
    let json = format!(r#"[{{"uuid": "{UUID_A}", "name": "Player1"}}, {{"uuid": "{UUID_B}"}}]"#);
    let list = BanList::parse(json.as_bytes()).expect("parse");
    let debug = format!("{list:?}");
    assert_eq!(debug, "BanList { entries: 2 }");
    assert!(!debug.contains("550e8400"));
    assert!(!debug.contains("6ba7b810"));
    assert!(!debug.contains("Player1"));
}

#[test]
fn error_codes_and_display_are_fixed() {
    assert_eq!(
        AdmissionDataError::TooLarge.code(),
        "admission_data_too_large"
    );
    assert_eq!(
        AdmissionDataError::Malformed.code(),
        "admission_data_malformed"
    );
    assert_eq!(
        AdmissionDataError::Unavailable.code(),
        "admission_data_unavailable"
    );
    assert_eq!(
        AdmissionDataError::Malformed.to_string(),
        "admission_data_malformed"
    );
    let _: &dyn Error = &AdmissionDataError::Unavailable;
}
