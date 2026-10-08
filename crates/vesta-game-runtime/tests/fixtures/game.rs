//! Inert child for runtime tests only. Never Minecraft, a provider or a supervisor.
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, BufRead, Write},
    process, thread,
    time::Duration,
};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("graceful");
    if mode == "hold-pipes" {
        // Bounded, test-only inherited-handle witness. Always self-terminates.
        fs::write("descendant-ready", b"ready").expect("fixture descendant handshake");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !std::path::Path::new("release-descendant").exists()
            && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        return;
    }
    #[cfg(windows)]
    let _lifetime_lease = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .share_mode(0)
            .open("lifetime-lease")
            .expect("fixture lifetime lease")
    };
    let mut starts = OpenOptions::new()
        .create(true)
        .append(true)
        .open("starts.txt")
        .expect("fixture starts");
    writeln!(starts, "start").expect("fixture starts write");
    fs::write(
        "cwd.txt",
        env::current_dir()
            .expect("fixture cwd")
            .to_string_lossy()
            .as_bytes(),
    )
    .expect("fixture cwd write");
    // Length-prefixed bytes provide a literal argv oracle without quoting/JSON ambiguity.
    let mut recorded = Vec::new();
    for arg in &args {
        recorded.extend_from_slice(&(arg.len() as u64).to_le_bytes());
        recorded.extend_from_slice(arg.as_bytes());
    }
    fs::write("args.bin", recorded).expect("fixture argv write");
    println!("fixture-ready");
    eprintln!("fixture-stderr-ready");
    io::stdout().flush().expect("fixture flush");
    if mode == "crash" {
        process::exit(23);
    }
    if mode == "inherit-pipes" {
        let descendant = process::Command::new(env::current_exe().expect("fixture executable"))
            .arg("hold-pipes")
            .stdin(process::Stdio::null())
            .stdout(process::Stdio::inherit())
            .stderr(process::Stdio::inherit())
            .spawn()
            .expect("fixture inherited pipes");
        // Intentionally model the unimplemented process-tree boundary, not production cleanup.
        // The descendant has explicit release and a ten-second self-termination fallback.
        drop(descendant);
        return;
    }
    if mode == "closed-stdin" {
        // This mode never reads; the pipe can fill while the child stays alive.
        loop {
            thread::sleep(Duration::from_millis(20));
        }
    }
    let mut commands = OpenOptions::new()
        .create(true)
        .append(true)
        .open("commands.txt")
        .expect("fixture commands");
    for line in io::stdin().lock().lines() {
        let line = line.expect("fixture stdin");
        writeln!(commands, "{line}").expect("fixture command write");
        commands.flush().expect("fixture command flush");
        match line.as_str() {
            "stop" if mode == "ignore-stop" => println!("stop-ignored"),
            "stop" if mode == "slow-stop" => {
                println!("stop-pending");
                io::stdout().flush().expect("fixture pending flush");
                // Test releases explicitly, not by hoping a sleep races the timeout.
                while !std::path::Path::new("release-stop").exists() {
                    thread::sleep(Duration::from_millis(5));
                }
                println!("fixture-stopped");
                return;
            }
            "stop" => {
                println!("fixture-stopped");
                return;
            }
            "crash" => process::exit(23),
            "flood" => {
                // Each stream alone exceeds a normal pipe's capacity, without snapshots.
                let chunk = "x".repeat(4096);
                for _ in 0..1024 {
                    println!("{chunk}");
                    eprintln!("{chunk}");
                }
                println!("flood-stdout-done");
                eprintln!("flood-stderr-done");
            }
            "long-line" => {
                let chunk = [b'x'; 8192];
                let mut stdout = io::stdout().lock();
                for _ in 0..128 {
                    stdout.write_all(&chunk).expect("fixture long line");
                }
                stdout
                    .write_all(b"\nafter-long-line\n")
                    .expect("fixture next line");
            }
            _ => println!("echo:{line}"),
        }
        io::stdout().flush().expect("fixture stdout flush");
        io::stderr().flush().expect("fixture stderr flush");
    }
}
