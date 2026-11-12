//! Independent player-list parser cases with explicit golden expectations.
//!
//! Every expected count and name below is written literally, not derived from a helper
//! that parses the same fixture. A test that recomputed its expectation with the code
//! under test would pass even if the grammar were wrong.
use super::{
    MAX_INPUT_BYTES, MAX_NAME_BYTES, MAX_PLAYERS, PlayerListError, parse_player_list_line,
};

fn names(list: &super::PlayerList) -> Vec<&str> {
    list.players().iter().map(|entry| entry.name()).collect()
}

/// Every negative case asserts the exact refusal code. This checks the error only,
/// because a successful parse has deliberately no PartialEq.
#[track_caller]
fn assert_refused(line: &str, expected: PlayerListError) {
    match parse_player_list_line(line) {
        Err(actual) => assert_eq!(actual, expected, "{line:?}"),
        Ok(list) => panic!("expected {expected:?} for {line:?}, parsed {list:?}"),
    }
}

/// An empty server reports a stated count of zero and lists nobody.
#[test]
fn an_empty_vanilla_list_reports_zero_and_no_players() {
    let list = parse_player_list_line("There are 0 of a max of 20 players online:")
        .expect("a vanilla empty list is recognized");
    assert_eq!(list.online(), 0);
    assert_eq!(list.max(), 20);
    assert_eq!(names(&list), Vec::<&str>::new());
}

/// Two names, in the order written, with no UUID suffix.
#[test]
fn a_vanilla_list_keeps_names_in_written_order_without_uuids() {
    let list = parse_player_list_line("There are 2 of a max of 20 players online: Steve, Alex")
        .expect("a vanilla named list is recognized");
    assert_eq!(list.online(), 2);
    assert_eq!(list.max(), 20);
    assert_eq!(names(&list), vec!["Steve", "Alex"]);
    assert_eq!(list.players()[0].uuid(), None);
    assert_eq!(list.players()[1].uuid(), None);
}

/// The plugin slash form and a bracketed console prefix both parse to the same shape.
#[test]
fn the_slash_form_and_a_console_prefix_both_parse() {
    let slashed =
        parse_player_list_line("There are 1/20 players online: Notch").expect("slash form");
    assert_eq!(slashed.online(), 1);
    assert_eq!(slashed.max(), 20);
    assert_eq!(names(&slashed), vec!["Notch"]);

    let prefixed = parse_player_list_line(
        "[12:00:01] [Server thread/INFO]: There are 1/20 players online: Notch",
    )
    .expect("a prefixed slash-form line");
    assert_eq!(prefixed.online(), 1);
    assert_eq!(prefixed.max(), 20);
    assert_eq!(names(&prefixed), vec!["Notch"]);
}

/// A parenthesized UUID is split from the name and canonicalized to lowercase.
#[test]
fn a_parenthesized_uuid_is_separated_from_the_name_and_lowercased() {
    let list = parse_player_list_line(
        "There are 1 of a max of 8 players online: Steve (00000000-0000-0000-0000-000000000001)",
    )
    .expect("a name with a UUID suffix");
    assert_eq!(list.online(), 1);
    assert_eq!(names(&list), vec!["Steve"]);
    assert_eq!(
        list.players()[0].uuid(),
        Some("00000000-0000-0000-0000-000000000001")
    );

    let upper = parse_player_list_line(
        "There are 1 of a max of 8 players online: Steve (A1B2C3D4-0000-0000-0000-00000000000F)",
    )
    .expect("an uppercase UUID suffix");
    assert_eq!(
        upper.players()[0].uuid(),
        Some("a1b2c3d4-0000-0000-0000-00000000000f"),
        "an uppercase UUID is canonicalized rather than stored verbatim"
    );
}

/// A bare UUID carries no visible name, so it is dropped instead of inventing one.
#[test]
fn a_bare_uuid_is_dropped_rather_than_given_a_fabricated_name() {
    let list = parse_player_list_line(
        "There are 2 of a max of 20 players online: Steve, 00000000-0000-0000-0000-000000000002",
    )
    .expect("a mixed name and bare UUID list");
    assert_eq!(names(&list), vec!["Steve"], "only the named entry survives");
    assert!(
        list.online() >= list.players().len() as u32,
        "a hidden name does not make the list inconsistent"
    );
}

/// Legacy section codes and ANSI colour sequences are removed before parsing.
#[test]
fn formatting_codes_are_removed_before_the_grammar_is_applied() {
    let legacy = parse_player_list_line("\u{a7}cThere are 1 of a max of 20 players online: Steve")
        .expect("a legacy-coloured line");
    assert_eq!(names(&legacy), vec!["Steve"]);

    let coloured = parse_player_list_line(
        "\u{1b}[32mThere are 1 of a max of 20 players online: Steve\u{1b}[0m",
    )
    .expect("an ANSI-coloured line");
    assert_eq!(coloured.online(), 1);
    assert_eq!(names(&coloured), vec!["Steve"]);
}

/// A dangling escape is a malformed list, not a line that happens to be unrecognized:
/// the shape was recognized and the body could not be trusted.
#[test]
fn a_dangling_format_escape_is_malformed_rather_than_unrecognized() {
    for line in [
        "There are 1 of a max of 20 players online: Steve\u{1b}",
        "There are 1 of a max of 20 players online: Steve\u{1b}[",
        "There are 1 of a max of 20 players online: Steve\u{1b}[3",
        "There are 1 of a max of 20 players online: Steve\u{a7}",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

/// An ordinary console line is not a player list. This is the negative case that must
/// stay distinct from every refusal above.
#[test]
fn an_unrelated_console_line_is_not_recognized() {
    for line in [
        "Done (3.2s)! For help, type \"help\"",
        "",
        "   ",
        "[12:00:01] [Server thread/INFO]: Done (3.2s)! For help, type \"help\"",
        "there is 1 of a max of 20 players online: Steve",
        // No "There are " prefix, so the shape is never recognized.
        "There were 2 of a max of 20 players online: Steve",
        "There are1/20 players online: Notch",
        "There are2 of a max of 20 players online: Steve",
        "There  are 1 of a max of 20 players online: Steve",
    ] {
        assert_refused(line, PlayerListError::NotRecognized);
    }
}

/// Any remaining control scalar is refused before any trim or grammar step.
///
/// This is what keeps the "one line" promise real. Without this, a leading newline
/// would be trimmed away and a trailing carriage return would be trimmed off a name,
/// so a caller could pass a whole block of console output and have the first line read
/// as a complete player list.
#[test]
fn a_control_scalar_anywhere_is_refused_before_trimming() {
    for line in [
        // Leading CR, LF and CRLF: exactly what `trim` would discard.
        "\nThere are 1 of a max of 20 players online: Steve",
        "\rThere are 1 of a max of 20 players online: Steve",
        "\r\nThere are 1 of a max of 20 players online: Steve",
        // Trailing CR and LF, which name trimming would otherwise absorb.
        "There are 1 of a max of 20 players online: Steve\r",
        "There are 1 of a max of 20 players online: Steve\n",
        "There are 1 of a max of 20 players online: Steve\r\n",
        "There are 1 of a max of 20 players online: Steve \r\n",
        // A second line appended, which must not be silently ignored.
        "There are 1 of a max of 20 players online: Steve\nThere are 2/20 players online: Alex",
        // Tabs at the grammar edges and inside the separators.
        "\tThere are 1 of a max of 20 players online: Steve",
        "There are\t1 of a max of 20 players online: Steve",
        "There are 1\tof a max of 20 players online: Steve",
        "There are 1 of a max of 20\tplayers online: Steve",
        "There are 1 of a max of 20 players online:\tSteve",
        "There are 1/20\tplayers online: Steve",
        // Tabs at the name edges.
        "There are 1 of a max of 20 players online: \tSteve",
        "There are 1 of a max of 20 players online: Steve\t",
        "There are 1 of a max of 20 players online: Steve\t, Alex",
        // Other control scalars.
        "There are 1 of a max of 20 players online: Steve\u{b}",
        "There are 1 of a max of 20 players online: Steve\u{c}",
        "There are 1 of a max of 20 players online: Steve\u{0}",
        "There are 1 of a max of 20 players online: \u{7}",
        // A vertical tab and form feed, which `trim` does not remove but which are
        // still control scalars.
        "There are 1 of a max of 20 players online: Steve\u{b}",
        "There are 1 of a max of 20 players online: Steve\u{c}",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

/// Recognized escapes still strip cleanly, and a stripped line still parses.
///
/// This guards the control-scalar refusal against over-reach: a complete SGR sequence
/// is consumed before the control check runs, so real formatting is not mistaken for
/// raw control input.
#[test]
fn recognized_escapes_are_stripped_before_the_control_refusal() {
    for line in [
        "\u{1b}[32mThere are 1 of a max of 20 players online: Steve\u{1b}[0m",
        "\u{1b}[1;32mThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[38;5;200mThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[38:2:255:0:0mThere are 1 of a max of 20 players online: Steve",
        "\u{a7}cThere are 1 of a max of 20 players online: Steve",
        "\u{a7}X\u{a7}f\u{a7}f\u{a7}0\u{a7}0\u{a7}a\u{a7}aThere are 1 of a max of 20 players online: Steve",
    ] {
        let list = parse_player_list_line(line).expect("a complete escape is removed first");
        assert_eq!(list.online(), 1, "{line:?}");
        assert_eq!(names(&list), vec!["Steve"], "{line:?}");
    }
}

/// A log prefix only counts when the line actually begins with a bracket.
///
/// Otherwise "chat]: There are ..." would be accepted purely because it happens to
/// contain the bytes a console prefix ends with.
#[test]
fn an_unbracketed_impostor_prefix_is_rejected() {
    for line in [
        "chat]: There are 1 of a max of 20 players online: Steve",
        "]: There are 1 of a max of 20 players online: Steve",
        "<chat>: There are 1/20 players online: Notch",
        "x]: There are 2 of a max of 20 players online: Steve, Alex",
        // A bracket that is not the first character does not open a prefix.
        " chat]: There are 1 of a max of 20 players online: Steve",
    ] {
        assert_refused(line, PlayerListError::NotRecognized);
    }
}

/// Real bracketed console prefixes still parse, including multi-byte bracket content.
#[test]
fn bracketed_log_prefixes_are_still_accepted() {
    // Each entry is the line and the exact counts and names it must yield.
    for (line, online, expected_names) in [
        (
            "[12:00:01] [Server thread/INFO]: There are 1/20 players online: Notch",
            1u32,
            vec!["Notch"],
        ),
        (
            "[12:00:01] [Server thread/INFO]: There are 2 of a max of 20 players online: Steve, Alex",
            2,
            vec!["Steve", "Alex"],
        ),
        (
            "[\u{4e2d}\u{6587}]: There are 1/20 players online: Notch",
            1,
            vec!["Notch"],
        ),
        (
            "[INFO]: There are 1 of a max of 20 players online: Steve",
            1,
            vec!["Steve"],
        ),
    ] {
        let list = parse_player_list_line(line).expect("a bracketed prefix is accepted");
        assert_eq!(list.online(), online, "{line:?}");
        assert_eq!(list.max(), 20, "{line:?}");
        assert_eq!(names(&list), expected_names, "{line:?}");
    }

    // A bracketed line whose message is not a list stays unrecognized.
    assert_refused(
        "[12:00:01] [Server thread/INFO]: Done (3.2s)! For help, type \"help\"",
        PlayerListError::NotRecognized,
    );
}

/// A line that opens with the head token and its separator, but whose count slot holds
/// a word rather than a number, is a mangled list rather than an unrelated line.
///
/// This is a deliberately narrow case: the head token really did match and the body
/// really is unusable, so it is refused as a malformed list. It does not extend the
/// grammar — anything that lacks the "There are " prefix stays unrecognized.
#[test]
fn a_word_where_the_count_belongs_is_malformed_rather_than_unrecognized() {
    assert_refused(
        "There are wolves of a max of 20 players online: Steve",
        PlayerListError::Malformed,
    );
    assert_refused(
        "There are three of a max of 20 players online: Steve",
        PlayerListError::Malformed,
    );
}

/// The separator after the colon is required whenever a body follows it.
///
/// Without this, "players online:Steve" would be accepted as a list whose first
/// player is named "Steve", which is a concatenation this grammar never produces.
#[test]
fn the_colon_separator_is_required_when_a_body_follows() {
    assert_refused(
        "There are 1 of a max of 20 players online:Steve",
        PlayerListError::Malformed,
    );
    assert_refused(
        "There are 1/20 players online:Notch",
        PlayerListError::Malformed,
    );
    // The empty tail stays legal: that is the "nobody online" form.
    let empty = parse_player_list_line("There are 0 of a max of 20 players online:")
        .expect("an empty tail needs no separator");
    assert_eq!(empty.online(), 0);
    assert_eq!(empty.players().len(), 0);
}

/// Extra spaces inside the grammar are legal, including around the slash.
///
/// This is the exact shape Java's `\s+` forms accepted, so the plugin slash form is
/// not narrowed by requiring separators.
#[test]
fn extra_spaces_inside_the_grammar_are_accepted() {
    let slash = parse_player_list_line("There are 1 / 20 players online: Notch")
        .expect("spaces around the slash are legal");
    assert_eq!(slash.online(), 1);
    assert_eq!(slash.max(), 20);
    assert_eq!(names(&slash), vec!["Notch"]);

    let words =
        parse_player_list_line("There are  2  of a max of  20  players online:  Steve, Alex")
            .expect("repeated separators are legal");
    assert_eq!(words.online(), 2);
    assert_eq!(words.max(), 20);
    assert_eq!(names(&words), vec!["Steve", "Alex"]);
}

/// A declared count may exceed the announced capacity.
///
/// This is not policed. An operator can raise `max-player` above the real capacity to
/// admit overflow slots, so `online <= max` is a real-world possibility rather than an
/// invariant, and inventing one here would reject legitimate lines. The counts are
/// descriptive only and are never proof that a server is up or that a reader is
/// entitled to the list.
#[test]
fn a_count_above_the_announced_capacity_is_not_policed() {
    let names = format!("{}S", "S, ".repeat(903));
    let line = format!("There are 905 of a max of 20 players online: {names}");
    let list = parse_player_list_line(&line).expect("no online<=max policy is applied");
    assert_eq!(list.online(), 905);
    assert_eq!(list.max(), 20);
    assert_eq!(list.players().len(), 904);
    assert_eq!(list.players()[903].name(), "S");
}

/// Once tokens exist, every token must exist too.
///
/// "Steve,,Alex" and "Steve," are refused rather than read as two names and one name.
#[test]
fn an_empty_interior_or_trailing_token_is_refused() {
    for line in [
        "There are 2 of a max of 20 players online: Steve,,Alex",
        "There are 2 of a max of 20 players online: Steve,",
        "There are 1 of a max of 20 players online: ,Steve",
        "There are 3 of a max of 20 players online: Steve, , Alex",
        "There are 2 of a max of 20 players online: Steve,   ",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

/// Only real legacy colour and style codes are consumed after a section sign.
///
/// Each code is exercised on its own complete line, because codes are consumed one at
/// a time: a single section sign followed by every code in sequence would strip only
/// the first and leave the rest as ordinary text, which is correct but proves nothing
/// about the remaining codes.
#[test]
fn valid_legacy_section_codes_are_removed() {
    // Every accepted code, in either case, each on its own line.
    for code in "0123456789abcdefklmnorABCDEFKLMNOR".chars() {
        let line = format!("\u{a7}{code}There are 1 of a max of 20 players online: Steve");
        let list = parse_player_list_line(&line).expect("every real legacy code is removed");
        assert_eq!(list.online(), 1, "{code:?}");
        assert_eq!(names(&list), vec!["Steve"], "{code:?}");
    }
}

/// The hex colour form is written as `§x` followed by six further `§<hex digit>` pairs.
///
/// This is the real output format, not a marker followed by six bare digits.
#[test]
fn the_real_hex_section_form_is_removed() {
    for hex in [
        "\u{a7}x\u{a7}A\u{a7}1\u{a7}B\u{a7}2\u{a7}C\u{a7}3",
        "\u{a7}X\u{a7}a\u{a7}f\u{a7}0\u{a7}0\u{a7}a\u{a7}a",
        "\u{a7}X\u{a7}f\u{a7}f\u{a7}0\u{a7}0\u{a7}a\u{a7}a",
        "\u{a7}X\u{a7}0\u{a7}0\u{a7}0\u{a7}0\u{a7}0\u{a7}0",
    ] {
        let line = format!("{hex}There are 1 of a max of 20 players online: Steve");
        let list = parse_player_list_line(&line).expect("a real hex colour form is removed");
        assert_eq!(names(&list), vec!["Steve"], "{hex:?}");
    }
}

/// A hex run that stops short, or carries a non-hex digit, is refused whole.
///
/// Nothing is partly consumed, and no plain text after the run is used to "complete" it.
#[test]
fn a_truncated_or_malformed_hex_section_run_is_refused() {
    for line in [
        // Five pairs: one short of a complete colour.
        "\u{a7}x\u{a7}A\u{a7}1\u{a7}B\u{a7}2\u{a7}CThere are 1 of a max of 20 players online: Steve",
        "\u{a7}X\u{a7}f\u{a7}f\u{a7}0\u{a7}0\u{a7}aThere are 1 of a max of 20 players online: Steve",
        // The marker with bare digits, which is not the real format.
        "\u{a7}XA1B2C3There are 1 of a max of 20 players online: Steve",
        "\u{a7}xff00aaThere are 1 of a max of 20 players online: Steve",
        // A pair whose digit is not hex.
        "\u{a7}X\u{a7}f\u{a7}f\u{a7}g\u{a7}0\u{a7}a\u{a7}aThere are 1 of a max of 20 players online: Steve",
        "\u{a7}X\u{a7}f\u{a7}f\u{a7} \u{a7}0\u{a7}a\u{a7}aThere are 1 of a max of 20 players online: Steve",
        // A missing section sign inside the run.
        "\u{a7}X\u{a7}f\u{a7}f0\u{a7}0\u{a7}a\u{a7}aThere are 1 of a max of 20 players online: Steve",
        // The marker with nothing after it.
        "\u{a7}xThere are 1 of a max of 20 players online: Steve",
        "\u{a7}XThere are 1 of a max of 20 players online: Steve",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

/// A completed hex run followed by more codes is two independent escapes, not one
/// over-long run.
///
/// The six-pair form is fixed width, so whatever follows begins a fresh escape and is
/// validated on its own. Pinning this stops the fixed width from quietly becoming a
/// "keep consuming section signs" loop.
#[test]
fn a_hex_run_followed_by_another_code_parses_as_two_escapes() {
    let line = "\u{a7}X\u{a7}f\u{a7}f\u{a7}0\u{a7}0\u{a7}a\u{a7}a\u{a7}aThere are 1 of a max of 20 players online: Steve";
    let list = parse_player_list_line(line).expect("a trailing code is its own escape");
    assert_eq!(list.online(), 1);
    assert_eq!(names(&list), vec!["Steve"]);
}

/// A section sign followed by something that is not a code is refused, not eaten.
#[test]
fn an_unknown_or_incomplete_section_code_is_refused() {
    for line in [
        // Unknown letters: p, q and z are not legacy codes.
        "\u{a7}zThere are 1 of a max of 20 players online: Steve",
        "\u{a7}PThere are 1 of a max of 20 players online: Steve",
        "\u{a7}qThere are 1 of a max of 20 players online: Steve",
        // Punctuation and a space.
        "\u{a7}.There are 1 of a max of 20 players online: Steve",
        "\u{a7} There are 1 of a max of 20 players online: Steve",
        "\u{a7}\u{1b}[32mThere are 1 of a max of 20 players online: Steve",
        // A section sign that consumed a real name character instead of a code.
        "There are 1 of a max of 20 players online: Ste\u{a7}ve",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

/// Complete SGR sequences are removed; every other escape is refused.
#[test]
fn valid_sgr_sequences_are_removed() {
    for line in [
        "\u{1b}[32mThere are 1 of a max of 20 players online: Steve\u{1b}[0m",
        "\u{1b}[1;32mThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[mThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[38;5;200mThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[38:2:255:0:0mThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[31m\u{1b}[1mThere are 1 of a max of 20 players online: Steve",
    ] {
        let list = parse_player_list_line(line).expect("a complete SGR sequence is removed");
        assert_eq!(list.online(), 1, "{line:?}");
        assert_eq!(names(&list), vec!["Steve"], "{line:?}");
    }
}

/// Anything that is not a complete SGR sequence is refused rather than dropped.
#[test]
fn an_unsupported_or_truncated_ansi_escape_is_refused() {
    for line in [
        // A non-CSI escape: consuming the next scalar would delete real text.
        "\u{1b}cThere are 1 of a max of 20 players online: Steve",
        "\u{1b}\u{1b}There are 1 of a max of 20 players online: Steve",
        "\u{1b}There are 1 of a max of 20 players online: Steve",
        // An escape whose next scalar is multi-byte, which must not be eaten.
        "\u{1b}\u{1f600}There are 1 of a max of 20 players online: Steve",
        "\u{1b}\nThere are 1 of a max of 20 players online: Steve",
        // A CSI that is complete but not SGR.
        "\u{1b}[2JThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[HThere are 1 of a max of 20 players online: Steve",
        "\u{1b}[?25lThere are 1 of a max of 20 players online: Steve",
        // Truncated CSI.
        "\u{1b}[There are 1 of a max of 20 players online: Steve",
        "\u{1b}[32There are 1 of a max of 20 players online: Steve",
        "\u{1b}[32;There are 1 of a max of 20 players online: Steve",
        // An OSC string introducer is not understood here.
        "\u{1b}]There are 1 of a max of 20 players online: Steve",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

/// Multi-byte text at and around every structural boundary must be refused or parsed,
/// never panic. Every fixture here is a legal Rust `&str`, so a boundary slip in the
/// byte-indexed prefix matching would show up as a panic rather than a wrong answer.
#[test]
fn multibyte_text_around_the_grammar_boundaries_never_panics() {
    let lines = [
        // Multi-byte text where a count belongs.
        "\u{4f60}\u{597d} 1 of a max of 20 players online: Steve",
        "There are \u{4f60}\u{597d} of a max of 20 players online: Steve",
        "There are 1 of a max of \u{4f60}\u{597d} players online: Steve",
        // Multi-byte text where the list head belongs.
        "\u{00e9}\u{00e9}\u{00e9}There are 1 of a max of 20 players online: Steve",
        "There \u{00e9} 1 of a max of 20 players online: Steve",
        // Multi-byte names and a multi-byte log prefix.
        "There are 1 of a max of 20 players online: \u{5c71}\u{7530}\u{592a}",
        "[\u{4e2d}\u{6587}]: There are 1/20 players online: Notch",
        // Multi-byte scalars where an escape follow-on belongs.
        "There are 1 of a max of 20 players online: Steve\u{1b}\u{754c}",
        "There are 1 of a max of 20 players online: Steve\u{a7}\u{754c}",
        // Emoji, combining marks, and a bare surrogate-free boundary pair.
        "There are 1 of a max of 20 players online: \u{1f600}",
        "There are 1 of a max of 20 players online: e\u{0301}",
        // A combining mark immediately after the head, where the separator is required.
        "There ar\u{0301}e 1 of a max of 20 players online: Steve",
        // A section sign inside the head token.
        "\u{a7}There are 1 of a max of 20 players online: Steve",
        // An odd number of bytes before the head, so no 3-byte slice alignment helps.
        "\u{00e9}\u{4f60}\u{1f600}There are 1 of a max of 20 players online: Steve",
    ];

    for line in lines {
        // The only requirement is a clean refusal: never a panic.
        match parse_player_list_line(line) {
            Ok(list) => {
                // Nothing is asserted about a successful parse here beyond reaching it.
                assert_eq!(list.online(), 1, "{line:?}");
            }
            Err(error) => assert_eq!(error.to_string(), "Player list parsing failed"),
        }
    }

    // Pin the multi-byte cases that must parse, so the loop above is not a blanket
    // "nothing panics" that also hides a rejection of legitimate input.
    let named = parse_player_list_line(
        "There are 1 of a max of 20 players online: \u{5c71}\u{7530}\u{592a}",
    )
    .expect("a multi-byte name is accepted");
    assert_eq!(names(&named), vec!["\u{5c71}\u{7530}\u{592a}"]);
    let prefixed =
        parse_player_list_line("[\u{4e2d}\u{6587}]: There are 1/20 players online: Notch")
            .expect("a multi-byte log prefix is accepted");
    assert_eq!(names(&prefixed), vec!["Notch"]);
    assert_eq!(prefixed.online(), 1);
    assert_eq!(prefixed.max(), 20);
}

/// A count that contradicts the visible names is refused. Java silently replaces a
/// zero count with the number of names; that substitution is exactly what is not done.
#[test]
fn a_count_that_contradicts_the_names_is_refused_rather_than_repaired() {
    for line in [
        "There are 0 of a max of 20 players online: Steve",
        "There are 1 of a max of 20 players online: Steve, Alex",
        "There are 1/20 players online: Steve, Alex, Notch",
    ] {
        assert_refused(line, PlayerListError::Inconsistent);
    }
}

/// A count may legitimately exceed the visible names when a server hides players.
#[test]
fn a_count_above_the_visible_names_is_accepted() {
    let list = parse_player_list_line("There are 3 of a max of 20 players online: Steve")
        .expect("a server may hide names by policy");
    assert_eq!(list.online(), 3);
    assert_eq!(list.players().len(), 1);
}

/// An absent, non-numeric or oversized count field is refused, never read as zero.
#[test]
fn a_missing_or_non_numeric_count_is_refused_instead_of_read_as_zero() {
    for line in [
        "There are  of a max of 20 players online:",
        "There are x of a max of 20 players online:",
        "There are -1 of a max of 20 players online:",
        "There are 2 of a max of  players online:",
        "There are 2 players online:",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

/// A count too large for the type, or longer than any real capacity, is refused.
#[test]
fn an_out_of_range_count_is_refused_as_an_overflow() {
    for line in [
        "There are 99999999999 of a max of 20 players online:",
        "There are 4294967296 of a max of 20 players online:",
        "There are 1/99999999999 players online:",
    ] {
        assert_refused(line, PlayerListError::NumberOverflow);
    }
}

/// Exactly `MAX_INPUT_BYTES` is accepted; one more byte is refused before any parsing.
///
/// The line is filled with real bounded entries rather than one huge name, so a
/// rejection here can only come from the input bound and not from the name bound.
#[test]
fn the_input_bound_is_inclusive_and_refuses_one_byte_more() {
    const ENTRIES: usize = 905;
    const FIRST_NAME: &str = "nnnnnnnn";

    let head = format!("There are {ENTRIES} of a max of 20 players online: ");
    let mut names = String::new();
    for index in 0..ENTRIES {
        if index > 0 {
            names.push(',');
        }
        names.push_str(FIRST_NAME);
    }
    // The last entry absorbs the remaining bytes so the whole line lands exactly on
    // the bound; the padding is checked rather than assumed.
    // Whitespace belongs to the grammar and does not inflate any individual name.
    names.insert_str(0, &" ".repeat(MAX_INPUT_BYTES - 8192));
    let pad = MAX_INPUT_BYTES - head.len() - names.len();
    assert_eq!(pad, 3, "the chosen entry count must leave a small pad");
    names.push_str("xxx");

    let line = format!("{head}{names}");
    assert_eq!(line.len(), MAX_INPUT_BYTES);
    let list = parse_player_list_line(&line).expect("exactly MAX_INPUT_BYTES is accepted");
    assert_eq!(list.online(), 905);
    assert_eq!(list.players().len(), 905);
    assert_eq!(list.players()[0].name(), FIRST_NAME);
    assert_eq!(list.players()[904].name(), "nnnnnnnnxxx");

    let over = format!("{line}y");
    assert_eq!(over.len(), MAX_INPUT_BYTES + 1);
    assert_refused(&over, PlayerListError::TooLarge);
}

/// Exactly `MAX_NAME_BYTES` is accepted; one more byte is a malformed list.
#[test]
fn the_name_bound_is_inclusive_and_refuses_one_byte_more() {
    let at_bound = "n".repeat(MAX_NAME_BYTES);
    let line = format!("There are 1 of a max of 20 players online: {at_bound}");
    let list = parse_player_list_line(&line).expect("exactly MAX_NAME_BYTES is accepted");
    assert_eq!(list.players()[0].name(), at_bound);
    assert_eq!(list.players()[0].name().len(), MAX_NAME_BYTES);

    let over = "n".repeat(MAX_NAME_BYTES + 1);
    let line = format!("There are 1 of a max of 20 players online: {over}");
    assert_refused(&line, PlayerListError::Malformed);
}

/// More listed tokens than `MAX_PLAYERS` is refused, including when the surplus tokens
/// would have been dropped as bare UUIDs. This bounds work per line, not server size.
#[test]
fn a_list_longer_than_the_player_bound_is_refused() {
    let entry = "Steve, ";
    let count = MAX_PLAYERS + 1;
    let names: String = entry.repeat(count);
    let line = format!("There are 1024 of a max of 2000 players online: {names}");
    assert_refused(&line, PlayerListError::TooManyPlayers);

    // Exactly at the bound the same shape is not over-long by token count.
    let at_bound: String = "S, ".repeat(MAX_PLAYERS - 1);
    let line = format!(
        "There are {} of a max of 2000 players online: {at_bound}S",
        MAX_PLAYERS
    );
    let list = parse_player_list_line(&line).expect("exactly MAX_PLAYERS tokens is accepted");
    assert_eq!(list.players().len(), MAX_PLAYERS);
    assert_eq!(list.online(), MAX_PLAYERS as u32);
}

#[test]
fn dropped_bare_uuid_still_consumes_a_bounded_token_slot() {
    const UUID: &str = "00000000-0000-0000-0000-000000000002";
    let names = "S, ".repeat(MAX_PLAYERS - 1);
    let line = format!("There are {MAX_PLAYERS}/20000 players online: {names}{UUID}");
    assert!(line.len() < MAX_INPUT_BYTES);
    let list = parse_player_list_line(&line).unwrap();
    assert_eq!(list.players().len(), MAX_PLAYERS - 1);
    assert_eq!(list.online(), MAX_PLAYERS as u32);
    let names = "S, ".repeat(MAX_PLAYERS);
    let over = format!("There are 1024/2000 players online: {names}{UUID}");
    assert!(over.len() < MAX_INPUT_BYTES);
    assert_refused(&over, PlayerListError::TooManyPlayers);
}

/// Player names and the whole list are redacted in Debug; counts are not secret.
#[test]
fn debug_output_redacts_names_but_keeps_the_shape() {
    let list = parse_player_list_line("There are 2 of a max of 20 players online: Steve, Alex")
        .expect("a two-name list");
    let rendered = format!("{list:?}");
    assert!(!rendered.contains("Steve"), "{rendered}");
    assert!(!rendered.contains("Alex"), "{rendered}");
    assert!(rendered.contains("online"), "{rendered}");
    assert!(rendered.contains("player_count"), "{rendered}");

    let entry = format!("{:?}", list.players()[0]);
    assert!(!entry.contains("Steve"), "{entry}");
    assert!(entry.contains("has_uuid"), "{entry}");
}

#[test]
fn replacement_markers_are_refused_even_outside_the_name() {
    for line in [
        "There are 1/20 players online: Al\u{fffd}ice",
        "There are 1/20 players online: Al\u{fffd}ice (00000000-0000-0000-0000-000000000001)",
        "[\u{fffd}]: There are 1/20 players online: Alice",
        "There are 0/20 players online: \u{fffd}",
    ] {
        assert_refused(line, PlayerListError::Malformed);
    }
}

#[test]
fn embedded_response_markers_are_not_player_names() {
    for name in [
        "AliceThere are 1/20 players online: Bob",
        "AliceTHERE ARE 1 of a max of 20 players online: Bob",
        "Aliceplayers online: Bob",
        "AlicePLAYERS ONLINE: Bob",
        "Alice[INFO]: Bob",
        "AliceThere \u{a7}aare 1/20 players online: Bob",
        "Alice]: Bob (00000000-0000-0000-0000-000000000001)",
    ] {
        assert_refused(
            &format!("There are 1/20 players online: {name}"),
            PlayerListError::Malformed,
        );
    }
}

/// Error rendering is fixed and input-free: no line text, no name, no source detail.
#[test]
fn error_rendering_is_fixed_and_carries_no_input() {
    for error in [
        PlayerListError::NotRecognized,
        PlayerListError::Malformed,
        PlayerListError::TooLarge,
        PlayerListError::TooManyPlayers,
        PlayerListError::NumberOverflow,
        PlayerListError::Inconsistent,
    ] {
        assert_eq!(error.to_string(), "Player list parsing failed");
        assert!(std::error::Error::source(&error).is_none());
        assert!(error.code().starts_with("players_"), "{}", error.code());
    }
}

#[test]
fn modern_vanilla_system_chat_command_feedback_is_a_list() {
    let list = parse_player_list_line(
        "[12:00:01] [Server thread/INFO]: System chat: There are 0 of a max of 20 players online: ",
    )
    .unwrap();
    assert_eq!(list.online(), 0);
    assert!(parse_player_list_line("[12:00:01] [Server thread/INFO]: <Alex> System chat: There are 0 of a max of 20 players online: ").is_err());
}
