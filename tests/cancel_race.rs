//! Cancel-vs-complete race regression test. Once a task is cancelled,
//! a late completion call must NOT silently overwrite the cancellation.
//! This was a real bug in v0.1.

use std::sync::Arc;

use coord::core::{store::Store, types::TaskState};

#[test]
fn cancel_is_sticky_against_late_complete() {
    let dir = tempdir();
    let store = Arc::new(Store::open(&dir.join("cancel.db")).unwrap());

    let task = store
        .create_task("auth-fix", serde_json::json!({}))
        .unwrap();
    store.heartbeat("worker-1", "worker").unwrap();
    let claimed = store.claim_task(task.id, "worker-1").unwrap();
    assert!(claimed.is_some(), "first claim should win");

    // Operator cancels the task while the worker is still 'working'.
    store.cancel_task(task.id).unwrap();

    // Worker finishes obliviously and tries to mark complete.
    let completed = store
        .complete_task(task.id, serde_json::json!({"result": 42}))
        .unwrap();
    assert!(
        !completed,
        "complete after cancel must report failure, not silently win"
    );

    let final_state = store.get_task(task.id).unwrap().unwrap();
    assert_eq!(
        final_state.state,
        TaskState::Cancelled,
        "cancel must be sticky"
    );
    assert!(
        final_state.result.is_none(),
        "cancelled task must not record a worker result"
    );
}

fn tempdir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("coord-cancel-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
