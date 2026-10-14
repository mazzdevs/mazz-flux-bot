//! Versioned project API. External mutations are serialized with project ticks;
//! delivery metadata is never inferred from automation or provider state.
use crate::{AppState, models::*, projects::*};
use axum::{
    Json, Router,
    extract::{FromRequest, Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = if let Some(e) = self.0.downcast_ref::<DomainError>() {
            let status = match e.code {
                "not_found" => StatusCode::NOT_FOUND,
                "validation_error" | "read_only_field" => StatusCode::UNPROCESSABLE_ENTITY,
                "provider_unavailable" => StatusCode::BAD_GATEWAY,
                _ => StatusCode::CONFLICT,
            };
            (status, e.code, e.message.clone())
        } else {
            tracing::error!(error=%self.0,"project API failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "project operation failed".into(),
            )
        };
        (
            status,
            Json(json!({"error":{"code":code,"message":message,"details":{}}})),
        )
            .into_response()
    }
}
pub struct Input<T>(pub T);
impl<S, T> FromRequest<S> for Input<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Response;
    async fn from_request(req: Request, state: &S) -> std::result::Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| Self(v))
            .map_err(|e| {
                Error(failure(
                    if e.body_text().contains("unknown field") {
                        "read_only_field"
                    } else {
                        "validation_error"
                    },
                    e.body_text(),
                ))
                .into_response()
            })
    }
}
#[derive(Debug, Default)]
pub enum Field<T> {
    #[default]
    Missing,
    Set(Option<T>),
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Field<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Self::Set(Option::deserialize(d)?))
    }
}
fn non_null<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
fn validate_interval(value: u64) -> anyhow::Result<()> {
    if !(5..=31_536_000).contains(&value) {
        return Err(failure(
            "validation_error",
            "interval must be 5..31536000 seconds",
        ));
    }
    Ok(())
}
fn apply<T>(field: Field<T>, value: &mut Option<T>) {
    if let Field::Set(v) = field {
        *value = v;
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Purge {
    expected_revision: u64,
    confirmation: String,
}
#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    archived: bool,
}
#[derive(Serialize)]
pub struct InstanceView {
    #[serde(flatten)]
    pub relationship: Relationship,
    pub is_primary: bool,
    pub live: LiveStatus,
}
#[derive(Serialize, Default)]
pub struct LiveStatus {
    pub status: Option<String>,
    pub ready: Option<bool>,
    pub status_source: String,
    pub fetched_at: Option<String>,
    pub error: Option<String>,
}
#[derive(Serialize)]
pub struct FlagView {
    #[serde(flatten)]
    pub flag: FlagReference,
    pub status: crate::launchdarkly::FlagStatus,
}
#[derive(Serialize)]
pub struct ProjectView {
    #[serde(flatten)]
    pub project: ProjectRecord,
    pub primary_instance_relationship_id: Option<String>,
    pub instances: Vec<InstanceView>,
    pub additional_notes: Vec<UserNote>,
    pub flag_statuses: Vec<FlagView>,
}
// Serialized flag entries contain status inline, not two competing collections.
async fn view(state: &AppState, p: Project, enrich: bool) -> Result<Value> {
    let all = state.store.relationships(&p.id).await?;
    let archived = p.tracking.archived_at.is_some();
    let (instances, fetched_at, error) = if enrich && !archived && !all.instances.is_empty() {
        match state.vape.list_instances_cached().await {
            Ok((i, fetched)) => (i, Some(fetched), None),
            Err(_) => (vec![], None, Some("VAPE status unavailable".to_string())),
        }
    } else {
        (vec![], None, None)
    };
    let instances =
        all.instances
            .into_iter()
            .map(|r| {
                let live = if archived {
                    LiveStatus {
                        status_source: "archived".into(),
                        ..Default::default()
                    }
                } else if r.vape_instance_id.is_none() {
                    LiveStatus {
                        status: Some("pending".into()),
                        status_source: "local".into(),
                        error: r.provision_error.clone(),
                        ..Default::default()
                    }
                } else if let Some(i) = instances
                    .iter()
                    .find(|i| Some(&i.id) == r.vape_instance_id.as_ref())
                {
                    LiveStatus {
                        status: Some(i.status.clone()),
                        ready: Some(i.ready),
                        status_source: "live".into(),
                        fetched_at: fetched_at.clone(),
                        error: None,
                    }
                } else {
                    LiveStatus {
                        status_source: "unavailable".into(),
                        error: Some(error.clone().unwrap_or_else(|| {
                            "upstream instance not found or not fetched".into()
                        })),
                        ..Default::default()
                    }
                };
                InstanceView {
                    is_primary: all.primary_instance_relationship_id.as_deref() == Some(&r.id),
                    relationship: r,
                    live,
                }
            })
            .collect();
    let mut flags = vec![];
    if enrich {
        let mut tasks = tokio::task::JoinSet::new();
        for (position, f) in p.tracking.launchdarkly_flags.iter().cloned().enumerate() {
            tasks.spawn(async move {
                let status = if archived {
                    crate::launchdarkly::FlagStatus::unavailable(
                        "archived project: status not fetched",
                    )
                } else {
                    crate::launchdarkly::status(&f).await
                };
                (position, FlagView { flag: f, status })
            });
        }
        let mut sorted = vec![];
        while let Some(row) = tasks.join_next().await {
            sorted.push(row.map_err(|_| failure("provider_unavailable", "flag task failed"))?);
        }
        sorted.sort_by_key(|(position, _)| *position);
        flags = sorted.into_iter().map(|(_, flag)| flag).collect();
    }
    let notes = state.store.user_notes(&p.id).await?;
    let dto = ProjectView {
        project: p.into(),
        primary_instance_relationship_id: all.primary_instance_relationship_id,
        instances,
        additional_notes: notes,
        flag_statuses: flags,
    };
    let mut value = serde_json::to_value(dto)?;
    if enrich {
        value["launchdarkly_flags"] = value["flag_statuses"].take();
    }
    value.as_object_mut().unwrap().remove("flag_statuses");
    value.as_object_mut().unwrap().remove("note_order");
    Ok(value)
}
async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Result<Json<Value>> {
    let mut projects = vec![];
    for p in state.store.list_projects().await? {
        if q.archived || p.tracking.archived_at.is_none() {
            projects.push(view(&state, p, true).await?);
        }
    }
    Ok(Json(json!({"projects":projects})))
}
async fn detail(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>> {
    let p = state.store.require_project(&id).await?;
    Ok(Json(json!({"project":view(&state,p,true).await?})))
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LinkInput {
    url: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    label: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FlagInput {
    key: String,
    #[serde(default = "default_project")]
    project_key: String,
    #[serde(default = "production")]
    environment_key: String,
    #[serde(default = "yes")]
    fully_enabled_value: bool,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}
fn default_project() -> String {
    "default".into()
}
fn production() -> String {
    "production".into()
}
fn yes() -> bool {
    true
}
fn make_link(v: LinkInput) -> anyhow::Result<ProjectLink> {
    Ok(ProjectLink {
        id: id(),
        url: normalize_url(&v.url)?,
        key: v.key,
        label: v.label,
    })
}
fn make_flag(v: FlagInput) -> anyhow::Result<FlagReference> {
    required(&v.key, "key")?;
    for key in [&v.key, &v.project_key, &v.environment_key] {
        if key.len() > 256
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            return Err(failure("validation_error", "invalid flag key or scope"));
        }
    }
    Ok(FlagReference {
        id: id(),
        key: v.key,
        project_key: v.project_key,
        environment_key: v.environment_key,
        fully_enabled_value: v.fully_enabled_value,
        label: v.label,
        notes: v.notes,
    })
}
fn unique_links(links: &[ProjectLink]) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for l in links {
        if !seen.insert(&l.url) {
            return Err(failure("validation_error", "duplicate link URL"));
        }
    }
    Ok(())
}
fn unique_flags(flags: &[FlagReference]) -> anyhow::Result<()> {
    if flags.len() > 32 {
        return Err(failure(
            "validation_error",
            "at most 32 tracked flags per project",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for f in flags {
        if !seen.insert((&f.key, &f.project_key, &f.environment_key)) {
            return Err(failure("validation_error", "duplicate flag reference"));
        }
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    #[serde(default)]
    name: Option<String>,
    goal: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    status: DeliveryStatus,
    #[serde(default)]
    heartbeat_prompt: Option<String>,
    #[serde(default)]
    constellation: Option<String>,
    #[serde(default)]
    heartbeat_interval_secs: Option<u64>,
    #[serde(default)]
    original_jira_tickets: Vec<LinkInput>,
    #[serde(default)]
    ec_tickets: Vec<LinkInput>,
    #[serde(default)]
    vape_pages: Vec<LinkInput>,
    #[serde(default)]
    launchdarkly_flags: Vec<FlagInput>,
}
async fn create(State(state): State<AppState>, Input(req): Input<Create>) -> Result<Json<Value>> {
    required(&req.goal, "goal")?;
    if let Some(n) = &req.name {
        required(n, "name")?;
        if n.chars().count() > 120 {
            return Err(failure("validation_error", "name must be at most 120 characters").into());
        }
    }
    if let Some(interval) = req.heartbeat_interval_secs {
        validate_interval(interval)?;
    }
    let jira = req
        .original_jira_tickets
        .into_iter()
        .map(make_link)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ec = req
        .ec_tickets
        .into_iter()
        .map(make_link)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let pages = req
        .vape_pages
        .into_iter()
        .map(make_link)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let flags = req
        .launchdarkly_flags
        .into_iter()
        .map(make_flag)
        .collect::<anyhow::Result<Vec<_>>>()?;
    for links in [&jira, &ec, &pages] {
        unique_links(links)?;
    }
    unique_flags(&flags)?;
    let naming = req.name.is_none();
    let goal = req.goal.clone();
    let p = state
        .store
        .create_project(CreateProjectRequest {
            name: req.name,
            goal: req.goal,
            heartbeat_prompt: req.heartbeat_prompt,
            constellation: req.constellation,
            heartbeat_interval_secs: req.heartbeat_interval_secs,
        })
        .await?;
    let p = state
        .store
        .mutate_project(&p.id, p.tracking.revision, |p| {
            p.tracking.description = req.description;
            p.tracking.delivery_status = req.status;
            p.tracking.original_jira_tickets = jira;
            p.tracking.ec_tickets = ec;
            p.tracking.vape_pages = pages;
            p.tracking.launchdarkly_flags = flags;
            Ok(())
        })
        .await?;
    audit(&state, &p.id, None, "project_created").await?;
    if naming {
        let store = state.store.clone();
        let pid = p.id.clone();
        let original = p.name.clone();
        tokio::spawn(async move {
            let c = crate::conductor::Conductor::from_sources(&store).await;
            if let Ok(Ok(name)) = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                c.suggest_project_name(&goal),
            )
            .await
            {
                if let Ok(_guard) = store.project_guard(&pid).await {
                    if let Ok(current) = store.require_project(&pid).await {
                        if current.name == original && !name.trim().is_empty() {
                            let _ = store.set_project_name(&pid, name.trim()).await;
                        }
                    }
                }
            }
        });
    }
    Ok(Json(
        json!({"revision":p.tracking.revision,"project":view(&state,p,false).await?}),
    ))
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Patch {
    expected_revision: u64,
    #[serde(default, deserialize_with = "non_null")]
    name: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    description: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    goal: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    status: Option<DeliveryStatus>,
    #[serde(default)]
    heartbeat_prompt: Field<String>,
    #[serde(default, deserialize_with = "non_null")]
    constellation: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    heartbeat_interval_secs: Option<u64>,
    #[serde(default, deserialize_with = "non_null")]
    original_jira_tickets: Option<Vec<LinkInput>>,
    #[serde(default, deserialize_with = "non_null")]
    ec_tickets: Option<Vec<LinkInput>>,
    #[serde(default, deserialize_with = "non_null")]
    vape_pages: Option<Vec<LinkInput>>,
    #[serde(default, deserialize_with = "non_null")]
    launchdarkly_flags: Option<Vec<FlagInput>>,
}
async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Input(req): Input<Patch>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&id).await?;
    let p = state
        .store
        .mutate_project(&id, req.expected_revision, |p| {
            if let Some(n) = req.name {
                required(&n, "name")?;
                if n.chars().count() > 120 {
                    return Err(failure(
                        "validation_error",
                        "name must be at most 120 characters",
                    ));
                }
                p.name = n.trim().into();
            }
            if let Some(d) = req.description {
                p.tracking.description = d;
            }
            if let Some(g) = req.goal {
                required(&g, "goal")?;
                p.goal = g;
            }
            if let Some(s) = req.status {
                p.tracking.delivery_status = s;
            }
            apply(req.heartbeat_prompt, &mut p.heartbeat_prompt);
            if let Some(c) = req.constellation {
                required(&c, "constellation")?;
                p.constellation = c;
            }
            if let Some(n) = req.heartbeat_interval_secs {
                validate_interval(n)?;
                p.heartbeat_interval_secs = n;
            }
            for (input, dest) in [
                (
                    req.original_jira_tickets,
                    &mut p.tracking.original_jira_tickets,
                ),
                (req.ec_tickets, &mut p.tracking.ec_tickets),
                (req.vape_pages, &mut p.tracking.vape_pages),
            ] {
                if let Some(links) = input {
                    let links = links
                        .into_iter()
                        .map(make_link)
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    unique_links(&links)?;
                    *dest = links;
                }
            }
            if let Some(flags) = req.launchdarkly_flags {
                let flags = flags
                    .into_iter()
                    .map(make_flag)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                unique_flags(&flags)?;
                p.tracking.launchdarkly_flags = flags;
            }
            Ok(())
        })
        .await?;
    audit(&state, &id, None, "project_updated").await?;
    Ok(Json(
        json!({"revision":p.tracking.revision,"project":view(&state,p,false).await?}),
    ))
}
async fn archive(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Input(r): Input<Revision>,
) -> Result<Json<Value>> {
    archive_as(&state, &id, r.expected_revision, true).await
}
async fn restore(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Input(r): Input<Revision>,
) -> Result<Json<Value>> {
    archive_as(&state, &id, r.expected_revision, false).await
}
async fn archive_as(state: &AppState, id: &str, rev: u64, archived: bool) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(id).await?;
    state.store.set_archived(id, rev, archived).await?;
    audit(
        state,
        id,
        None,
        if archived {
            "project_archived"
        } else {
            "project_restored"
        },
    )
    .await?;
    acknowledgement(state, id, json!({"ok":true})).await
}
async fn purge(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Input(r): Input<Purge>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&id).await?;
    let p = state.store.require_project(&id).await?;
    if p.tracking.revision != r.expected_revision {
        return Err(failure("stale_revision", "project changed").into());
    }
    if p.tracking.archived_at.is_none() || p.name != r.confirmation {
        return Err(failure(
            "confirmation_required",
            "archive first and confirm exact project name",
        )
        .into());
    }
    audit(&state, &id, None, "project_purged_instances_retained").await?;
    state
        .store
        .purge_project(&id, r.expected_revision, &r.confirmation)
        .await?;
    Ok(Json(json!({"ok":true})))
}
// Called while the project's mutation guard is held, so the UI can rebase
// sibling drafts to this acknowledged revision, not an unrelated later GET.
async fn acknowledgement(state: &AppState, pid: &str, mut value: Value) -> Result<Json<Value>> {
    value["revision"] = json!(state.store.require_project(pid).await?.tracking.revision);
    Ok(Json(value))
}
async fn audit(state: &AppState, pid: &str, iid: Option<&str>, action: &str) -> Result<()> {
    state
        .store
        .log_action(Some(pid), iid, action, None, None, None)
        .await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddInstance {
    expected_revision: u64,
    mode: String,
    #[serde(default)]
    vape_instance_id: Option<String>,
    label: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    constellation: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    is_primary: bool,
}
async fn instances(State(state): State<AppState>, Path(pid): Path<String>) -> Result<Json<Value>> {
    let p = state.store.require_project(&pid).await?;
    Ok(Json(
        json!({"instances":view(&state,p,true).await?["instances"]}),
    ))
}
async fn instance(
    State(state): State<AppState>,
    Path((pid, rid)): Path<(String, String)>,
) -> Result<Json<Value>> {
    state.store.require_relationship(&pid, &rid).await?;
    let p = state.store.require_project(&pid).await?;
    let v = view(&state, p, true).await?;
    let item = v["instances"]
        .as_array()
        .and_then(|instances| instances.iter().find(|r| r["id"] == rid))
        .ok_or_else(|| failure("not_found", "relationship no longer belongs to project"))?;
    Ok(Json(json!({"instance":item})))
}
async fn add_instance(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Input(r): Input<AddInstance>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    let p = state
        .store
        .check_writable(&pid, r.expected_revision)
        .await?;
    required(&r.label, "label")?;
    let mut rel = Relationship::pending(
        &p,
        r.label,
        r.constellation.unwrap_or(p.constellation.clone()),
        r.model,
    );
    if let Some(role) = r.role {
        required(&role, "role")?;
        rel.role = role;
    }
    match r.mode.as_str() {
        "attach" => {
            let iid = r
                .vape_instance_id
                .ok_or_else(|| failure("validation_error", "vape_instance_id required"))?;
            validate_id(&iid)?;
            state
                .vape
                .get_instance(&iid)
                .await
                .map_err(|_| failure("provider_unavailable", "cannot verify upstream instance"))?;
            rel.vape_instance_id = Some(iid);
            rel.created_by = "attached".into();
        }
        "provision" => {
            if r.vape_instance_id.is_some() {
                return Err(
                    failure("validation_error", "provision cannot set upstream identity").into(),
                );
            }
        }
        _ => return Err(failure("validation_error", "mode must be attach or provision").into()),
    }
    required(&rel.constellation, "constellation")?;
    let rel = state
        .store
        .add_relationship(&pid, r.expected_revision, rel, r.is_primary)
        .await?;
    audit(
        &state,
        &pid,
        rel.vape_instance_id.as_deref(),
        "instance_attached_or_pending",
    )
    .await?;
    acknowledgement(&state, &pid, json!({"instance":rel})).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstancePatch {
    expected_revision: u64,
    #[serde(default, deserialize_with = "non_null")]
    label: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    role: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    constellation: Option<String>,
    #[serde(default)]
    model: Field<String>,
    #[serde(default)]
    goal_override: Field<String>,
    #[serde(default)]
    heartbeat_prompt_override: Field<String>,
    #[serde(default, deserialize_with = "non_null")]
    is_primary: Option<bool>,
}
async fn patch_instance(
    State(state): State<AppState>,
    Path((pid, rid)): Path<(String, String)>,
    Input(req): Input<InstancePatch>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    let old_primary = state
        .store
        .relationships(&pid)
        .await?
        .primary_instance_relationship_id;
    state
        .store
        .edit_relationship(&pid, &rid, req.expected_revision, req.is_primary, |r| {
            for (input, dest, name) in [
                (req.label, &mut r.label, "label"),
                (req.role, &mut r.role, "role"),
                (req.constellation, &mut r.constellation, "constellation"),
            ] {
                if let Some(v) = input {
                    required(&v, name)?;
                    *dest = v;
                }
            }
            apply(req.model, &mut r.model);
            apply(req.goal_override, &mut r.goal_override);
            apply(
                req.heartbeat_prompt_override,
                &mut r.heartbeat_prompt_override,
            );
            Ok(())
        })
        .await?;
    let updated = state.store.relationships(&pid).await?;
    state.store.log_action(Some(&pid), None, "relationship_updated_or_promoted", Some(&json!({"relationship_id":rid,"old_primary":old_primary,"new_primary":updated.primary_instance_relationship_id})), None, None).await?;
    acknowledgement(&state, &pid, json!({"ok":true})).await
}
async fn detach(
    State(state): State<AppState>,
    Path((pid, rid)): Path<(String, String)>,
    Input(req): Input<Revision>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .detach_relationship(&pid, &rid, req.expected_revision)
        .await?;
    audit(&state, &pid, None, "instance_detached_retained").await?;
    acknowledgement(&state, &pid, json!({"ok":true})).await
}

/// Caller holds project_guard. A persisted attempt is reconciled before any retry;
/// ambiguous requests are never blindly retried into duplicate VAPE instances.
pub async fn provision(state: &AppState, pid: &str, rid: &str) -> anyhow::Result<()> {
    let rel = state.store.require_relationship(pid, rid).await?;
    if rel.vape_instance_id.is_some() {
        return Ok(());
    }
    if !state.vape.live {
        return Err(failure(
            "dry_run",
            "provision skipped because MAZZ_FLUX_LIVE=0",
        ));
    }
    let (_, existing) = state.vape.list_instances().await.map_err(|_| {
        failure(
            "provider_unavailable",
            "cannot reconcile upstream instance list",
        )
    })?;
    let matches: Vec<_> = existing
        .iter()
        .filter(|i| {
            ["mazz-flux-relationship", "vape.io/mazz-flux-relationship"]
                .iter()
                .any(|key| i.labels.get(*key) == Some(&rel.id))
        })
        .collect();
    if matches.len() > 1 {
        return Err(failure(
            "conflict",
            "multiple upstream instances match relationship; reconcile manually",
        ));
    }
    if let Some(i) = matches.first() {
        return state.store.bind_instance(pid, rid, &i.id).await;
    }
    if rel.provision_attempted {
        return Err(failure(
            "provision_uncertain",
            "prior create may have reached VAPE; no labeled instance found yet. Retry reconciliation later; do not duplicate the request",
        ));
    }
    let p = state.store.require_project(pid).await?;
    let all = state.store.relationships(pid).await?;
    let primary = all.primary_instance_relationship_id.as_deref() == Some(rid);
    let goal = if primary {
        rel.goal_override.as_deref().unwrap_or(&p.goal)
    } else {
        &p.goal
    };
    let c = crate::conductor::Conductor::from_sources(&state.store).await;
    let archetypes = serde_json::to_string(&state.store.list_archetypes().await?)?;
    let board = serde_json::to_string(&state.store.get_kanban_board(pid).await?)?;
    let composed = c
        .compose_initial_prompt(
            &p.name,
            goal,
            Some(&archetypes),
            if primary { Some(&board) } else { None },
        )
        .await
        .unwrap_or_else(|_| goal.into());
    let resolution = crate::public_url::resolve_public_url(&state.store).await;
    let (prompt, _) = crate::heartbeat::apply_live_agent_context(composed, pid, &resolution);
    let model = match rel.model {
        Some(m) => m,
        None => {
            crate::conductor::resolve_model(
                &state.store,
                "instance_model",
                "MAZZ_FLUX_INSTANCE_MODEL",
                crate::conductor::DEFAULT_MODEL,
            )
            .await
        }
    };
    let name = format!(
        "{}-{}",
        crate::heartbeat::slugify(&rel.label),
        &rid[..8.min(rid.len())]
    );
    let req = CreateInstanceRequest {
        name,
        constellation: rel.constellation,
        subdomain: None,
        ticket: None,
        job: Some(JobConfig {
            prompt,
            harness: Some("pida".into()),
            model: Some(model),
        }),
        labels: std::collections::HashMap::from([
            ("worker".into(), "true".into()),
            ("mazz-flux-project".into(), pid.into()),
            ("mazz-flux-relationship".into(), rid.into()),
        ]),
    };
    let request = match state.vape.prepare_create(&req).await {
        Ok(request) => request,
        Err(error) => {
            state
                .store
                .provision_state(pid, rid, false, Some(error.to_string()))
                .await?;
            return Err(failure("provider_unavailable", error.to_string()));
        }
    };
    state
        .store
        .provision_state(
            pid,
            rid,
            true,
            Some("create requested; reconciliation required on unknown outcome".into()),
        )
        .await?;
    match state.vape.send_create(request).await {
        Ok(v) => {
            let iid = v["id"].as_str().ok_or_else(|| {
                failure("provision_uncertain", "provider response omitted identity")
            })?;
            state.store.bind_instance(pid, rid, iid).await?;
            state
                .store
                .log_action(
                    Some(pid),
                    Some(iid),
                    "instance_provisioned",
                    Some(&json!({"relationship_id":rid})),
                    None,
                    None,
                )
                .await?;
            Ok(())
        }
        Err(error) => {
            state
                .store
                .provision_state(pid, rid, error.is_ambiguous(), Some(error.to_string()))
                .await?;
            Err(failure("provider_unavailable", error.to_string()))
        }
    }
}

pub async fn ensure_primary(state: &AppState, pid: &str) -> anyhow::Result<Relationship> {
    let mut all = state.store.relationships(pid).await?;
    if all.instances.is_empty() {
        let p = state.store.require_project(pid).await?;
        let r = Relationship::pending(&p, "Primary".into(), p.constellation.clone(), None);
        state
            .store
            .add_relationship(pid, p.tracking.revision, r, true)
            .await?;
        all = state.store.relationships(pid).await?;
    }
    let r = all
        .primary()
        .ok_or_else(|| failure("missing_primary", "select a primary instance"))?
        .clone();
    provision(state, pid, &r.id).await?;
    state.store.require_relationship(pid, &r.id).await
}
async fn start(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Input(r): Input<Revision>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .check_writable(&pid, r.expected_revision)
        .await?;
    ensure_primary(&state, &pid).await?;
    state
        .store
        .set_project_status_only(&pid, ProjectStatus::Running)
        .await?;
    state.store.set_heartbeat_enabled(&pid, true).await?;
    audit(&state, &pid, None, "heartbeat_started").await?;
    acknowledgement(&state, &pid, json!({"ok":true})).await
}
async fn pause(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Input(r): Input<Revision>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .check_writable(&pid, r.expected_revision)
        .await?;
    state.store.set_heartbeat_enabled(&pid, false).await?;
    state
        .store
        .set_project_status_only(&pid, ProjectStatus::Paused)
        .await?;
    audit(&state, &pid, None, "heartbeat_paused").await?;
    acknowledgement(&state, &pid, json!({"ok":true})).await
}
async fn force(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Input(r): Input<Revision>,
) -> Result<Json<Value>> {
    let revision = crate::heartbeat::force_tick_checked(&state, &pid, r.expected_revision).await?;
    Ok(Json(json!({"ok":true,"revision":revision})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    expected_revision: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    message: Option<String>,
}
async fn action(
    State(state): State<AppState>,
    Path((pid, rid, action)): Path<(String, String, String)>,
    Input(req): Input<Action>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .check_writable(&pid, req.expected_revision)
        .await?;
    let rel = state.store.require_relationship(&pid, &rid).await?;
    if action == "provision" {
        provision(&state, &pid, &rid).await?;
        return acknowledgement(&state, &pid, json!({"ok":true})).await;
    }
    let iid = rel
        .vape_instance_id
        .ok_or_else(|| failure("conflict", "instance is pending provision"))?;
    let result = match action.as_str() {
        "start" => state.vape.start_instance(&iid).await,
        "stop" => state.vape.stop_instance(&iid).await,
        "rename" => {
            let n = req
                .name
                .ok_or_else(|| failure("validation_error", "name required"))?;
            required(&n, "name")?;
            state.vape.rename_instance(&iid, &n).await
        }
        "message" => {
            let m = req
                .message
                .ok_or_else(|| failure("validation_error", "message required"))?;
            required(&m, "message")?;
            let a = state
                .vape
                .agent_status(&iid)
                .await
                .map_err(|_| failure("provider_unavailable", "agent status unavailable"))?;
            if a.active_harness.as_deref() != Some("pida") {
                return Err(failure("conflict", "only pida messages are supported").into());
            }
            state.vape.pida_send(&iid, &m).await
        }
        _ => return Err(failure("not_found", "unknown action").into()),
    }
    .map_err(|_| failure("provider_unavailable", "upstream instance action failed"))?;
    audit(
        &state,
        &pid,
        Some(&iid),
        &format!("manual_instance_{action}"),
    )
    .await?;
    acknowledgement(&state, &pid, json!({"result":result})).await
}
async fn instance_read(
    State(state): State<AppState>,
    Path((pid, rid, kind)): Path<(String, String, String)>,
) -> Result<Json<Value>> {
    let rel = state.store.require_relationship(&pid, &rid).await?;
    let iid = rel
        .vape_instance_id
        .ok_or_else(|| failure("conflict", "pending instance"))?;
    match kind.as_str() {
        "status" => {
            let i = state
                .vape
                .get_instance(&iid)
                .await
                .map_err(|_| failure("provider_unavailable", "instance unavailable"))?;
            let a = if i.is_running() {
                state.vape.agent_status(&iid).await.ok()
            } else {
                None
            };
            let p = if a.as_ref().and_then(|s| s.active_harness.as_deref()) == Some("pida") {
                state.vape.pida_status(&iid).await.ok()
            } else {
                None
            };
            Ok(Json(json!({"instance":i,"agent_status":a,"pida_status":p})))
        }
        "session" => {
            let mut session = state
                .vape
                .pida_session(&iid)
                .await
                .map_err(|_| failure("provider_unavailable", "session unavailable"))?;
            session.messages = session
                .messages
                .into_iter()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            Ok(Json(json!({"session":session})))
        }
        _ => Err(failure("not_found", "unknown read operation").into()),
    }
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ResourceWrite {
    expected_revision: u64,
    #[serde(default, deserialize_with = "non_null")]
    url: Option<String>,
    #[serde(default)]
    key: Field<String>,
    #[serde(default)]
    label: Field<String>,
    #[serde(default)]
    title: Field<String>,
    #[serde(default, deserialize_with = "non_null")]
    content: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    position: Option<usize>,
    #[serde(default, deserialize_with = "non_null")]
    project_key: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    environment_key: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    fully_enabled_value: Option<bool>,
    #[serde(default)]
    notes: Field<String>,
}
fn links_mut<'a>(p: &'a mut Project, kind: &str) -> anyhow::Result<&'a mut Vec<ProjectLink>> {
    match kind {
        "jira-tickets" => Ok(&mut p.tracking.original_jira_tickets),
        "ec-tickets" => Ok(&mut p.tracking.ec_tickets),
        "vape-pages" => Ok(&mut p.tracking.vape_pages),
        _ => Err(failure("not_found", "unknown collection")),
    }
}
async fn resources(
    State(state): State<AppState>,
    Path((pid, kind)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let mut p = state.store.require_project(&pid).await?;
    let entries = match kind.as_str() {
        "notes" => serde_json::to_value(state.store.user_notes(&pid).await?)?,
        "launchdarkly-flags" => view(&state, p, true).await?["launchdarkly_flags"].clone(),
        _ => serde_json::to_value(links_mut(&mut p, &kind)?)?,
    };
    Ok(Json(json!({"entries":entries})))
}
async fn resource(
    State(state): State<AppState>,
    Path((pid, kind, rid)): Path<(String, String, String)>,
) -> Result<Json<Value>> {
    let Json(v) = resources(State(state), Path((pid, kind))).await?;
    let item = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == rid)
        .ok_or_else(|| failure("not_found", "resource not found"))?;
    Ok(Json(json!({"entry":item})))
}
async fn resource_create(
    State(state): State<AppState>,
    Path((pid, kind)): Path<(String, String)>,
    Input(req): Input<ResourceWrite>,
) -> Result<Json<Value>> {
    write_resource(&state, &pid, &kind, None, req).await
}
async fn resource_patch(
    State(state): State<AppState>,
    Path((pid, kind, rid)): Path<(String, String, String)>,
    Input(req): Input<ResourceWrite>,
) -> Result<Json<Value>> {
    write_resource(&state, &pid, &kind, Some(rid), req).await
}
async fn write_resource(
    state: &AppState,
    pid: &str,
    kind: &str,
    rid: Option<String>,
    req: ResourceWrite,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(pid).await?;
    let entry = if kind == "notes" {
        let mut note = if let Some(nid) = &rid {
            state
                .store
                .user_notes(pid)
                .await?
                .into_iter()
                .find(|n| &n.id == nid)
                .ok_or_else(|| failure("not_found", "note not found"))?
        } else {
            UserNote {
                id: id(),
                title: None,
                content: String::new(),
                position: 0,
                created_at: timestamp(),
                updated_at: timestamp(),
            }
        };
        apply(req.title, &mut note.title);
        if let Some(c) = req.content {
            note.content = c;
        }
        let note = state
            .store
            .save_user_note(pid, req.expected_revision, note, rid.is_none())
            .await?;
        if let Some(pos) = req.position {
            let notes = state.store.user_notes(pid).await?;
            let mut ids: Vec<_> = notes
                .into_iter()
                .map(|n| n.id)
                .filter(|id| id != &note.id)
                .collect();
            ids.insert(pos.min(ids.len()), note.id.clone());
            let p = state.store.require_project(pid).await?;
            state
                .store
                .reorder_notes(pid, p.tracking.revision, ids)
                .await?;
        }
        serde_json::to_value(
            state
                .store
                .user_notes(pid)
                .await?
                .into_iter()
                .find(|n| n.id == note.id)
                .ok_or_else(|| failure("not_found", "note not found"))?,
        )?
    } else {
        let eid = rid.clone().unwrap_or_else(id);
        let mut result = Value::Null;
        state
            .store
            .mutate_project(pid, req.expected_revision, |p| {
                if kind == "launchdarkly-flags" {
                    let flags = &mut p.tracking.launchdarkly_flags;
                    let mut f = if rid.is_some() {
                        flags
                            .iter()
                            .find(|f| f.id == eid)
                            .cloned()
                            .ok_or_else(|| failure("not_found", "flag not found"))?
                    } else {
                        FlagReference {
                            id: eid.clone(),
                            key: String::new(),
                            project_key: default_project(),
                            environment_key: production(),
                            fully_enabled_value: true,
                            label: None,
                            notes: None,
                        }
                    };
                    if let Field::Set(key) = req.key {
                        f.key = key.unwrap_or_default();
                    }
                    if let Some(k) = req.project_key {
                        f.project_key = k;
                    }
                    if let Some(k) = req.environment_key {
                        f.environment_key = k;
                    }
                    if let Some(v) = req.fully_enabled_value {
                        f.fully_enabled_value = v;
                    }
                    apply(req.label, &mut f.label);
                    apply(req.notes, &mut f.notes);
                    make_flag(FlagInput {
                        key: f.key.clone(),
                        project_key: f.project_key.clone(),
                        environment_key: f.environment_key.clone(),
                        fully_enabled_value: f.fully_enabled_value,
                        label: f.label.clone(),
                        notes: f.notes.clone(),
                    })?;
                    f.id = eid.clone();
                    flags.retain(|f| f.id != eid);
                    flags.push(f.clone());
                    unique_flags(flags)?;
                    result = serde_json::to_value(f)?;
                } else {
                    let links = links_mut(p, kind)?;
                    let mut l = if rid.is_some() {
                        links
                            .iter()
                            .find(|l| l.id == eid)
                            .cloned()
                            .ok_or_else(|| failure("not_found", "link not found"))?
                    } else {
                        ProjectLink {
                            id: eid.clone(),
                            url: String::new(),
                            key: None,
                            label: None,
                        }
                    };
                    if let Some(url) = req.url {
                        l.url = url;
                    }
                    l.url = normalize_url(&l.url)?;
                    apply(req.key, &mut l.key);
                    apply(req.label, &mut l.label);
                    links.retain(|l| l.id != eid);
                    links.push(l.clone());
                    unique_links(links)?;
                    result = serde_json::to_value(l)?;
                }
                Ok(())
            })
            .await?;
        result
    };
    audit(state, pid, None, "project_resource_saved").await?;
    acknowledgement(state, pid, json!({"entry":entry})).await
}
async fn resource_delete(
    State(state): State<AppState>,
    Path((pid, kind, rid)): Path<(String, String, String)>,
    Input(req): Input<Revision>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    if kind == "notes" {
        state
            .store
            .delete_user_note(&pid, &rid, req.expected_revision)
            .await?;
    } else {
        state
            .store
            .mutate_project(&pid, req.expected_revision, |p| {
                let removed = if kind == "launchdarkly-flags" {
                    let c = &mut p.tracking.launchdarkly_flags;
                    let len = c.len();
                    c.retain(|f| f.id != rid);
                    len != c.len()
                } else {
                    let c = links_mut(p, &kind)?;
                    let len = c.len();
                    c.retain(|l| l.id != rid);
                    len != c.len()
                };
                if !removed {
                    return Err(failure("not_found", "resource not found"));
                }
                Ok(())
            })
            .await?;
    }
    audit(&state, &pid, None, "project_resource_deleted").await?;
    acknowledgement(&state, &pid, json!({"ok":true})).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reorder {
    expected_revision: u64,
    ids: Vec<String>,
}
async fn reorder(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Input(r): Input<Reorder>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .reorder_notes(&pid, r.expected_revision, r.ids)
        .await?;
    audit(&state, &pid, None, "notes_reordered").await?;
    acknowledgement(&state, &pid, json!({"ok":true})).await
}
pub async fn agent_context(
    State(state): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>> {
    let p = state.store.require_project(&pid).await?;
    let mut v = view(&state, p, false).await?;
    for field in ["created_at", "updated_at", "note_order"] {
        v.as_object_mut().unwrap().remove(field);
    }
    Ok(Json(
        json!({"project":v,"kanban":state.store.get_kanban_board(&pid).await?,"archetypes":state.store.list_archetypes().await?}),
    ))
}

async fn memory(State(state): State<AppState>, Path(pid): Path<String>) -> Result<Json<Value>> {
    state.store.require_project(&pid).await?;
    Ok(Json(json!({"memory":state.store.read_memory(&pid).await?})))
}
async fn activity_notes(
    State(state): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>> {
    state.store.require_project(&pid).await?;
    Ok(Json(
        json!({"notes":state.store.list_project_notes(&pid).await?}),
    ))
}
async fn kanban(State(state): State<AppState>, Path(pid): Path<String>) -> Result<Json<Value>> {
    state.store.require_project(&pid).await?;
    Ok(Json(
        json!({"board":state.store.get_kanban_board(&pid).await?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskWrite {
    expected_revision: u64,
    #[serde(default, deserialize_with = "non_null")]
    title: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    description: Option<String>,
    #[serde(default, deserialize_with = "non_null")]
    status: Option<KanbanStatus>,
}
async fn create_task(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Input(r): Input<TaskWrite>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .check_writable(&pid, r.expected_revision)
        .await?;
    let title = r
        .title
        .ok_or_else(|| failure("validation_error", "title required"))?;
    required(&title, "title")?;
    let task = state
        .store
        .create_kanban_task(
            &pid,
            &title,
            &r.description.unwrap_or_default(),
            r.status.unwrap_or(KanbanStatus::Assigned),
        )
        .await?;
    state
        .store
        .mutate_project(&pid, r.expected_revision, |_| Ok(()))
        .await?;
    audit(&state, &pid, None, "kanban_task_created").await?;
    acknowledgement(&state, &pid, json!({"task":task})).await
}
async fn update_task(
    State(state): State<AppState>,
    Path((pid, tid)): Path<(String, String)>,
    Input(r): Input<TaskWrite>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .check_writable(&pid, r.expected_revision)
        .await?;
    if let Some(t) = &r.title {
        required(t, "title")?;
    }
    let task = state
        .store
        .update_kanban_task(
            &pid,
            &tid,
            r.title.as_deref(),
            r.description.as_deref(),
            r.status,
        )
        .await?
        .ok_or_else(|| failure("not_found", "task not found"))?;
    state
        .store
        .mutate_project(&pid, r.expected_revision, |_| Ok(()))
        .await?;
    audit(&state, &pid, None, "kanban_task_updated").await?;
    acknowledgement(&state, &pid, json!({"task":task})).await
}
async fn delete_task(
    State(state): State<AppState>,
    Path((pid, tid)): Path<(String, String)>,
    Input(r): Input<Revision>,
) -> Result<Json<Value>> {
    let _guard = state.store.project_guard(&pid).await?;
    state
        .store
        .check_writable(&pid, r.expected_revision)
        .await?;
    if !state.store.delete_kanban_task(&pid, &tid).await? {
        return Err(failure("not_found", "task not found").into());
    }
    state
        .store
        .mutate_project(&pid, r.expected_revision, |_| Ok(()))
        .await?;
    audit(&state, &pid, None, "kanban_task_deleted").await?;
    acknowledgement(&state, &pid, json!({"ok":true})).await
}

async fn openapi() -> Json<Value> {
    Json(
        serde_json::from_str(include_str!("../docs/openapi.json"))
            .expect("checked OpenAPI document"),
    )
}
pub async fn gone() -> impl IntoResponse {
    (
        StatusCode::GONE,
        Json(
            json!({"error":{"code":"api_moved","message":"Project API moved to /api/v2/projects","details":{}}}),
        ),
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v2/openapi.json", get(openapi))
        .route("/api/v2/projects", get(list).post(create))
        .route(
            "/api/v2/projects/{pid}",
            get(detail).patch(patch).delete(purge),
        )
        .route("/api/v2/projects/{pid}/agent-context", get(agent_context))
        .route("/api/projects/{id}/agent-context", get(agent_context))
        .route("/api/projects", get(gone).post(gone))
        .route(
            "/api/projects/{id}",
            get(gone).delete(gone).post(gone).patch(gone),
        )
        .route("/api/v2/projects/{pid}/archive", post(archive))
        .route("/api/v2/projects/{pid}/restore", post(restore))
        .route("/api/v2/projects/{pid}/start", post(start))
        .route("/api/v2/projects/{pid}/pause", post(pause))
        .route("/api/v2/projects/{pid}/heartbeat/force", post(force))
        .route("/api/v2/projects/{pid}/memory", get(memory))
        .route("/api/v2/projects/{pid}/activity-notes", get(activity_notes))
        .route(
            "/api/v2/projects/{pid}/kanban",
            get(kanban).post(create_task),
        )
        .route(
            "/api/v2/projects/{pid}/kanban/{rid}",
            post(update_task).delete(delete_task),
        )
        .route(
            "/api/v2/projects/{pid}/instances",
            get(instances).post(add_instance),
        )
        .route(
            "/api/v2/projects/{pid}/instances/{rid}",
            get(instance).patch(patch_instance).delete(detach),
        )
        .route(
            "/api/v2/projects/{pid}/instances/{rid}/{action}",
            get(instance_read).post(action),
        )
        .route("/api/v2/projects/{pid}/notes/reorder", post(reorder))
        .route(
            "/api/v2/projects/{pid}/{collection}",
            get(resources).post(resource_create),
        )
        .route(
            "/api/v2/projects/{pid}/{collection}/{rid}",
            get(resource).patch(resource_patch).delete(resource_delete),
        )
}
