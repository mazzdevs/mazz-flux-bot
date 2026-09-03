//! The heartbeat orchestrator, built as a metalcraft graph.
//!
//! One tick, one project, one graph run:
//!
//! ```text
//!  route --(no instance)--> create_instance --> END
//!    \--(has instance)--> fetch_status --(harness != pida)--> END
//!                              \--(pida)--> decide --> act --> END
//! ```
//!
//! `fetch_status` interrupts (metalcraft human-in-the-loop) instead of
//! continuing to `decide` when the instance has a pending question and no
//! `ANTHROPIC_API_KEY` is configured to safely answer it — see
//! `FetchStatusNode`. The graph itself is acyclic; a `StepGuard` is still
//! wired in as defence-in-depth against a future edit accidentally
//! introducing a cycle (see `loop_guard`).
//!
//! DB writes (the durable record of what happened) live in `persist_tick`,
//! not in the nodes — nodes only touch the vape/anthropic clients and
//! compute state transitions, which keeps the graph itself testable with
//! `spice_framework` without a database in the loop (see `tests/`).

use std::sync::Arc;
use std::time::Duration;

use metalcraft::*;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::anthropic_client::AnthropicClient;
use crate::db;
use crate::models::{CreateInstanceRequest, JobConfig, PidaStatus, Project, ProjectStatus};
use crate::vape_client::VapeClient;
use crate::AppState;

const DEFAULT_INTERVAL_SECS: u64 = 60;

/// The brain's structured answer for one tick. We ask Anthropic to respond
/// with exactly this JSON shape; if it doesn't (or the key isn't configured),
/// we fall back to `wait` so a bad/missing brain never takes a destructive
/// action by accident.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Decision {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Decision {
    fn wait(note: impl Into<String>) -> Self {
        Decision { action: "wait".to_string(), message: None, note: Some(note.into()) }
    }
}

/// Pure parse of the brain's raw text response into a [`Decision`]. Any
/// failure to parse — empty response, markdown-fenced JSON, an action we
/// don't recognize as non-empty — falls back to `wait`. This is the
/// safety-critical path (an unparseable brain response must never be treated
/// as license to act) and it's exactly what the `spice_framework` behavioral
/// tests in `tests/heartbeat_brain.rs` exercise directly, with no live LLM
/// call needed.
const KNOWN_ACTIONS: [&str; 4] = ["wait", "send_message", "mark_done", "mark_error"];

pub fn parse_brain_response(raw: &str) -> Decision {
    match serde_json::from_str::<Decision>(strip_markdown_fence(raw.trim())) {
        Ok(d) if KNOWN_ACTIONS.contains(&d.action.as_str()) => d,
        Ok(d) => Decision::wait(format!("brain returned unknown action '{}': {raw}", d.action)),
        Err(_) => Decision::wait(format!("brain returned unparseable response: {raw}")),
    }
}

/// Strips a leading/trailing ``` or ```json fence. The system prompt asks the
/// model not to use one, but models don't always comply — this is cheap
/// insurance against an otherwise-valid JSON response being rejected for
/// nothing more than whitespace and three backticks.
fn strip_markdown_fence(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else { return s };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    let rest = rest.trim_start_matches(['\n', '\r']);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

const SYSTEM_PROMPT: &str = "You are an autonomous orchestrator managing a single 'pida' coding-agent \
    instance on behalf of a developer, working toward one stated goal. Each tick you see \
    the instance's current status and the most recent chat turns. Respond with ONLY a JSON \
    object (no markdown fences, no prose) of the shape: \
    {\"action\": \"wait\" | \"send_message\" | \"mark_done\" | \"mark_error\", \
    \"message\": \"<only for send_message>\", \"note\": \"<short human-readable reason>\"}. \
    Use send_message to steer the agent or answer a pending question in prose. Use mark_done \
    only when the transcript clearly shows the goal is achieved. Use mark_error only when the \
    agent is stuck in a way a message can't fix. Prefer wait when unsure.";

#[derive(Debug, Clone)]
pub(crate) enum TickOutcome {
    Waited,
    Sent(String),
    Done,
    Error,
}

/// Per-tick graph state. One of these is built fresh from a `Project` DB row
/// at the start of every tick and thrown away at the end — durability lives
/// in sqlite (see `persist_tick`), not here.
#[derive(Clone)]
pub(crate) struct HeartbeatState {
    project_id: String,
    goal: String,
    constellation: String,
    vape_instance_id: Option<String>,
    harness: Option<String>,
    pida_status: Option<PidaStatus>,
    recent_messages: Vec<serde_json::Value>,
    decision: Option<Decision>,
    outcome: Option<TickOutcome>,
    note: Option<String>,
}

pub(crate) enum Update {
    Noop,
    InstanceCreated(String),
    StatusFetched { harness: String, pida_status: Option<PidaStatus>, messages: Vec<serde_json::Value> },
    Decided(Decision),
    ActionTaken(TickOutcome, Option<String>),
    Noted(String),
}

impl Reducer for HeartbeatState {
    type Update = Update;
    fn apply(&mut self, update: Update) {
        match update {
            Update::Noop => {}
            Update::InstanceCreated(id) => self.vape_instance_id = Some(id),
            Update::StatusFetched { harness, pida_status, messages } => {
                self.harness = Some(harness);
                self.pida_status = pida_status;
                self.recent_messages = messages;
            }
            Update::Decided(d) => self.decision = Some(d),
            Update::ActionTaken(outcome, note) => {
                self.outcome = Some(outcome);
                self.note = note;
            }
            Update::Noted(note) => self.note = Some(note),
        }
    }
}

fn node_err(node: &'static str) -> impl Fn(anyhow::Error) -> GraphError {
    move |e| GraphError::Node { node: node.to_string(), message: e.to_string() }
}

// ---- Nodes ----------------------------------------------------------------

struct RouteNode;

#[async_trait::async_trait]
impl Node<HeartbeatState> for RouteNode {
    async fn run(&self, _state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        Ok(NodeOutcome::Update(Update::Noop))
    }
}

/// mazz-flux-bot standardizes on the `pida` harness for every instance it
/// creates (per project decision) — one instance per project.
struct CreateInstanceNode {
    vape: Arc<VapeClient>,
}

#[async_trait::async_trait]
impl Node<HeartbeatState> for CreateInstanceNode {
    async fn run(&self, state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        let short = &state.project_id[..8.min(state.project_id.len())];
        let req = CreateInstanceRequest {
            name: format!("mfb-{short}"),
            constellation: state.constellation.clone(),
            subdomain: None,
            ticket: None,
            job: Some(JobConfig { prompt: state.goal.clone(), harness: Some("pida".to_string()) }),
        };

        let resp = self.vape.create_instance(&req).await.map_err(node_err("create_instance"))?;

        if resp.get("dry_run").is_some() {
            return Ok(NodeOutcome::Update(Update::Noted(
                "dry-run: would create instance (set MAZZ_FLUX_LIVE=1 to actually create)".to_string(),
            )));
        }

        let id = resp.get("id").and_then(|v| v.as_str()).ok_or_else(|| GraphError::Node {
            node: "create_instance".to_string(),
            message: format!("create_instance response had no id: {resp}"),
        })?;
        Ok(NodeOutcome::Update(Update::InstanceCreated(id.to_string())))
    }
}

/// Fetches unified + pida-specific status and the recent chat transcript.
/// Interrupts (metalcraft human-in-the-loop) instead of proceeding to
/// `decide` when there's a pending question and no brain configured to
/// safely answer it.
struct FetchStatusNode {
    vape: Arc<VapeClient>,
    anthropic: Arc<AnthropicClient>,
}

#[async_trait::async_trait]
impl Node<HeartbeatState> for FetchStatusNode {
    async fn run(&self, state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        let instance_id = state.vape_instance_id.as_deref().ok_or_else(|| GraphError::Node {
            node: "fetch_status".to_string(),
            message: "fetch_status reached with no instance id".to_string(),
        })?;

        let agent_status = self.vape.agent_status(instance_id).await.map_err(node_err("fetch_status"))?;
        let harness = agent_status.active_harness.clone().unwrap_or_else(|| "pida".to_string());

        if harness != "pida" {
            return Ok(NodeOutcome::Update(Update::StatusFetched { harness, pida_status: None, messages: vec![] }));
        }

        let pida_status = self.vape.pida_status(instance_id).await.map_err(node_err("fetch_status"))?;
        let session = self.vape.pida_session(instance_id).await.map_err(node_err("fetch_status"))?;
        let recent_messages: Vec<serde_json::Value> = session.messages.into_iter().rev().take(6).collect();
        let pending = pida_status.pending_ask.is_some();

        let update = Update::StatusFetched { harness, pida_status: Some(pida_status), messages: recent_messages };

        if pending && !self.anthropic.enabled() {
            return Ok(NodeOutcome::interrupt_with(
                update,
                "pending question on the instance, but ANTHROPIC_API_KEY is not set — nothing will auto-answer it. Answer manually via the UI or set the key.",
            ));
        }
        Ok(NodeOutcome::Update(update))
    }
}

struct DecideNode {
    anthropic: Arc<AnthropicClient>,
}

#[async_trait::async_trait]
impl Node<HeartbeatState> for DecideNode {
    async fn run(&self, state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        if !self.anthropic.enabled() {
            return Ok(NodeOutcome::Update(Update::Decided(Decision::wait(
                "no ANTHROPIC_API_KEY configured — heartbeat is only observing, not acting",
            ))));
        }

        let user = serde_json::json!({
            "goal": state.goal,
            "pida_status": state.pida_status,
            "recent_messages": state.recent_messages,
        })
        .to_string();

        let decision = match self.anthropic.decide(SYSTEM_PROMPT, &user).await {
            Ok(text) => parse_brain_response(&text),
            Err(e) => {
                warn!(project_id = %state.project_id, error = %e, "anthropic call failed — waiting");
                Decision::wait(format!("anthropic call failed: {e}"))
            }
        };
        Ok(NodeOutcome::Update(Update::Decided(decision)))
    }
}

struct ActNode {
    vape: Arc<VapeClient>,
}

#[async_trait::async_trait]
impl Node<HeartbeatState> for ActNode {
    async fn run(&self, state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        let decision = state.decision.clone().unwrap_or_else(|| Decision::wait("act reached with no decision"));

        match decision.action.as_str() {
            "send_message" => {
                let message = decision.message.clone().unwrap_or_default();
                if message.is_empty() {
                    warn!(project_id = %state.project_id, "brain said send_message with no message body — treating as wait");
                    return Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Waited, decision.note.clone())));
                }
                let instance_id = state.vape_instance_id.as_deref().ok_or_else(|| GraphError::Node {
                    node: "act".to_string(),
                    message: "send_message with no instance id".to_string(),
                })?;
                self.vape.pida_send(instance_id, &message).await.map_err(node_err("act"))?;
                Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Sent(message), decision.note.clone())))
            }
            "mark_done" => Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Done, decision.note.clone()))),
            "mark_error" => Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Error, decision.note.clone()))),
            _ => Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Waited, decision.note.clone()))),
        }
    }
}

/// The graph is acyclic by construction (see module docs). This guard exists
/// purely as defence-in-depth: if a future edit ever wires an edge back onto
/// the node that just ran, fail the tick safely instead of spinning.
fn loop_guard() -> StepGuard<HeartbeatState> {
    Arc::new(|_state, event| {
        if event.node == event.next {
            GuardAction::Stop(format!("step guard: node '{}' would repeat itself", event.node))
        } else {
            GuardAction::Continue
        }
    })
}

fn build_graph(vape: Arc<VapeClient>, anthropic: Arc<AnthropicClient>) -> CompiledGraph<HeartbeatState> {
    Graph::<HeartbeatState>::new()
        .add_node("route", RouteNode)
        .add_conditional("route", |s: &HeartbeatState| {
            if s.vape_instance_id.is_none() { "create_instance".to_string() } else { "fetch_status".to_string() }
        })
        .add_node("create_instance", CreateInstanceNode { vape: vape.clone() })
        .add_edge("create_instance", END)
        .add_node("fetch_status", FetchStatusNode { vape: vape.clone(), anthropic: anthropic.clone() })
        .add_conditional("fetch_status", |s: &HeartbeatState| {
            if s.harness.as_deref() == Some("pida") { "decide".to_string() } else { END.to_string() }
        })
        .add_node("decide", DecideNode { anthropic })
        .add_edge("decide", "act")
        .add_node("act", ActNode { vape })
        .add_edge("act", END)
        .set_entry("route")
        .compile()
        .expect("heartbeat graph is statically valid")
}

// ---- Outer polling loop ----------------------------------------------------

pub async fn run(state: AppState) {
    let interval_secs: u64 = std::env::var("HEARTBEAT_INTERVAL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_INTERVAL_SECS);
    info!(interval_secs, "heartbeat loop starting");

    let graph = build_graph(state.vape.clone(), state.anthropic.clone());
    let executor = Executor::new(graph).max_steps(10).with_step_guard(loop_guard());

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        ticker.tick().await;
        if let Err(e) = tick(&state, &executor).await {
            error!(error = %e, "heartbeat tick failed");
        }
    }
}

async fn tick(state: &AppState, executor: &Executor<HeartbeatState>) -> anyhow::Result<()> {
    let projects = db::list_running_projects(&state.db).await?;
    for project in projects {
        if let Err(e) = process_project(state, executor, &project).await {
            error!(project_id = %project.id, error = %e, "failed to process project this tick");
            let _ = db::log_action(&state.db, Some(&project.id), project.vape_instance_id.as_deref(), "heartbeat_error", None, None, Some(&e.to_string())).await;
        }
    }
    Ok(())
}

async fn process_project(state: &AppState, executor: &Executor<HeartbeatState>, project: &Project) -> anyhow::Result<()> {
    let initial = HeartbeatState {
        project_id: project.id.clone(),
        goal: project.goal.clone(),
        constellation: project.constellation.clone(),
        vape_instance_id: project.vape_instance_id.clone(),
        harness: None,
        pida_status: None,
        recent_messages: vec![],
        decision: None,
        outcome: None,
        note: None,
    };

    match executor.run(initial, &project.id).await? {
        RunOutcome::Completed(final_state) => persist_tick(state, project, &final_state, None).await,
        RunOutcome::Interrupted { state: final_state, reason, .. } => persist_tick(state, project, &final_state, Some(reason)).await,
        RunOutcome::Failed { state: final_state, node, error } => {
            warn!(project_id = %project.id, %node, %error, "heartbeat graph node failed");
            db::log_action(&state.db, Some(&project.id), final_state.vape_instance_id.as_deref(), "heartbeat_node_failed", None, None, Some(&format!("{node}: {error}"))).await?;
            db::touch_heartbeat(&state.db, &project.id, Some(&format!("node '{node}' failed: {error}"))).await?;
            Ok(())
        }
    }
}

/// Translates one graph run's final state into the durable sqlite record —
/// the only place heartbeat.rs talks to the database.
async fn persist_tick(state: &AppState, project: &Project, final_state: &HeartbeatState, interrupt_reason: Option<String>) -> anyhow::Result<()> {
    if project.vape_instance_id.is_none() {
        if let Some(id) = &final_state.vape_instance_id {
            db::set_project_instance(&state.db, &project.id, id).await?;
            db::log_action(&state.db, Some(&project.id), Some(id), "create_instance", None, Some(id), None).await?;
            db::touch_heartbeat(&state.db, &project.id, Some("instance created")).await?;
        } else {
            let note = final_state.note.clone().unwrap_or_else(|| "create_instance did not return an id".to_string());
            db::log_action(&state.db, Some(&project.id), None, "create_instance_dry_run", None, Some(&note), None).await?;
            db::touch_heartbeat(&state.db, &project.id, Some(&note)).await?;
        }
        return Ok(());
    }

    if let Some(reason) = interrupt_reason {
        db::log_action(&state.db, Some(&project.id), project.vape_instance_id.as_deref(), "heartbeat_interrupted", None, Some(&reason), None).await?;
        db::touch_heartbeat(&state.db, &project.id, Some(&reason)).await?;
        return Ok(());
    }

    if let Some(h) = &final_state.harness {
        if h != "pida" {
            warn!(project_id = %project.id, harness = %h, "instance is not running the pida harness — mazz-flux-bot only drives pida instances for now");
            db::touch_heartbeat(&state.db, &project.id, Some(&format!("skipped: instance harness is '{h}', not 'pida'"))).await?;
            return Ok(());
        }
    }

    let instance_id = project.vape_instance_id.as_deref();
    match &final_state.outcome {
        Some(TickOutcome::Sent(msg)) => {
            db::log_action(&state.db, Some(&project.id), instance_id, "pida_send", Some(&serde_json::json!({"message": msg})), final_state.note.as_deref(), None).await?;
            db::touch_heartbeat(&state.db, &project.id, Some(&format!("sent: {msg}"))).await?;
        }
        Some(TickOutcome::Done) => {
            db::set_project_status(&state.db, &project.id, ProjectStatus::Done, final_state.note.as_deref()).await?;
            db::set_heartbeat_enabled(&state.db, &project.id, false).await?;
            db::log_action(&state.db, Some(&project.id), instance_id, "mark_done", None, final_state.note.as_deref(), None).await?;
        }
        Some(TickOutcome::Error) => {
            db::set_project_status(&state.db, &project.id, ProjectStatus::Error, final_state.note.as_deref()).await?;
            db::set_heartbeat_enabled(&state.db, &project.id, false).await?;
            db::log_action(&state.db, Some(&project.id), instance_id, "mark_error", None, final_state.note.as_deref(), None).await?;
        }
        _ => {
            db::touch_heartbeat(&state.db, &project.id, final_state.note.as_deref()).await?;
        }
    }
    Ok(())
}
