use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use mazz_flux_bot::{
    AppState,
    heartbeat::{HeartbeatClock, force_tick_checked},
    models::*,
    projects::*,
    store::Store,
    v2,
    vape_client::VapeClient,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
fn dir() -> std::path::PathBuf {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-state")
        .join(id());
    std::fs::create_dir_all(&p).unwrap();
    p
}
async fn project(s: &Store) -> Project {
    s.create_project(CreateProjectRequest {
        name: Some("test".into()),
        goal: "deliver".into(),
        heartbeat_prompt: None,
        constellation: Some("empty".into()),
        heartbeat_interval_secs: None,
    })
    .await
    .unwrap()
}
async fn app() -> (Router, Arc<Store>, std::path::PathBuf) {
    let d = dir();
    let s = Arc::new(Store::open(&d).await.unwrap());
    let state = AppState {
        store: s.clone(),
        vape: Arc::new(VapeClient::new()),
        heartbeat_clock: Arc::new(HeartbeatClock::new(15)),
    };
    (v2::routes().with_state(state), s, d)
}
async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(
                    body.map(|v| Body::from(v.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[test]
fn statuses_and_strict_schema() {
    for text in [
        "pending",
        "in_progress",
        "review",
        "merged",
        "flags_enabled",
        "flags_cleaned",
    ] {
        let v: DeliveryStatus = serde_json::from_value(json!(text)).unwrap();
        assert_eq!(serde_json::to_value(v).unwrap(), text);
    }
    assert!(serde_json::from_value::<DeliveryStatus>(json!("done")).is_err());
}
#[tokio::test]
async fn migration_dry_run_preserves_legacy_and_apply_is_idempotent() {
    let d = dir();
    std::fs::create_dir_all(d.join("projects")).unwrap();
    let raw = json!({"id":"old","name":"Legacy","goal":"goal","constellation":"empty","status":"blocked","vape_instance_id":"upstream-a","heartbeat_enabled":false,"created_at":"2026-01-01","updated_at":"2026-02-01","last_note":"keep","last_heartbeat_at":"2026-02-01"});
    let path = d.join("projects/old.json");
    std::fs::write(&path, raw.to_string()).unwrap();
    let s = Store::open_for_migration(&d).await.unwrap();
    let report = s.migrate(true).await.unwrap();
    assert!(report.errors.is_empty());
    assert!(report.records[0].migrated);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), raw.to_string());
    assert!(!d.join("project_instances").exists());
    let s = Store::open(&d).await.unwrap();
    let p = s.require_project("old").await.unwrap();
    assert_eq!(p.status, "blocked");
    assert_eq!(p.last_note.as_deref(), Some("keep"));
    assert_eq!(p.tracking.delivery_status, DeliveryStatus::Pending);
    assert_eq!(p.vape_instance_id.as_deref(), Some("upstream-a"));
    assert_eq!(p.heartbeat_interval_secs, 900);
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["status"], "pending");
    assert_eq!(after["automation"]["status"], "blocked");
    assert!(after.get("vape_instance_id").is_none());
    assert!(!s.migrate(false).await.unwrap().records[0].migrated);
    std::fs::remove_dir_all(d).unwrap();
}
#[tokio::test]
async fn malformed_migration_preflight_writes_nothing() {
    let d = dir();
    let s = Store::open(&d).await.unwrap();
    let p = project(&s).await;
    let path = d.join("projects").join(format!("{}.json", p.id));
    let original = std::fs::read(&path).unwrap();
    std::fs::write(d.join("projects/broken.json"), "not-json").unwrap();
    let report = s.migrate(false).await.unwrap();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(Store::open(&d).await.is_err());
    std::fs::remove_dir_all(d).unwrap();
}
#[tokio::test]
async fn exclusive_ownership_promotion_archive_and_purge() {
    let d = dir();
    let s = Store::open(&d).await.unwrap();
    let a = project(&s).await;
    let b = project(&s).await;
    let mut r = Relationship::pending(&a, "Primary".into(), "empty".into(), None);
    r.vape_instance_id = Some("live-a".into());
    let r = s
        .add_relationship(&a.id, a.tracking.revision, r, true)
        .await
        .unwrap();
    let a = s.require_project(&a.id).await.unwrap();
    let mut r2 = Relationship::pending(&a, "Related".into(), "empty".into(), None);
    r2.vape_instance_id = Some("live-b".into());
    let r2 = s
        .add_relationship(&a.id, a.tracking.revision, r2, false)
        .await
        .unwrap();
    let a = s.require_project(&a.id).await.unwrap();
    s.write_memory(&a.id, "preserved memory").await.unwrap();
    s.touch_heartbeat(&a.id, None).await.unwrap();
    let a = s.require_project(&a.id).await.unwrap();
    s.edit_relationship(&a.id, &r2.id, a.tracking.revision, Some(true), |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(
        s.require_project(&a.id)
            .await
            .unwrap()
            .vape_instance_id
            .as_deref(),
        Some("live-b")
    );
    assert!(
        s.require_project(&a.id)
            .await
            .unwrap()
            .last_heartbeat_at
            .is_none()
    );
    let memory = s.read_memory(&a.id).await.unwrap().unwrap();
    assert!(memory.contains("preserved memory") && memory.contains("Primary promotion"));
    assert_eq!(s.relationships(&a.id).await.unwrap().instances.len(), 2);
    let mut duplicate = Relationship::pending(&b, "Duplicate".into(), "empty".into(), None);
    duplicate.vape_instance_id = Some("live-a".into());
    assert!(
        s.add_relationship(&b.id, b.tracking.revision, duplicate.clone(), false)
            .await
            .is_err()
    );
    let a = s.require_project(&a.id).await.unwrap();
    s.set_archived(&a.id, a.tracking.revision, true)
        .await
        .unwrap();
    assert!(
        s.add_relationship(&b.id, b.tracking.revision, duplicate, false)
            .await
            .is_err()
    );
    assert!(s.list_running_projects().await.unwrap().is_empty());
    let a = s.require_project(&a.id).await.unwrap();
    assert!(
        s.purge_project(&a.id, a.tracking.revision, "wrong")
            .await
            .is_err()
    );
    s.purge_project(&a.id, a.tracking.revision, &a.name)
        .await
        .unwrap();
    assert!(s.get_project(&a.id).await.unwrap().is_none());
    assert!(!d.join("memory").join(format!("{}.md", a.id)).exists());
    assert!(s.relationships(&a.id).await.unwrap().instances.is_empty());
    assert_ne!(r.id, r2.id);
    std::fs::remove_dir_all(d).unwrap();
}
#[tokio::test]
async fn v2_crud_clear_errors_and_get_only_alias() {
    let (app, s, d) = app().await;
    let (status,v)=call(&app,"POST","/api/v2/projects",Some(json!({"name":"API","goal":"Ship","description":"context","status":"review","original_jira_tickets":[{"url":"https://example.com/POS-1"}],"launchdarkly_flags":[{"key":"feature.flag"}]}))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let pid = v["project"]["id"].as_str().unwrap().to_string();
    let base = format!("/api/v2/projects/{pid}");
    let rev = v["project"]["revision"].as_u64().unwrap();
    assert_eq!(v["project"]["status"], "review");
    assert_eq!(v["project"]["automation"]["status"], "draft");
    let (status,_)=call(&app,"PATCH",&base,Some(json!({"expected_revision":rev,"status":"merged","heartbeat_prompt":null,"original_jira_tickets":[]}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, error) = call(
        &app,
        "PATCH",
        &base,
        Some(json!({"expected_revision":rev,"description":"stale"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "stale_revision");
    for body in [
        json!({"expected_revision":rev,"status":"bad"}),
        json!({"expected_revision":rev,"id":"forged"}),
    ] {
        assert_eq!(
            call(&app, "PATCH", &base, Some(body)).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let ctx = format!("{base}/agent-context");
    let old = format!("/api/projects/{pid}/agent-context");
    assert_eq!(
        call(&app, "GET", &ctx, None).await.1,
        call(&app, "GET", &old, None).await.1
    );
    assert_eq!(
        call(&app, "POST", &old, Some(json!({}))).await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        call(&app, "GET", "/api/projects", None).await.0,
        StatusCode::GONE
    );
    let p = s.require_project(&pid).await.unwrap();
    assert_eq!(p.tracking.delivery_status, DeliveryStatus::Merged);
    assert!(p.tracking.original_jira_tickets.is_empty());
    let (status, n) = call(
        &app,
        "POST",
        &format!("{base}/notes"),
        Some(json!({"expected_revision":p.tracking.revision,"title":"Notes","content":"first"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{n}");
    let nid = n["entry"]["id"].as_str().unwrap();
    let p = s.require_project(&pid).await.unwrap();
    assert_eq!(
        call(
            &app,
            "PATCH",
            &format!("{base}/notes/{nid}"),
            Some(json!({"expected_revision":p.tracking.revision,"title":null,"content":"edited"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(s.user_notes(&pid).await.unwrap()[0].content, "edited");
    let p = s.require_project(&pid).await.unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/archive"),
            Some(json!({"expected_revision":p.tracking.revision}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(
        call(&app, "GET", "/api/v2/projects", None).await.1["projects"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let p = s.require_project(&pid).await.unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/restore"),
            Some(json!({"expected_revision":p.tracking.revision}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!s.require_project(&pid).await.unwrap().heartbeat_enabled);
    std::fs::remove_dir_all(d).unwrap();
}
#[tokio::test]
async fn project_guard_excludes_forced_ticks_and_archived_ticks_are_rejected() {
    let d = dir();
    let s = Arc::new(Store::open(&d).await.unwrap());
    let p = project(&s).await;
    let guard = s.project_guard(&p.id).await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), s.project_guard(&p.id))
            .await
            .is_err()
    );
    drop(guard);
    s.set_archived(&p.id, p.tracking.revision, true)
        .await
        .unwrap();
    let state = AppState {
        store: s.clone(),
        vape: Arc::new(VapeClient::new()),
        heartbeat_clock: Arc::new(HeartbeatClock::new(15)),
    };
    assert!(
        force_tick_checked(
            &state,
            &p.id,
            s.require_project(&p.id).await.unwrap().tracking.revision
        )
        .await
        .is_err()
    );
    std::fs::remove_dir_all(d).unwrap();
}
#[test]
fn launchdarkly_default_values_and_inverted_targets() {
    let f = FlagReference {
        id: "f".into(),
        key: "test".into(),
        project_key: "default".into(),
        environment_key: "production".into(),
        fully_enabled_value: false,
        label: None,
        notes: None,
    };
    let item = json!({"variations":[{"value":false},{"value":true}],"environments":{"production":{"on":false,"offVariation":0}}});
    let v = mazz_flux_bot::launchdarkly::evaluate(&item, &f, "now");
    assert!(v.available);
    assert_eq!(v.matches_fully_enabled, Some(true));
    assert_eq!(v.targeting_on, Some(false));
}

#[tokio::test]
async fn stable_resources_and_openapi_response_contract() {
    let (app, store, d) = app().await;
    let p = project(&store).await;
    let base = format!("/api/v2/projects/{}", p.id);
    for (kind, fields) in [
        (
            "jira-tickets",
            json!({"url":"https://example.com/POS-1","key":"POS-1"}),
        ),
        (
            "ec-tickets",
            json!({"url":"https://example.com/EC-1","label":"Rollout"}),
        ),
        (
            "vape-pages",
            json!({"url":"https://example.com/docs","label":"Guide"}),
        ),
        (
            "launchdarkly-flags",
            json!({"key":"flag.example","fully_enabled_value":false}),
        ),
    ] {
        let p = store.require_project(&p.id).await.unwrap();
        let mut request = fields;
        request["expected_revision"] = json!(p.tracking.revision);
        let (status, response) = call(&app, "POST", &format!("{base}/{kind}"), Some(request)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let id = response["entry"]["id"].as_str().unwrap();
        let p = store.require_project(&p.id).await.unwrap();
        let (status, response) = call(
            &app,
            "PATCH",
            &format!("{base}/{kind}/{id}"),
            Some(json!({"expected_revision":p.tracking.revision,"label":"Edited"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response["entry"]["id"], id);
        let p = store.require_project(&p.id).await.unwrap();
        if kind != "launchdarkly-flags" {
            assert_eq!(
                call(
                    &app,
                    "PATCH",
                    &format!("{base}/{kind}/{id}"),
                    Some(
                        json!({"expected_revision":p.tracking.revision,"url":"javascript:alert(1)"})
                    )
                )
                .await
                .0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        assert_eq!(
            call(
                &app,
                "DELETE",
                &format!("{base}/{kind}/{id}"),
                Some(json!({"expected_revision":p.tracking.revision}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let mut ids = vec![];
    for text in ["one", "two"] {
        let p = store.require_project(&p.id).await.unwrap();
        let (_, value) = call(
            &app,
            "POST",
            &format!("{base}/notes"),
            Some(json!({"expected_revision":p.tracking.revision,"content":text})),
        )
        .await;
        ids.push(value["entry"]["id"].as_str().unwrap().to_string());
    }
    ids.reverse();
    let p = store.require_project(&p.id).await.unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/notes/reorder"),
            Some(json!({"expected_revision":p.tracking.revision,"ids":ids}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(store.user_notes(&p.id).await.unwrap()[0].content, "two");
    let (_, response) = call(&app, "GET", &base, None).await;
    let (_, contract) = call(&app, "GET", "/api/v2/openapi.json", None).await;
    assert_eq!(contract["openapi"], "3.1.0");
    let schema = &contract["components"]["schemas"]["Project"];
    for property in schema["required"].as_array().unwrap() {
        assert!(
            response["project"]
                .get(property.as_str().unwrap())
                .is_some(),
            "missing required property {property}"
        );
    }
    for property in response["project"].as_object().unwrap().keys() {
        assert!(
            schema["properties"].get(property).is_some(),
            "undocumented response property {property}"
        );
    }
    assert!(
        contract["paths"]
            .get("/api/v2/projects/{pid}/instances/{rid}/message")
            .is_some()
    );
    assert!(
        contract["paths"]
            .get("/api/v2/projects/{pid}/notes/reorder")
            .is_some()
    );
    std::fs::remove_dir_all(d).unwrap();
}

fn legacy_record(pid: &str, iid: Option<&str>) -> Value {
    json!({"id":pid,"name":"Legacy","goal":"keep","constellation":"empty","status":"paused","vape_instance_id":iid,"heartbeat_enabled":false,"created_at":"2026-01-01","updated_at":"2026-01-02"})
}
fn resumed_collection(pid: &str, iid: &str) -> Value {
    json!({"project_id":pid,"primary_instance_relationship_id":"legacy-primary","instances":[{"id":"legacy-primary","project_id":pid,"vape_instance_id":iid,"label":"Primary","role":"worker","constellation":"empty","model":null,"goal_override":null,"heartbeat_prompt_override":null,"created_by":"legacy-migration","created_at":"2026-01-01","updated_at":"2026-01-02"}]})
}
#[tokio::test]
async fn migration_resume_requires_exact_legacy_target() {
    for (iid, expected_success) in [("original", true), ("wrong-target", false)] {
        let d = dir();
        std::fs::create_dir_all(d.join("projects")).unwrap();
        std::fs::create_dir_all(d.join("project_instances")).unwrap();
        let original = legacy_record("legacy", Some("original")).to_string();
        std::fs::write(d.join("projects/legacy.json"), &original).unwrap();
        std::fs::write(
            d.join("project_instances/legacy.json"),
            resumed_collection("legacy", iid).to_string(),
        )
        .unwrap();
        let store = Store::open_for_migration(&d).await.unwrap();
        let report = store.migrate(false).await.unwrap();
        assert_eq!(
            report.errors.is_empty(),
            expected_success,
            "{:?}",
            report.errors
        );
        if expected_success {
            assert_eq!(
                Store::open(&d)
                    .await
                    .unwrap()
                    .require_project("legacy")
                    .await
                    .unwrap()
                    .vape_instance_id
                    .as_deref(),
                Some("original")
            );
        } else {
            assert!(report.errors.iter().any(|e| e.contains("legacy primary")));
            assert_eq!(
                std::fs::read_to_string(d.join("projects/legacy.json")).unwrap(),
                original
            );
        }
        std::fs::remove_dir_all(d).unwrap();
    }
}
#[tokio::test]
async fn migration_inventories_orphan_malformed_and_misnamed_collections() {
    let d = dir();
    std::fs::create_dir_all(d.join("projects")).unwrap();
    std::fs::create_dir_all(d.join("project_instances")).unwrap();
    let original = legacy_record("legacy", Some("shared-instance")).to_string();
    std::fs::write(d.join("projects/legacy.json"), &original).unwrap();
    std::fs::write(
        d.join("project_instances/orphan.json"),
        resumed_collection("orphan", "shared-instance").to_string(),
    )
    .unwrap();
    std::fs::write(d.join("project_instances/malformed.json"), "broken JSON").unwrap();
    std::fs::write(
        d.join("project_instances/misnamed.json"),
        resumed_collection("actual-id", "other-instance").to_string(),
    )
    .unwrap();
    let store = Store::open_for_migration(&d).await.unwrap();
    let report = store.migrate(false).await.unwrap();
    for text in [
        "orphan relationship collection",
        "malformed.json",
        "filename/project_id mismatch",
        "already owned by orphan",
    ] {
        assert!(
            report.errors.iter().any(|e| e.contains(text)),
            "missing {text}: {:?}",
            report.errors
        );
    }
    assert_eq!(
        std::fs::read_to_string(d.join("projects/legacy.json")).unwrap(),
        original
    );
    assert!(!d.join("project_instances/legacy.json").exists());
    std::fs::remove_dir_all(d).unwrap();
    // Relationship files must also be inventoried when the projects directory is absent.
    let d = dir();
    std::fs::create_dir_all(d.join("project_instances")).unwrap();
    std::fs::write(
        d.join("project_instances/orphan.json"),
        resumed_collection("orphan", "live-instance").to_string(),
    )
    .unwrap();
    let store = Store::open_for_migration(&d).await.unwrap();
    assert!(
        store
            .migrate(true)
            .await
            .unwrap()
            .errors
            .iter()
            .any(|e| e.contains("orphan"))
    );
    std::fs::remove_dir_all(d).unwrap();
}
#[tokio::test]
async fn interval_limits_and_non_nullable_patch_contract() {
    let (app, store, d) = app().await;
    for n in [4, 31_536_001, u64::MAX] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v2/projects",
                Some(json!({"name":"Limits","goal":"test","heartbeat_interval_secs":n}))
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let (status, v) = call(
        &app,
        "POST",
        "/api/v2/projects",
        Some(json!({"name":"Limits","goal":"test","heartbeat_interval_secs":31_536_000})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pid = v["project"]["id"].as_str().unwrap();
    let base = format!("/api/v2/projects/{pid}");
    let rev = v["revision"].as_u64().unwrap();
    assert_eq!(
        store
            .require_project(pid)
            .await
            .unwrap()
            .heartbeat_interval_secs,
        31_536_000
    );
    for field in [
        "name",
        "goal",
        "description",
        "status",
        "constellation",
        "heartbeat_interval_secs",
        "original_jira_tickets",
        "ec_tickets",
        "vape_pages",
        "launchdarkly_flags",
    ] {
        let mut body = json!({"expected_revision":rev});
        body[field] = Value::Null;
        let (status, error) = call(&app, "PATCH", &base, Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}: {error}");
    }
    for n in [4, 31_536_001, u64::MAX] {
        assert_eq!(
            call(
                &app,
                "PATCH",
                &base,
                Some(json!({"expected_revision":rev,"heartbeat_interval_secs":n}))
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(call(&app,"PATCH",&base,Some(json!({"expected_revision":rev,"heartbeat_interval_secs":31_536_000,"heartbeat_prompt":null}))).await.0,StatusCode::OK);
    let p = store.require_project(pid).await.unwrap();
    let r = Relationship::pending(
        &p,
        "Primary".into(),
        "empty".into(),
        Some("example/model".into()),
    );
    let r = store
        .add_relationship(pid, p.tracking.revision, r, true)
        .await
        .unwrap();
    let p = store.require_project(pid).await.unwrap();
    for field in ["label", "role", "constellation", "is_primary"] {
        let mut body = json!({"expected_revision":p.tracking.revision});
        body[field] = Value::Null;
        assert_eq!(
            call(
                &app,
                "PATCH",
                &format!("{base}/instances/{}", r.id),
                Some(body)
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(call(&app,"PATCH",&format!("{base}/instances/{}",r.id),Some(json!({"expected_revision":p.tracking.revision,"model":null,"goal_override":null,"heartbeat_prompt_override":null}))).await.0,StatusCode::OK);
    let p = store.require_project(pid).await.unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/notes"),
            Some(json!({"expected_revision":p.tracking.revision,"content":null}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    std::fs::remove_dir_all(d).unwrap();
}
#[test]
fn percentage_defaults_preserve_targeting_and_cached_fetch_time() {
    let f = FlagReference {
        id: "flag".into(),
        key: "example".into(),
        project_key: "default".into(),
        environment_key: "production".into(),
        fully_enabled_value: true,
        label: None,
        notes: None,
    };
    let percentage = json!({"variations":[{"value":false},{"value":true}],"environments":{"production":{"on":true,"fallthrough":{"rollout":{"variations":[{"variation":0,"weight":50000},{"variation":1,"weight":50000}]}}}}});
    let fetched = "2026-09-30T01:02:03Z";
    for _ in 0..2 {
        let status = mazz_flux_bot::launchdarkly::evaluate(&percentage, &f, fetched);
        assert!(!status.available);
        assert_eq!(status.targeting_on, Some(true));
        assert_eq!(status.fetched_at, fetched);
        assert_eq!(status.current_value, None);
        assert_eq!(status.matches_fully_enabled, None);
    }
}
#[tokio::test]
async fn detached_relationship_detail_is_not_found() {
    let (app, store, d) = app().await;
    let p = project(&store).await;
    let r = Relationship::pending(&p, "Pending".into(), "empty".into(), None);
    let r = store
        .add_relationship(&p.id, p.tracking.revision, r, false)
        .await
        .unwrap();
    let p = store.require_project(&p.id).await.unwrap();
    store
        .detach_relationship(&p.id, &r.id, p.tracking.revision)
        .await
        .unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/v2/projects/{}/instances/{}", p.id, r.id),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    std::fs::remove_dir_all(d).unwrap();
}
