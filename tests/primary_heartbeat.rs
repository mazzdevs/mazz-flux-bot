//! End-to-end heartbeat test using only loopback providers and a test-only gh
//! executable. No real VAPE/LLM credentials or external mutations are used.
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use mazz_flux_bot::{models::*, projects::*, store::Store};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
struct Provider {
    calls: Arc<Mutex<Vec<String>>>,
    created: Arc<Mutex<Vec<Value>>>,
}
async fn handler(
    State(s): State<Provider>,
    req: axum::extract::Request,
) -> (StatusCode, Json<Value>) {
    let path = req.uri().path().to_string();
    s.calls.lock().unwrap().push(path.clone());
    if path == "/api/v1/instances" && req.method() == "POST" {
        let bytes = axum::body::to_bytes(req.into_body(), 100000).await.unwrap();
        let mut request: Value = serde_json::from_slice(&bytes).unwrap();
        if request["constellation"] == "invalid" {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"unknown constellation"})),
            );
        }
        let mut created = s.created.lock().unwrap();
        request["id"] = json!(format!("created-{}", created.len() + 1));
        request["status"] = json!("Starting");
        let id = request["id"].clone();
        let uncertain = request["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("uncertain"));
        created.push(request);
        return (
            if uncertain {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            },
            Json(json!({"id":id})),
        );
    }
    let body = match path.as_str() {
        "/api/v1/instances" => {
            let mut instances = vec![
                json!({"id":"primary-a","name":"primary","status":"Running","ready":true}),
                json!({"id":"secondary-b","name":"secondary","status":"Stopped","ready":false}),
            ];
            instances.extend(s.created.lock().unwrap().iter().cloned());
            json!(instances)
        }
        "/api/v1/instances/primary-a" => {
            json!({"id":"primary-a","name":"primary","status":"Running","ready":true})
        }
        "/api/v1/instances/secondary-b" => {
            json!({"id":"secondary-b","name":"secondary","status":"Stopped","ready":false})
        }
        "/api/v1/instances/primary-a/agent-status" => {
            json!({"state":"ready","active_harness":"pida"})
        }
        "/api/v1/instances/primary-a/pida/api/status" => json!({"ready":true}),
        "/api/v1/instances/primary-a/pida/api/session" => {
            json!({"messages":[{"role":"assistant","content":"implementation underway"}]})
        }
        "/api/v1/instances/primary-a/pida/api/chat" => json!({"ok":true}),
        "/llm" => {
            json!({"choices":[{"message":{"content":"{\"action\":\"send_message\",\"message\":\"Check the tests\",\"memory\":\"Primary is implementing\"}"}}]})
        }
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"unexpected test path"})),
            );
        }
    };
    (StatusCode::OK, Json(body))
}
#[tokio::test]
async fn forced_tick_only_observes_and_messages_primary() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let d = root.join("target/test-state").join(id());
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let s = Store::open(&d).await.unwrap();
    let p = s
        .create_project(CreateProjectRequest {
            name: Some("Heartbeat test".into()),
            goal: "ship".into(),
            heartbeat_prompt: None,
            constellation: Some("empty".into()),
            heartbeat_interval_secs: None,
        })
        .await
        .unwrap();
    let mut r = Relationship::pending(&p, "Primary".into(), "empty".into(), None);
    r.vape_instance_id = Some("primary-a".into());
    s.add_relationship(&p.id, p.tracking.revision, r, true)
        .await
        .unwrap();
    let p = s.require_project(&p.id).await.unwrap();
    let mut related = Relationship::pending(&p, "Related".into(), "empty".into(), None);
    related.vape_instance_id = Some("secondary-b".into());
    s.add_relationship(&p.id, p.tracking.revision, related, false)
        .await
        .unwrap();
    let p = s.require_project(&p.id).await.unwrap();
    s.mutate_project(&p.id, p.tracking.revision, |p| {
        p.tracking.delivery_status = DeliveryStatus::Merged;
        Ok(())
    })
    .await
    .unwrap();
    let gh = d.join("bin/gh");
    std::fs::write(&gh, "#!/bin/sh\nprintf 'test-only-token\\n'\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let provider = Provider {
        calls: Arc::new(Mutex::new(vec![])),
        created: Arc::new(Mutex::new(vec![])),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/llm", post(handler))
        .fallback(handler)
        .with_state(provider.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let path = format!(
        "{}:{}",
        d.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mazz-flux-bot"))
        .current_dir(&root)
        .env("MAZZ_FLUX_DATA_DIR", &d)
        .env("PORT", port.to_string())
        .env("PATH", path)
        .env("CADMIUM_VAPE_URL", &upstream)
        .env("OPENROUTER_API_URL", format!("{upstream}/llm"))
        .env("OPENROUTER_API_KEY", "test-only")
        .env("MAZZ_FLUX_LIVE", "1")
        .env("HEARTBEAT_SCAN_INTERVAL_SECS", "3600")
        .env_remove("LD_ACCESS_TOKEN")
        .env_remove("MAZZ_FLUX_STATE_REPO_URL")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}/api/v2/projects/{}", p.id);
    let mut ready = false;
    for _ in 0..100 {
        if client
            .get(format!("{base}/agent-context"))
            .send()
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ready, "test binary did not start");
    let p = s.require_project(&p.id).await.unwrap();
    let response = client
        .post(format!("{base}/heartbeat/force"))
        .json(&json!({"expected_revision":p.tracking.revision}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "{body}");
    let calls = provider.calls.lock().unwrap().clone();
    assert!(
        calls.iter().any(|p| p.ends_with("primary-a/pida/api/chat")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|p| p.contains("secondary-b")),
        "secondary was observed or targeted: {calls:?}"
    );
    assert_eq!(
        s.require_project(&p.id)
            .await
            .unwrap()
            .tracking
            .delivery_status,
        DeliveryStatus::Merged
    );
    assert!(
        s.read_memory(&p.id)
            .await
            .unwrap()
            .unwrap()
            .contains("Primary is implementing")
    );
    // Empty-project Start provisions exactly one labeled primary. A repeat
    // provision returns the existing binding rather than creating another pod.
    let empty = s
        .create_project(CreateProjectRequest {
            name: Some("Empty".into()),
            goal: "ship".into(),
            heartbeat_prompt: None,
            constellation: Some("empty".into()),
            heartbeat_interval_secs: None,
        })
        .await
        .unwrap();
    let empty_base = format!("http://127.0.0.1:{port}/api/v2/projects/{}", empty.id);
    let start = client
        .post(format!("{empty_base}/start"))
        .json(&json!({"expected_revision":empty.tracking.revision}))
        .send()
        .await
        .unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let all = s.relationships(&empty.id).await.unwrap();
    assert_eq!(all.instances.len(), 1);
    let primary = all.primary().unwrap();
    assert_eq!(primary.vape_instance_id.as_deref(), Some("created-1"));
    let empty = s.require_project(&empty.id).await.unwrap();
    let retry = client
        .post(format!("{empty_base}/instances/{}/provision", primary.id))
        .json(&json!({"expected_revision":empty.tracking.revision}))
        .send()
        .await
        .unwrap();
    assert!(retry.status().is_success());
    {
        let created = provider.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["labels"]["mazz-flux-project"], empty.id);
        assert_eq!(created[0]["labels"]["mazz-flux-relationship"], primary.id);
    }

    // A failed create response leaves the relationship pending. Reconciliation
    // finds the already-created upstream instance on retry without another POST.
    let uncertain = s
        .create_project(CreateProjectRequest {
            name: Some("Uncertain".into()),
            goal: "ship".into(),
            heartbeat_prompt: None,
            constellation: Some("empty".into()),
            heartbeat_interval_secs: None,
        })
        .await
        .unwrap();
    let r = Relationship::pending(&uncertain, "Uncertain".into(), "empty".into(), None);
    let r = s
        .add_relationship(&uncertain.id, uncertain.tracking.revision, r, true)
        .await
        .unwrap();
    let uncertain_base = format!("http://127.0.0.1:{port}/api/v2/projects/{}", uncertain.id);
    let uncertain = s.require_project(&uncertain.id).await.unwrap();
    let failed = client
        .post(format!("{uncertain_base}/start"))
        .json(&json!({"expected_revision":uncertain.tracking.revision}))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert!(
        s.require_relationship(&uncertain.id, &r.id)
            .await
            .unwrap()
            .vape_instance_id
            .is_none()
    );
    let before_attempt_revision = uncertain.tracking.revision;
    let uncertain = s.require_project(&uncertain.id).await.unwrap();
    assert!(uncertain.tracking.revision > before_attempt_revision);
    let stale = client
        .delete(format!("{uncertain_base}/instances/{}", r.id))
        .json(&json!({"expected_revision":before_attempt_revision}))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let retry = client
        .post(format!("{uncertain_base}/instances/{}/provision", r.id))
        .json(&json!({"expected_revision":uncertain.tracking.revision}))
        .send()
        .await
        .unwrap();
    assert!(
        retry.status().is_success(),
        "{}",
        retry.text().await.unwrap()
    );
    assert_eq!(
        s.require_relationship(&uncertain.id, &r.id)
            .await
            .unwrap()
            .vape_instance_id
            .as_deref(),
        Some("created-2")
    );
    assert_eq!(provider.created.lock().unwrap().len(), 2);

    // A definitive rejection is retryable on the same relationship after its
    // configuration is corrected. Attempt/error writes invalidate old drafts.
    let rejected = s
        .create_project(CreateProjectRequest {
            name: Some("Rejected".into()),
            goal: "ship".into(),
            heartbeat_prompt: None,
            constellation: Some("invalid".into()),
            heartbeat_interval_secs: None,
        })
        .await
        .unwrap();
    let rejected_base = format!("http://127.0.0.1:{port}/api/v2/projects/{}", rejected.id);
    let rejected_rel = Relationship::pending(&rejected, "Rejected".into(), "invalid".into(), None);
    let rejected_rel = s
        .add_relationship(&rejected.id, rejected.tracking.revision, rejected_rel, true)
        .await
        .unwrap();
    let before = s
        .require_project(&rejected.id)
        .await
        .unwrap()
        .tracking
        .revision;
    let failure = client
        .post(format!("{rejected_base}/start"))
        .json(&json!({"expected_revision":before}))
        .send()
        .await
        .unwrap();
    assert_eq!(failure.status(), reqwest::StatusCode::BAD_GATEWAY);
    let rejected = s.require_project(&rejected.id).await.unwrap();
    assert_eq!(rejected.tracking.revision, before + 2);
    let failed_rel = s
        .require_relationship(&rejected.id, &rejected_rel.id)
        .await
        .unwrap();
    assert!(!failed_rel.provision_attempted);
    assert!(
        failed_rel
            .provision_error
            .as_deref()
            .unwrap()
            .contains("422")
    );
    let stale = client
        .patch(format!("{rejected_base}/instances/{}", rejected_rel.id))
        .json(&json!({"expected_revision":before,"constellation":"empty"}))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let correction = client
        .patch(format!("{rejected_base}/instances/{}", rejected_rel.id))
        .json(&json!({"expected_revision":rejected.tracking.revision,"constellation":"empty"}))
        .send()
        .await
        .unwrap();
    assert!(correction.status().is_success());
    let corrected = s.require_project(&rejected.id).await.unwrap();
    let success = client
        .post(format!("{rejected_base}/start"))
        .json(&json!({"expected_revision":corrected.tracking.revision}))
        .send()
        .await
        .unwrap();
    assert!(
        success.status().is_success(),
        "{}",
        success.text().await.unwrap()
    );
    assert_eq!(
        s.require_relationship(&rejected.id, &rejected_rel.id)
            .await
            .unwrap()
            .vape_instance_id
            .as_deref(),
        Some("created-3")
    );
    assert_eq!(provider.created.lock().unwrap().len(), 3);

    // Nested status contracts describe the actual compound response, not a
    // fictitious `status` wrapper.
    let response: Value = client
        .get(format!(
            "{base}/instances/{}/status",
            s.relationships(&p.id).await.unwrap().primary().unwrap().id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let contract: Value = client
        .get(format!("http://127.0.0.1:{port}/api/v2/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let schema = &contract["components"]["schemas"]["InstanceStatusResponse"];
    assert_contract(&response, schema, &contract);
    assert_eq!(response["instance"]["id"], "primary-a");
    assert_eq!(response["agent_status"]["active_harness"], "pida");
    assert_eq!(response["pida_status"]["ready"], true);
    let related_id = s
        .relationships(&p.id)
        .await
        .unwrap()
        .instances
        .into_iter()
        .find(|r| r.vape_instance_id.as_deref() == Some("secondary-b"))
        .unwrap()
        .id;
    let stopped: Value = client
        .get(format!("{base}/instances/{related_id}/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_contract(&stopped, schema, &contract);
    assert!(stopped["agent_status"].is_null());
    assert!(stopped["pida_status"].is_null());

    // No LD token is configured in the child process; reads still return the
    // complete project with an explicit unavailable result for each flag.
    let p = s.require_project(&p.id).await.unwrap();
    s.mutate_project(&p.id, p.tracking.revision, |p| {
        p.tracking.launchdarkly_flags.push(FlagReference {
            id: id(),
            key: "feature.test".into(),
            project_key: "default".into(),
            environment_key: "production".into(),
            fully_enabled_value: true,
            label: None,
            notes: None,
        });
        Ok(())
    })
    .await
    .unwrap();
    let detail: Value = client
        .get(&base)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["project"]["launchdarkly_flags"][0]["status"]["available"],
        false
    );
    assert!(
        detail["project"]["launchdarkly_flags"][0]["status"]["error"]
            .as_str()
            .unwrap()
            .contains("not configured")
    );
    child.kill().await.unwrap();
    task.abort();
    std::fs::remove_dir_all(d).unwrap();
}

fn assert_contract(value: &Value, schema: &Value, contract: &Value) {
    if let Some(reference) = schema["$ref"].as_str() {
        return assert_contract(
            value,
            &contract.pointer(reference.trim_start_matches('#')).unwrap(),
            contract,
        );
    }
    if let Some(variants) = schema["anyOf"].as_array() {
        if value.is_null() {
            assert!(variants.iter().any(|s| s["type"] == "null"));
            return;
        }
        return assert_contract(
            value,
            variants.iter().find(|s| s["type"] != "null").unwrap(),
            contract,
        );
    }
    if schema["type"] == "object" {
        let object = value.as_object().expect("contract object");
        if let Some(required) = schema["required"].as_array() {
            for key in required {
                assert!(object.contains_key(key.as_str().unwrap()), "missing {key}");
            }
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (key, value) in object {
                let schema = properties
                    .get(key)
                    .unwrap_or_else(|| panic!("undocumented nested status property {key}"));
                assert_contract(value, schema, contract);
            }
        }
    } else if schema["type"] == "boolean" {
        assert!(value.is_boolean());
    } else if schema["type"] == "string" {
        assert!(value.is_string());
    }
}
