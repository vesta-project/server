use super::*;

#[test]
fn templates_cover_all_states_truthful_offline_counts_and_safe_unknowns() {
    let mut document = Document::default();
    let input = PublicInputs {
        server_name: "Vesta",
        online_players: Some(25),
        ..PublicInputs::default()
    };
    for state in State::ALL {
        document.states.get_mut(&state).unwrap().template =
            "{server_name}: {state}\n{online_players}/{max_players} ETA {estimated_ready_seconds}"
                .into();
        let rendered = document.render(state, &input).unwrap();
        assert_eq!(rendered.plain_lines.len(), 2);
        assert!(
            rendered.plain_lines[1].starts_with(if state == State::Online {
                "25/0"
            } else {
                "0/0"
            })
        );
        assert!(rendered.plain_lines[1].ends_with("unknown"));
    }
    document.states.remove(&State::Offline);
    assert!(!document.valid());
}
#[test]
fn template_and_expansion_bounds_reject_secrets_recursion_and_format_injection() {
    for text in [
        "{private_path}",
        "{password}",
        "{exception}",
        "{unknown}",
        "{server_name",
        "server_name}",
        "a\nb\nc",
        "\r",
        "\0",
        "§z",
        "§",
    ] {
        assert!(validate_template(text).is_err(), "{text:?}");
    }
    assert!(validate_template(&"😀".repeat(257)).is_err());
    let mut document = Document::default();
    document.states.get_mut(&State::Offline).unwrap().template =
        "{server_name} {public_failure}".into();
    let injected = PublicInputs {
        server_name: "§cprivate\nline",
        ..PublicInputs::default()
    };
    assert!(document.render(State::Offline, &injected).is_err());
    let input = PublicInputs {
        server_name: "{password}",
        public_failure: Some(PublicFailure::RecoveryRequired),
        ..PublicInputs::default()
    };
    let rendered = document.render(State::Offline, &input).unwrap();
    assert_eq!(rendered.plain_lines[0], "{password} recovery_required");
    document.states.get_mut(&State::Offline).unwrap().template = "{server_name}".repeat(5);
    let input = PublicInputs {
        server_name: &"x".repeat(128),
        ..PublicInputs::default()
    };
    assert!(document.render(State::Offline, &input).is_err());
}
#[test]
fn colors_styles_reset_and_json_are_structured_without_click_events() {
    let rendered = component("§a§lGreen\n§cRed§r plain {\"clickEvent\":\"bad\"}").unwrap();
    let spans = rendered["extra"].as_array().unwrap();
    assert_eq!(spans[0]["color"], "green");
    assert_eq!(spans[0]["bold"], true);
    assert_eq!(spans[1]["color"], "red");
    assert!(spans[1].get("bold").is_none());
    assert!(spans[2].get("color").is_none());
    assert!(spans.iter().all(|v| v.get("clickEvent").is_none()));
}
#[test]
fn icon_validator_strips_metadata_and_rejects_corruption_size_and_deflate_bombs() {
    let valid = fallback_icon();
    assert!(normalize_icon(&valid).is_some());
    let mut decorated = valid[..valid.len() - 12].to_vec();
    chunk(&mut decorated, b"tEXt", b"private server path /secret");
    chunk(&mut decorated, b"IEND", &[]);
    let sanitized = normalize_icon(&decorated).unwrap();
    assert!(!sanitized.windows(7).any(|w| w == b"/secret"));
    let mut damaged = valid.clone();
    damaged[20] ^= 1;
    assert!(normalize_icon(&damaged).is_none());
    assert!(normalize_icon(&vec![0; 65537]).is_none());
    let wrong_size = encode_icon(&[0, 0, 0, 65, 0, 0, 0, 64, 8, 6, 0, 0, 0], &vec![0; 16448]);
    assert!(normalize_icon(&wrong_size).is_none());
    let bomb = encode_icon(
        &[0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0],
        &vec![0; 1_000_000],
    );
    assert!(normalize_icon(&bomb).is_none());
    let mut document = Document::default();
    document.states.get_mut(&State::Offline).unwrap().icon_png = Some("broken".into());
    let rendered = document
        .render(State::Offline, &PublicInputs::default())
        .unwrap();
    assert_eq!(
        rendered.favicon_data_uri,
        format!("data:image/png;base64,{}", STANDARD.encode(valid))
    );
}
