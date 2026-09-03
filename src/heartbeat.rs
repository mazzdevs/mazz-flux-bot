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
//! conductor (`ANTHROPIC_API_KEY` or `OPENROUTER_API_KEY`, see `conductor.rs`) is
//! configured to safely answer it — see `FetchStatusNode`. The graph itself
//! is acyclic; a `StepGuard` is still wired in as defence-in-depth against a
//! future edit accidentally introducing a cycle (see `loop_guard`).
//!
//! DB writes (the durable record of what happened) live in `persist_tick`,
//! not in the nodes — nodes only touch the vape client and conductor, and
//! compute state transitions, which keeps the graph itself testable with
//! `spice_framework` without a database in the loop (see `tests/`).

use std::sync::Arc;
use std::time::Duration;

use metalcraft::*;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::conductor::Conductor;
use crate::models::{CreateInstanceRequest, JobConfig, PidaStatus, Project, ProjectStatus};
use crate::vape_client::VapeClient;
use crate::AppState;

const DEFAULT_INTERVAL_SECS: u64 = 60;

/// The conductor's structured answer for one tick. We ask Anthropic to respond
/// with exactly this JSON shape; if it doesn't (or the key isn't configured),
/// we fall back to `wait` so a bad/missing conductor never takes a destructive
/// action by accident.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Decision {
    #[serde(default)]
    pub action: String,
    /// For `send_message`: the steering text. For `create_human_task`: the
    /// description of what a human needs to do.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Optional markdown note to persist to this project's notes regardless
    /// of `action` — a passive record-keeping side channel, not mutually
    /// exclusive with any decision (the conductor can e.g. `wait` and still
    /// jot down what it found). See `ProjectNote`.
    #[serde(default)]
    pub add_note: Option<String>,
}

impl Decision {
    fn wait(note: impl Into<String>) -> Self {
        Decision { action: "wait".to_string(), message: None, note: Some(note.into()), add_note: None }
    }
}

/// Pure parse of the conductor's raw text response into a [`Decision`]. Any
/// failure to parse — empty response, markdown-fenced JSON, an action we
/// don't recognize as non-empty — falls back to `wait`. This is the
/// safety-critical path (an unparseable conductor response must never be treated
/// as license to act) and it's exactly what the `spice_framework` behavioral
/// tests in `tests/heartbeat_conductor.rs` exercise directly, with no live LLM
/// call needed.
const KNOWN_ACTIONS: [&str; 5] = ["wait", "send_message", "mark_done", "mark_error", "create_human_task"];

pub fn parse_conductor_response(raw: &str) -> Decision {
    match serde_json::from_str::<Decision>(strip_markdown_fence(raw.trim())) {
        Ok(d) if KNOWN_ACTIONS.contains(&d.action.as_str()) => d,
        Ok(d) => Decision::wait(format!("conductor returned unknown action '{}': {raw}", d.action)),
        Err(_) => Decision::wait(format!("conductor returned unparseable response: {raw}")),
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
    {\"action\": \"wait\" | \"send_message\" | \"mark_done\" | \"mark_error\" | \"create_human_task\", \
    \"message\": \"<only for send_message (steering text) or create_human_task (what the human needs to do)>\", \
    \"note\": \"<short human-readable reason>\", \
    \"add_note\": \"<optional markdown, any action — your own running notes on this project>\"}. \
    Use send_message to steer the agent or answer a pending question in prose. Use mark_done \
    only when the transcript clearly shows the goal is achieved. Use mark_error only when the \
    agent is stuck in a way a message can't fix. Use create_human_task when you hit a blocker \
    only a person can resolve — missing credentials/access, an ambiguous decision outside your \
    authority, something requiring approval — this pauses the project until a person resolves \
    it, so don't use it for things you could instead ask the agent about via send_message. Use \
    add_note on any tick (including wait) to record findings, context, or progress worth \
    keeping — it does not change what action is taken. Prefer wait when unsure.";

#[derive(Debug, Clone)]
pub(crate) enum TickOutcome {
    Waited,
    Sent(String),
    Done,
    Error,
    /// Carries the human task description — see `ActNode`.
    Blocked(String),
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
    add_note: Option<String>,
}

pub(crate) enum Update {
    Noop,
    InstanceCreated(String),
    StatusFetched { harness: String, pida_status: Option<PidaStatus>, messages: Vec<serde_json::Value> },
    Decided(Decision),
    ActionTaken(TickOutcome, Option<String>, Option<String>),
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
            Update::ActionTaken(outcome, note, add_note) => {
                self.outcome = Some(outcome);
                self.note = note;
                self.add_note = add_note;
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
/// `decide` when there's a pending question and no conductor configured to
/// safely answer it.
struct FetchStatusNode {
    vape: Arc<VapeClient>,
    conductor: Arc<Conductor>,
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

        if pending && !self.conductor.enabled() {
            return Ok(NodeOutcome::interrupt_with(
                update,
                "pending question on the instance, but no conductor is configured (set ANTHROPIC_API_KEY or OPENROUTER_API_KEY) — nothing will auto-answer it. Answer manually via the UI or set a key.",
            ));
        }
        Ok(NodeOutcome::Update(update))
    }
}

struct DecideNode {
    conductor: Arc<Conductor>,
}

#[async_trait::async_trait]
impl Node<HeartbeatState> for DecideNode {
    async fn run(&self, state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        if !self.conductor.enabled() {
            return Ok(NodeOutcome::Update(Update::Decided(Decision::wait(
                "no conductor configured (set ANTHROPIC_API_KEY or OPENROUTER_API_KEY) — heartbeat is only observing, not acting",
            ))));
        }

        let user = serde_json::json!({
            "goal": state.goal,
            "pida_status": state.pida_status,
            "recent_messages": state.recent_messages,
        })
        .to_string();

        let decision = match self.conductor.decide(SYSTEM_PROMPT, &user).await {
            Ok(text) => parse_conductor_response(&text),
            Err(e) => {
                warn!(project_id = %state.project_id, error = %e, "conductor call failed — waiting");
                Decision::wait(format!("conductor call failed: {e}"))
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

        let note = decision.note.clone();
        let add_note = decision.add_note.clone();

        match decision.action.as_str() {
            "send_message" => {
                let message = decision.message.clone().unwrap_or_default();
                if message.is_empty() {
                    warn!(project_id = %state.project_id, "conductor said send_message with no message body — treating as wait");
                    return Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Waited, note, add_note)));
                }
                let instance_id = state.vape_instance_id.as_deref().ok_or_else(|| GraphError::Node {
                    node: "act".to_string(),
                    message: "send_message with no instance id".to_string(),
                })?;
                self.vape.pida_send(instance_id, &message).await.map_err(node_err("act"))?;
                Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Sent(message), note, add_note)))
            }
            "mark_done" => Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Done, note, add_note))),
            "mark_error" => Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Error, note, add_note))),
            "create_human_task" => {
                let description = decision.message.clone().unwrap_or_else(|| note.clone().unwrap_or_else(|| "conductor requested human intervention".to_string()));
                Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Blocked(description), note, add_note)))
            }
            _ => Ok(NodeOutcome::Update(Update::ActionTaken(TickOutcome::Waited, note, add_note))),
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

fn build_graph(vape: Arc<VapeClient>, conductor: Arc<Conductor>) -> CompiledGraph<HeartbeatState> {
    Graph::<HeartbeatState>::new()
        .add_node("route", RouteNode)
        .add_conditional("route", |s: &HeartbeatState| {
            if s.vape_instance_id.is_none() { "create_instance".to_string() } else { "fetch_status".to_string() }
        })
        .add_node("create_instance", CreateInstanceNode { vape: vape.clone() })
        .add_edge("create_instance", END)
        .add_node("fetch_status", FetchStatusNode { vape: vape.clone(), conductor: conductor.clone() })
        .add_conditional("fetch_status", |s: &HeartbeatState| {
            if s.harness.as_deref() == Some("pida") { "decide".to_string() } else { END.to_string() }
        })
        .add_node("decide", DecideNode { conductor })
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

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        ticker.tick().await;

        // Resolved fresh every tick (not cached) so a key saved through the
        // settings UI mid-run takes effect on the very next tick.
        let conductor = Arc::new(Conductor::from_sources(&state.store).await);
        let graph = build_graph(state.vape.clone(), conductor);
        let executor = Executor::new(graph).max_steps(10).with_step_guard(loop_guard());

        if let Err(e) = tick(&state, &executor).await {
            error!(error = %e, "heartbeat tick failed");
        }
    }
}

async fn tick(state: &AppState, executor: &Executor<HeartbeatState>) -> anyhow::Result<()> {
    let projects = state.store.list_running_projects().await?;
    for project in projects {
        if let Err(e) = process_project(state, executor, &project).await {
            error!(project_id = %project.id, error = %e, "failed to process project this tick");
            let _ = state.store.log_action(Some(&project.id), project.vape_instance_id.as_deref(), "heartbeat_error", None, None, Some(&e.to_string())).await;
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
        add_note: None,
    };

    match executor.run(initial, &project.id).await? {
        RunOutcome::Completed(final_state) => persist_tick(state, project, &final_state, None).await,
        RunOutcome::Interrupted { state: final_state, reason, .. } => persist_tick(state, project, &final_state, Some(reason)).await,
        RunOutcome::Failed { state: final_state, node, error } => {
            warn!(project_id = %project.id, %node, %error, "heartbeat graph node failed");
            state.store.log_action(Some(&project.id), final_state.vape_instance_id.as_deref(), "heartbeat_node_failed", None, None, Some(&format!("{node}: {error}"))).await?;
            state.store.touch_heartbeat(&project.id, Some(&format!("node '{node}' failed: {error}"))).await?;
            Ok(())
        }
    }
}

/// Translates one graph run's final state into the durable sqlite record —
/// the only place heartbeat.rs talks to the database.
async fn persist_tick(state: &AppState, project: &Project, final_state: &HeartbeatState, interrupt_reason: Option<String>) -> anyhow::Result<()> {
    if project.vape_instance_id.is_none() {
        if let Some(id) = &final_state.vape_instance_id {
            state.store.set_project_instance(&project.id, id).await?;
            state.store.log_action(Some(&project.id), Some(id), "create_instance", None, Some(id), None).await?;
            state.store.touch_heartbeat(&project.id, Some("instance created")).await?;
        } else {
            let note = final_state.note.clone().unwrap_or_else(|| "create_instance did not return an id".to_string());
            state.store.log_action(Some(&project.id), None, "create_instance_dry_run", None, Some(&note), None).await?;
            state.store.touch_heartbeat(&project.id, Some(&note)).await?;
        }
        return Ok(());
    }

    if let Some(reason) = interrupt_reason {
        state.store.log_action(Some(&project.id), project.vape_instance_id.as_deref(), "heartbeat_interrupted", None, Some(&reason), None).await?;
        state.store.touch_heartbeat(&project.id, Some(&reason)).await?;
        return Ok(());
    }

    if let Some(h) = &final_state.harness {
        if h != "pida" {
            warn!(project_id = %project.id, harness = %h, "instance is not running the pida harness — mazz-flux-bot only drives pida instances for now");
            state.store.touch_heartbeat(&project.id, Some(&format!("skipped: instance harness is '{h}', not 'pida'"))).await?;
            return Ok(());
        }
    }

    let instance_id = project.vape_instance_id.as_deref();

    // Persisted regardless of which action was taken — see Decision::add_note.
    if let Some(note_md) = &final_state.add_note {
        if !note_md.is_empty() {
            state.store.add_project_note(&project.id, note_md).await?;
            state.store.log_action(Some(&project.id), instance_id, "add_note", None, Some(&format!("{} chars", note_md.len())), None).await?;
        }
    }

    match &final_state.outcome {
        Some(TickOutcome::Sent(msg)) => {
            state.store.log_action(Some(&project.id), instance_id, "pida_send", Some(&serde_json::json!({"message": msg})), final_state.note.as_deref(), None).await?;
            state.store.touch_heartbeat(&project.id, Some(&format!("sent: {msg}"))).await?;
        }
        Some(TickOutcome::Done) => {
            state.store.set_project_status(&project.id, ProjectStatus::Done, final_state.note.as_deref()).await?;
            state.store.set_heartbeat_enabled(&project.id, false).await?;
            state.store.log_action(Some(&project.id), instance_id, "mark_done", None, final_state.note.as_deref(), None).await?;
        }
        Some(TickOutcome::Error) => {
            state.store.set_project_status(&project.id, ProjectStatus::Error, final_state.note.as_deref()).await?;
            state.store.set_heartbeat_enabled(&project.id, false).await?;
            state.store.log_action(Some(&project.id), instance_id, "mark_error", None, final_state.note.as_deref(), None).await?;
        }
        Some(TickOutcome::Blocked(description)) => {
            let task = state.store.create_human_task(&project.id, description).await?;
            state.store.set_project_status(&project.id, ProjectStatus::Blocked, final_state.note.as_deref()).await?;
            state.store.set_heartbeat_enabled(&project.id, false).await?;
            state.store.log_action(Some(&project.id), instance_id, "create_human_task", Some(&serde_json::json!({"task_id": task.id, "description": description})), final_state.note.as_deref(), None).await?;
        }
        _ => {
            state.store.touch_heartbeat(&project.id, final_state.note.as_deref()).await?;
        }
    }
    Ok(())
}
