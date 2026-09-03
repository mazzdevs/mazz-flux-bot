//! Unit coverage for heartbeat decision-parsing (including the multi-task
//! create_human_task shape) and instance naming — pure functions, no
//! network/store involved.

use mazz_flux_bot::heartbeat::{instance_name, parse_conductor_response, slugify};

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
