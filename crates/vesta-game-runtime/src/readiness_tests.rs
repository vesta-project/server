use super::*;
use std::net::TcpListener;
use std::thread::{self, JoinHandle};

const DONE: &str = "Done (1.234s)! For help, type \"help\"";
const SUMMARY: StatusSummary = StatusSummary {
    online: 2,
    max: 20,
    protocol: 767,
};
const JSON: &str = r#"{"version":{"name":"untrusted","protocol":767},"players":{"max":20,"online":2},"description":{"text":"hello"}}"#;

fn config() -> ReadinessConfig {
    ReadinessConfig {
        probe: Some("127.0.0.1:25565".parse().unwrap()),
        ..ReadinessConfig::default()
    }
}

fn tracker_with_done() -> (ReadinessTracker, Instant) {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(config());
    tracker.generation_started(now);
    tracker.observe_line(DONE);
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Ready);
    (tracker, now)
}

#[test]
fn default_config_and_generation_lifecycle() {
    let config = ReadinessConfig::default();
    assert_eq!(config.startup_timeout, Duration::from_secs(300));
    assert_eq!(config.probe_interval, Duration::from_secs(15));
    assert_eq!(config.probe_timeout, Duration::from_secs(3));
    assert_eq!(config.probe, None);
    let mut tracker = ReadinessTracker::new(config);
    let now = Instant::now();
    tracker.observe_line(DONE);
    tracker.observe_gap();
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Stopped);
    assert_eq!(tracker.probe_due(now), None);
    assert!(!tracker.probe_configured());
    tracker.generation_started(now);
    assert_eq!(tracker.state(now), Readiness::Starting);
    tracker.observe_line(DONE);
    assert_eq!(tracker.state(now), Readiness::Ready);
    tracker.generation_stopped();
    assert_eq!(tracker.state(now), Readiness::Stopped);
    tracker.generation_started(now);
    assert_eq!(tracker.state(now), Readiness::Starting);
}

#[test]
fn all_done_forms_and_help_versions() {
    let prefixes = [
        "",
        "[12:00:00] [Server thread/INFO]: ",
        "[12:00:00 INFO]: ",
        "[12:00:00] [Server thread/INFO] [minecraft/MinecraftServer]: ",
        "[12:00:00] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: ",
    ];
    for prefix in prefixes {
        for seconds in ["1", "0.001", "1.234", "300.0"] {
            for suffix in ["\"help\"", "\"help\" or \"?\""] {
                let mut tracker = ReadinessTracker::new(ReadinessConfig::default());
                let now = Instant::now();
                tracker.generation_started(now);
                let line = format!("{prefix}Done ({seconds}s)! For help, type {suffix}");
                tracker.observe_line(&line);
                assert_eq!(tracker.state(now), Readiness::Ready, "{line}");
            }
        }
    }
}

#[test]
fn spoofed_or_malformed_done_lines_do_not_match() {
    for line in [
        format!("<Steve> {DONE}"),
        format!("[12:00:00] [Server thread/INFO]: <Steve> {DONE}"),
        format!("[12:00:00 INFO]: <Steve> {DONE}"),
        format!("[12:00:00] [Async Chat Thread - #1/INFO]: {DONE}"),
        format!("[12:00:00] [main/INFO]: {DONE}"),
        format!("[12:00:00] [Server thread/WARN]: {DONE}"),
        format!("[12:00:00 ERROR]: {DONE}"),
        format!("[12:00:00] [Server thread/DEBUG]: {DONE}"),
        format!("[24:00:00 INFO]: {DONE}"),
        format!("[12:60:00 INFO]: {DONE}"),
        format!("[12:00:60 INFO]: {DONE}"),
        format!("[１２:00:00 INFO]: {DONE}"),
        format!("[12:00:00] [Server thread/INFO] []: {DONE}"),
        format!("[12:00:00] [Server thread/INFO] [<Steve>]: {DONE}"),
        format!(" [12:00:00 INFO]: {DONE}"),
        format!("[12:00:00 INFO]: {DONE} extra"),
        "Done (s)! For help, type \"help\"".to_owned(),
        "Done (.1s)! For help, type \"help\"".to_owned(),
        "Done (1.s)! For help, type \"help\"".to_owned(),
        "Done (1.2.3s)! For help, type \"help\"".to_owned(),
        "Done (-1s)! For help, type \"help\"".to_owned(),
        "Done (NaNs)! For help, type \"help\"".to_owned(),
        "Done (1s)! For help, type help".to_owned(),
    ] {
        let now = Instant::now();
        let mut tracker = ReadinessTracker::new(ReadinessConfig::default());
        tracker.generation_started(now);
        tracker.observe_line(&line);
        assert_eq!(tracker.state(now), Readiness::Starting, "{line}");
    }
}

#[test]
fn done_requires_a_later_success_when_probe_is_configured() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(config());
    tracker.generation_started(now);
    assert!(tracker.probe_configured());
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Starting);
    tracker.observe_line(DONE);
    assert_eq!(tracker.state(now), Readiness::Starting);
    tracker.record_probe(now + Duration::from_secs(1), Err(ProbeError::Protocol));
    assert_eq!(tracker.state(now), Readiness::Starting);
    tracker.record_probe(now + Duration::from_secs(2), Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Ready);
    tracker.observe_line(DONE);
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn startup_timeout_is_pure_and_late_readiness_recovers() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(config());
    tracker.generation_started(now);
    assert_eq!(
        tracker.state(now + Duration::from_secs(299)),
        Readiness::Starting
    );
    assert_eq!(
        tracker.state(now + Duration::from_secs(300)),
        Readiness::Degraded(DegradedReason::StartupTimeout)
    );
    assert_eq!(tracker.state(now), Readiness::Starting);
    if let Some(before) = now.checked_sub(Duration::from_secs(1)) {
        assert_eq!(tracker.state(before), Readiness::Starting);
    }
    tracker.observe_line(DONE);
    assert_eq!(
        tracker.state(now + Duration::from_secs(301)),
        Readiness::Degraded(DegradedReason::StartupTimeout)
    );
    tracker.record_probe(now + Duration::from_secs(302), Ok(SUMMARY));
    assert_eq!(
        tracker.state(now + Duration::from_secs(302)),
        Readiness::Ready
    );
}

#[test]
fn zero_startup_timeout_and_log_only_late_recovery() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(ReadinessConfig {
        startup_timeout: Duration::ZERO,
        ..ReadinessConfig::default()
    });
    tracker.generation_started(now);
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::StartupTimeout)
    );
    tracker.observe_line(DONE);
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn probe_failure_threshold_and_recovery() {
    let (mut tracker, now) = tracker_with_done();
    for i in 1..=2 {
        tracker.record_probe(now + Duration::from_secs(i), Err(ProbeError::Timeout));
        assert_eq!(tracker.state(now), Readiness::Ready);
    }
    tracker.record_probe(now, Err(ProbeError::Connect));
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::ProbeFailed)
    );
    for _ in 0..300 {
        tracker.record_probe(now, Err(ProbeError::Protocol));
    }
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::ProbeFailed)
    );
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Ready);
    tracker.record_probe(now, Err(ProbeError::TooLarge));
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn successful_probe_resets_consecutive_failure_count() {
    let (mut tracker, now) = tracker_with_done();
    for _ in 0..4 {
        tracker.record_probe(now, Err(ProbeError::Connect));
        tracker.record_probe(now, Err(ProbeError::Connect));
        tracker.record_probe(now, Ok(SUMMARY));
        assert_eq!(tracker.state(now), Readiness::Ready);
    }
}

#[test]
fn starting_failures_do_not_fake_ready_or_probe_failed() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(config());
    tracker.generation_started(now);
    for _ in 0..3 {
        tracker.record_probe(now, Err(ProbeError::Connect));
    }
    assert_eq!(tracker.state(now), Readiness::Starting);
}

#[test]
fn probes_are_scheduled_from_recorded_completion() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(config());
    assert_eq!(tracker.probe_due(now), None);
    tracker.generation_started(now);
    assert_eq!(tracker.probe_due(now), config().probe);
    // Pure due queries do not implicitly start/reserve probes.
    assert_eq!(tracker.probe_due(now), config().probe);
    tracker.record_probe(now, Err(ProbeError::Timeout));
    assert_eq!(tracker.probe_due(now + Duration::from_secs(14)), None);
    assert_eq!(
        tracker.probe_due(now + Duration::from_secs(15)),
        config().probe
    );
    tracker.record_probe(now + Duration::from_secs(16), Ok(SUMMARY));
    assert_eq!(tracker.probe_due(now + Duration::from_secs(30)), None);
    assert_eq!(
        tracker.probe_due(now + Duration::from_secs(31)),
        config().probe
    );
    assert_eq!(tracker.probe_due(now), None);
    tracker.generation_started(now);
    assert_eq!(tracker.probe_due(now), config().probe);
}

#[test]
fn zero_probe_interval_is_always_due_when_running() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(ReadinessConfig {
        probe_interval: Duration::ZERO,
        ..config()
    });
    tracker.generation_started(now);
    tracker.record_probe(now, Err(ProbeError::Connect));
    assert_eq!(tracker.probe_due(now), config().probe);
}

#[test]
fn fatal_patterns_are_sticky_and_reset_per_generation() {
    for prefix in [
        "[12:00:00] [Server thread/INFO]: ",
        "[12:00:00] [Server thread/WARN]: ",
        "[12:00:00] [Server thread/ERROR]: ",
        "[12:00:00 INFO]: ",
        "[12:00:00 WARN]: ",
        "[12:00:00 ERROR]: ",
        "[12:00:00] [Server thread/ERROR] [minecraft/MinecraftServer]: ",
    ] {
        for fatal in [
            "**** FAILED TO BIND TO PORT!",
            "Failed to start the minecraft server",
            "Encountered an unexpected exception",
            "This crash report has been saved to: C:\\server\\crash.txt",
        ] {
            let now = Instant::now();
            let mut tracker = ReadinessTracker::new(config());
            tracker.generation_started(now);
            tracker.observe_line(&format!("{prefix}{fatal}"));
            assert_eq!(
                tracker.state(now),
                Readiness::Degraded(DegradedReason::FatalOutput),
                "{prefix}{fatal}"
            );
            tracker.observe_gap();
            tracker.observe_line(DONE);
            tracker.record_probe(now, Ok(SUMMARY));
            assert_eq!(
                tracker.state(now),
                Readiness::Degraded(DegradedReason::FatalOutput)
            );
            tracker.generation_started(now);
            assert_eq!(tracker.state(now), Readiness::Starting);
        }
    }
}

#[test]
fn spoofed_fatal_output_is_ignored() {
    for fatal in [
        "**** FAILED TO BIND TO PORT!",
        "Failed to start the minecraft server",
        "Encountered an unexpected exception",
        "This crash report has been saved to: crash.txt",
    ] {
        for prefix in [
            "",
            "<Steve> ",
            "[12:00:00 INFO]: <Steve> ",
            "[12:00:00] [Server thread/INFO]: <Steve> ",
            "[12:00:00] [Async Chat Thread/INFO]: ",
            "[12:00:00] [main/ERROR]: ",
        ] {
            let now = Instant::now();
            let mut tracker = ReadinessTracker::new(config());
            tracker.generation_started(now);
            tracker.observe_line(&format!("{prefix}{fatal}"));
            assert_eq!(tracker.state(now), Readiness::Starting, "{prefix}{fatal}");
        }
    }
}

#[test]
fn fatal_output_after_ready_is_ignored() {
    let (mut tracker, now) = tracker_with_done();
    tracker.observe_line("[12:00:00 ERROR]: Encountered an unexpected exception");
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn gap_can_be_recovered_by_probe_without_done() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(config());
    tracker.generation_started(now);
    tracker.observe_gap();
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::ConsoleGap)
    );
    assert_eq!(
        tracker.state(now + Duration::from_secs(301)),
        Readiness::Degraded(DegradedReason::ConsoleGap)
    );
    tracker.record_probe(now, Err(ProbeError::Timeout));
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::ConsoleGap)
    );
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Ready);
    tracker.observe_line(DONE);
    assert_eq!(tracker.state(now), Readiness::Ready);
    for _ in 0..3 {
        tracker.record_probe(now, Err(ProbeError::Connect));
    }
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::ProbeFailed)
    );
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn gap_requires_success_after_gap_not_before() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(config());
    tracker.generation_started(now);
    tracker.record_probe(now, Ok(SUMMARY));
    tracker.observe_gap();
    tracker.observe_line(DONE);
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::ConsoleGap)
    );
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn gap_without_probe_stays_degraded_even_with_done() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(ReadinessConfig::default());
    tracker.generation_started(now);
    tracker.observe_gap();
    tracker.observe_line(DONE);
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(
        tracker.state(now),
        Readiness::Degraded(DegradedReason::ConsoleGap)
    );
    tracker.generation_started(now);
    tracker.observe_line(DONE);
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn gap_after_ready_does_not_invalidate_confirmed_world() {
    let (mut tracker, now) = tracker_with_done();
    tracker.observe_gap();
    assert_eq!(tracker.state(now), Readiness::Ready);
}

#[test]
fn generation_replacement_drops_all_prior_evidence() {
    let (mut tracker, now) = tracker_with_done();
    for _ in 0..3 {
        tracker.record_probe(now, Err(ProbeError::Connect));
    }
    tracker.generation_started(now);
    assert_eq!(tracker.state(now), Readiness::Starting);
    tracker.record_probe(now, Ok(SUMMARY));
    assert_eq!(tracker.state(now), Readiness::Starting);
    tracker.generation_stopped();
    tracker.observe_line("[12:00:00 ERROR]: Encountered an unexpected exception");
    tracker.generation_started(now);
    assert_eq!(tracker.state(now), Readiness::Starting);
}

#[test]
fn console_input_is_bounded() {
    let now = Instant::now();
    let mut tracker = ReadinessTracker::new(ReadinessConfig::default());
    tracker.generation_started(now);
    let decimal = "1".repeat(MAX_INPUT);
    tracker.observe_line(&format!("Done ({decimal}s)! For help, type \"help\""));
    assert_eq!(tracker.state(now), Readiness::Starting);
    let fatal = format!(
        "[12:00:00 ERROR]: This crash report has been saved to: {}",
        "a".repeat(MAX_INPUT)
    );
    tracker.observe_line(&fatal);
    assert_eq!(tracker.state(now), Readiness::Starting);
}

#[test]
fn properties_defaults_wildcards_and_literal_addresses() {
    for (properties, expected) in [
        ("", "127.0.0.1:25565"),
        ("# comment\n! another\nunknown=x", "127.0.0.1:25565"),
        ("server-ip=\nserver-port=25566", "127.0.0.1:25566"),
        ("server-ip=0.0.0.0", "127.0.0.1:25565"),
        ("server-ip=::", "[::1]:25565"),
        ("server-ip=::1\nserver-port=25566", "[::1]:25566"),
        ("server-ip=192.0.2.1\nserver-port=12345", "192.0.2.1:12345"),
        (
            "  server-ip = 2001:db8::1 \r\n server-port = 12345 \r\n",
            "[2001:db8::1]:12345",
        ),
        ("server-port=invalid\nserver-port=1234", "127.0.0.1:1234"),
        ("server-ip=invalid\nserver-ip=0.0.0.0", "127.0.0.1:25565"),
        ("server-port=65535", "127.0.0.1:65535"),
    ] {
        assert_eq!(
            probe_address(properties),
            Some(expected.parse().unwrap()),
            "{properties}"
        );
    }
}

#[test]
fn properties_invalid_values_and_bounds() {
    for properties in [
        "server-ip=example.com",
        "server-ip=[::1]",
        "server-ip=999.1.1.1",
        "server-port=0",
        "server-port=65536",
        "server-port=-1",
        "server-port=",
        "server-port=1.5",
        "server-port=25565 # comment",
        "server-ip=127\\.0.0.1",
        "server-ip=::1%1",
        "server-port=1234\nserver-port=invalid",
    ] {
        assert_eq!(probe_address(properties), None, "{properties}");
    }
    assert_eq!(
        probe_address(&"#".repeat(MAX_INPUT)),
        Some("127.0.0.1:25565".parse().unwrap())
    );
    assert_eq!(probe_address(&"#".repeat(MAX_INPUT + 1)), None);
}

fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut length_bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        length_bytes.push(byte[0]);
        assert!(length_bytes.len() <= 5);
        if byte[0] & 0x80 == 0 {
            break;
        }
    }
    let mut cursor = 0;
    let length = varint_read(&length_bytes, &mut cursor).unwrap() as usize;
    assert!(length <= 256);
    let mut frame = vec![0; length];
    stream.read_exact(&mut frame).unwrap();
    frame
}

fn fake_server<F>(reply: F) -> (SocketAddr, JoinHandle<()>)
where
    F: FnOnce(TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let handshake = read_frame(&mut stream);
        let mut cursor = 0;
        assert_eq!(varint_read(&handshake, &mut cursor).unwrap(), 0);
        assert_eq!(varint_read(&handshake, &mut cursor).unwrap(), 47);
        let host_length = varint_read(&handshake, &mut cursor).unwrap() as usize;
        assert_eq!(&handshake[cursor..cursor + host_length], b"127.0.0.1");
        cursor += host_length;
        assert_eq!(
            &handshake[cursor..cursor + 2],
            &address.port().to_be_bytes()
        );
        cursor += 2;
        assert_eq!(varint_read(&handshake, &mut cursor).unwrap(), 1);
        assert_eq!(cursor, handshake.len());
        assert_eq!(read_frame(&mut stream), [0]);
        reply(stream);
    });
    (address, handle)
}

fn response(json: &[u8]) -> Vec<u8> {
    let mut packet = vec![0];
    varint_push(&mut packet, json.len() as u32);
    packet.extend_from_slice(json);
    let mut wire = Vec::new();
    varint_push(&mut wire, packet.len() as u32);
    wire.extend_from_slice(&packet);
    wire
}

fn probe_wire(wire: Vec<u8>) -> Result<StatusSummary, ProbeError> {
    let (address, handle) = fake_server(move |mut stream| {
        // The peer may reject a frame prefix before consuming the body.
        let _ = stream.write_all(&wire);
    });
    let result = status_probe(address, Duration::from_secs(2));
    handle.join().unwrap();
    result
}

#[test]
fn status_probe_valid_response_and_handshake() {
    assert_eq!(probe_wire(response(JSON.as_bytes())), Ok(SUMMARY));
}

#[test]
fn status_probe_valid_fragmented_response() {
    let (address, handle) = fake_server(|mut stream| {
        for byte in response(JSON.as_bytes()) {
            stream.write_all(&[byte]).unwrap();
        }
    });
    assert_eq!(status_probe(address, Duration::from_secs(2)), Ok(SUMMARY));
    handle.join().unwrap();
}

#[test]
fn status_probe_oversized_length() {
    for length in [MAX_INPUT as u32, MAX_INPUT as u32 + 1, u32::MAX] {
        let mut prefix = Vec::new();
        varint_push(&mut prefix, length);
        assert_eq!(probe_wire(prefix), Err(ProbeError::TooLarge));
    }
}

#[test]
fn status_probe_garbage_varints() {
    for wire in [
        vec![0x80; 6],
        vec![0xff, 0xff, 0xff, 0xff, 0x10],
        vec![0x80],
        vec![1, 0x80],
    ] {
        assert_eq!(probe_wire(wire), Err(ProbeError::Protocol));
    }
}

#[test]
fn status_probe_incorrect_packet_id_json_length_and_truncation() {
    for wire in [
        vec![0],
        vec![2, 1, 0],
        vec![2, 0, 1],
        vec![4, 0, 1, b'{', b'}'],
        vec![10, 0, 2, b'{'],
    ] {
        assert_eq!(probe_wire(wire), Err(ProbeError::Protocol));
    }
}

#[test]
fn status_probe_invalid_utf8_and_json() {
    for bytes in [
        b"garbage".as_slice(),
        b"{}",
        b"[]",
        &[0xff],
        b"{\"version\":null}",
    ] {
        assert_eq!(probe_wire(response(bytes)), Err(ProbeError::Protocol));
    }
}

#[test]
fn status_probe_slow_server_times_out() {
    let (address, handle) = fake_server(|_stream| thread::sleep(Duration::from_millis(300)));
    assert_eq!(
        status_probe(address, Duration::from_millis(100)),
        Err(ProbeError::Timeout)
    );
    handle.join().unwrap();
}

#[test]
fn status_probe_trickle_cannot_reset_overall_deadline() {
    let (address, handle) = fake_server(|mut stream| {
        for byte in response(JSON.as_bytes()) {
            if stream.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    assert_eq!(
        status_probe(address, Duration::from_millis(150)),
        Err(ProbeError::Timeout)
    );
    handle.join().unwrap();
}

#[test]
fn status_probe_refused_connection_and_zero_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    assert_eq!(
        status_probe(address, Duration::from_secs(10)),
        Err(ProbeError::Connect)
    );
    assert_eq!(
        status_probe(address, Duration::ZERO),
        Err(ProbeError::Timeout)
    );
    assert_eq!(
        status_probe(address, Duration::MAX),
        Err(ProbeError::Timeout)
    );
}

#[test]
fn status_probe_response_total_wire_size_boundary() {
    let base = response(JSON.as_bytes()).len();
    // Added whitespace grows both the frame and JSON length VarInts from 1 to
    // 3 bytes. Choose exactly the total response budget, not just JSON length.
    let mut json = JSON.as_bytes().to_vec();
    json.extend(std::iter::repeat_n(b' ', MAX_INPUT - base - 4));
    let wire = response(&json);
    assert_eq!(wire.len(), MAX_INPUT);
    assert_eq!(probe_wire(wire), Ok(SUMMARY));
    json.push(b' ');
    assert_eq!(probe_wire(response(&json)), Err(ProbeError::TooLarge));
}

#[test]
fn json_handles_unknown_structures_unicode_and_escaped_keys() {
    let json = br#"{
        "\u0076ersion":{"protocol":-1,"name":"\uD83D\uDE00"},
        "players":{"online":0,"max":4294967295,"sample":[{"name":"test"}]},
        "description":{"text":"MOTD \"quoted\" \\ \b\f\n\r\t\/ \u00E9"},
        "unknown":[true,false,null,1.25e+2,-0,[],{},-1.0E-5]
    }"#;
    assert_eq!(
        JsonParser::parse(json),
        Ok(StatusSummary {
            online: 0,
            max: u32::MAX,
            protocol: -1
        })
    );
    let json = format!(
        r#"{{"version":{{"protocol":{}}},"players":{{"online":1,"max":1}},"description":"你好"}}"#,
        i32::MIN
    );
    assert_eq!(
        JsonParser::parse(json.as_bytes()).unwrap().protocol,
        i32::MIN
    );
}

#[test]
fn json_rejects_missing_wrong_type_duplicate_and_out_of_range_fields() {
    for json in [
        r#"{"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":1},"players":{"online":1}}"#,
        r#"{"version":{"protocol":1},"players":{"max":2}}"#,
        r#"{"version":1,"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":"1"},"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":1.0},"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":2147483648},"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":-2147483649},"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":1},"players":{"online":-1,"max":2}}"#,
        r#"{"version":{"protocol":1},"players":{"online":1,"max":4294967296}}"#,
        r#"{"version":{"protocol":1},"players":{"online":1e0,"max":2}}"#,
        r#"{"version":{"protocol":1},"players":{"online":1,"max":null}}"#,
        r#"{"version":{"protocol":1},"players":[]}"#,
        r#"{"version":{"protocol":1,"protocol":2},"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":1},"players":{"online":1,"online":2,"max":2}}"#,
        r#"{"version":{"protocol":1},"players":{"online":1,"max":2,"max":3}}"#,
        r#"{"version":{"protocol":1},"version":{"protocol":2},"players":{"online":1,"max":2}}"#,
        r#"{"version":{"protocol":1},"players":{"online":1,"max":2},"players":{"online":2,"max":2}}"#,
        r#"{"nested":{"version":{"protocol":1},"players":{"online":1,"max":2}}}"#,
    ] {
        assert_eq!(
            JsonParser::parse(json.as_bytes()),
            Err(ProbeError::Protocol),
            "{json}"
        );
    }
}

#[test]
fn json_rejects_malformed_unknown_values_and_trailing_data() {
    for value in [
        "01",
        "-01",
        ".1",
        "1.",
        "1e",
        "1e+",
        "--1",
        "tru",
        "falsee",
        "undefined",
        "[1,]",
        "{\"a\":1,}",
        "{\"a\" 1}",
        "{a:1}",
        "[}",
        "\"unterminated",
        r#""\x20""#,
        r#""\uQQQQ""#,
        r#""\uD800""#,
        r#""\uDC00""#,
        r#""\uD800\u0041""#,
        "\"raw\nnewline\"",
        "\"raw\tcontrol\"",
    ] {
        let json = format!(
            r#"{{"version":{{"protocol":1}},"players":{{"online":1,"max":2}},"unknown":{value}}}"#
        );
        assert_eq!(
            JsonParser::parse(json.as_bytes()),
            Err(ProbeError::Protocol),
            "{json}"
        );
    }
    for suffix in ["{}", "true", "!", ","] {
        let json = format!("{JSON}{suffix}");
        assert_eq!(
            JsonParser::parse(json.as_bytes()),
            Err(ProbeError::Protocol)
        );
    }
}

#[test]
fn json_depth_and_size_are_bounded() {
    let json = format!(
        r#"{{"version":{{"protocol":1}},"players":{{"online":1,"max":2}},"unknown":{}0{}}}"#,
        "[".repeat(32),
        "]".repeat(32)
    );
    assert_eq!(
        JsonParser::parse(json.as_bytes()),
        Err(ProbeError::Protocol)
    );
    assert_eq!(
        JsonParser::parse(&vec![b' '; MAX_INPUT + 1]),
        Err(ProbeError::TooLarge)
    );
    let json = format!(
        r#"{{"version":{{"protocol":1}},"players":{{"online":1,"max":2}},"description":"{}"}}"#,
        "x".repeat(60000)
    );
    assert_eq!(
        JsonParser::parse(json.as_bytes()),
        Ok(StatusSummary {
            online: 1,
            max: 2,
            protocol: 1
        })
    );
}

#[test]
fn malformed_corpus_and_all_truncations_do_not_panic() {
    for end in 0..JSON.len() {
        assert!(JsonParser::parse(&JSON.as_bytes()[..end]).is_err());
    }
    let mut seed = 7_u32;
    for length in 0..512 {
        let bytes: Vec<_> = (0..length)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect();
        let _ = JsonParser::parse(&bytes);
        let text = String::from_utf8_lossy(&bytes);
        let _ = probe_address(&text);
        let mut tracker = ReadinessTracker::new(ReadinessConfig::default());
        tracker.generation_started(Instant::now());
        tracker.observe_line(&text);
    }
}

#[test]
fn varints_round_trip_and_reject_overflow() {
    for number in [0, 1, 127, 128, 255, 65535, u32::MAX] {
        let mut encoded = Vec::new();
        varint_push(&mut encoded, number);
        let mut cursor = 0;
        assert_eq!(varint_read(&encoded, &mut cursor), Ok(number));
        assert_eq!(cursor, encoded.len());
    }
    for bytes in [vec![], vec![0x80; 5], vec![0xff, 0xff, 0xff, 0xff, 0x10]] {
        assert_eq!(varint_read(&bytes, &mut 0), Err(ProbeError::Protocol));
    }
}
