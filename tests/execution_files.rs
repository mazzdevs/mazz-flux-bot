//! Files can inspect execution evidence but cannot modify or erase replay guards.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::get,
};
use mazz_flux_bot::{
    AppState, api,
    heartbeat::HeartbeatClock,
    projects::id,
    store::{BrowseResult, Store},
    vape_client::VapeClient,
};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
#[tokio::test]
async fn execution_and_bootstrap_files_are_read_only_in_store_and_api() {
    let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-state")
        .join(id());
    let store = Arc::new(Store::open(&d).await.unwrap());
    let state = AppState {
        store: store.clone(),
        vape: Arc::new(VapeClient::new()),
        heartbeat_clock: Arc::new(HeartbeatClock::new(15)),
    };
    let app = Router::new()
        .route(
            "/api/files",
            get(api::browse_files)
                .put(api::write_file)
                .delete(api::delete_file),
        )
        .with_state(state);
    for namespace in ["executions", "bootstrap"] {
        let rel = format!("{namespace}/project/guard.json");
        let path = d.join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "guard evidence").unwrap();
        assert!(
            matches!(store.browse(&rel).await.unwrap(),BrowseResult::File{content,..} if content=="guard evidence")
        );
        assert!(store.write_file(&rel, "replacement").await.is_err());
        assert!(store.delete_file(&rel).await.is_err());
        assert!(
            store
                .write_file(&format!("/{namespace}/new.json"), "new")
                .await
                .is_err()
        );
        std::os::unix::fs::symlink(d.join(namespace), d.join(format!("alias-{namespace}")))
            .unwrap();
        assert!(
            store
                .write_file(
                    &format!("alias-{namespace}/project/guard.json"),
                    "replacement"
                )
                .await
                .is_err()
        );
        assert!(
            store
                .delete_file(&format!("alias-{namespace}/project/guard.json"))
                .await
                .is_err()
        );
        assert!(
            store
                .write_file(
                    &format!("alias-{namespace}/project/new.json"),
                    "replacement"
                )
                .await
                .is_err()
        );
        for candidate in [rel.clone(), format!("alias-{namespace}/project/guard.json")] {
            for method in ["PUT", "DELETE"] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(format!("/api/files?path={candidate}"))
                            .header("content-type", "application/json")
                            .body(Body::from(json!({"content":"replacement"}).to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                let body = to_bytes(response.into_body(), 4096).await.unwrap();
                assert!(
                    String::from_utf8(body.to_vec())
                        .unwrap()
                        .contains("read-only")
                );
            }
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/files?path={candidate}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), "guard evidence");
        assert!(!d.join(namespace).join("new.json").exists());
    }
    store
        .write_file("notes/editable.md", "normal notes remain writable")
        .await
        .unwrap();
    store.delete_file("notes/editable.md").await.unwrap();
    std::fs::remove_dir_all(d).unwrap();
}
