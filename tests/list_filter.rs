//! Regression test for the pre-0.4 "filter after LIMIT" bug.
//!
//! Before 0.4, `tasks/list` pulled `LIMIT N` rows ordered by `created_at
//! DESC` and *then* `retain()`-ed by state/kind/priority in Rust. With
//! a busy bulletin this silently dropped matches: a watcher that asked
//! for "the most recent `bug`" wouldn't see one if 50 newer
//! non-`bug` rows pushed it past the limit.
//!
//! This test seeds 60 non-matching rows followed by 1 matching older
//! row. With the SQL-side filter applied first, the matching row
//! still comes back. With the old in-memory filter, the result would
//! be empty.

use std::sync::Arc;

use coord::core::store::{Store, TaskFilter};

#[test]
fn filter_pushdown_finds_match_beyond_naive_limit() {
    let dir = tempdir("filter");
    let store = Arc::new(Store::open(&dir.join("db")).unwrap());

    // 60 normal `task` rows. These come back first when ordered by
    // created_at DESC, so they'd shadow any filter applied in-memory
    // after a `LIMIT 50` fetch.
    for i in 0..60 {
        let _ = store
            .create_task(&format!("noise-{i}"), serde_json::json!({"i": i}))
            .unwrap();
    }

    // One matching row. Same kind doesn't matter; we want to filter
    // by `state='completed'` (which announcement kinds get on create),
    // so use `ack` here.
    let ack = store
        .create_task_full(
            "the-one-ack",
            "ack",
            "high",
            serde_json::json!({"summary": "v1.1 stable"}),
        )
        .unwrap();

    let pending = TaskFilter {
        state: Some("completed".into()),
        kind: Some("ack".into()),
        priority: None,
    };
    let found = store.list_tasks_filtered(50, &pending).unwrap();
    let ids: Vec<String> = found.iter().map(|t| t.id.to_string()).collect();
    assert!(
        ids.contains(&ack.id.to_string()),
        "filter pushdown must surface the matching ack even though \
         60 newer non-ack rows exist; got: {ids:?}"
    );

    // And filters compose: only the matching ack should come back.
    assert_eq!(
        found.len(),
        1,
        "exactly one row matches state=completed AND kind=ack; got {}",
        found.len()
    );
}

fn tempdir(prefix: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("coord-{prefix}-{}-{n:x}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
