//! Serialization coverage for request/response shapes that talk to the real
//! vape API — catches accidental field drops/renames.

use mazz_flux_bot::models::{CreateInstanceRequest, CreateProjectRequest, JobConfig, Project};

#[test]
fn job_config_model_field_serializes_when_present() {
    let req = CreateInstanceRequest {
        name: "test-instance".to_string(),
        constellation: "back-office".to_string(),
        subdomain: None,
        ticket: None,
        job: Some(JobConfig { prompt: "do a thing".to_string(), harness: Some("pida".to_string()), model: Some("openai/gpt-5.6-sol".to_string()) }),
        labels: std::collections::HashMap::from([("worker".to_string(), "true".to_string())]),
    };
    let value = serde_json::to_value(&req).unwrap();
    assert_eq!(value["job"]["model"], "openai/gpt-5.6-sol");
    assert_eq!(value["labels"]["worker"], "true");
    assert_eq!(value["job"]["harness"], "pida");
    assert_eq!(value["job"]["prompt"], "do a thing");
}

#[test]
fn job_config_model_field_omitted_when_none() {
    let job = JobConfig { prompt: "goal".to_string(), harness: None, model: None };
    let value = serde_json::to_value(&job).unwrap();
    assert!(value.get("model").is_none());
    assert!(value.get("harness").is_none());
}

/// `name` is optional on create — the create-project dialog leaves it out
/// entirely when blank so the API can generate one from `goal`.
#[test]
fn create_project_request_name_is_optional() {
    let json = r#"{"goal": "do a thing"}"#;
    let req: CreateProjectRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.name, None);
    assert_eq!(req.goal, "do a thing");
}

/// Project JSON files written before `heartbeat_prompt` existed must still
/// deserialize — missing field means `None`, not a hard error.
#[test]
fn project_deserializes_without_heartbeat_prompt_field() {
    let old_json = r#"{
        "id": "abc",
        "name": "test",
        "goal": "do a thing",
        "constellation": "back-office",
        "status": "draft",
        "vape_instance_id": null,
        "heartbeat_enabled": false,
        "last_note": null,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "last_heartbeat_at": null
    }"#;
    let project: Project = serde_json::from_str(old_json).unwrap();
    assert_eq!(project.heartbeat_prompt, None);
    assert_eq!(project.heartbeat_interval_secs, 15 * 60);
}
