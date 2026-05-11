//! `tasks/list` long-poll integration test.
//!
//! Spins up a real `coord serve`, fires a long-poll `tasks/list` from
//! one client and a `tasks/send` from another a moment later, and
//! asserts the long-poll returns the new task *quickly* — well under
//! the polling cadence that the pre-0.4 implementation would have
//! used. This is the test that proves the watch primitive is now
//! server-pushed, not client-polled.

use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const WAIT_MS: u64 = 10_000;

#[test]
fn list_long_poll_unblocks_quickly_after_matching_send() {
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
    let tmp = tempdir("coord-longpoll");
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

    for _ in 0..40 {
        sleep(Duration::from_millis(75));
        let ok = Command::new(&coord)
            .args(["--url", &url, "status"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            break;
        }
    }

    // Start the long-poll in a child shell process so we can measure
    // the wall-clock between `tasks/send` and the long-poll returning.
    // curl is the simplest portable way to fire a single JSON-RPC
    // call and time it.
    let url_clone = url.clone();
    let waiter = std::thread::spawn(move || {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tasks/list","params":{{"limit":10,"state":"pending","kind":"bug","wait_ms":{WAIT_MS}}}}}"#
        );
        let started = Instant::now();
        let out = Command::new("curl")
            .args([
                "-sS",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-d",
                &body,
                "--max-time",
                "30",
                &url_clone,
            ])
            .output()
            .expect("curl ran");
        (started.elapsed(), out)
    });

    // Give the waiter a beat to actually subscribe to the bus before
    // we send the matching task.
    sleep(Duration::from_millis(300));

    let send = Command::new(&coord)
        .args([
            "--url",
            &url,
            "send",
            "urgent-bug",
            "--kind",
            "bug",
            "--priority",
            "high",
        ])
        .output()
        .expect("send ran");
    assert!(
        send.status.success(),
        "send failed: {}",
        String::from_utf8_lossy(&send.stderr)
    );

    let (elapsed, curl_out) = waiter.join().expect("waiter thread");
    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(
        curl_out.status.success(),
        "curl failed: {}",
        String::from_utf8_lossy(&curl_out.stderr)
    );

    let body: serde_json::Value =
        serde_json::from_slice(&curl_out.stdout).expect("response is JSON");
    let result = body.get("result").expect("rpc result").clone();
    let arr = result.as_array().expect("result is array");
    assert!(
        !arr.is_empty(),
        "long-poll returned empty but a matching task was sent: {body:#}"
    );
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    assert!(
        names.contains(&"urgent-bug"),
        "long-poll returned the wrong task: {names:?}"
    );

    // The whole point: this must finish much sooner than WAIT_MS. We
    // give ourselves 2 seconds of slack for slow CI runners and the
    // 300ms head-start sleep above.
    assert!(
        elapsed < Duration::from_secs(3),
        "long-poll took {elapsed:?}; should unblock within ~300ms of the matching send"
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
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("{prefix}-{}-{n:x}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
