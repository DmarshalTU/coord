//! Hammer the atomic claim primitive from many threads. Exactly one
//! claimer must win each task. This is the core correctness guarantee
//! of coord — everything else is UX on top.

use std::sync::Arc;
use std::thread;

use coord::core::{store::Store, types::TaskState};

#[test]
fn one_claimer_per_task_under_concurrent_load() {
    let dir = tempdir();
    let db_path = dir.join("race.db");
    let store = Arc::new(Store::open(&db_path).expect("open store"));

    const TASKS: usize = 200;
    const CLAIMERS_PER_TASK: usize = 8;

    let mut task_ids = Vec::with_capacity(TASKS);
    for i in 0..TASKS {
        let t = store
            .create_task(&format!("race-{i}"), serde_json::json!({ "i": i }))
            .expect("create");
        task_ids.push(t.id);
    }

    // Sanity-check default kind+priority survived the schema migration.
    let sample = store.get_task(task_ids[0]).unwrap().unwrap();
    assert_eq!(sample.kind, "task");
    assert_eq!(sample.priority, "normal");

    let mut handles = Vec::new();
    for task_id in &task_ids {
        for c in 0..CLAIMERS_PER_TASK {
            let store = store.clone();
            let id = *task_id;
            let agent = format!("claimer-{c}");
            handles.push(thread::spawn(move || {
                store.claim_task(id, &agent, None).unwrap().is_some()
            }));
        }
    }

    let wins: usize = handles
        .into_iter()
        .map(|h| h.join().unwrap() as usize)
        .sum();
    assert_eq!(
        wins, TASKS,
        "expected exactly one win per task (got {wins} wins for {TASKS} tasks)"
    );

    for id in &task_ids {
        let task = store.get_task(*id).unwrap().unwrap();
        assert_eq!(
            task.state,
            TaskState::Claimed,
            "task {id} should be claimed"
        );
        assert!(
            task.claimed_by.is_some(),
            "task {id} should record its winner"
        );
    }
}

fn tempdir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("coord-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
