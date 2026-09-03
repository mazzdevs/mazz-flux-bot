//! Behavioral tests for the heartbeat "brain" response parser, using
//! `spice_framework` (the real rust4ai test harness — not the unrelated
//! `spice` crate on crates.io, see PLAN.md).
//!
//! `parse_brain_response` is the safety-critical seam: whatever the Anthropic
//! call returns, this function decides whether it's safe to act on. These
//! tests exercise it directly with no live LLM call, no network, no API key —
//! spice's `AgentUnderTest` contract wants a "send a message, get output back"
//! shape, so `BrainAdapter` below treats the test's `user_message` as the raw
//! text the brain said, and reports the resulting `Decision` as if `action`
//! were a tool call. That's an honest fit: the thing under test really is
//! "text in, one bounded, validated decision out."

use async_trait::async_trait;
use mazz_flux_bot::heartbeat::parse_brain_response;
use serde_json::json;
use spice_framework::{test, AgentConfig, AgentOutput, AgentUnderTest, Runner, RunnerConfig, SpiceError, ToolCall, Turn};
use std::sync::Arc;

struct BrainAdapter;

#[async_trait]
impl AgentUnderTest for BrainAdapter {
    async fn run(&self, user_message: &str, _config: &AgentConfig) -> Result<AgentOutput, SpiceError> {
        let decision = parse_brain_response(user_message);
        let note = decision.note.clone().unwrap_or_default();
        let tool_call = ToolCall {
            id: "1".to_string(),
            name: decision.action.clone(),
            arguments: json!({ "message": decision.message }),
        };
        Ok(AgentOutput {
            final_text: note,
            tools_called: vec![decision.action.clone()],
            turns: vec![Turn {
                index: 0,
                output_text: decision.note.clone(),
                tool_calls: vec![tool_call],
                tool_results: vec![],
                stop_reason: None,
                duration: std::time::Duration::default(),
            }],
            ..Default::default()
        })
    }

    fn available_tools(&self, _config: &AgentConfig) -> Vec<String> {
        vec!["wait".to_string(), "send_message".to_string(), "mark_done".to_string(), "mark_error".to_string()]
    }

    fn name(&self) -> &str {
        "heartbeat-brain"
    }
}

#[tokio::test]
async fn brain_response_parsing_is_safe() {
    let tests = vec![
        // Valid, well-formed responses pass through as-is.
        test("valid-wait", r#"{"action": "wait", "note": "looks fine, waiting"}"#)
            .name("valid wait")
            .expect_tools(&["wait"])
            .expect_text_contains("looks fine")
            .build(),
        test("valid-send-message", r#"{"action": "send_message", "message": "please add a test", "note": "steering"}"#)
            .name("valid send_message")
            .expect_tools(&["send_message"])
            .expect_tool_arg("send_message", "message", json!("please add a test"))
            .expect_text_contains("steering")
            .build(),
        test("valid-mark-done", r#"{"action": "mark_done", "note": "goal achieved"}"#)
            .name("valid mark_done")
            .expect_tools(&["mark_done"])
            .build(),
        // Not JSON at all — must fall back to wait, never crash or misfire.
        test("not-json", "I think we should wait and see what happens.")
            .name("plain prose, not JSON")
            .expect_tools(&["wait"])
            .expect_text_contains("unparseable")
            .build(),
        // Valid JSON but missing/empty action — must fall back to wait.
        test("empty-object", "{}")
            .name("empty JSON object")
            .expect_tools(&["wait"])
            .build(),
        // A hallucinated action outside the known set must be neutralized to
        // wait, not passed through — this is the gap the tests caught before
        // parse_brain_response validated against KNOWN_ACTIONS.
        test("unknown-action", r#"{"action": "delete_everything", "note": "oops"}"#)
            .name("hallucinated/unknown action")
            .expect_tools(&["wait"])
            .expect_text_contains("unknown action")
            .build(),
        // Models sometimes wrap JSON in a markdown fence despite being told
        // not to — must still parse correctly, not be rejected as garbage.
        test("markdown-fenced", "```json\n{\"action\": \"mark_done\", \"note\": \"done via fenced json\"}\n```")
            .name("markdown-fenced JSON")
            .expect_tools(&["mark_done"])
            .expect_text_contains("fenced json")
            .build(),
        // Empty string — must not panic, must fall back to wait.
        test("empty-string", "")
            .name("empty response")
            .expect_tools(&["wait"])
            .build(),
    ];

    let suite = spice_framework::suite("heartbeat brain response parsing", tests);
    let runner = Runner::new(RunnerConfig { console_output: false, ..Default::default() });
    let report = runner.run(suite, Arc::new(BrainAdapter)).await;

    assert_eq!(
        report.failed, 0,
        "{} of {} spice tests failed: {:#?}",
        report.failed,
        report.total,
        report.tests.iter().filter(|t| !t.passed).collect::<Vec<_>>()
    );
}
