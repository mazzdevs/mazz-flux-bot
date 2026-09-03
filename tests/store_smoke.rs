//! Smoke test for the file-backed `Store` — notes, human tasks, settings,
//! action log round-trip through real files on disk (a tempdir), not mocks.

use mazz_flux_bot::models::{CreateProjectRequest, ProjectStatus};
use mazz_flux_bot::store::Store;

#[tokio::test]
async fn store_round_trips_everything() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    let project = store
        .create_project(CreateProjectRequest { name: "t".into(), goal: "g".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None })
        .await
        .expect("create project");

    store.set_project_status(&project.id, ProjectStatus::Running, Some("started")).await.unwrap();
    let fetched = store.get_project(&project.id).await.unwrap().unwrap();
    assert_eq!(fetched.status, "running");
    assert_eq!(fetched.last_note.as_deref(), Some("started"));

    assert_eq!(project.heartbeat_interval_secs, mazz_flux_bot::heartbeat::DEFAULT_HEARTBEAT_INTERVAL_SECS);
    store.set_heartbeat_interval(&project.id, 90).await.unwrap();
    let updated = store.get_project(&project.id).await.unwrap().unwrap();
    assert_eq!(updated.heartbeat_interval_secs, 90);
    // Below-minimum values are clamped, not rejected.
    store.set_heartbeat_interval(&project.id, 1).await.unwrap();
    let clamped = store.get_project(&project.id).await.unwrap().unwrap();
    assert_eq!(clamped.heartbeat_interval_secs, 5);

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

#[tokio::test]
async fn memory_overwrites_not_appends() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    let project = store
        .create_project(CreateProjectRequest { name: "t".into(), goal: "g".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None })
        .await
        .unwrap();

    assert_eq!(store.read_memory(&project.id).await.unwrap(), None);

    store.write_memory(&project.id, "first summary").await.unwrap();
    assert_eq!(store.read_memory(&project.id).await.unwrap(), Some("first summary".to_string()));

    // Second write must fully replace, not append.
    store.write_memory(&project.id, "second summary, much shorter").await.unwrap();
    let content = store.read_memory(&project.id).await.unwrap().unwrap();
    assert_eq!(content, "second summary, much shorter");
    assert!(!content.contains("first summary"));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn goal_and_heartbeat_prompt_editable() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    let project = store
        .create_project(CreateProjectRequest { name: "t".into(), goal: "original goal".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None })
        .await
        .unwrap();
    assert_eq!(project.heartbeat_prompt, None);

    store.set_goal(&project.id, "updated goal").await.unwrap();
    store.set_heartbeat_prompt(&project.id, "focus on tests this tick").await.unwrap();
    let updated = store.get_project(&project.id).await.unwrap().unwrap();
    assert_eq!(updated.goal, "updated goal");
    assert_eq!(updated.heartbeat_prompt, Some("focus on tests this tick".to_string()));

    // Empty string clears it back to None.
    store.set_heartbeat_prompt(&project.id, "").await.unwrap();
    let cleared = store.get_project(&project.id).await.unwrap().unwrap();
    assert_eq!(cleared.heartbeat_prompt, None);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn file_browser_reads_writes_and_blocks_escapes() {
    use mazz_flux_bot::store::BrowseResult;

    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    // Root listing starts empty.
    match store.browse("").await.unwrap() {
        BrowseResult::Dir { entries, .. } => assert!(entries.is_empty()),
        _ => panic!("expected dir"),
    }

    // Write a new file, list it, read it back, edit it, delete it.
    store.write_file("notes/scratch.md", "# hello").await.unwrap();
    match store.browse("notes").await.unwrap() {
        BrowseResult::Dir { entries, .. } => assert_eq!(entries.len(), 1),
        _ => panic!("expected dir"),
    }
    match store.browse("notes/scratch.md").await.unwrap() {
        BrowseResult::File { content, .. } => assert_eq!(content, "# hello"),
        _ => panic!("expected file"),
    }
    store.write_file("notes/scratch.md", "# edited").await.unwrap();
    match store.browse("notes/scratch.md").await.unwrap() {
        BrowseResult::File { content, .. } => assert_eq!(content, "# edited"),
        _ => panic!("expected file"),
    }
    store.delete_file("notes/scratch.md").await.unwrap();
    assert!(store.browse("notes/scratch.md").await.is_err());

    // Path escapes are rejected.
    assert!(store.browse("../../etc/passwd").await.is_err());
    assert!(store.write_file("../escape.md", "nope").await.is_err());

    // Dotfiles/dirs (notably the state repo's own .git) are blocked too.
    assert!(store.browse(".git").await.is_err());
    assert!(store.write_file(".git/config", "nope").await.is_err());

    std::fs::remove_dir_all(&dir).ok();
}

fn tempfile_dir() -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("mfb-store-test-{}", uuid::Uuid::new_v4()));
    dir
}
