//! Native execution tests use a subprocess and loopback providers only.
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use mazz_flux_bot::{execution::*, models::PidaStatus, projects::*, store::Store};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
fn dir() -> std::path::PathBuf {
    let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-state")
        .join(id());
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}
fn active(state: &str, condition: &str, started: u64) -> Value {
    json!({"condition":condition,"startedAt":started,"turns":1,"maxTurns":20,"state":state})
}
fn snapshot(goal: Value, terminal: Option<Value>) -> String {
    frame(
        json!({"type":"history","messages":terminal.map(|goal|vec![json!({"id":"terminal-row","role":"assistant","blocks":[{"type":"command_result","command":"flux-goal","goal":goal}]})]).unwrap_or_default()}),
    ) + &frame(json!({"type":"goal_state","goal":goal}))
}
#[test]
fn native_wire_and_structured_snapshots() {
    let s:PidaStatus=serde_json::from_value(json!({"ready":true,"isStreaming":true,"pendingAsk":{"id":"ask"},"isCompacting":true,"lastTurnSettledAt":"now","activeSubagents":2,"turnLiveness":{"state":"working"},"sessionId":"session-a","autoMode":false,"model":"p/m","capabilities":{"idempotencyKeys":true}})).unwrap();
    assert_eq!(s.is_streaming, Some(true));
    assert_eq!(s.is_compacting, Some(true));
    assert_eq!(s.active_subagents, Some(2));
    assert_eq!(s.session_id.as_deref(), Some("session-a"));
    assert_eq!(s.auto_mode, Some(false));
    assert!(s.pending_ask.is_some());
    assert!(s.turn_liveness.is_some());
    let old:PidaStatus=serde_json::from_value(json!({"ready":true,"is_streaming":false,"is_compacting":false,"active_subagents":0,"session_id":"old","auto_mode":false})).unwrap();
    assert_eq!(old.session_id.as_deref(), Some("old"));
    assert_eq!(old.is_streaming, Some(false));
    let unknown: PidaStatus = serde_json::from_value(json!({"ready":true})).unwrap();
    assert_eq!(unknown.is_streaming, None);
    assert_eq!(unknown.auto_mode, None);
    for state in ["active", "evaluating", "paused"] {
        let o = parse_snapshot(&snapshot(active(state, "tests pass", 10), None));
        assert_eq!(o.state, state);
        assert_eq!(o.started_at, Some(10));
    }
    let o = parse_snapshot(&snapshot(Value::Null, None));
    assert_eq!(o.state, "no_goal");
    assert_eq!(o.verdict, None);
    for (kind, stop, status) in [
        ("verdict", "met", "met"),
        ("verdict", "impossible", "impossible"),
        ("failed", "turn_limit", "not_met"),
        ("failed", "stalled", "not_met"),
        ("clear", "clear", ""),
    ] {
        let o = parse_snapshot(&snapshot(
            Value::Null,
            Some(json!({"kind":kind,"condition":"tests pass","status":status,"stop":stop})),
        ));
        assert_eq!(o.state, "terminal");
        assert_eq!(o.terminal_reason.as_deref(), Some(stop));
        assert!(o.terminal_fingerprint.is_some());
    }
    assert_eq!(
        parse_snapshot(&frame(
            json!({"type":"history","messages":[{"role":"assistant","text":"goal met"}]})
        ))
        .state,
        "unknown"
    );
    assert_eq!(parse_snapshot("data: not json\n\n").state, "unknown");
    assert_eq!(
        parse_snapshot(&snapshot(json!({"condition":"x","state":"future"}), None)).state,
        "unknown"
    );
    let no_goal = frame(json!({"type":"goal_state","goal":null}));
    for bad in [
        "data: {\"type\":\"goal_state\",\"goal\":null}",
        "data: broken\n\n",
        "data: {\"type\":\"goal_state\"}\n\n",
    ] {
        assert_eq!(parse_snapshot(&(no_goal.clone() + bad)).state, "unknown");
    }
    let stale = json!({"kind":"clear","condition":"same","reason":"old clear"});
    let old_fingerprint =
        parse_snapshot(&snapshot(Value::Null, Some(stale.clone()))).terminal_fingerprint;
    for state in ["active", "paused"] {
        let o = parse_snapshot(&snapshot(active(state, "same", 3), Some(stale.clone())));
        assert_eq!(o.state, state);
        assert_eq!(o.terminal_fingerprint, old_fingerprint);
    }
    assert_eq!(goal_command(" clear ").unwrap(), "/goal -- clear");
    assert_eq!(
        goal_command("--require unsafe").unwrap(),
        "/goal -- --require unsafe"
    );
    assert_eq!(
        goal_command("\u{feff}tests pass\u{feff}").unwrap(),
        "/goal tests pass"
    );
    assert!(goal_command(&"😀".repeat(2000)).is_ok());
    assert!(goal_command(&"😀".repeat(2001)).is_err());
    let with_reason = parse_snapshot(&snapshot(
        Value::Null,
        Some(
            json!({"kind":"verdict","condition":"tests","status":"met","stop":"met","reason":"Fresh test evidence"}),
        ),
    ));
    assert_eq!(
        with_reason.terminal_reason.as_deref(),
        Some("Fresh test evidence")
    );
    assert!(goal_command("  ").is_err());
    assert!(
        goal_command("Read @repo/file and finish")
            .unwrap_err()
            .to_string()
            .contains("@-mentions")
    );
    let terminal = json!({"kind":"verdict","condition":"x","stop":"met","status":"met"});
    let history = parse_snapshot(&snapshot(Value::Null, Some(terminal.clone())));
    let replay = parse_snapshot(
        &(frame(json!({"type":"command_result","command":"flux-goal","goal":terminal}))
            + &frame(json!({"type":"goal_state","goal":null}))),
    );
    assert_eq!(
        history.terminal_fingerprint, replay.terminal_fingerprint,
        "same terminal cannot look new when replay transport changes"
    );
}
#[tokio::test]
async fn schema_two_migrates_to_legacy_while_new_projects_bootstrap() {
    let d = dir();
    let store = Store::open(&d).await.unwrap();
    let p = store
        .create_project(mazz_flux_bot::models::CreateProjectRequest {
            name: Some("New".into()),
            goal: "".into(),
            heartbeat_prompt: None,
            constellation: Some("empty".into()),
            heartbeat_interval_secs: None,
        })
        .await
        .unwrap();
    assert_eq!(p.tracking.execution_mode, ExecutionMode::BootstrapOnly);
    let mut rel = Relationship::pending(&p, "Primary".into(), "empty".into(), None);
    rel.goal_override = Some("preserve primary override".into());
    rel.vape_instance_id = Some("legacy-instance".into());
    store
        .add_relationship(&p.id, p.tracking.revision, rel, true)
        .await
        .unwrap();

    let path = d.join("projects").join(format!("{}.json", p.id));
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["schema_version"] = json!(2);
    v["goal"] = json!("preserve old direction");
    v.as_object_mut().unwrap().remove("execution_mode");
    std::fs::write(&path, v.to_string()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let store = Store::open_for_migration(&d).await.unwrap();
    let dry = store.migrate(true).await.unwrap();
    assert!(dry.errors.is_empty());
    assert!(dry.records[0].migrated);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let store = Store::open(&d).await.unwrap();
    let old = store.require_project(&p.id).await.unwrap();
    assert_eq!(old.tracking.execution_mode, ExecutionMode::LegacyConductor);
    assert_eq!(old.goal, "preserve old direction");
    assert_eq!(old.tracking.schema_version, 3);
    assert_eq!(
        store
            .relationships(&p.id)
            .await
            .unwrap()
            .primary()
            .unwrap()
            .goal_override
            .as_deref(),
        Some("preserve primary override")
    );

    assert!(!store.migrate(true).await.unwrap().records[0].migrated);
    for mode in [ExecutionMode::BootstrapOnly, ExecutionMode::NativeGoal] {
        let p = store.require_project(&p.id).await.unwrap();
        let p = store
            .mutate_project(&p.id, p.tracking.revision, |p| {
                p.tracking.execution_mode = mode;
                p.goal.clear();
                p.heartbeat_enabled = true;
                p.status = "running".into();
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            p.heartbeat_enabled,
            "deprecated goals must not control nonlegacy monitoring"
        );
    }
    std::fs::remove_dir_all(d).unwrap();
}
#[derive(Clone)]
struct Provider {
    data: Arc<Mutex<ProviderData>>,
}
struct ProviderData {
    calls: Vec<String>,
    created: Vec<Value>,
    sent: Vec<String>,
    status: String,
    session: String,
    stream: String,
    commands: bool,
    auto_mode: bool,
    ready: bool,
    delayed_chunks: Vec<(u64, String)>,
    switch_on_commands: Option<String>,
    status_reads: usize,
    auto_on_status_read: Option<usize>,
}
async fn provider(State(provider): State<Provider>, req: axum::extract::Request) -> Response {
    let path = req.uri().path().to_owned();
    let method = req.method().to_string();
    let bytes = axum::body::to_bytes(req.into_body(), 200000).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut p = provider.data.lock().unwrap();
    p.calls.push(format!("{method} {path}"));
    let ok = |v: Value| Json(v).into_response();
    if path == "/api/v1/instances" {
        if method == "POST" {
            assert!(
                body.get("job").is_none(),
                "nonlegacy create dispatched a job: {body}"
            );
            assert!(body["model"].is_string());
            let mut v = body.clone();
            let iid = if p.created.is_empty() {
                "created-primary".to_string()
            } else {
                format!("created-{}", p.created.len() + 1)
            };
            v["id"] = json!(iid);
            v["status"] = json!("Starting");
            v["ready"] = json!(false);
            p.created.push(v);
            p.status = "Starting".into();
            return ok(json!({"id":iid}));
        }
        return ok(json!(p.created));
    }
    if let Some(iid) = path
        .strip_prefix("/api/v1/instances/")
        .filter(|suffix| !suffix.contains('/'))
    {
        if !p.created.iter().any(|v| v["id"] == iid) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if p.status == "Missing" {
            return StatusCode::NOT_FOUND.into_response();
        }
        return ok(
            json!({"id":iid,"name":"Primary","status":p.status,"ready":p.status=="Running"}),
        );
    }
    if path.starts_with("/api/v1/instances/created-") && path.ends_with("/start") {
        p.status = "Starting".into();
        return ok(json!({"ok":true}));
    }
    if path.ends_with("/pida/api/status") {
        p.status_reads += 1;
        if p.auto_on_status_read == Some(p.status_reads) {
            p.auto_mode = true;
        }
        return ok(
            json!({"ready":p.ready,"isStreaming":false,"isCompacting":false,"pendingAsk":null,"activeSubagents":0,"sessionId":p.session,"autoMode":p.auto_mode,"handoff":"chat","capabilities":{"idempotencyKeys":true}}),
        );
    }
    if path.ends_with("/pida/api/commands") {
        if let Some(session) = p.switch_on_commands.take() {
            p.session = session;
        }
        return ok(
            json!({"commands":if p.commands {vec![json!({"name":"goal","execution":"prompt","source":"extension"})]}else{vec![]}}),
        );
    }
    if path.ends_with("/pida/api/stream") {
        if !p.delayed_chunks.is_empty() {
            let chunks = p.delayed_chunks.clone();
            drop(p);
            let (sender, receiver) =
                tokio::sync::mpsc::channel::<Result<Vec<u8>, std::convert::Infallible>>(1);
            tokio::spawn(async move {
                for (delay, chunk) in chunks {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    if sender.send(Ok(chunk.into_bytes())).await.is_err() {
                        break;
                    }
                }
            });
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(
                    receiver,
                )),
            )
                .into_response();
        }
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            p.stream.clone(),
        )
            .into_response();
    }
    if path.contains("/pida/api/sessions/") && path.ends_with("/chat") {
        p.sent.push(body["message"].as_str().unwrap().into());
        return (StatusCode::ACCEPTED, Json(json!({"ok":true}))).into_response();
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":"unexpected test provider path"})),
    )
        .into_response()
}
async fn call(client: &reqwest::Client, method: &str, path: &str, body: Value) -> (u16, Value) {
    let mut r = client.request(method.parse().unwrap(), path);
    if method != "GET" {
        r = r.json(&body);
    }
    let response = r.send().await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.bytes().await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn get(client: &reqwest::Client, base: &str) -> Value {
    let (status, v) = call(client, "GET", base, Value::Null).await;
    assert_eq!(status, 200, "{v}");
    v["project"].clone()
}
#[tokio::test]
async fn loopback_bootstrap_and_native_goal_ownership() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let d = dir();
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let gh = d.join("bin/gh");
    std::fs::write(&gh, "#!/bin/sh\nprintf 'test-only-token\\n'\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let provider = Provider {
        data: Arc::new(Mutex::new(ProviderData {
            calls: vec![],
            created: vec![],
            sent: vec![],
            status: "Starting".into(),
            session: "session-a".into(),
            stream: snapshot(Value::Null, None),
            commands: false,
            auto_mode: false,
            ready: true,
            delayed_chunks: vec![],
            switch_on_commands: None,
            status_reads: 0,
            auto_on_status_read: None,
        })),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(crate::provider)
        .with_state(provider.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mazz-flux-bot"))
        .current_dir(&root)
        .env("PORT", port.to_string())
        .env("MAZZ_FLUX_DATA_DIR", &d)
        .env("MAZZ_FLUX_LIVE", "1")
        .env(
            "PATH",
            format!(
                "{}:{}",
                d.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("CADMIUM_VAPE_URL", &upstream)
        .env("OPENROUTER_API_URL", format!("{upstream}/llm-must-not-run"))
        .env("MAZZ_FLUX_PUBLIC_BASE_URL", "https://bot.example")
        .env("HEARTBEAT_SCAN_INTERVAL_SECS", "3600")
        .env_remove("LD_ACCESS_TOKEN")
        .env_remove("MAZZ_FLUX_STATE_REPO_URL")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/api/v2/projects");
    let mut ready = false;
    for _ in 0..150 {
        if client
            .get(format!("http://127.0.0.1:{port}/api/v2/openapi.json"))
            .send()
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ready);
    let (status,v)=call(&client,"POST",&url,json!({"name":"Tracking only","description":"Do not execute production rollout tasks","constellation":"empty"})).await;
    assert_eq!(status, 200, "{v}");
    let pid = v["project"]["id"].as_str().unwrap();
    let base = format!("{url}/{pid}");
    assert_eq!(v["project"]["execution_mode"], "bootstrap_only");
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/start"),
        json!({"expected_revision":v["revision"]}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(provider.data.lock().unwrap().created.len(), 1);
    assert!(provider.data.lock().unwrap().sent.is_empty());
    provider.data.lock().unwrap().status = "Running".into();
    provider.data.lock().unwrap().commands = true;
    provider.data.lock().unwrap().switch_on_commands = Some("switched-session".into());
    let p = get(&client, &base).await;
    let (status, value) = call(
        &client,
        "POST",
        &format!("{base}/heartbeat/force"),
        json!({"expected_revision":p["revision"]}),
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert!(
        provider.data.lock().unwrap().sent.is_empty(),
        "session A bootstrap must not dispatch after observing session B"
    );
    assert!(
        !d.join("bootstrap")
            .join(pid)
            .join("created-primary/session-a.json")
            .exists()
    );
    assert!(
        !d.join("bootstrap")
            .join(pid)
            .join("created-primary/switched-session.json")
            .exists()
    );
    provider.data.lock().unwrap().session = "session-a".into();
    provider.data.lock().unwrap().status_reads = 0;
    provider.data.lock().unwrap().auto_on_status_read = Some(5);
    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/heartbeat/force"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    assert!(
        provider.data.lock().unwrap().sent.is_empty(),
        "last-moment Auto Tend activation must block bootstrap"
    );
    assert!(
        !d.join("bootstrap")
            .join(pid)
            .join("created-primary/session-a.json")
            .exists()
    );
    provider.data.lock().unwrap().auto_mode = false;
    provider.data.lock().unwrap().auto_on_status_read = None;
    provider.data.lock().unwrap().commands = false;
    let p = get(&client, &base).await;
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/heartbeat/force"),
        json!({"expected_revision":p["revision"]}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    {
        let p = provider.data.lock().unwrap();
        assert_eq!(p.sent.len(), 1);
        assert!(p.sent[0].contains("context-only bootstrap"));
        assert!(p.sent[0].contains("wait for explicit"));
        assert!(p.sent[0].contains("/agent-context"));
        assert!(!p.sent[0].contains("/goal "));
    }
    let p = get(&client, &base).await;
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/start"),
        json!({"expected_revision":p["revision"]}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(provider.data.lock().unwrap().sent.len(), 1);
    provider.data.lock().unwrap().session = "session-b".into();
    let p = get(&client, &base).await;
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/heartbeat/force"),
        json!({"expected_revision":p["revision"]}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(provider.data.lock().unwrap().sent.len(), 2);
    // Detach/reattach creates a new local relationship, not a new Pida session.
    // It must not resend the same project's context bootstrap.
    let p = get(&client, &base).await;
    let old_relationship = p["primary_instance_relationship_id"].as_str().unwrap();
    assert_eq!(
        call(
            &client,
            "DELETE",
            &format!("{base}/instances/{old_relationship}"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    let p = get(&client, &base).await;
    let (status,value)=call(&client,"POST",&format!("{base}/instances"),json!({"expected_revision":p["revision"],"mode":"attach","vape_instance_id":"created-primary","label":"Reattached","is_primary":true})).await;
    assert_eq!(status, 200, "{value}");
    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/start"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    assert_eq!(provider.data.lock().unwrap().sent.len(), 2);
    provider.data.lock().unwrap().status = "Stopped".into();
    let p = get(&client, &base).await;
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/start"),
        json!({"expected_revision":p["revision"]}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert!(
        provider
            .data
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|p| p.ends_with("/created-primary/start"))
    );
    assert_eq!(provider.data.lock().unwrap().sent.len(), 2);
    provider.data.lock().unwrap().status = "Missing".into();
    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/start"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        502
    );
    assert_eq!(provider.data.lock().unwrap().created.len(), 1);
    provider.data.lock().unwrap().status = "Running".into();
    let p = get(&client, &base).await;
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/execution/mode"),
        json!({"expected_revision":p["revision"],"execution_mode":"native_goal"}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(provider.data.lock().unwrap().sent.len(), 2);
    let p = get(&client, &base).await;
    let rev = p["revision"].clone();
    for native in [
        active("active", "external", 1),
        active("evaluating", "external", 1),
        active("paused", "external", 1),
        json!({"state":"unsupported"}),
    ] {
        provider.data.lock().unwrap().stream = snapshot(native, None);
        for mode in ["bootstrap_only", "legacy_conductor"] {
            let (status, value) = call(
                &client,
                "POST",
                &format!("{base}/execution/mode"),
                json!({"expected_revision":rev,"execution_mode":mode}),
            )
            .await;
            assert_eq!(status, 409, "{mode}: {value}");
        }
    }
    provider.data.lock().unwrap().stream = snapshot(Value::Null, None);
    let request = json!({"expected_revision":rev,"request_id":"request-one","expected_session_id":"session-b","condition":"clear"});
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/execution/goals/start"),
        request.clone(),
    )
    .await;
    assert_eq!(status, 409, "{v}");
    assert_eq!(v["error"]["code"], "goal_unavailable");
    provider.data.lock().unwrap().commands = true;
    provider.data.lock().unwrap().auto_mode = true;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/execution/goals/start"),
            request.clone()
        )
        .await
        .0,
        409
    );
    provider.data.lock().unwrap().auto_mode = false;
    provider.data.lock().unwrap().stream = ":".repeat(1_048_577);
    let before = provider.data.lock().unwrap().sent.len();
    let (_, oversized) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert_eq!(oversized["observation"]["state"], "unknown");
    assert_eq!(provider.data.lock().unwrap().sent.len(), before);
    provider.data.lock().unwrap().stream = frame(json!({"type":"history","messages":[]}));
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/execution/goals/start"),
            request.clone()
        )
        .await
        .0,
        409
    );
    provider.data.lock().unwrap().stream = snapshot(Value::Null, None);
    provider.data.lock().unwrap().session = "".into();
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/execution/goals/start"),
            request.clone()
        )
        .await
        .0,
        409
    );
    provider.data.lock().unwrap().session = "session-b".into();
    // Keep complete no_goal followed by delayed state; incomplete/malformed
    // evidence must not leave the earlier no_goal actionable.
    for tail in [
        "data: {\"type\":\"goal_state\",\"goal\":null}".to_string(),
        "data: not-json\n\n".to_string(),
    ] {
        provider.data.lock().unwrap().delayed_chunks =
            vec![(0, snapshot(Value::Null, None)), (100, tail)];
        let (_, result) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
        assert_eq!(result["observation"]["state"], "unknown", "{result}");
    }
    provider.data.lock().unwrap().delayed_chunks = vec![
        (0, snapshot(Value::Null, None)),
        (
            300,
            frame(json!({"type":"goal_state","goal":active("active","delayed",2)})),
        ),
    ];
    let (_, result) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert_eq!(result["observation"]["state"], "active");
    let mut cutoff = vec![(0, snapshot(Value::Null, None))];
    cutoff.extend((0..7).map(|_| (350, ": heartbeat\n\n".into())));
    provider.data.lock().unwrap().delayed_chunks = cutoff;
    let started = std::time::Instant::now();
    let (_, result) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert_eq!(result["observation"]["state"], "unknown");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    provider.data.lock().unwrap().delayed_chunks = vec![];
    let too_long = json!({"expected_revision":rev,"request_id":"too-long","expected_session_id":"session-b","condition":"😀".repeat(2001)});
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/execution/goals/start"),
            too_long
        )
        .await
        .0,
        422
    );
    let mentions = json!({"expected_revision":rev,"request_id":"at-mention","expected_session_id":"session-b","condition":"Finish @repo/file"});
    let before = provider.data.lock().unwrap().sent.len();
    let (status, value) = call(
        &client,
        "POST",
        &format!("{base}/execution/goals/start"),
        mentions,
    )
    .await;
    assert_eq!(status, 422, "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("@-mentions")
    );
    assert_eq!(provider.data.lock().unwrap().sent.len(), before);
    assert!(
        !d.join("executions")
            .join(pid)
            .join("requests/at-mention.json")
            .exists()
    );
    let wrong_session = json!({"expected_revision":rev,"request_id":"wrong-session","expected_session_id":"stale-session","condition":"Do not dispatch"});
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/execution/goals/start"),
            wrong_session
        )
        .await
        .0,
        409
    );
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/execution/goals/start"),
        request.clone(),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["activation"], "pending_observation");
    assert_eq!(
        provider.data.lock().unwrap().sent.last().unwrap(),
        "/goal -- clear"
    );
    let count = provider.data.lock().unwrap().sent.len();
    let (status, v) = call(
        &client,
        "POST",
        &format!("{base}/execution/goals/start"),
        request.clone(),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    let (_, observed) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert_eq!(
        observed["receipts"][0]["progress"]["dispatch_status"],
        "pending"
    );
    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/archive"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        409
    );
    provider.data.lock().unwrap().stream = snapshot(active("active", "clear", 100), None);
    let (_, observed) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert_eq!(
        observed["receipts"][0]["progress"]["dispatch_status"],
        "observed"
    );
    assert_eq!(observed["receipts"][0]["request"]["condition"], "clear");
    let receipt_path = d
        .join("executions")
        .join(pid)
        .join("requests/request-one.json");
    let immutable = std::fs::read(&receipt_path).unwrap();
    let p = get(&client, &base).await;
    let pause_count = provider.data.lock().unwrap().sent.len();
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/pause"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        provider.data.lock().unwrap().sent.len(),
        pause_count,
        "monitoring pause must never send native stop/clear"
    );
    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/archive"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        409
    );
    assert_eq!(call(&client,"POST",&format!("{base}/execution/goals/start"),json!({"expected_revision":p["revision"],"request_id":"active-conflict","expected_session_id":"session-b","condition":"Another goal"})).await.0,409);

    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/heartbeat/force"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    provider.data.lock().unwrap().stream = snapshot(active("paused", "clear", 100), None);
    let (_, v) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert_eq!(v["observation"]["state"], "paused");
    let p = get(&client, &base).await;
    assert_eq!(call(&client,"POST",&format!("{base}/execution/goals/resume"),json!({"expected_revision":p["revision"],"request_id":"resume-one","expected_session_id":"session-b"})).await.0,200);
    assert_eq!(
        provider.data.lock().unwrap().sent.last().unwrap(),
        "/goal resume"
    );
    provider.data.lock().unwrap().stream = snapshot(
        Value::Null,
        Some(json!({"kind":"verdict","status":"met","stop":"met","condition":"clear"})),
    );
    let (_, v) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert!(
        v["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["progress"]["dispatch_status"] == "terminal")
    );
    assert_eq!(std::fs::read(&receipt_path).unwrap(), immutable);
    for mode in [
        "bootstrap_only",
        "native_goal",
        "legacy_conductor",
        "native_goal",
    ] {
        let p = get(&client, &base).await;
        let count = provider.data.lock().unwrap().sent.len();
        assert_eq!(
            call(
                &client,
                "POST",
                &format!("{base}/execution/mode"),
                json!({"expected_revision":p["revision"],"execution_mode":mode})
            )
            .await
            .0,
            200
        );
        assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    }
    // A new request is explicit; its receipt can never follow a later focused session.
    let p = get(&client, &base).await;
    let (status,v)=call(&client,"POST",&format!("{base}/execution/goals/start"),json!({"expected_revision":p["revision"],"request_id":"request-two","expected_session_id":"session-b","condition":"--bounded condition"})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        provider.data.lock().unwrap().sent.last().unwrap(),
        "/goal -- --bounded condition"
    );
    let stale_clear =
        json!({"kind":"clear","condition":"--bounded condition","reason":"historical clear"});
    provider.data.lock().unwrap().stream = snapshot(
        active("active", "--bounded condition", 200),
        Some(stale_clear.clone()),
    );
    call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    let p = get(&client, &base).await;
    let (status,value)=call(&client,"POST",&format!("{base}/execution/goals/clear"),json!({"expected_revision":p["revision"],"request_id":"clear-one","expected_session_id":"session-b"})).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["activation"], "pending_observation");
    assert_eq!(
        provider.data.lock().unwrap().sent.last().unwrap(),
        "/goal clear"
    );
    provider.data.lock().unwrap().stream = snapshot(Value::Null, Some(stale_clear));
    let (_, observed) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    let clear = observed["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request"]["request_id"] == "clear-one")
        .unwrap();
    assert_ne!(
        clear["progress"]["dispatch_status"], "terminal",
        "unchanged historical clear cannot settle new clear receipt"
    );
    provider.data.lock().unwrap().stream = snapshot(
        Value::Null,
        Some(
            json!({"kind":"clear","condition":"different condition","reason":"off-condition clear"}),
        ),
    );
    let (_, observed) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    let clear = observed["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request"]["request_id"] == "clear-one")
        .unwrap();
    assert_ne!(
        clear["progress"]["dispatch_status"], "terminal",
        "clear rows must match the receipt condition"
    );
    provider.data.lock().unwrap().stream = snapshot(
        Value::Null,
        Some(
            json!({"kind":"clear","condition":"--bounded condition","stop":"clear","reason":"fresh clear after request"}),
        ),
    );
    call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    let p = get(&client, &base).await;
    assert_eq!(call(&client,"POST",&format!("{base}/execution/goals/start"),json!({"expected_revision":p["revision"],"request_id":"request-three","expected_session_id":"session-b","condition":"New exact condition"})).await.0,200);
    provider.data.lock().unwrap().session = "session-c".into();
    provider.data.lock().unwrap().stream = snapshot(Value::Null, None);
    let (_, v) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert!(
        v["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["request"]["request_id"] == "request-three"
                && r["progress"]["dispatch_status"] == "session_changed")
    );
    let count = provider.data.lock().unwrap().sent.len();
    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/heartbeat/force"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    provider.data.lock().unwrap().stream = frame(json!({"type":"history","messages":[]}));
    let p = get(&client, &base).await;
    let (status, error) = call(
        &client,
        "POST",
        &format!("{base}/archive"),
        json!({"expected_revision":p["revision"]}),
    )
    .await;
    assert_eq!(status, 409, "{error}");
    provider.data.lock().unwrap().stream = snapshot(Value::Null, None);
    // Reopening the on-disk store preserves receipts and never dispatches work.
    let receipts_before = provider.data.lock().unwrap().sent.len();
    let reopened = Store::open(&d).await.unwrap();
    assert!(
        reopened
            .execution_receipts(pid)
            .await
            .unwrap()
            .iter()
            .any(|r| r.request_id == "request-three")
    );
    assert_eq!(std::fs::read(&receipt_path).unwrap(), immutable);
    assert_eq!(provider.data.lock().unwrap().sent.len(), receipts_before);
    // Rebinding a relationship must not carry a native request onto another
    // instance, even when the old session label could be reused/restored.
    let p = get(&client, &base).await;
    assert_eq!(call(&client,"POST",&format!("{base}/execution/goals/start"),json!({"expected_revision":p["revision"],"request_id":"request-four","expected_session_id":"session-c","condition":"Explicit new request"})).await.0,200);
    let relationship = p["primary_instance_relationship_id"].as_str().unwrap();
    reopened
        .bind_instance(pid, relationship, "replacement-missing")
        .await
        .unwrap();
    let count = provider.data.lock().unwrap().sent.len();
    let (_, observed) = call(&client, "GET", &format!("{base}/execution"), Value::Null).await;
    assert!(
        observed["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["request"]["request_id"] == "request-four"
                && r["progress"]["dispatch_status"] == "session_changed")
    );
    assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    reopened
        .bind_instance(pid, relationship, "created-primary")
        .await
        .unwrap();

    let p = get(&client, &base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{base}/archive"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    assert!(
        !provider
            .data
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|p| p.contains("llm"))
    );
    assert_eq!(get(&client, &base).await["status"], "pending");
    // Resolved native evidence permits both ownership transfers without
    // dispatching any work; switching back to native also launches nothing.
    for mode in [
        "bootstrap_only",
        "native_goal",
        "legacy_conductor",
        "native_goal",
    ] {
        let p = get(&client, &base).await;
        // Project was archived above; restore it before explicit mode selection.
        if !p["archived_at"].is_null() {
            assert_eq!(
                call(
                    &client,
                    "POST",
                    &format!("{base}/restore"),
                    json!({"expected_revision":p["revision"]})
                )
                .await
                .0,
                200
            );
        }
        let p = get(&client, &base).await;
        let count = provider.data.lock().unwrap().sent.len();
        assert_eq!(
            call(
                &client,
                "POST",
                &format!("{base}/execution/mode"),
                json!({"expected_revision":p["revision"],"execution_mode":mode})
            )
            .await
            .0,
            200
        );
        assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    }
    let (status, new) = call(
        &client,
        "POST",
        &url,
        json!({"name":"Native initialization","constellation":"empty"}),
    )
    .await;
    assert_eq!(status, 200);
    let native_base = format!("{url}/{}", new["project"]["id"].as_str().unwrap());
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{native_base}/execution/mode"),
            json!({"expected_revision":new["revision"],"execution_mode":"native_goal"})
        )
        .await
        .0,
        200
    );
    let p = get(&client, &native_base).await;
    let count = provider.data.lock().unwrap().sent.len();
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{native_base}/start"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    let p = get(&client, &native_base).await;
    let native_pid = p["id"].as_str().unwrap();
    let native_rid = p["primary_instance_relationship_id"].as_str().unwrap();
    let initialization_path = d
        .join("bootstrap")
        .join(native_pid)
        .join("initialization")
        .join(format!("{native_rid}.json"));
    let pending: Value =
        serde_json::from_slice(&std::fs::read(&initialization_path).unwrap()).unwrap();
    assert_eq!(pending["pending"], true);
    assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    provider.data.lock().unwrap().status = "Running".into();
    provider.data.lock().unwrap().session = "native-ready-session".into();
    provider.data.lock().unwrap().commands = false;
    provider.data.lock().unwrap().ready = false;
    let p = get(&client, &native_base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{native_base}/heartbeat/force"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    assert_eq!(provider.data.lock().unwrap().sent.len(), count);
    let pending: Value =
        serde_json::from_slice(&std::fs::read(&initialization_path).unwrap()).unwrap();
    assert_eq!(pending["pending"], true);
    provider.data.lock().unwrap().ready = true;
    for _ in 0..2 {
        let p = get(&client, &native_base).await;
        assert_eq!(
            call(
                &client,
                "POST",
                &format!("{native_base}/heartbeat/force"),
                json!({"expected_revision":p["revision"]})
            )
            .await
            .0,
            200
        );
    }
    assert_eq!(provider.data.lock().unwrap().sent.len(), count + 1);
    assert!(
        provider
            .data
            .lock()
            .unwrap()
            .sent
            .last()
            .unwrap()
            .contains("context-only bootstrap")
    );
    let settled: Value =
        serde_json::from_slice(&std::fs::read(&initialization_path).unwrap()).unwrap();
    assert_eq!(settled["pending"], false);
    provider.data.lock().unwrap().status = "Stopped".into();
    provider.data.lock().unwrap().session = "native-restarted-session".into();
    let p = get(&client, &native_base).await;
    assert_eq!(
        call(
            &client,
            "POST",
            &format!("{native_base}/start"),
            json!({"expected_revision":p["revision"]})
        )
        .await
        .0,
        200
    );
    let pending: Value =
        serde_json::from_slice(&std::fs::read(&initialization_path).unwrap()).unwrap();
    assert_eq!(pending["pending"], true);
    assert_eq!(provider.data.lock().unwrap().sent.len(), count + 1);
    provider.data.lock().unwrap().status = "Running".into();
    for _ in 0..2 {
        let p = get(&client, &native_base).await;
        assert_eq!(
            call(
                &client,
                "POST",
                &format!("{native_base}/heartbeat/force"),
                json!({"expected_revision":p["revision"]})
            )
            .await
            .0,
            200
        );
    }
    assert_eq!(provider.data.lock().unwrap().sent.len(), count + 2);
    assert!(
        provider.data.lock().unwrap().sent[count..]
            .iter()
            .all(|message| message.contains("context-only bootstrap")
                && !message.starts_with("/goal"))
    );
    child.kill().await.unwrap();
    task.abort();
    std::fs::remove_dir_all(d).unwrap();
}
