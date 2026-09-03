use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Lifecycle of a mazz-flux-bot Project. Stored as TEXT in sqlite (see db.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// Created but heartbeat not yet enabled — no instance exists yet.
    Draft,
    /// Heartbeat is actively working the goal.
    Running,
    /// Heartbeat enabled previously, temporarily paused by the user.
    Paused,
    /// The conductor decided the goal is achieved.
    Done,
    /// The conductor (or vape) hit an unrecoverable error working this project.
    Error,
}

impl ProjectStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectStatus::Draft => "draft",
            ProjectStatus::Running => "running",
            ProjectStatus::Paused => "paused",
            ProjectStatus::Done => "done",
            ProjectStatus::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "running" => ProjectStatus::Running,
            "paused" => ProjectStatus::Paused,
            "done" => ProjectStatus::Done,
            "error" => ProjectStatus::Error,
            _ => ProjectStatus::Draft,
        }
    }
}

/// A user-declared goal that the heartbeat loop tries to drive to completion by
/// managing (usually) a single vape instance running the `pida` harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub constellation: String,
    pub status: String,
    pub vape_instance_id: Option<String>,
    pub heartbeat_enabled: bool,
    pub last_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_heartbeat_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub goal: String,
    #[serde(default)]
    pub constellation: Option<String>,
}

/// One row in the action_log table — every mutating call the tool made (or, in
/// dry-run mode, would have made) plus the conductor's reasoning ticks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLogEntry {
    pub id: i64,
    pub project_id: Option<String>,
    pub instance_id: Option<String>,
    pub action: String,
    pub detail: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
}

// ---- Vape API response shapes -------------------------------------------------
//
// Fields we don't strictly need to introspect are left out (unknown fields are
// ignored by serde by default) or captured as serde_json::Value so schema drift
// on vape's side doesn't break deserialization of the fields we *do* rely on.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VapeInstance {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub constellation: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub pod_ip: Option<String>,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

/// `GET /api/v1/instances/{id}/agent-status` — tells you which harness (bilda vs
/// pida) is actually live on this instance. Always check this before building a
/// harness-scoped proxy path; don't assume "pida" even though that's our default
/// for instances *we* create.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    pub state: String,
    #[serde(default)]
    pub active_harness: Option<String>,
    #[serde(default)]
    pub harnesses: HashMap<String, serde_json::Value>,
}

/// `GET /api/v1/instances/{id}/pida/api/status` — subset of fields we act on.
/// Verified live 2026-09-03. Extra fields from the real response are dropped
/// silently (serde ignores unknown fields by default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidaStatus {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub is_streaming: bool,
    #[serde(default)]
    pub pending_ask: Option<serde_json::Value>,
    #[serde(default)]
    pub plan_mode: bool,
    #[serde(default)]
    pub handoff: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub turn_liveness: Option<serde_json::Value>,
}

/// `GET /api/v1/instances/{id}/pida/api/session` — verified live. Kept generic
/// since message shapes vary (regular turn vs `compactionSummary` vs tool use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidaSession {
    #[serde(default)]
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub todos: Vec<serde_json::Value>,
}

/// Body for `POST /api/v1/instances` (create). Field names/paths for `name` and
/// `constellation` come from the vape source (`internal/handlers/api.go`,
/// `CreateInstanceRequest`) — high confidence. `harness` and the nested job
/// fields do NOT exist in that same local source file (it's stale relative to
/// what's actually deployed — confirmed separately: the literal string
/// `"harness"` exists in the live cadmium binary, but nothing in the checked-out
/// Go source references it at all). Placement here (top-level on the job block)
/// is a best-effort guess — confirm by firing a real create call before trusting
/// it in anger.
#[derive(Debug, Clone, Serialize)]
pub struct CreateInstanceRequest {
    pub name: String,
    pub constellation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdomain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<JobConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobConfig {
    pub prompt: String,
    /// Best-effort field per the note on CreateInstanceRequest above. Default
    /// "pida" per project decision to standardize on the pida harness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateInstanceResponse {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constellation {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}
