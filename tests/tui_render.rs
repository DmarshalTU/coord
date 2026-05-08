//! Headless render of `coord top` against a seeded daemon.
//!
//! Spins up a real `coord serve` on a free port, seeds a varied set of
//! agents and tasks, then runs `coord top --once` to render one frame
//! to stdout. The captured output is asserted against so the TUI keeps
//! showing the columns and counts the README promises.
//!
//! Skipped automatically when the binaries aren't available (e.g. on a
//! clean checkout before `cargo build`).

use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

#[test]
fn top_once_renders_priorities_and_kinds() {
    let bin_dir = match find_target_bin_dir() {
        Some(p) => p,
        None => {
            eprintln!("skipping: target/debug binaries not found — run `cargo build` first");
            return;
        }
    };
    let coord = bin_dir.join("coord");
    if !coord.exists() {
        eprintln!("skipping: built binary missing");
        return;
    }

    let port = free_port();
    let url = format!("http://127.0.0.1:{port}");
    let tmp = tempdir("coord-tui");
    let db = tmp.join("state.db");

    let mut daemon = Command::new(&coord)
        .arg("serve")
        .arg("--addr")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--db")
        .arg(&db)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // Wait for the daemon to bind. ~25 attempts at 100ms = 2.5s ceiling.
    for _ in 0..25 {
        sleep(Duration::from_millis(100));
        if Command::new(&coord)
            .args(["--url", &url, "status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            break;
        }
    }

    let ctl = |args: &[&str]| -> String {
        let mut full = vec!["--url", &url];
        full.extend_from_slice(args);
        let out = Command::new(&coord).args(&full).output().expect("coord");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    ctl(&["heartbeat", "feature-a-v1.2", "--name", "feature-a"]);
    ctl(&["heartbeat", "docker-build", "--name", "docker"]);
    ctl(&[
        "send",
        "validate_token rejects valid tokens at day boundary",
        "--kind",
        "bug",
        "--priority",
        "urgent",
    ]);
    ctl(&[
        "send",
        "implement /me endpoint",
        "--kind",
        "feature",
        "--priority",
        "normal",
    ]);
    ctl(&[
        "send",
        "use JWT not sessions",
        "--kind",
        "decision",
        "--priority",
        "normal",
    ]);

    let output = Command::new(&coord)
        .args(["--url", &url, "top", "--once"])
        .output()
        .expect("top --once");
    let _ = daemon.kill();
    let _ = daemon.wait();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "top --once failed: {stdout}");

    // Header counts (the bug we shipped to fix: filtered count was hidden).
    assert!(
        stdout.contains("active"),
        "header missing 'active' agent bucket"
    );
    assert!(
        stdout.contains("idle"),
        "header missing 'idle' agent bucket"
    );
    assert!(
        stdout.contains("stale"),
        "header missing 'stale' agent bucket"
    );
    // bug + feature start pending; decision is an announcement so it's
    // completed-on-creation. Header should reflect that.
    assert!(
        stdout.contains("pending=2"),
        "expected pending=2 in header (bug + feature; decision is auto-completed), got:\n{stdout}"
    );
    assert!(
        stdout.contains("completed=1"),
        "expected completed=1 in header (the auto-completed decision), got:\n{stdout}"
    );
    assert!(stdout.contains("filter:"), "header missing filter label");

    // Tasks pane new columns.
    assert!(
        stdout.contains("PRIO"),
        "tasks pane missing PRIO column header"
    );
    assert!(
        stdout.contains("KIND"),
        "tasks pane missing KIND column header"
    );
    // The PRIO column may render as "‼ urgent" or get truncated to "‼ urgen"
    // depending on terminal width. Match either substring; the detail pane
    // also shows the full word when a task is selected.
    assert!(
        stdout.contains("urgent") || stdout.contains("urgen"),
        "expected urgent priority to render somewhere, got:\n{stdout}",
    );
    assert!(
        stdout.contains("decision") || stdout.contains("decis"),
        "expected decision kind to render, got:\n{stdout}",
    );

    // Detail pane on by default.
    assert!(
        stdout.contains("detail"),
        "detail pane should be visible by default"
    );
}

fn find_target_bin_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("CARGO_TARGET_DIR") {
        let dir = std::path::PathBuf::from(p).join("debug");
        if dir.exists() {
            return Some(dir);
        }
    }
    let local = std::env::current_dir().ok()?.join("target").join("debug");
    if local.exists() {
        Some(local)
    } else {
        None
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn tempdir(prefix: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("{prefix}-{}-{}", std::process::id(), rand_suffix()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn rand_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{n:x}")
}
