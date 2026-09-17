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
fn project_and_instance_lifecycles_are_distinct() {
    let dashboard_html = include_str!("../static/index.html");
    let project_html = include_str!("../static/project.html");
    let dashboard_js = include_str!("../static/app.js");
    let project_js = include_str!("../static/project.js");

    assert!(dashboard_html.contains("Project status"));
    assert!(dashboard_html.contains("Instance status"));
    assert!(project_html.contains("Start project"));
    assert!(project_html.contains("Pause project"));
    assert!(project_html.contains("Start instance"));
    assert!(project_html.contains("Stop instance"));
    assert!(dashboard_js.contains("/instance/start"));
    assert!(dashboard_js.contains("/instance/stop"));
    assert!(project_js.contains("/instance/${action}"));
}
