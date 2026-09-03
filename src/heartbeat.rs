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
use crate::models::{CreateInstanceRequest, JobConfig, KanbanStatus, PidaStatus, Project, ProjectStatus};
use crate::vape_client::VapeClient;
use crate::AppState;

/// Default per-project heartbeat cadence (see `Project::heartbeat_interval_secs`) —
/// 15 minutes, editable per project via `PATCH /api/projects/{id}/heartbeat-interval`.
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 15 * 60;

/// How often the outer loop wakes up to check which projects are due —
/// independent of any individual project's own interval. Short on purpose
/// (project intervals can be much shorter than 15 minutes), overridable via
/// `HEARTBEAT_SCAN_INTERVAL_SECS` for tests/tuning.
pub const DEFAULT_SCAN_INTERVAL_SECS: u64 = 15;

/// Tracks the outer scan loop's own cadence (NOT any individual project's
/// heartbeat interval — see `Project::heartbeat_interval_secs` for that). UI
/// consumers should compute each project's own next-due time from its
/// `last_heartbeat_at` + `heartbeat_interval_secs`; this clock is just proof
/// the scan loop is alive and roughly how fresh its own view is.
pub struct HeartbeatClock {
    pub interval_secs: u64,
    last_tick_at: std::sync::RwLock<chrono::DateTime<chrono::Utc>>,
}

impl HeartbeatClock {
    pub fn new(interval_secs: u64) -> Self {
        Self { interval_secs, last_tick_at: std::sync::RwLock::new(chrono::Utc::now()) }
    }

    fn mark(&self) {
        *self.last_tick_at.write().unwrap() = chrono::Utc::now();
    }

    pub fn status(&self) -> serde_json::Value {
        let last = *self.last_tick_at.read().unwrap();
        let next = last + chrono::Duration::seconds(self.interval_secs as i64);
        serde_json::json!({
            "scan_interval_secs": self.interval_secs,
            "last_scan_at": last.to_rfc3339(),
            "next_scan_at": next.to_rfc3339(),
        })
    }
}

/// True if `project` is due for a heartbeat tick right now: never ticked, or
/// its own `heartbeat_interval_secs` has elapsed since `last_heartbeat_at`.
/// An unparseable timestamp is treated as "due" (fail open — better to tick
/// an extra time than to silently stop ticking a project forever).
fn is_due(project: &Project) -> bool {
    let Some(last) = &project.last_heartbeat_at else { return true };
    let Ok(last) = chrono::DateTime::parse_from_rfc3339(last) else { return true };
    let elapsed = chrono::Utc::now().signed_duration_since(last.with_timezone(&chrono::Utc));
    elapsed.num_seconds() >= project.heartbeat_interval_secs as i64
}

/// The conductor's structured answer for one tick. We ask Anthropic to respond
/// with exactly this JSON shape; if it doesn't (or the key isn't configured),
/// we fall back to `wait` so a bad/missing conductor never takes a destructive
/// action by accident.
/// Board mutations are a side channel on a conductor decision, so a single
/// heartbeat can move a task to In Progress and send the assignment message
/// to pida atomically from the orchestrator's point of view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum KanbanAction {
    CreateTask {
        title: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        status: Option<KanbanStatus>,
    },
    UpdateTask {
        task_id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        status: Option<KanbanStatus>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Decision {
    #[serde(default)]
    pub action: String,
    /// For `send_message`: the steering text. For `create_human_task` when
    /// there's exactly one blocker: the description of what a human needs
    /// to do. If the conductor has multiple distinct blockers, it should use
    /// `tasks` instead (see below) rather than cramming them into one
    /// monolithic message.
    #[serde(default)]
    pub message: Option<String>,
    /// For `create_human_task` with more than one discrete blocker — e.g. a
    /// pida instance's reply lists three separate numbered asks. Each
    /// element becomes its own `HumanTask` row, so a person can resolve them
    /// independently instead of one giant paragraph. If both `tasks` and
    /// `message` are present, `tasks` wins; if `tasks` is empty/absent,
    /// `message` is used as a single task (see `ActNode`).
    #[serde(default)]
    pub tasks: Option<Vec<String>>,
    #[serde(default)]
    pub note: Option<String>,
    /// Optional markdown note to persist to this project's notes regardless
    /// of `action` — a passive record-keeping side channel, not mutually
    /// exclusive with any decision (the conductor can e.g. `wait` and still
    /// jot down what it found). See `ProjectNote`.
    #[serde(default)]
    pub add_note: Option<String>,
    /// The conductor's fully rewritten, self-contained compacted summary for
    /// this tick — REPLACES whatever was in `memory/{project_id}.md` before
    /// (see `Store::write_memory`), independent of `action`. Distinct from
    /// `add_note`: notes are an append-only historical log worth permanently
    /// keeping; memory is "worth remembering right now" and is expected to
    /// be overwritten every tick. `None`/absent leaves the existing memory
    /// file untouched (e.g. a conductor that doesn't support this field yet).
    #[serde(default)]
    pub memory: Option<String>,
    /// Zero or more mutations to apply alongside the primary action. This is
    /// intentionally not the primary `action`: assigning work requires both
    /// an `update_task` mutation and a composed `send_message` in one tick.
    #[serde(default)]
    pub kanban_actions: Vec<KanbanAction>,
}

impl Decision {
    fn wait(note: impl Into<String>) -> Self {
        Decision {
            action: "wait".to_string(),
            message: None,
            tasks: None,
            note: Some(note.into()),
            add_note: None,
            memory: None,
            kanban_actions: Vec::new(),
        }
    }
}

/// Pure parse of the conductor's raw text response into a [`Decision`]. Any
/// failure to parse — empty response, markdown-fenced JSON, an action we
/// don't recognize as non-empty — falls back to `wait`. This is the
/// safety-critical path (an unparseable conductor response must never be treated
/// as license to act) and it's exactly what the `spice_framework` behavioral
/// tests in `tests/heartbeat_conductor.rs` exercise directly, with no live LLM
/// call needed.
const KNOWN_ACTIONS: [&str; 5] = [
    "wait",
    "send_message",
    "mark_done",
    "mark_error",
    "create_human_task",
];

pub fn parse_conductor_response(raw: &str) -> Decision {
    match serde_json::from_str::<Decision>(strip_markdown_fence(raw.trim())) {
        Ok(d) if KNOWN_ACTIONS.contains(&d.action.as_str()) => d,
        Ok(d) => Decision::wait(format!(
            "conductor returned unknown action '{}': {raw}",
            d.action
        )),
        Err(_) => Decision::wait(format!("conductor returned unparseable response: {raw}")),
    }
}

/// Strips a leading/trailing ``` or ```json fence. The system prompt asks the
/// model not to use one, but models don't always comply — this is cheap
/// insurance against an otherwise-valid JSON response being rejected for
/// nothing more than whitespace and three backticks.
fn strip_markdown_fence(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    let rest = rest.trim_start_matches(['\n', '\r']);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

const SYSTEM_PROMPT: &str = "You are an autonomous orchestrator managing a single 'pida' coding-agent \
    instance on behalf of a developer, working toward one stated goal. Each tick you see the \
    project's overall `goal` (always keep this in mind), an optional `heartbeat_prompt` (guidance \
    for what THIS check-in specifically should focus on, if set), your own `memory` from the \
    previous tick (a compacted summary you wrote — empty on the first tick), the project's full \
    `kanban` board (stable task IDs, descriptions, and statuses), and the instance's current \
    status and most recent chat turns. Respond with ONLY a JSON object (no markdown \
    fences, no prose) of the shape: \
    {\"action\": \"wait\" | \"send_message\" | \"mark_done\" | \"mark_error\" | \"create_human_task\", \
    \"message\": \"<only for send_message, or create_human_task with exactly ONE blocker — for \
    send_message, COMPOSE this yourself in your own words using heartbeat_prompt (if set) and \
    memory as guidance, while keeping goal in mind — never just restate heartbeat_prompt or goal \
    verbatim>\", \
    \"tasks\": [\"<blocker 1>\", \"<blocker 2>\", ...] (only for create_human_task — use this instead of \
    message when the agent's reply lists two or more DISTINCT blockers, e.g. a numbered list of \
    separate asks. Each entry becomes its own independently-resolvable task — never merge several \
    unrelated blockers into one message string), \
    \"note\": \"<short human-readable reason>\", \
    \"add_note\": \"<optional markdown, any action — your own permanent, append-only running notes>\", \
    \"memory\": \"<a fully rewritten, self-contained compacted summary of everything worth \
    remembering about this project's progress, decisions, and state — include this on every \
    response. This REPLACES your previous memory entirely, so carry forward anything still \
    relevant rather than assuming it persists on its own. Keep it concise — this is what lets \
    you avoid re-reading full history every tick.>\", \
    \"kanban_actions\": [{\"action\":\"create_task\",\"title\":\"<title>\",\"description\":\"<actionable details>\",\"status\":\"assigned\"} \
    OR {\"action\":\"update_task\",\"task_id\":\"<stable id from kanban>\",\"title\":\"<optional>\",\"description\":\"<optional>\",\"status\":\"assigned\"|\"in_progress\"|\"done\"}]}. \
    Use send_message to steer the agent or answer a pending question in prose. Use mark_done \
    only when the transcript clearly shows the goal is achieved. Use mark_error only when the \
    agent is stuck in a way a message can't fix. Use create_human_task when you hit a blocker \
    only a person can resolve — missing credentials/access, an ambiguous decision outside your \
    authority, something requiring approval — this pauses the project until a person resolves \
    it, so don't use it for things you could instead ask the agent about via send_message. \
    IMPORTANT: if the agent's reply enumerates multiple separate blockers (numbered or bulleted), \
    split them into distinct entries in `tasks` rather than one combined `message` — this lets a \
    human resolve each one independently. Use add_note on any tick (including wait) to record \
    findings, context, or progress worth keeping permanently — it does not change what action \
    is taken. \
    You are also given `archetypes` — a catalog of reusable agent personas, each with a name, \
    description, and preferred model. When `goal`, `heartbeat_prompt`, or `memory` implies \
    spinning up a sub-agent for a specific kind of work (e.g. \"spin up a sub_agent to validate \
    the implementation\", \"resolve this nitpick with a sub_agent\"), pick the archetype whose \
    description best matches that kind of work and, in your `send_message` text, tell the pida \
    instance explicitly which archetype to use — name it and summarize its description/preferred \
    model so pida has enough to act on without needing to look it up itself. If no archetype \
    fits well, proceed without recommending one rather than forcing a bad match. \
    Use `kanban_actions` as a side channel alongside any primary action. When assigning an \
    Assigned task, emit `action: send_message` with a composed, actionable message naming the \
    task and best-fitting archetype, AND emit an `update_task` for that exact task ID with \
    `status: in_progress` in the same response. On a later heartbeat, only move it to `done` \
    when recent pida progress clearly shows that task is complete. Never invent a task ID; use \
    the stable ID in `kanban`. You may create new board work with `create_task`. An empty \
    `kanban_actions` array means no board change. Prefer wait when unsure.";

#[derive(Debug, Clone)]
pub(crate) enum TickOutcome {
    Waited,
    Sent(String),
    Done,
    Error,
    /// Carries one or more human task descriptions — see `ActNode`. Almost
    /// always length 1, but can be more when the conductor identifies
    /// multiple discrete blockers in one tick.
    Blocked(Vec<String>),
}

/// Per-tick graph state. One of these is built fresh from a `Project` DB row
/// at the start of every tick and thrown away at the end — durability lives
/// in sqlite (see `persist_tick`), not here.
#[derive(Clone)]
pub(crate) struct HeartbeatState {
    project_id: String,
    project_name: String,
    goal: String,
    heartbeat_prompt: Option<String>,
    constellation: String,
    vape_instance_id: Option<String>,
    harness: Option<String>,
    pida_status: Option<PidaStatus>,
    recent_messages: Vec<serde_json::Value>,
    decision: Option<Decision>,
    outcome: Option<TickOutcome>,
    note: Option<String>,
    add_note: Option<String>,
    /// The conductor's rewritten memory for THIS tick (from `Decision.memory`),
    /// persisted by `persist_tick` regardless of which action was taken —
    /// same treatment as `add_note`. Not the same field as `memory` above
    /// (that's last tick's, read-only input; this is this tick's, write-only
    /// output) — kept separate rather than overwritten in place so
    /// `persist_tick` can tell "no update this tick" (`None`) apart from
    /// "explicitly cleared" if that's ever needed.
    new_memory: Option<String>,
    kanban_actions: Vec<KanbanAction>,
}

pub(crate) enum Update {
    Noop,
    InstanceCreated(String),
    StatusFetched {
        harness: String,
        pida_status: Option<PidaStatus>,
        messages: Vec<serde_json::Value>,
    },
    Decided(Decision),
    ActionTaken(
        TickOutcome,
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<KanbanAction>,
    ),
    Noted(String),
}

impl Reducer for HeartbeatState {
    type Update = Update;
    fn apply(&mut self, update: Update) {
        match update {
            Update::Noop => {}
            Update::InstanceCreated(id) => self.vape_instance_id = Some(id),
            Update::StatusFetched {
                harness,
                pida_status,
                messages,
            } => {
                self.harness = Some(harness);
                self.pida_status = pida_status;
                self.recent_messages = messages;
            }
            Update::Decided(d) => self.decision = Some(d),
            Update::ActionTaken(outcome, note, add_note, new_memory, kanban_actions) => {
                self.outcome = Some(outcome);
                self.note = note;
                self.add_note = add_note;
                self.new_memory = new_memory;
                self.kanban_actions = kanban_actions;
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
    conductor: Arc<Conductor>,
    store: Arc<crate::store::Store>,
}

/// Turns a project name into a vape/k8s-safe instance-name slug: lowercase,
/// anything outside `[a-z0-9-]` becomes `-`, repeated `-` collapse to one,
/// leading/trailing `-` trimmed, clamped to 24 chars (leaving room for the
/// id-suffix appended by the caller). Empty result (e.g. an all-emoji
/// project name) signals the caller to fall back to the old id-only scheme.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let clamped = &trimmed[..24.min(trimmed.len())];
    clamped.trim_matches('-').to_string()
}

/// Human-readable instance name: `{slug}-{first-6-of-project-id}`, falling
/// back to the old `mfb-{first-8-of-project-id}` scheme if the project name
/// slugifies to nothing (all-symbols/emoji/empty).
pub fn instance_name(project_name: &str, project_id: &str) -> String {
    let slug = slugify(project_name);
    if slug.is_empty() {
        let short = &project_id[..8.min(project_id.len())];
        return format!("mfb-{short}");
    }
    let short = &project_id[..6.min(project_id.len())];
    format!("{slug}-{short}")
}

/// Best-effort LLM-suggested instance-name slug, run through the same
/// safety net as the deterministic path (`slugify`) so raw model output is
/// never trusted as a k8s resource name unsanitized. Any failure (disabled
/// conductor, network error, empty/unusable response) returns `None` —
/// callers must fall back to the deterministic scheme, never block instance
/// creation on this.
async fn try_llm_slug(conductor: &Conductor, project_name: &str, goal: &str) -> Option<String> {
    if !conductor.enabled() {
        return None;
    }
    let raw = conductor.suggest_instance_slug(project_name, goal).await.ok()?;
    let cleaned = slugify(raw.trim());
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Best-effort: asks the conductor to compose the initial session prompt in
/// its own words (see `Conductor::compose_initial_prompt`). Falls back to
/// sending `goal` verbatim on any failure/empty response — must never block
/// instance creation. Returns `(prompt, source)` for logging.
async fn compose_initial_prompt(
    conductor: &Conductor,
    project_name: &str,
    goal: &str,
    archetypes_json: Option<&str>,
    kanban_json: Option<&str>,
) -> (String, &'static str) {
    if conductor.enabled() {
        if let Ok(text) = conductor
            .compose_initial_prompt(project_name, goal, archetypes_json, kanban_json)
            .await
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return (trimmed.to_string(), "llm_composed");
            }
        }
    }
    (goal.to_string(), "verbatim_goal_fallback")
}

#[async_trait::async_trait]
impl Node<HeartbeatState> for CreateInstanceNode {
    async fn run(&self, state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        let (name, naming_source) =
            match try_llm_slug(&self.conductor, &state.project_name, &state.goal).await {
                Some(slug) => (
                    format!(
                        "{slug}-{}",
                        &state.project_id[..6.min(state.project_id.len())]
                    ),
                    "llm_slug",
                ),
                None => (
                    instance_name(&state.project_name, &state.project_id),
                    "deterministic_slug",
                ),
            };

        let model = crate::conductor::resolve_model(
            &self.store,
            "instance_model",
            "MAZZ_FLUX_INSTANCE_MODEL",
            crate::conductor::DEFAULT_MODEL,
        )
        .await;
        let archetypes = self.store.list_archetypes().await.unwrap_or_default();
        let archetypes_json = serde_json::to_string(&archetypes).ok();
        let kanban = self
            .store
            .get_kanban_board(&state.project_id)
            .await
            .unwrap_or_else(|_| crate::models::KanbanBoard {
                project_id: state.project_id.clone(),
                tasks: Vec::new(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            });
        let kanban_json = serde_json::to_string(&kanban).ok();
        let (prompt, prompt_source) = compose_initial_prompt(
            &self.conductor,
            &state.project_name,
            &state.goal,
            archetypes_json.as_deref(),
            kanban_json.as_deref(),
        )
        .await;

        let req = CreateInstanceRequest {
            name,
            constellation: state.constellation.clone(),
            subdomain: None,
            ticket: None,
            job: Some(JobConfig {
                prompt,
                harness: Some("pida".to_string()),
                model: Some(model),
            }),
            labels: std::collections::HashMap::from([("worker".to_string(), "true".to_string())]),
        };

        let resp = self
            .vape
            .create_instance(&req)
            .await
            .map_err(node_err("create_instance"))?;

        let _ = self
            .store
            .log_action(
                Some(&state.project_id),
                None,
                "instance_name_chosen",
                Some(&serde_json::json!({"name": req.name, "source": naming_source})),
                None,
                None,
            )
            .await;
        let _ = self
            .store
            .log_action(
                Some(&state.project_id),
                None,
                "instance_prompt_composed",
                Some(&serde_json::json!({"source": prompt_source})),
                None,
                None,
            )
            .await;

        if resp.get("dry_run").is_some() {
            return Ok(NodeOutcome::Update(Update::Noted(
                "dry-run: would create instance (set MAZZ_FLUX_LIVE=0 to force dry-run)"
                    .to_string(),
            )));
        }

        let id = resp
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GraphError::Node {
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
    store: Arc<crate::store::Store>,
}

#[async_trait::async_trait]
impl Node<HeartbeatState> for DecideNode {
    async fn run(&self, state: &HeartbeatState) -> Result<NodeOutcome<Update>> {
        if !self.conductor.enabled() {
            return Ok(NodeOutcome::Update(Update::Decided(Decision::wait(
                "no conductor configured (set OPENROUTER_API_KEY) — heartbeat is only observing, not acting",
            ))));
        }

        // Read fresh every tick (not cached) so editing agent_prompts/validation.md
        // through the Files tab takes effect on the very next heartbeat, no
        // restart needed — same pattern as conductor key resolution used to be.
        let system = match self.store.read_agent_prompt("validation").await {
            Ok(Some(validation)) if !validation.trim().is_empty() => format!(
                "{SYSTEM_PROMPT}\n\nBefore choosing mark_done, you MUST also confirm the following validation criteria are satisfied:\n\n{validation}"
            ),
            _ => SYSTEM_PROMPT.to_string(),
        };

        let memory = self
            .store
            .read_memory(&state.project_id)
            .await
            .unwrap_or(None);
        let archetypes = self.store.list_archetypes().await.unwrap_or_default();
        let kanban = self.store.get_kanban_board(&state.project_id).await.unwrap_or_else(|error| {
            warn!(project_id = %state.project_id, %error, "failed to read kanban board for conductor");
            crate::models::KanbanBoard {
                project_id: state.project_id.clone(),
                tasks: Vec::new(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            }
        });

        let user = serde_json::json!({
            "goal": state.goal,
            "heartbeat_prompt": state.heartbeat_prompt,
            "memory": memory,
            "archetypes": archetypes,
            "kanban": kanban,
            "pida_status": state.pida_status,
            "recent_messages": state.recent_messages,
        })
        .to_string();

        let decision = match self.conductor.decide(&system, &user).await {
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
        let decision = state
            .decision
            .clone()
            .unwrap_or_else(|| Decision::wait("act reached with no decision"));

        let note = decision.note.clone();
        let add_note = decision.add_note.clone();
        let new_memory = decision.memory.clone();
        let kanban_actions = decision.kanban_actions.clone();

        match decision.action.as_str() {
            "send_message" => {
                let message = decision.message.clone().unwrap_or_default();
                if message.is_empty() {
                    warn!(project_id = %state.project_id, "conductor said send_message with no message body — treating as wait");
                    return Ok(NodeOutcome::Update(Update::ActionTaken(
                        TickOutcome::Waited,
                        note,
                        add_note,
                        new_memory.clone(),
                        kanban_actions,
                    )));
                }
                let instance_id =
                    state
                        .vape_instance_id
                        .as_deref()
                        .ok_or_else(|| GraphError::Node {
                            node: "act".to_string(),
                            message: "send_message with no instance id".to_string(),
                        })?;
                self.vape
                    .pida_send(instance_id, &message)
                    .await
                    .map_err(node_err("act"))?;
                Ok(NodeOutcome::Update(Update::ActionTaken(
                    TickOutcome::Sent(message),
                    note,
                    add_note,
                    new_memory.clone(),
                    kanban_actions,
                )))
            }
            "mark_done" => Ok(NodeOutcome::Update(Update::ActionTaken(
                TickOutcome::Done,
                note,
                add_note,
                new_memory.clone(),
                kanban_actions,
            ))),
            "mark_error" => Ok(NodeOutcome::Update(Update::ActionTaken(
                TickOutcome::Error,
                note,
                add_note,
                new_memory.clone(),
                kanban_actions,
            ))),
            "create_human_task" => {
                // Prefer `tasks` (one or more discrete blockers) over the
                // legacy single-`message` shape — lets the conductor split a
                // multi-item reply (e.g. a numbered list from the pida
                // instance) into separate, independently-resolvable
                // HumanTask rows instead of one monolithic paragraph.
                let descriptions: Vec<String> = match &decision.tasks {
                    Some(tasks) if !tasks.is_empty() => tasks
                        .iter()
                        .filter(|t| !t.trim().is_empty())
                        .cloned()
                        .collect(),
                    _ => vec![decision.message.clone().unwrap_or_else(|| {
                        note.clone()
                            .unwrap_or_else(|| "conductor requested human intervention".to_string())
                    })],
                };
                Ok(NodeOutcome::Update(Update::ActionTaken(
                    TickOutcome::Blocked(descriptions),
                    note,
                    add_note,
                    new_memory.clone(),
                    kanban_actions,
                )))
            }
            _ => Ok(NodeOutcome::Update(Update::ActionTaken(
                TickOutcome::Waited,
                note,
                add_note,
                new_memory.clone(),
                kanban_actions,
            ))),
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

fn build_graph(vape: Arc<VapeClient>, conductor: Arc<Conductor>, store: Arc<crate::store::Store>) -> CompiledGraph<HeartbeatState> {
    Graph::<HeartbeatState>::new()
        .add_node("route", RouteNode)
        .add_conditional("route", |s: &HeartbeatState| {
            if s.vape_instance_id.is_none() { "create_instance".to_string() } else { "fetch_status".to_string() }
        })
        .add_node("create_instance", CreateInstanceNode { vape: vape.clone(), conductor: conductor.clone(), store: store.clone() })
        .add_edge("create_instance", END)
        .add_node("fetch_status", FetchStatusNode { vape: vape.clone(), conductor: conductor.clone() })
        .add_conditional("fetch_status", |s: &HeartbeatState| {
            if s.harness.as_deref() == Some("pida") { "decide".to_string() } else { END.to_string() }
        })
        .add_node("decide", DecideNode { conductor, store })
        .add_edge("decide", "act")
        .add_node("act", ActNode { vape })
        .add_edge("act", END)
        .set_entry("route")
        .compile()
        .expect("heartbeat graph is statically valid")
}

// ---- Outer polling loop ----------------------------------------------------

pub async fn run(state: AppState) {
    let interval_secs = state.heartbeat_clock.interval_secs;
    info!(interval_secs, "heartbeat scan loop starting (each project ticks on its own interval, default {}s)", DEFAULT_HEARTBEAT_INTERVAL_SECS);

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        ticker.tick().await;
        state.heartbeat_clock.mark();

        let conductor = Arc::new(Conductor::from_sources(&state.store).await);
        let graph = build_graph(state.vape.clone(), conductor, state.store.clone());
        let executor = Executor::new(graph).max_steps(10).with_step_guard(loop_guard());

        if let Err(e) = tick(&state, &executor).await {
            error!(error = %e, "heartbeat tick failed");
        }
    }
}

/// Forces one heartbeat tick for a single project right now, bypassing both
/// the `heartbeat_enabled`/`status == running` filter AND the per-project
/// `is_due` interval check that the periodic scan loop applies — this is an
/// explicit "do it now" request (e.g. the project detail page's "Force
/// heartbeat" button, useful when an instance was stuck `Instance not
/// ready` and the project's own interval hasn't elapsed yet). Does NOT
/// reset the scan loop's own `HeartbeatClock` — that's about the scan
/// loop's cadence, not any individual project's last-processed time. It DOES
/// update the project's own `last_heartbeat_at` via the normal
/// `persist_tick` path, which pushes this project's next-due time forward
/// by its interval, same as an automatic tick would.
pub async fn force_tick(state: &AppState, project_id: &str) -> anyhow::Result<()> {
    let project = state.store.get_project(project_id).await?.ok_or_else(|| anyhow::anyhow!("project {project_id} not found"))?;

    let conductor = Arc::new(Conductor::from_sources(&state.store).await);
    let graph = build_graph(state.vape.clone(), conductor, state.store.clone());
    let executor = Executor::new(graph).max_steps(10).with_step_guard(loop_guard());

    process_project(state, &executor, &project).await
}

async fn tick(state: &AppState, executor: &Executor<HeartbeatState>) -> anyhow::Result<()> {
    let projects = state.store.list_running_projects().await?;
    for project in projects {
        if !is_due(&project) {
            continue;
        }
        if let Err(e) = process_project(state, executor, &project).await {
            error!(project_id = %project.id, error = %e, "failed to process project this tick");
            let _ = state.store.log_action(Some(&project.id), project.vape_instance_id.as_deref(), "heartbeat_error", None, None, Some(&e.to_string())).await;
        }
    }
    Ok(())
}

async fn process_project(
    state: &AppState,
    executor: &Executor<HeartbeatState>,
    project: &Project,
) -> anyhow::Result<()> {
    let initial = HeartbeatState {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        goal: project.goal.clone(),
        heartbeat_prompt: project.heartbeat_prompt.clone(),
        constellation: project.constellation.clone(),
        vape_instance_id: project.vape_instance_id.clone(),
        harness: None,
        pida_status: None,
        recent_messages: vec![],
        decision: None,
        outcome: None,
        note: None,
        add_note: None,
        new_memory: None,
        kanban_actions: Vec::new(),
    };

    match executor.run(initial, &project.id).await? {
        RunOutcome::Completed(final_state) => {
            persist_tick(state, project, &final_state, None).await
        }
        RunOutcome::Interrupted {
            state: final_state,
            reason,
            ..
        } => persist_tick(state, project, &final_state, Some(reason)).await,
        RunOutcome::Failed {
            state: final_state,
            node,
            error,
        } => {
            warn!(project_id = %project.id, %node, %error, "heartbeat graph node failed");
            state
                .store
                .log_action(
                    Some(&project.id),
                    final_state.vape_instance_id.as_deref(),
                    "heartbeat_node_failed",
                    None,
                    None,
                    Some(&format!("{node}: {error}")),
                )
                .await?;
            state
                .store
                .touch_heartbeat(&project.id, Some(&format!("node '{node}' failed: {error}")))
                .await?;
            Ok(())
        }
    }
}

/// Applies conductor-requested board changes in order. Unknown task IDs are
/// logged and skipped rather than failing the heartbeat or rewriting the board.
/// Other storage errors still fail the tick so they are visible and retried.
pub async fn apply_kanban_actions(
    store: &crate::store::Store,
    project_id: &str,
    instance_id: Option<&str>,
    actions: &[KanbanAction],
) -> anyhow::Result<()> {
    for action in actions {
        match action {
            KanbanAction::CreateTask {
                title,
                description,
                status,
            } => {
                if title.trim().is_empty() {
                    store
                        .log_action(
                            Some(project_id),
                            instance_id,
                            "kanban_action_rejected",
                            Some(&serde_json::json!({"action": "create_task"})),
                            None,
                            Some("task title is required"),
                        )
                        .await?;
                    continue;
                }
                let task = store
                    .create_kanban_task(
                        project_id,
                        title,
                        description,
                        status.unwrap_or(KanbanStatus::Assigned),
                    )
                    .await?;
                store
                    .log_action(
                        Some(project_id),
                        instance_id,
                        "kanban_task_created_by_conductor",
                        Some(&serde_json::json!({"task_id": task.id, "status": task.status})),
                        None,
                        None,
                    )
                    .await?;
            }
            KanbanAction::UpdateTask {
                task_id,
                title,
                description,
                status,
            } => {
                let updated = store
                    .update_kanban_task(
                        project_id,
                        task_id,
                        title.as_deref(),
                        description.as_deref(),
                        *status,
                    )
                    .await?;
                match updated {
                    Some(task) => {
                        store
                            .log_action(
                                Some(project_id),
                                instance_id,
                                "kanban_task_updated_by_conductor",
                                Some(
                                    &serde_json::json!({"task_id": task.id, "status": task.status}),
                                ),
                                None,
                                None,
                            )
                            .await?;
                    }
                    None => {
                        store
                            .log_action(
                                Some(project_id),
                                instance_id,
                                "kanban_action_rejected",
                                Some(&serde_json::json!({"action": "update_task", "task_id": task_id})),
                                None,
                                Some("kanban task not found"),
                            )
                            .await?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Translates one graph run's final state into the durable file store.
async fn persist_tick(
    state: &AppState,
    project: &Project,
    final_state: &HeartbeatState,
    interrupt_reason: Option<String>,
) -> anyhow::Result<()> {
    if project.vape_instance_id.is_none() {
        if let Some(id) = &final_state.vape_instance_id {
            state.store.set_project_instance(&project.id, id).await?;
            state
                .store
                .log_action(
                    Some(&project.id),
                    Some(id),
                    "create_instance",
                    None,
                    Some(id),
                    None,
                )
                .await?;
            state
                .store
                .touch_heartbeat(&project.id, Some("instance created"))
                .await?;
        } else {
            let note = final_state
                .note
                .clone()
                .unwrap_or_else(|| "create_instance did not return an id".to_string());
            state
                .store
                .log_action(
                    Some(&project.id),
                    None,
                    "create_instance_dry_run",
                    None,
                    Some(&note),
                    None,
                )
                .await?;
            state
                .store
                .touch_heartbeat(&project.id, Some(&note))
                .await?;
        }
        return Ok(());
    }

    if let Some(reason) = interrupt_reason {
        state
            .store
            .log_action(
                Some(&project.id),
                project.vape_instance_id.as_deref(),
                "heartbeat_interrupted",
                None,
                Some(&reason),
                None,
            )
            .await?;
        state
            .store
            .touch_heartbeat(&project.id, Some(&reason))
            .await?;
        return Ok(());
    }

    if let Some(h) = &final_state.harness {
        if h != "pida" {
            warn!(project_id = %project.id, harness = %h, "instance is not running the pida harness — mazz-flux-bot only drives pida instances for now");
            state
                .store
                .touch_heartbeat(
                    &project.id,
                    Some(&format!("skipped: instance harness is '{h}', not 'pida'")),
                )
                .await?;
            return Ok(());
        }
    }

    let instance_id = project.vape_instance_id.as_deref();

    apply_kanban_actions(
        &state.store,
        &project.id,
        instance_id,
        &final_state.kanban_actions,
    )
    .await?;

    // Persisted regardless of which action was taken — see Decision::add_note.
    if let Some(note_md) = &final_state.add_note {
        if !note_md.is_empty() {
            state.store.add_project_note(&project.id, note_md).await?;
            state
                .store
                .log_action(
                    Some(&project.id),
                    instance_id,
                    "add_note",
                    None,
                    Some(&format!("{} chars", note_md.len())),
                    None,
                )
                .await?;
        }
    }

    // Also persisted regardless of which action was taken — see
    // Decision::memory. Unlike add_note this REPLACES the file, it never
    // appends (that's the whole point of the compaction mechanism).
    if let Some(memory_md) = &final_state.new_memory {
        if !memory_md.is_empty() {
            state.store.write_memory(&project.id, memory_md).await?;
            state
                .store
                .log_action(
                    Some(&project.id),
                    instance_id,
                    "memory_updated",
                    None,
                    Some(&format!("{} chars", memory_md.len())),
                    None,
                )
                .await?;
        }
    }

    match &final_state.outcome {
        Some(TickOutcome::Sent(msg)) => {
            state
                .store
                .log_action(
                    Some(&project.id),
                    instance_id,
                    "pida_send",
                    Some(&serde_json::json!({"message": msg})),
                    final_state.note.as_deref(),
                    None,
                )
                .await?;
            state
                .store
                .touch_heartbeat(&project.id, Some(&format!("sent: {msg}")))
                .await?;
        }
        Some(TickOutcome::Done) => {
            state
                .store
                .set_project_status(
                    &project.id,
                    ProjectStatus::Done,
                    final_state.note.as_deref(),
                )
                .await?;
            state
                .store
                .set_heartbeat_enabled(&project.id, false)
                .await?;
            state
                .store
                .log_action(
                    Some(&project.id),
                    instance_id,
                    "mark_done",
                    None,
                    final_state.note.as_deref(),
                    None,
                )
                .await?;
        }
        Some(TickOutcome::Error) => {
            state
                .store
                .set_project_status(
                    &project.id,
                    ProjectStatus::Error,
                    final_state.note.as_deref(),
                )
                .await?;
            state
                .store
                .set_heartbeat_enabled(&project.id, false)
                .await?;
            state
                .store
                .log_action(
                    Some(&project.id),
                    instance_id,
                    "mark_error",
                    None,
                    final_state.note.as_deref(),
                    None,
                )
                .await?;
        }
        Some(TickOutcome::Blocked(descriptions)) => {
            for description in descriptions {
                let task = state
                    .store
                    .create_human_task(&project.id, description)
                    .await?;
                state
                    .store
                    .log_action(
                        Some(&project.id),
                        instance_id,
                        "create_human_task",
                        Some(&serde_json::json!({"task_id": task.id, "description": description})),
                        final_state.note.as_deref(),
                        None,
                    )
                    .await?;
            }
            state
                .store
                .set_project_status(
                    &project.id,
                    ProjectStatus::Blocked,
                    final_state.note.as_deref(),
                )
                .await?;
            state
                .store
                .set_heartbeat_enabled(&project.id, false)
                .await?;
        }
        _ => {
            state
                .store
                .touch_heartbeat(&project.id, final_state.note.as_deref())
                .await?;
        }
    }
    Ok(())
}
