use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::routing::get;
use mazz_flux_bot::heartbeat::HeartbeatClock;
use mazz_flux_bot::models::{CreateProjectRequest, KanbanStatus};
use mazz_flux_bot::store::Store;
use mazz_flux_bot::vape_client::VapeClient;
use mazz_flux_bot::{AppState, api};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn test_app() -> (Router, Arc<Store>, std::path::PathBuf) {
    let mut dir = std::env::temp_dir();
    dir.push(format!("mfb-api-context-test-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(Store::open(&dir).await.unwrap());
    let state = AppState {
        store: store.clone(),
        vape: Arc::new(VapeClient::new()),
        heartbeat_clock: Arc::new(HeartbeatClock::new(15)),
    };
    let app = Router::new()
        .route(
            "/api/projects/{id}/agent-context",
            get(api::get_agent_context),
        )
        .route(
            "/api/settings",
            get(api::get_settings).post(api::update_settings),
        )
        .with_state(state);
    (app, store, dir)
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn agent_context_is_project_scoped_and_read_only_in_shape() {
    let (app, store, dir) = test_app().await;
    let project = store
        .create_project(CreateProjectRequest {
            name: Some("Context project".into()),
            goal: "Ship the feature".into(),
            heartbeat_prompt: Some("Review the board".into()),
            constellation: None,
            heartbeat_interval_secs: None,
        })
        .await
        .unwrap();
    let task = store
        .create_kanban_task(
            &project.id,
            "Implement endpoint",
            "Keep it scoped",
            KanbanStatus::Assigned,
        )
        .await
        .unwrap();
    store
        .create_archetype(
            "Reviewer",
            "Review exact behavior",
            Some("openai/test-model"),
        )
        .await
        .unwrap();
    store
        .create_project(CreateProjectRequest {
            name: Some("Other project".into()),
            goal: "other-project-secret".into(),
            heartbeat_prompt: None,
            constellation: None,
            heartbeat_interval_secs: None,
        })
        .await
        .unwrap();
    store
        .set_setting("secret_test_value", "must-not-leak")
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{}/agent-context", project.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["project"]["id"], project.id);
    assert_eq!(body["project"]["goal"], "Ship the feature");
    assert_eq!(body["kanban"]["project_id"], project.id);
    assert_eq!(body["kanban"]["tasks"][0]["title"], "Implement endpoint");
    assert_eq!(body["archetypes"][0]["name"], "Reviewer");
    assert!(body.get("settings").is_none());
    assert!(body.get("logs").is_none());
    assert!(body.get("session").is_none());
    assert!(!body.to_string().contains("must-not-leak"));
    assert!(!body.to_string().contains("other-project-secret"));
    assert!(body["project"].get("created_at").is_none());

    store
        .update_kanban_task(
            &project.id,
            &task.id,
            None,
            None,
            Some(KanbanStatus::InProgress),
        )
        .await
        .unwrap();
    let refreshed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{}/agent-context", project.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        json_body(refreshed).await["kanban"]["tasks"][0]["status"],
        "in_progress"
    );

    let write_attempt = Request::builder()
        .method("POST")
        .uri(format!("/api/projects/{}/agent-context", project.id))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(write_attempt).await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn agent_context_returns_404_for_unknown_project() {
    let (app, _store, dir) = test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/projects/missing/agent-context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(response).await["error"], "project not found");
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn settings_override_normalizes_preserves_and_clears() {
    let (app, store, dir) = test_app().await;
    let save = Request::builder()
        .method("POST")
        .uri("/api/settings")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"bot_public_base_url":"https://bot.example/"}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(save).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["bot_public_base_url"], "https://bot.example");
    assert_eq!(body["effective_bot_public_base_url"], "https://bot.example");
    assert_eq!(body["bot_public_base_url_source"], "settings");

    let models_only = Request::builder()
        .method("POST")
        .uri("/api/settings")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"conductor_model":"openai/next"}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(models_only).await.unwrap();
    assert_eq!(
        json_body(response).await["bot_public_base_url"],
        "https://bot.example"
    );

    let clear = Request::builder()
        .method("POST")
        .uri("/api/settings")
        .header("content-type", "application/json")
        .body(Body::from(json!({"bot_public_base_url":""}).to_string()))
        .unwrap();
    let response = app.oneshot(clear).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["bot_public_base_url"],
        Value::Null
    );
    assert_eq!(
        store.get_setting("bot_public_base_url").await.unwrap(),
        None
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn invalid_settings_url_is_rejected_before_partial_writes() {
    let (app, store, dir) = test_app().await;
    store
        .set_setting("conductor_model", "original/model")
        .await
        .unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/api/settings")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "conductor_model":"should/not/save",
                "bot_public_base_url":"https://user:password@example.com"
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        store
            .get_setting("conductor_model")
            .await
            .unwrap()
            .as_deref(),
        Some("original/model")
    );
    std::fs::remove_dir_all(dir).ok();
}
