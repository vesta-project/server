//! cargo run -p vesta-game-runtime --release --example scale_benchmark
use std::time::Instant;
use vesta_game_runtime::{
    console::{ConsoleCapture, ConsoleStream, MAX_PAGE_LINES},
    players::parse_player_list_line,
};
fn main() {
    let count = 12_000;
    let names: Vec<_> = (0..count).map(|i| format!("Player{i:05}")).collect();
    let seed = format!(
        "There are {count}/20000 players online: {}",
        names.join(", ")
    );
    let capture = ConsoleCapture::new();
    let start = Instant::now();
    capture.ingest(ConsoleStream::Stdout, &seed, false).unwrap();
    let page = capture.snapshot(0, 1).unwrap();
    assert!(!page.lines[0].truncated);
    let roster = parse_player_list_line(&page.lines[0].text).unwrap();
    assert_eq!(roster.players().len(), count);
    let seed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut cursor = page.next_cursor;
    let start = Instant::now();
    let mut consumed = 0;
    for batch in 0..100 {
        for i in 0..200 {
            capture
                .ingest(
                    ConsoleStream::Stdout,
                    &format!(
                        "[12:00:00] [Server thread/INFO]: Player{:05} {} the game",
                        (batch * 200 + i) % count,
                        if i % 2 == 0 { "joined" } else { "left" }
                    ),
                    false,
                )
                .unwrap();
        }
        let page = capture.snapshot(cursor, MAX_PAGE_LINES).unwrap();
        assert!(!page.gap);
        consumed += page.lines.len();
        cursor = page.next_cursor;
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "players={count} seed_bytes={} seed_ms={seed_ms:.3} console_events={consumed} console_ms={ms:.3} events_per_second={:.0} latest_cursor={}",
        seed.len(),
        consumed as f64 / (ms / 1000.0),
        capture.snapshot(0, MAX_PAGE_LINES).unwrap().latest_cursor
    );
}
