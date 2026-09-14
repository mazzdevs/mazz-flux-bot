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
