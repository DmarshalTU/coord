//! Protocol-level two-tab integration test.
//!
//! 0.4.0 shipped with primitives that worked in isolation but a
//! protocol-layer workflow that didn't: the second tab in a real
//! multi-tab session couldn't find what the first tab had done by
//! following the exact queries `AGENTS.md` told it to run, and the
//! first tab didn't reliably leave acks behind because that required
//! a second tool call.
//!
//! This test pins both fixes from 0.4.1:
//!
//!   1. `tasks/complete` with `postAck: true` writes the ack in the
//!      same transaction as the state change.
//!   2. The verification queries from `AGENTS.md` — and **only** those
//!      queries — let a second tab discover the work and the ack
//!      without knowing the source task UUID up front.
//!
//! If this test passes, the protocol-layer workflow is genuinely
//! tested end-to-end over HTTP, against a real daemon, mimicking what
//! two Claude Code / Cursor / Codex tabs would do.

use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

#[test]
fn two_tabs_can_coordinate_through_complete_with_ack() {
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
    let tmp = tempdir("coord-protocol");
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

    // === Tab A: "tab-alpha". Heartbeat, post work, claim it, do the
    // work, then complete with post_ack. This is the full protocol
    // path AGENTS.md prescribes for finishing meaningful work.
    let rpc = |method: &str, params: serde_json::Value| -> serde_json::Value {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let out = Command::new("curl")
            .args([
                "-sS",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-d",
                &body.to_string(),
                &url,
            ])
            .output()
            .expect("curl ran");
        assert!(
            out.status.success(),
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .unwrap_or_else(|e| panic!("decoding: {e}\n{}", String::from_utf8_lossy(&out.stdout)))
    };

    rpc(
        "agents/heartbeat",
        serde_json::json!({ "id": "tab-alpha", "name": "tab alpha" }),
    );

    let source = rpc(
        "tasks/send",
        serde_json::json!({
            "name": "fix discount math regression",
            "kind": "feature",
            "priority": "high",
            "payload": {"file": "src/lib.rs"}
        }),
    );
    let source_id = source["result"]["id"]
        .as_str()
        .expect("source task id")
        .to_string();

    let claim = rpc(
        "tasks/claim",
        serde_json::json!({
            "id": source_id,
            "agentId": "tab-alpha",
            "leaseSeconds": 60,
        }),
    );
    assert_eq!(claim["result"]["state"], "claimed");

    // The 0.4.1 one-call pattern: completing the work and posting the
    // ack in the same transaction.
    let complete = rpc(
        "tasks/complete",
        serde_json::json!({
            "id": source_id,
            "result": {"sha": "e56f5fa", "files": ["src/lib.rs"]},
            "postAck": true,
            "ackName": "v1.1 prod stable: discount math regression fixed",
            "ackPriority": "high",
            "ackPayload": {"sha": "e56f5fa", "branch": "fix/v1.1-discount-sign"}
        }),
    );
    assert_eq!(complete["result"]["ok"], true, "complete must succeed");
    let ack = complete["result"]["ack"]
        .as_object()
        .expect("ack object returned alongside complete");
    assert_eq!(ack["kind"], "ack");
    assert_eq!(ack["state"], "completed");
    assert_eq!(
        ack["name"], "v1.1 prod stable: discount math regression fixed",
        "ack name must round-trip"
    );
    assert_eq!(
        ack["payload"]["fixed_bug_id"],
        serde_json::Value::String(source_id.clone()),
        "daemon must auto-inject fixed_bug_id pointing at source task"
    );
    let ack_id = ack["id"].as_str().expect("ack has id").to_string();
    assert_ne!(ack_id, source_id, "ack must be a distinct row");

    // === Tab B: "tab-beta". Its job is to audit what tab-alpha just
    // did. It runs ONLY the verification queries from AGENTS.md, with
    // no advance knowledge of the source UUID or ack UUID. This is
    // the part 0.4.0 silently broke.

    rpc(
        "agents/heartbeat",
        serde_json::json!({ "id": "tab-beta", "name": "tab beta" }),
    );

    // Query 1: most recent acks. AGENTS.md prescribes:
    //   tasks_list { kind: "ack", limit: 50 }
    let acks = rpc(
        "tasks/list",
        serde_json::json!({"kind": "ack", "limit": 50}),
    );
    let acks_arr = acks["result"].as_array().expect("list returned array");
    let found_ack = acks_arr
        .iter()
        .find(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.contains("v1.1 prod stable"))
                .unwrap_or(false)
        })
        .expect(
            "tab-beta must discover the ack with no UUID knowledge, \
             just by filtering kind=ack",
        );
    assert_eq!(found_ack["state"], "completed");
    assert_eq!(found_ack["id"], ack_id);

    // Query 2: walk the ack's payload to the source task. This is the
    // verification recipe in AGENTS.md.
    let linked_source_id = found_ack["payload"]["fixed_bug_id"]
        .as_str()
        .expect("fixed_bug_id in ack payload");
    let fetched = rpc("tasks/get", serde_json::json!({"id": linked_source_id}));
    let source_t = fetched["result"]
        .as_object()
        .expect("source task is fetchable from the ack");
    assert_eq!(source_t["state"], "completed");
    assert_eq!(source_t["kind"], "feature");
    assert_eq!(source_t["name"], "fix discount math regression");
    assert_eq!(source_t["result"]["sha"], "e56f5fa");

    // Query 3: recent completed work (independent of ack). AGENTS.md
    // prescribes:
    //   tasks_list { state: "completed", kind: "feature", limit: 50 }
    let completed_features = rpc(
        "tasks/list",
        serde_json::json!({"state": "completed", "kind": "feature", "limit": 50}),
    );
    let cf_arr = completed_features["result"]
        .as_array()
        .expect("list returned array");
    assert!(
        cf_arr.iter().any(|t| t["id"] == source_id),
        "tab-beta must see the completed feature via the state+kind filter"
    );

    // Query 4: the broken-pre-0.4.0 trap. AGENTS.md tells agents to
    // scan state=pending for work. Make sure that query returns
    // exactly the right thing (i.e. no completed/ack rows leak), and
    // make sure tab-beta doesn't incorrectly conclude "nothing
    // happened" — which is exactly the bug the dogfood trace showed.
    let pending = rpc(
        "tasks/list",
        serde_json::json!({"state": "pending", "limit": 50}),
    );
    assert!(
        pending["result"].as_array().unwrap().is_empty(),
        "pending bulletin is empty when all work is done"
    );
    // The point of this whole test: an empty pending list MUST NOT be
    // the only signal a verifier consults. The verification queries
    // above already proved that — this assertion is just here to
    // document the trap and make the failure mode obvious if a future
    // refactor swaps the filters around.

    let _ = daemon.kill();
    let _ = daemon.wait();
}

/// Regression marker for the 0.4.1 dogfood miss.
///
/// The failure we observed in the wild: an agent skipped
/// `tasks_send` + `tasks_claim` entirely and posted only a
/// standalone `kind=ack` row at the end. The ledger held exactly one
/// row, and tab B's `AGENTS.md` verification recipe (walk
/// `fixed_bug_id` to fetch the source task) had nothing to walk to.
///
/// At the wire level this is *allowed* — there's no daemon-side
/// enforcement that an ack must have a `fixed_bug_id`, because the
/// rare-exception path (publishing an external event) legitimately
/// wants standalone acks. This test pins both halves:
///
///   - the wire accepts a bare ack (so the rare-exception path keeps
///     working), AND
///   - the resulting ack lacks `fixed_bug_id`, so a verifier following
///     `AGENTS.md` can detect the absence and either (a) make do with
///     the ack's own payload, or (b) flag the producer for skipping
///     the front of the protocol.
///
/// If a future change adds server-side enforcement (e.g. "acks must
/// have fixed_bug_id"), this test must be updated to expect a
/// rejection instead — it is the marker for that decision.
#[test]
fn standalone_ack_is_allowed_but_unwalkable() {
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
    let tmp = tempdir("coord-standalone-ack");
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

    let rpc = |method: &str, params: serde_json::Value| -> serde_json::Value {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let out = Command::new("curl")
            .args([
                "-sS",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-d",
                &body.to_string(),
                &url,
            ])
            .output()
            .expect("curl ran");
        assert!(out.status.success());
        serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("decode response")
    };

    rpc(
        "agents/heartbeat",
        serde_json::json!({ "id": "skipper", "name": "skipper" }),
    );

    // Skip `tasks/send` for source + `tasks/claim` + `tasks/complete`
    // entirely, and just publish an ack. This is what 0.4.1 dogfood
    // tab A actually did.
    let bare = rpc(
        "tasks/send",
        serde_json::json!({
            "name": "greet v1 ready for review",
            "kind": "ack",
            "priority": "high",
            "payload": {"sha": "deadbeef", "files": ["greet.py"]}
        }),
    );
    let bare_obj = bare["result"]
        .as_object()
        .expect("bare ack publish must succeed at the wire level");
    assert_eq!(bare_obj["kind"], "ack");
    assert_eq!(
        bare_obj["state"], "completed",
        "ack kind is completed-on-creation"
    );

    // The diagnostic: there is no fixed_bug_id, because the producer
    // never declared a source task. A verifier tab following AGENTS.md
    // can detect this and refuse to claim the work is fully traceable.
    let payload = bare_obj["payload"].as_object().expect("payload object");
    assert!(
        !payload.contains_key("fixed_bug_id"),
        "standalone ack must NOT have a fixed_bug_id — that's the diagnostic \
         signal that the producer skipped the front of the protocol"
    );

    // Belt-and-braces: the only row in the ledger is the bare ack.
    // There is no feature/bug row for tab B to walk to. This is the
    // exact lopsided shape we observed in the wild.
    let everything = rpc("tasks/list", serde_json::json!({"limit": 50}));
    let arr = everything["result"].as_array().unwrap();
    assert_eq!(
        arr.len(),
        1,
        "skipped front-of-protocol leaves only the bare ack in the ledger"
    );
    assert_eq!(arr[0]["kind"], "ack");

    let _ = daemon.kill();
    let _ = daemon.wait();
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
