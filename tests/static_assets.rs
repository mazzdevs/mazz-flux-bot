#[test]
fn create_project_dialog_exposes_loading_state() {
    let html = include_str!("../static/index.html");
    let js = include_str!("../static/app.js");
    let css = include_str!("../static/style.css");

    assert!(html.contains("id=\"create-submit\""));
    assert!(html.contains("id=\"create-submit-label\""));
    assert!(html.contains("class=\"button-spinner\""));
    assert!(js.contains("Creating project…"));
    assert!(js.contains("setAttribute(\"aria-busy\", \"true\")"));
    assert!(js.contains("createSubmit.disabled = true"));
    assert!(css.contains(".btn.is-loading .button-spinner"));
}

#[test]
fn project_v2_ui_targets_relationships_and_separates_delivery() {
    let dashboard_html = include_str!("../static/index.html");
    let project_html = include_str!("../static/project.html");
    let dashboard_js = include_str!("../static/app.js");
    let project_js = include_str!("../static/project.js");
    assert!(dashboard_html.contains("Automation"));
    assert!(project_html.contains("primary only"));
    assert!(project_js.contains("/api/v2/projects/"));
    assert!(dashboard_js.contains("/api/v2/projects"));
    assert!(project_js.contains("/instances/${rid}"));
    assert!(project_js.contains("dataset.dirty"));
    assert!(project_js.contains("expected_revision"));
    assert!(!dashboard_js.contains("/instance/start"));
    assert!(!project_js.contains("/api/projects/"));
}
