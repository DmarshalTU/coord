//! Multi-process scaling test.
//!
//! Spins up a real `coord serve` daemon and fires N concurrent client
//! processes at it (mimicking N IDE tabs / agent apps). Each client
//! creates a task, then claims a different one. Asserts:
//!
//!   1. Every claim either wins exactly one task or fails cleanly — no
//!      task ever ends up with two claimers.
//!   2. The daemon survives the burst without dropping requests.
//!
//! This is the "works with more than 2 agents and more than 2 apps"
//! claim from the README, demonstrated end-to-end over HTTP.

use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

const NUM_CLIENTS: usize = 16;

#[test]
fn many_concurrent_clients_share_one_daemon_safely() {
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
    let tmp = tempdir("coord-multi");
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

    // Wait until the daemon is responsive.
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

    // Seed N pending tasks, one per client.
    let mut task_ids = Vec::with_capacity(NUM_CLIENTS);
    for i in 0..NUM_CLIENTS {
        let out = Command::new(&coord)
            .args(["--url", &url, "send", &format!("multi-task-{i}")])
            .output()
            .expect("send");
        assert!(
            out.status.success(),
            "send failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("send returns json");
        task_ids.push(
            v.get("id")
                .and_then(|s| s.as_str())
                .expect("id")
                .to_string(),
        );
    }

    // Spawn N independent client processes that race to claim every
    // task. Each writes its successful claim IDs to stdout, one per
    // line.
    let mut children = Vec::with_capacity(NUM_CLIENTS);
    for c in 0..NUM_CLIENTS {
        let agent = format!("multi-agent-{c}");
        // Heartbeat first so the agent shows up in agents/list.
        let _ = Command::new(&coord)
            .args(["--url", &url, "heartbeat", &agent, "--name", "multi"])
            .output();
        let mut script = String::new();
        for tid in &task_ids {
            script.push_str(&format!(
                "{coord} --url {url} claim {tid} --as {agent} >/dev/null 2>&1 && echo {tid}\n",
                coord = coord.display(),
                url = url,
                tid = tid,
                agent = agent,
            ));
        }
        let child = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn client");
        children.push(child);
    }

    let mut all_wins: Vec<(usize, String)> = Vec::new();
    for (idx, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("client output");
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let id = line.trim();
            if !id.is_empty() {
                all_wins.push((idx, id.to_string()));
            }
        }
    }

    let _ = daemon.kill();
    let _ = daemon.wait();

    // Every task must be claimed by exactly one agent.
    let mut by_task: std::collections::HashMap<String, Vec<usize>> = Default::default();
    for (idx, id) in &all_wins {
        by_task.entry(id.clone()).or_default().push(*idx);
    }
    let claimed_ids: HashSet<&String> = by_task.keys().collect();
    assert_eq!(
        claimed_ids.len(),
        NUM_CLIENTS,
        "expected every seeded task to be claimed exactly once; got {} unique winners for {} tasks",
        claimed_ids.len(),
        NUM_CLIENTS
    );
    for (id, winners) in &by_task {
        assert_eq!(
            winners.len(),
            1,
            "task {id} was claimed by {n} clients (race lost!): {winners:?}",
            id = id,
            n = winners.len(),
            winners = winners
        );
    }
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
