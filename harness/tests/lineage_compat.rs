use harness::hub::lineage::{Lineage, RootId};
use harness::scheduler::{JsonlQueueStore, QueueStore, QueueStoreError, QueuedTask};
use harness::task::Task;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lineage")
        .join(name)
}

fn queue_copy(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().join("queue.jsonl");
    std::fs::copy(fixture("queue_pre_lineage.jsonl"), &path).unwrap();
    path
}

#[test]
fn a_queue_written_before_lineage_loads_with_no_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlQueueStore::open(&queue_copy(&dir)).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(loaded.iter().all(|t| t.lineage.is_none()));
    assert_eq!(loaded[0].description, "plain task");
    assert_eq!(loaded[1].origin.as_ref().unwrap().issue, 42);
}

#[test]
fn loading_a_pre_lineage_queue_does_not_add_a_lineage_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = queue_copy(&dir);
    JsonlQueueStore::open(&path).unwrap().load().unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("lineage"));
}

#[test]
fn a_task_json_written_before_lineage_deserializes() {
    let text = std::fs::read_to_string(fixture("task_pre_lineage.json")).unwrap();
    let task: Task = serde_json::from_str(&text).unwrap();
    assert!(task.lineage.is_none());
    assert_eq!(task.description, "plain task");
}

#[test]
fn a_lineage_survives_the_queue_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.jsonl");
    let lineage = Lineage::root_child(RootId::backlog("example/repo", 9).unwrap());
    let entry =
        QueuedTask::new("d", "/repo".into(), "b", "m", 1).with_lineage(Some(lineage.clone()));
    JsonlQueueStore::open(&path)
        .unwrap()
        .insert(&entry)
        .unwrap();
    let reopened = JsonlQueueStore::open(&path).unwrap().load().unwrap();
    assert_eq!(reopened[0].lineage, Some(lineage));
}

#[test]
fn a_forged_lineage_in_the_queue_file_is_refused_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let path = queue_copy(&dir);
    let text = std::fs::read_to_string(&path).unwrap();
    let forged = text.replacen(
        r#""origin":null}"#,
        r#""origin":null,"lineage":{"root":{"kind":"plan","id":"x"},"parent":null,"depth":7}}"#,
        1,
    );
    assert_ne!(forged, text);
    std::fs::write(&path, forged).unwrap();
    let err = JsonlQueueStore::open(&path).unwrap().load().unwrap_err();
    assert!(matches!(err, QueueStoreError::Serde(_)), "{err}");
}
