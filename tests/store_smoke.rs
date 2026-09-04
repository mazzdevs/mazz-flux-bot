//! Smoke test for the file-backed `Store` — notes, human tasks, settings,
//! action log round-trip through real files on disk (a tempdir), not mocks.

use mazz_flux_bot::models::{CreateProjectRequest, KanbanStatus, ProjectStatus};
use mazz_flux_bot::store::Store;

#[tokio::test]
async fn store_round_trips_everything() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    let project = store
        .create_project(CreateProjectRequest { name: Some("t".into()), goal: "g".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None })
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
async fn archetype_crud_and_defaults() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    // Default model applies when omitted.
    let a = store.create_archetype("Test Archetype", "does test things", None).await.unwrap();
    assert_eq!(a.slug, "test-archetype");
    assert_eq!(a.preferred_model, "openai/gpt-5.6-sol-pro");
    assert_eq!(a.description, "does test things");

    let fetched = store.get_archetype("test-archetype").await.unwrap().unwrap();
    assert_eq!(fetched.name, "Test Archetype");

    // Custom model round-trips.
    let b = store.create_archetype("Custom Model", "uses a custom model", Some("anthropic/claude-sonnet-5")).await.unwrap();
    assert_eq!(b.preferred_model, "anthropic/claude-sonnet-5");

    let all = store.list_archetypes().await.unwrap();
    assert_eq!(all.len(), 2);

    // Slug collision is rejected, not silently overwritten.
    assert!(store.create_archetype("Test Archetype", "different description", None).await.is_err());

    // Partial update leaves other fields untouched.
    let updated = store.update_archetype("test-archetype", None, Some("updated description"), None).await.unwrap();
    assert_eq!(updated.name, "Test Archetype");
    assert_eq!(updated.description, "updated description");
    assert_eq!(updated.preferred_model, "openai/gpt-5.6-sol-pro");

    store.delete_archetype("test-archetype").await.unwrap();
    assert_eq!(store.get_archetype("test-archetype").await.unwrap(), None);
    assert_eq!(store.list_archetypes().await.unwrap().len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn archetype_seeding_is_idempotent_per_slug() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    store.seed_default_archetypes().await.unwrap();
    let seeded = store.list_archetypes().await.unwrap();
    assert_eq!(seeded.len(), 5);
    let names: Vec<&str> = seeded.iter().map(|a| a.name.as_str()).collect();
    for expected in ["Coder", "Researcher", "Planner", "Reviewer", "Designer"] {
        assert!(names.contains(&expected), "missing seeded archetype: {expected}");
    }

    // Editing a seeded archetype and re-seeding must leave the edit intact
    // — seeding only fires for a slug when NO archetype with that slug
    // exists at all, not on every startup unconditionally.
    store.update_archetype("coder", None, Some("a customized description"), None).await.unwrap();
    store.seed_default_archetypes().await.unwrap();
    let coder = store.get_archetype("coder").await.unwrap().unwrap();
    assert_eq!(coder.description, "a customized description");

    // Deleting one entirely and re-seeding DOES bring it back — its slug no
    // longer exists, so the seed guard's condition is met again.
    store.delete_archetype("researcher").await.unwrap();
    assert_eq!(store.list_archetypes().await.unwrap().len(), 4);
    store.seed_default_archetypes().await.unwrap();
    let after = store.list_archetypes().await.unwrap();
    assert_eq!(after.len(), 5);
    assert!(store.get_archetype("researcher").await.unwrap().is_some());

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn memory_overwrites_not_appends() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.expect("open store");

    let project = store
        .create_project(CreateProjectRequest { name: Some("t".into()), goal: "g".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None })
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
        .create_project(CreateProjectRequest { name: Some("t".into()), goal: "original goal".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None })
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

#[tokio::test]
async fn kanban_board_crud_and_missing_board_are_safe() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.unwrap();
    let project = store.create_project(CreateProjectRequest { name: Some("Kanban test".into()), goal: "exercise storage".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None }).await.unwrap();
    let board_path = dir.join("kanban").join(format!("{}.json", project.id));
    assert!(board_path.exists());
    assert!(store.get_kanban_board(&project.id).await.unwrap().tasks.is_empty());

    std::fs::remove_file(&board_path).unwrap();
    assert!(store.get_kanban_board(&project.id).await.unwrap().tasks.is_empty());
    let task = store.create_kanban_task(&project.id, "Add endpoint", "Implement and test it", KanbanStatus::Assigned).await.unwrap();
    let updated = store.update_kanban_task(&project.id, &task.id, None, None, Some(KanbanStatus::InProgress)).await.unwrap().unwrap();
    assert_eq!(updated.status, KanbanStatus::InProgress);

    let before = std::fs::read_to_string(&board_path).unwrap();
    assert!(store.update_kanban_task(&project.id, "missing-task", None, None, Some(KanbanStatus::Done)).await.unwrap().is_none());
    assert_eq!(std::fs::read_to_string(&board_path).unwrap(), before);
    assert!(store.delete_kanban_task(&project.id, &task.id).await.unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn deleting_project_removes_its_kanban_file() {
    let dir = tempfile_dir();
    let store = Store::open(&dir).await.unwrap();
    let project = store.create_project(CreateProjectRequest { name: Some("Delete test".into()), goal: "delete cleanly".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None }).await.unwrap();
    let board_path = dir.join("kanban").join(format!("{}.json", project.id));
    assert!(board_path.exists());
    store.delete_project(&project.id).await.unwrap();
    assert!(!board_path.exists());
    assert!(store.get_project(&project.id).await.unwrap().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

fn tempfile_dir() -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("mfb-store-test-{}", uuid::Uuid::new_v4()));
    dir
}
