//! Smoke test for the file-backed `Store` — notes, human tasks, settings,
//! action log round-trip through real files on disk (a tempdir), not mocks.

use mazz_flux_bot::models::{CreateProjectRequest, ProjectStatus};
use mazz_flux_bot::store::Store;

#[tokio::test]
async fn store_round_trips_everything() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    let project = store
        .create_project(CreateProjectRequest { name: "t".into(), goal: "g".into(), constellation: None })
        .await
        .expect("create project");

    store.set_project_status(&project.id, ProjectStatus::Running, Some("started")).await.unwrap();
    let fetched = store.get_project(&project.id).await.unwrap().unwrap();
    assert_eq!(fetched.status, "running");
    assert_eq!(fetched.last_note.as_deref(), Some("started"));

    let note = store.add_project_note(&project.id, "# hello\nsome markdown").await.unwrap();
    let notes = store.list_project_notes(&project.id).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].content, "# hello\nsome markdown");
    assert_eq!(notes[0].id, note.id);
    assert!(!notes[0].created_at.is_empty());

    let task = store.create_human_task(&project.id, "needs a human").await.unwrap();
    let open = store.list_human_tasks(Some(&project.id), true).await.unwrap();
    assert_eq!(open.len(), 1);
    store.resolve_human_task(&task.id).await.unwrap();
    let open_after = store.list_human_tasks(Some(&project.id), true).await.unwrap();
    assert_eq!(open_after.len(), 0);
    let all = store.list_human_tasks(Some(&project.id), false).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, "resolved");

    store.set_setting("openrouter_api_key", "sk-test").await.unwrap();
    assert_eq!(store.get_setting("openrouter_api_key").await.unwrap(), Some("sk-test".to_string()));
    store.set_setting("openrouter_api_key", "").await.unwrap();
    assert_eq!(store.get_setting("openrouter_api_key").await.unwrap(), None);

    store.log_action(Some(&project.id), None, "did_a_thing", None, None, None).await.unwrap();
    let log = store.list_action_log(Some(&project.id), 10).await.unwrap();
    assert!(log.iter().any(|e| e.action == "did_a_thing"));

    std::fs::remove_dir_all(&dir).ok();
}

fn tempfile_dir() -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("mfb-store-test-{}", uuid::Uuid::new_v4()));
    dir
}
