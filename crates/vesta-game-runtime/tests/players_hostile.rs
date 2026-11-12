//! Deterministic hostile-console witnesses. Only public APIs are exercised.
//! Names are descriptive text, not identities or commands; no fixture is executed.
use std::{error::Error, io::Cursor, panic::catch_unwind, thread};
use vesta_game_runtime::{
    console::{ConsoleCapture, ConsoleStream},
    players::{
        MAX_INPUT_BYTES, MAX_NAME_BYTES, MAX_PLAYERS, PlayerList, PlayerListError,
        parse_player_list_line,
    },
};

#[track_caller]
fn parse_without_panic(line: &str) -> Result<PlayerList, PlayerListError> {
    catch_unwind(|| parse_player_list_line(line)).expect("untrusted console text must not panic")
}

#[track_caller]
fn assert_refused(line: &str, expected: PlayerListError) {
    let result = parse_without_panic(line);
    // Even an unexpected success has redacted Debug, not the hostile fixture.
    let error = result.expect_err("unusable console text must not produce players or a zero count");
    assert_eq!(error, expected);
    let (code, debug) = match expected {
        PlayerListError::NotRecognized => ("players_not_recognized", "NotRecognized"),
        PlayerListError::Malformed => ("players_malformed_list", "Malformed"),
        PlayerListError::TooLarge => ("players_input_too_large", "TooLarge"),
        PlayerListError::TooManyPlayers => ("players_too_many_players", "TooManyPlayers"),
        PlayerListError::NumberOverflow => ("players_number_overflow", "NumberOverflow"),
        PlayerListError::Inconsistent => ("players_inconsistent_list", "Inconsistent"),
    };
    assert_eq!(error.code(), code);
    assert_eq!(format!("{error:?}"), debug);
    assert_eq!(error.to_string(), "Player list parsing failed");
    assert!(error.source().is_none());
}

fn names(list: &PlayerList) -> Vec<&str> {
    list.players().iter().map(|player| player.name()).collect()
}

#[test]
fn empty_and_space_only_lines_do_not_claim_an_empty_server() {
    for line in ["", " ", "   ", "\u{a0}\u{2003}\u{3000}"] {
        assert_refused(line, PlayerListError::NotRecognized);
    }
    for line in ["\t", "\r", "\n", " \t\r\n "] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

#[test]
fn oversized_lines_are_refused_before_grammar_or_escape_processing() {
    for size in [MAX_INPUT_BYTES + 1, MAX_INPUT_BYTES * 128] {
        let line = "x".repeat(size);
        assert_refused(&line, PlayerListError::TooLarge);
        let line = format!("\u{1b}{}", "x".repeat(size - 1));
        assert_refused(&line, PlayerListError::TooLarge);
    }
}

#[test]
fn input_byte_limit_accepts_exact_boundary_and_refuses_one_more_byte() {
    let head = "There are 1/20 players online: Alice";
    let line = format!("{head}{}", " ".repeat(MAX_INPUT_BYTES - head.len()));
    assert_eq!(line.len(), MAX_INPUT_BYTES);
    let list = parse_without_panic(&line).expect("inclusive byte bound");
    assert_eq!(list.online(), 1);
    assert_eq!(list.max(), 20);
    assert_eq!(names(&list), ["Alice"]);
    assert_refused(&format!("{line} "), PlayerListError::TooLarge);
}

#[test]
fn entry_limit_is_inclusive_and_refuses_the_next_token() {
    let line = format!(
        "There are {MAX_PLAYERS}/{} players online: {}a",
        MAX_PLAYERS * 2,
        "a,".repeat(MAX_PLAYERS - 1)
    );
    assert!(line.len() < MAX_INPUT_BYTES);
    let list = parse_without_panic(&line).expect("inclusive token bound");
    assert_eq!(list.online(), MAX_PLAYERS as u32);
    assert_eq!(list.players().len(), MAX_PLAYERS);
    assert!(list.players().iter().all(|player| player.name() == "a"));
    assert_refused(&format!("{line},a"), PlayerListError::TooManyPlayers);
}

#[test]
fn very_large_entry_lists_hit_the_byte_bound_without_returning_a_partial_list() {
    // Two bytes per repeated entry must exceed the actual input-byte budget,
    // independently of the current player-token budget.
    let entries = MAX_INPUT_BYTES / 2 + 1;
    let line = format!(
        "There are {entries}/{} players online: {}a",
        entries * 2,
        "a,".repeat(entries - 1)
    );
    assert!(line.len() > MAX_INPUT_BYTES);
    assert_refused(&line, PlayerListError::TooLarge);
}

#[test]
fn both_count_fields_refuse_integer_overflow_in_both_grammars() {
    for count in ["4294967296", "9999999999", "10000000000", "00000000000"] {
        for line in [
            format!("There are {count}/20 players online: private-player"),
            format!("There are 1/{count} players online: private-player"),
            format!("There are {count} of a max of 20 players online: private-player"),
            format!("There are 1 of a max of {count} players online: private-player"),
        ] {
            assert_refused(&line, PlayerListError::NumberOverflow);
        }
    }
}

#[test]
fn maximum_u32_counts_are_preserved_without_wrapping_or_inventing_names() {
    let list = parse_without_panic("There are 4294967295/4294967295 players online: Alice")
        .expect("counts are descriptive and may exceed the number of visible names");
    assert_eq!(list.online(), u32::MAX);
    assert_eq!(list.max(), u32::MAX);
    assert_eq!(names(&list), ["Alice"]);
}

#[test]
fn missing_signed_and_non_decimal_counts_never_default_to_zero() {
    for counts in [
        "/20", "1/", "-1/20", "+1/20", "1/-20", "1/+20", "x/20", "١/20",
    ] {
        assert_refused(
            &format!("There are {counts} players online: private-player"),
            PlayerListError::Malformed,
        );
    }
}

#[test]
fn every_control_scalar_in_a_name_is_refused_without_echoing_the_name() {
    for control in (0u32..=0x1f).chain(0x7f..=0x9f) {
        let control = char::from_u32(control).expect("control scalar");
        assert_refused(
            &format!("There are 1/20 players online: private{control}player"),
            PlayerListError::Malformed,
        );
    }
}

#[test]
fn multibyte_name_limits_are_measured_in_bytes_not_scalars() {
    let name = "é".repeat(MAX_NAME_BYTES / 2);
    let list = parse_without_panic(&format!("There are 1/20 players online: {name}"))
        .expect("name exactly at the UTF-8 byte limit");
    assert_eq!(list.online(), 1);
    assert_eq!(names(&list), [name.as_str()]);
    assert_refused(
        &format!("There are 1/20 players online: {name}é"),
        PlayerListError::Malformed,
    );
}

#[test]
fn command_like_and_non_comma_separator_names_remain_literal_descriptions() {
    for name in [
        "/stop",
        "op Alice",
        "Alice;stop",
        "Alice|stop",
        "$(stop)",
        "Alice:Bob",
    ] {
        let list = parse_without_panic(&format!("There are 1/20 players online: {name}"))
            .expect("text is descriptive, not a command grammar");
        assert_eq!(list.online(), 1);
        assert_eq!(names(&list), [name]);
        assert_eq!(list.players()[0].uuid(), None);
        assert!(!format!("{list:?}").contains(name));
        assert!(!format!("{:?}", list.players()[0]).contains(name));
    }
}

#[test]
fn comma_separators_do_not_create_empty_or_unannounced_players() {
    for tail in ["Alice,", ",Alice", "Alice,,Bob", "Alice, ,Bob"] {
        assert_refused(
            &format!("There are 2/20 players online: {tail}"),
            PlayerListError::Malformed,
        );
    }
    assert_refused(
        "There are 1/20 players online: Alice,Bob",
        PlayerListError::Inconsistent,
    );
}

#[test]
fn duplicate_names_are_preserved_as_text_without_synthesizing_identities() {
    let list = parse_without_panic("There are 2/20 players online: Alice, Alice")
        .expect("descriptive parsing does not infer unique identities");
    assert_eq!(list.online(), 2);
    assert_eq!(names(&list), ["Alice", "Alice"]);
    assert!(list.players().iter().all(|player| player.uuid().is_none()));
    assert_refused(
        "There are 1/20 players online: Alice, Alice",
        PlayerListError::Inconsistent,
    );
}

#[test]
fn contradictory_counts_are_refused_instead_of_repaired() {
    for line in [
        "There are 0/20 players online: Alice",
        "There are 1/20 players online: Alice,Bob",
        "There are 1/20 players online:",
        "There are 2/20 players online:   ",
        "There are 1/20 players online: 00000000-0000-0000-0000-000000000001",
    ] {
        assert_refused(line, PlayerListError::Inconsistent);
    }
}

#[test]
fn hidden_name_policy_does_not_fill_missing_names_or_replace_the_stated_count() {
    let list = parse_without_panic("There are 2/20 players online: Alice")
        .expect("the documented grammar allows servers to hide names");
    assert_eq!(list.online(), 2);
    assert_eq!(names(&list), ["Alice"]);
    // This text alone cannot distinguish a hidden name from a truncated second name.
}

#[test]
fn truncated_grammar_and_empty_name_tails_never_become_zero_player_results() {
    assert_refused("There are ", PlayerListError::NotRecognized);
    for line in [
        "There are 1",
        "There are 1/",
        "There are 1/20",
        "There are 1/20 players",
        "There are 1/20 players online",
        "There are 1 of a max of",
        "There are 1/20 players online: Alice,",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
    assert_refused(
        "There are 1/20 players online:",
        PlayerListError::Inconsistent,
    );
}

#[test]
fn every_utf8_prefix_of_an_incomplete_header_is_panic_free_and_refused() {
    let header = "[界]: There are 1/20 players online:";
    for end in (0..=header.len()).filter(|&end| header.is_char_boundary(end)) {
        let error = parse_without_panic(&header[..end]).expect_err("no complete named list yet");
        assert!(matches!(
            error,
            PlayerListError::NotRecognized
                | PlayerListError::Malformed
                | PlayerListError::Inconsistent
        ));
        assert_eq!(error.to_string(), "Player list parsing failed");
    }
}

#[test]
fn multiline_interleaving_is_refused_instead_of_combining_responses() {
    for separator in ["\n", "\r", "\r\n", "\u{85}"] {
        assert_refused(
            &format!(
                "There are 1/20 players online: Alice{separator}There are 1/20 players online: Bob"
            ),
            PlayerListError::Malformed,
        );
    }
}

#[test]
fn concurrent_stream_capture_keeps_complete_player_lines_separate() {
    let capture = ConsoleCapture::new();
    thread::scope(|scope| {
        let stdout = scope.spawn(|| {
            capture.capture_reader(
                ConsoleStream::Stdout,
                Cursor::new(b"There are 1/20 players online: Alice\n"),
            )
        });
        let stderr = scope.spawn(|| {
            capture.capture_reader(
                ConsoleStream::Stderr,
                Cursor::new(b"There are 1/20 players online: Bob\n"),
            )
        });
        assert_eq!(
            stdout.join().expect("reader must not panic").unwrap().lines,
            1
        );
        assert_eq!(
            stderr.join().expect("reader must not panic").unwrap().lines,
            1
        );
    });
    let page = capture.snapshot(0, 2).unwrap();
    assert_eq!(page.lines.len(), 2);
    for line in &page.lines {
        assert!(!line.truncated);
        let list =
            parse_without_panic(&line.text).expect("one complete response per captured line");
        assert_eq!(list.online(), 1);
        let expected = match line.stream {
            ConsoleStream::Stdout => "Alice",
            ConsoleStream::Stderr => "Bob",
        };
        assert_eq!(names(&list), [expected]);
    }
}

#[test]
fn ansi_sgr_is_removed_without_creating_additional_players() {
    let list = parse_without_panic(
        "\u{1b}[32mThere are 2/20 players online: \u{1b}[1mAlice\u{1b}[0m, \u{1b}[38:2:255:0:0mBob\u{1b}[m",
    )
    .expect("complete recognized SGR formatting");
    assert_eq!(list.online(), 2);
    assert_eq!(names(&list), ["Alice", "Bob"]);
    assert!(list.players().iter().all(|player| player.uuid().is_none()));
}

#[test]
fn ansi_cursor_osc_and_truncated_escapes_are_refused_and_redacted() {
    for escape in [
        "\u{1b}",
        "\u{1b}[",
        "\u{1b}[31",
        "\u{1b}[2J",
        "\u{1b}[H",
        "\u{1b}[?25l",
        "\u{1b}]0;private-title\u{7}",
        "\u{1b}]8;;https://private.invalid\u{1b}\\",
    ] {
        assert_refused(
            &format!("There are 1/20 players online: private{escape}player"),
            PlayerListError::Malformed,
        );
    }
}

#[test]
fn non_utf8_console_name_is_refused_instead_of_inventing_a_replacement_name() {
    // The parser takes &str: exercise the real byte-to-console-text boundary rather
    // than construct an invalid Rust string or introduce undefined behavior.
    let mut bytes = b"There are 1/20 players online: Al".to_vec();
    bytes.push(0xff);
    bytes.extend_from_slice(b"ice\n");
    assert!(std::str::from_utf8(&bytes).is_err());
    let capture = ConsoleCapture::new();
    capture
        .capture_reader(ConsoleStream::Stdout, Cursor::new(bytes))
        .unwrap();
    let page = capture.snapshot(0, 1).unwrap();
    assert_eq!(page.lines.len(), 1);
    assert!(!page.lines[0].truncated);
    assert_refused(&page.lines[0].text, PlayerListError::Malformed);
}

#[test]
fn fused_concurrent_responses_are_refused_instead_of_inventing_a_player_name() {
    // Deterministic byte interleaving: another complete response arrives before the
    // first writer emits its newline. There is no scheduling-dependent assertion.
    assert_refused(
        "There are 1/20 players online: AliceThere are 1/20 players online: Bob",
        PlayerListError::Malformed,
    );
}
