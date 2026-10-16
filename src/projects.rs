//! Project schema-v3 records. Automation retains its existing internal view; the wire
//! record deliberately separates manual delivery status from runtime state.
use crate::models::Project;
use serde::{Deserialize, Serialize};

pub fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    #[default]
    Pending,
    InProgress,
    Review,
    Merged,
    FlagsEnabled,
    FlagsCleaned,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    BootstrapOnly,
    NativeGoal,
    LegacyConductor,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectTracking {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub execution_mode: ExecutionMode,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub delivery_status: DeliveryStatus,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub original_jira_tickets: Vec<ProjectLink>,
    #[serde(default)]
    pub ec_tickets: Vec<ProjectLink>,
    #[serde(default)]
    pub vape_pages: Vec<ProjectLink>,
    #[serde(default)]
    pub launchdarkly_flags: Vec<FlagReference>,
    // Ordering is one atomic project write, rather than partial per-note rewrites.
    #[serde(default)]
    pub note_order: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectLink {
    pub id: String,
    pub url: String,
    pub key: Option<String>,
    pub label: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagReference {
    pub id: String,
    pub key: String,
    pub project_key: String,
    pub environment_key: String,
    pub fully_enabled_value: bool,
    pub label: Option<String>,
    pub notes: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserNote {
    pub id: String,
    pub title: Option<String>,
    pub content: String,
    pub position: usize,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub id: String,
    pub project_id: String,
    pub vape_instance_id: Option<String>,
    pub label: String,
    pub role: String,
    pub constellation: String,
    pub model: Option<String>,
    pub goal_override: Option<String>,
    pub heartbeat_prompt_override: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub provision_error: Option<String>,
    /// Once a request might have reached VAPE, retry only after reconciliation.
    #[serde(default)]
    pub provision_attempted: bool,
}
impl Relationship {
    pub fn pending(
        project: &Project,
        label: String,
        constellation: String,
        model: Option<String>,
    ) -> Self {
        let now = timestamp();
        Self {
            id: id(),
            project_id: project.id.clone(),
            vape_instance_id: None,
            label,
            role: "worker".into(),
            constellation,
            model,
            goal_override: None,
            heartbeat_prompt_override: None,
            created_by: "mazz-flux-bot".into(),
            created_at: now.clone(),
            updated_at: now,
            provision_error: None,
            provision_attempted: false,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Relationships {
    pub project_id: String,
    pub primary_instance_relationship_id: Option<String>,
    pub instances: Vec<Relationship>,
}
impl Relationships {
    pub fn primary(&self) -> Option<&Relationship> {
        self.primary_instance_relationship_id
            .as_ref()
            .and_then(|id| self.instances.iter().find(|r| &r.id == id))
    }
}
/// Blank overrides fall back to project direction; heartbeat guidance alone is
/// not an automation goal.
pub fn effective_goal<'a>(project: &'a Project, primary: Option<&'a Relationship>) -> &'a str {
    primary
        .and_then(|r| r.goal_override.as_deref())
        .filter(|goal| !goal.trim().is_empty())
        .unwrap_or(&project.goal)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Automation {
    pub status: String,
    pub heartbeat_enabled: bool,
    pub heartbeat_interval_secs: u64,
    pub last_heartbeat_at: Option<String>,
    pub last_note: Option<String>,
}

/// Reads the legacy shape and writes only the canonical v3 shape. No singular
/// instance projection is persisted after the relationship migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub execution_mode: Option<ExecutionMode>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub heartbeat_prompt: Option<String>,
    pub constellation: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<Automation>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub original_jira_tickets: Vec<ProjectLink>,
    #[serde(default)]
    pub ec_tickets: Vec<ProjectLink>,
    #[serde(default)]
    pub vape_pages: Vec<ProjectLink>,
    #[serde(default)]
    pub launchdarkly_flags: Vec<FlagReference>,
    #[serde(default)]
    pub note_order: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vape_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at: Option<String>,
}
impl TryFrom<ProjectRecord> for Project {
    type Error = anyhow::Error;
    fn try_from(r: ProjectRecord) -> anyhow::Result<Self> {
        if r.schema_version > 3 {
            anyhow::bail!("unsupported project schema");
        }
        let delivery_status = if r.schema_version >= 2 {
            serde_json::from_value(serde_json::json!(r.status))?
        } else {
            DeliveryStatus::Pending
        };
        let automation = r.automation.unwrap_or(Automation {
            status: r.status,
            heartbeat_enabled: r.heartbeat_enabled.unwrap_or(false),
            heartbeat_interval_secs: r.heartbeat_interval_secs.unwrap_or(900),
            last_note: r.last_note,
            last_heartbeat_at: r.last_heartbeat_at,
        });
        serde_json::from_value::<crate::models::ProjectStatus>(serde_json::json!(
            automation.status
        ))?;
        Ok(Self {
            id: r.id,
            name: r.name,
            goal: r.goal,
            heartbeat_prompt: r.heartbeat_prompt,
            constellation: r.constellation,
            status: automation.status,
            vape_instance_id: r.vape_instance_id,
            heartbeat_enabled: automation.heartbeat_enabled,
            heartbeat_interval_secs: automation.heartbeat_interval_secs,
            last_note: automation.last_note,
            last_heartbeat_at: automation.last_heartbeat_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
            tracking: ProjectTracking {
                schema_version: r.schema_version,
                revision: r.revision,
                execution_mode: if r.schema_version < 3 {
                    ExecutionMode::LegacyConductor
                } else {
                    r.execution_mode.ok_or_else(|| {
                        failure("validation_error", "schema 3 requires execution_mode")
                    })?
                },
                description: r.description,
                delivery_status,
                archived_at: r.archived_at,
                original_jira_tickets: r.original_jira_tickets,
                ec_tickets: r.ec_tickets,
                vape_pages: r.vape_pages,
                launchdarkly_flags: r.launchdarkly_flags,
                note_order: r.note_order,
            },
        })
    }
}
impl From<Project> for ProjectRecord {
    fn from(p: Project) -> Self {
        let t = p.tracking;
        Self {
            schema_version: 3,
            revision: t.revision,
            id: p.id,
            name: p.name,
            goal: p.goal,
            execution_mode: Some(t.execution_mode),
            description: t.description,
            heartbeat_prompt: p.heartbeat_prompt,
            constellation: p.constellation,
            status: serde_json::to_value(t.delivery_status)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            automation: Some(Automation {
                status: p.status,
                heartbeat_enabled: p.heartbeat_enabled,
                heartbeat_interval_secs: p.heartbeat_interval_secs,
                last_heartbeat_at: p.last_heartbeat_at,
                last_note: p.last_note,
            }),
            archived_at: t.archived_at,
            original_jira_tickets: t.original_jira_tickets,
            ec_tickets: t.ec_tickets,
            vape_pages: t.vape_pages,
            launchdarkly_flags: t.launchdarkly_flags,
            note_order: t.note_order,
            created_at: p.created_at,
            updated_at: p.updated_at,
            vape_instance_id: None,
            heartbeat_enabled: None,
            heartbeat_interval_secs: None,
            last_note: None,
            last_heartbeat_at: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DomainError {
    pub code: &'static str,
    pub message: String,
}
pub fn failure(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    DomainError {
        code,
        message: message.into(),
    }
    .into()
}
pub fn validate_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(failure("validation_error", "invalid resource id"));
    }
    Ok(())
}
pub fn required(value: &str, field: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        return Err(failure("validation_error", format!("{field} is required")));
    }
    Ok(())
}
pub fn normalize_url(value: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(value).map_err(|_| failure("validation_error", "invalid URL"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(failure(
            "validation_error",
            "URL must be HTTPS without credentials",
        ));
    }
    Ok(url.to_string())
}
