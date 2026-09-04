//! Unit coverage for heartbeat decision-parsing (including the multi-task
//! create_human_task shape) and instance naming — pure functions, no
//! network/store involved.

use mazz_flux_bot::heartbeat::{append_live_agent_context, apply_kanban_actions, apply_live_agent_context, instance_name, parse_conductor_response, slugify, KanbanAction};
use mazz_flux_bot::models::{CreateProjectRequest, KanbanStatus};
use mazz_flux_bot::public_url::{PublicUrlResolution, PublicUrlSource};
use mazz_flux_bot::store::Store;

#[test]
fn slugify_basic() {
    assert_eq!(slugify("Fix login bug"), "fix-login-bug");
    assert_eq!(slugify("  leading/trailing spaces  "), "leading-trailing-spaces");
    assert_eq!(slugify("Multiple---dashes"), "multiple-dashes");
    assert_eq!(slugify("UPPER_CASE_project"), "upper-case-project");
}

#[test]
fn slugify_clamps_length() {
    let long = "a".repeat(40);
    let slug = slugify(&long);
    assert!(slug.len() <= 24);
}

#[test]
fn slugify_empty_for_symbols_only() {
    assert_eq!(slugify("!!!"), "");
    assert_eq!(slugify(""), "");
    assert_eq!(slugify("🎉🎉🎉"), "");
}

#[test]
fn instance_name_uses_project_name_slug() {
    let id = "309a658c-2221-46ef-a210-2ff871d96e27";
    let name = instance_name("Fix login bug", id);
    assert_eq!(name, "fix-login-bug-309a65");
}

#[test]
fn instance_name_falls_back_when_slug_empty() {
    let id = "309a658c-2221-46ef-a210-2ff871d96e27";
    let name = instance_name("!!!", id);
    assert_eq!(name, "mfb-309a658c");
}

#[test]
fn create_human_task_with_multiple_tasks_parses_array() {
    let raw = r#"{"action": "create_human_task", "tasks": ["blocker one", "blocker two", "blocker three"], "note": "three separate asks"}"#;
    let decision = parse_conductor_response(raw);
    assert_eq!(decision.action, "create_human_task");
    assert_eq!(decision.tasks, Some(vec!["blocker one".to_string(), "blocker two".to_string(), "blocker three".to_string()]));
}

#[test]
fn create_human_task_with_single_message_still_works() {
    let raw = r#"{"action": "create_human_task", "message": "one blocker", "note": "single ask"}"#;
    let decision = parse_conductor_response(raw);
    assert_eq!(decision.action, "create_human_task");
    assert_eq!(decision.message, Some("one blocker".to_string()));
    assert_eq!(decision.tasks, None);
}

#[test]
fn memory_field_round_trips() {
    let raw = r#"{"action": "wait", "note": "still working", "memory": "compacted summary of progress so far"}"#;
    let decision = parse_conductor_response(raw);
    assert_eq!(decision.action, "wait");
    assert_eq!(decision.memory, Some("compacted summary of progress so far".to_string()));
}

#[test]
fn memory_field_absent_is_none() {
    let raw = r#"{"action": "wait", "note": "still working"}"#;
    let decision = parse_conductor_response(raw);
    assert_eq!(decision.memory, None);
}

#[test]
fn send_message_can_include_kanban_transition_in_same_decision() {
    let raw = r#"{"action":"send_message","message":"Use the Coder archetype.","kanban_actions":[{"action":"update_task","task_id":"task-123","status":"in_progress"}]}"#;
    let decision = parse_conductor_response(raw);
    assert_eq!(decision.action, "send_message");
    match &decision.kanban_actions[0] {
        KanbanAction::UpdateTask { task_id, status, .. } => {
            assert_eq!(task_id, "task-123");
            assert_eq!(*status, Some(KanbanStatus::InProgress));
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

#[test]
fn live_agent_context_is_appended_without_rewriting_composed_prompt() {
    let composed = "Inspect the repository and begin with the assigned work.";
    let result = append_live_agent_context(composed, "project-123", "https://preview-4270--bot.example");
    assert!(result.starts_with(composed));
    assert_eq!(&result[..composed.len()], composed);
    assert!(result.contains("Project ID: `project-123`"));
    assert!(result.contains("Bot API: `https://preview-4270--bot.example`"));
    assert!(result.contains("https://preview-4270--bot.example/api/projects/project-123/agent-context"));
    assert!(result.contains("curl --fail --silent --show-error"));
    assert!(result.contains("source of truth"));
    assert!(result.contains("Do not call other mazz-flux-bot endpoints"));
    assert!(!result.contains("archetypes\": ["));
}

#[test]
fn unavailable_public_url_preserves_composed_prompt() {
    let composed = "Keep this conductor-authored text unchanged.".to_string();
    let resolution = PublicUrlResolution { url: None, source: PublicUrlSource::Unavailable };
    let (result, attached) = apply_live_agent_context(composed.clone(), "project-123", &resolution);
    assert_eq!(result, composed);
    assert!(!attached);
}

#[tokio::test]
async fn conductor_kanban_actions_apply_and_missing_ids_are_safe() {
    let mut dir = std::env::temp_dir();
    dir.push(format!("mfb-heartbeat-kanban-test-{}", uuid::Uuid::new_v4()));
    let store = Store::open(&dir).await.unwrap();
    let project = store.create_project(CreateProjectRequest { name: Some("Board actions".into()), goal: "test board actions".into(), heartbeat_prompt: None, constellation: None, heartbeat_interval_secs: None }).await.unwrap();
    let task = store.create_kanban_task(&project.id, "Work item", "Do the work", KanbanStatus::Assigned).await.unwrap();

    apply_kanban_actions(&store, &project.id, None, &[
        KanbanAction::UpdateTask { task_id: task.id.clone(), title: None, description: None, status: Some(KanbanStatus::InProgress) },
        KanbanAction::UpdateTask { task_id: "missing-task".into(), title: None, description: None, status: Some(KanbanStatus::Done) },
        KanbanAction::CreateTask { title: "Follow-up".into(), description: "Run final checks".into(), status: Some(KanbanStatus::Assigned) },
    ]).await.unwrap();

    let board = store.get_kanban_board(&project.id).await.unwrap();
    assert_eq!(board.tasks.len(), 2);
    assert_eq!(board.tasks.iter().find(|item| item.id == task.id).unwrap().status, KanbanStatus::InProgress);
    assert!(board.tasks.iter().any(|item| item.title == "Follow-up"));
    let log = store.list_action_log(Some(&project.id), 20).await.unwrap();
    assert!(log.iter().any(|entry| entry.action == "kanban_action_rejected"));
    std::fs::remove_dir_all(&dir).ok();
}
