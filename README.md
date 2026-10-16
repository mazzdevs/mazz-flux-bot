# mazz-flux-bot

A file-backed project dashboard and heartbeat conductor for Flux/VAPE instances.
Projects group multiple instances with **one explicitly selected primary**. Project tracking,
context bootstrap, and autonomous execution are separate. Related instances remain manual-only.

## Project tracking

- Manual delivery status: **Pending, In Progress, Review, Merged, Flags Enabled, Flags Cleaned**.
- Separate `automation.status`: Draft, Running, Paused, Done, Error, Blocked.
- Description and delivery context; no automation goal is required to create or Start a new project.
- Explicit execution owner: context bootstrap, Flux Goal Loop, or deprecated legacy conductor.
- Original Jira tickets, EC tickets, VAPE pages, stable editable notes.
- LaunchDarkly flag references with live, read-only default value / targeting / target match.
- Multiple Flux relationships with labels, roles and launch constellation/model. Deprecated legacy
  goal/prompt overrides are preserved for existing conductor projects, not inferred as native goals.
- Archive/restore and separately confirmed permanent local purge.

Neither automation outcomes nor LaunchDarkly values automatically change delivery status.
Detach, archive and purge **never stop or delete a real VAPE instance**. An archived project
retains exclusive instance ownership; detach first to reuse an instance elsewhere.

## Run and verify

```sh
bolt up
bolt test
bolt fmt
bolt run
```

The HTTP service listens on port 4270 by default. Static HTML/CSS/JS is served directly;
there is no frontend framework/build step. Never expose the API outside the existing
trusted VAPE ingress: it is a single-user operator tool, not a multi-tenant auth service.

## Execution ownership

New schema-v3 projects default to `bootstrap_only`. Existing schema-v2 projects migrate to
`legacy_conductor`, preserving local goal/override/prompt data and delivery status. Conversion is
explicit, revision-checked, pauses local monitoring, and never launches work.

| Mode | Start / periodic heartbeat | Who continues work |
|---|---|---|
| `bootstrap_only` | Provision/resume primary; context-only bootstrap once per observed Pida session | Nobody automatically; the agent summarizes and waits for direction |
| `native_goal` | Start initializes/resumes; heartbeat completes pending context bootstrap once ready, then observes Flux and receipts | Flux Goal Loop, only after an explicit native Start goal request |
| `legacy_conductor` | Existing goal-based Decide/Act flow, with its prior direction prerequisite | mazz conductor; retained for deliberate legacy use |

Start with no relationships creates a default primary. Existing relationships without a primary
require explicit selection (`409 missing_primary`). If the bound primary is stopped, Start requests
upstream start and leaves bootstrap pending until Pida is ready with a known session identity.
A missing bound instance is an error, never silently replaced. Nonlegacy create uses top-level
`model` and **no create-time job**. The context bootstrap loads the live project context/Kanban,
summarizes status/open decisions, prohibits executing tasks/deployments/data/flags, and waits.
Descriptions and Kanban are not authorization or completion conditions. Bootstrap acceptance is
recorded per project/instance/session; uncertain sends are never retried automatically. In native
mode Start persists pending initialization so heartbeat/Force can finish this context-only step
after provisioning or restart readiness; mode selection alone never requests initialization.

Select `native_goal` in Work → Execution ownership before submitting an exact user-authored
condition. The adapter checks that the `goal` extension command exists, Pida is ready, the observed
session matches, Auto Tend is off, and native state is known and non-conflicting. It sends only the
escaped `/goal` command through that session's scoped chat endpoint. It never uses `job.goal=true`
on a bootstrap prompt. Limits follow upstream: trimmed one to four thousand UTF-16 code units;
flag-like and control-word conditions use `--` escaping. **Conditions containing `@` are rejected**
with 422: Pida chat expands @-file mentions, which could rewrite the judged condition. Use plain
text/paths without @ until Flux exposes a session-fenced raw-goal API.

HTTP acceptance means **pending**, not activated or complete. `GET .../execution` and native
heartbeats read a bounded structured SSE snapshot: active/evaluating only observe; paused only
reports; structured terminal outcomes are retained; missing/malformed/null-only evidence never
means success. A null goal without a terminal row is `no_goal`, not completed. Auto Tend/Goal Loop
capability failure never falls back to the conductor. `Resume observed goal` and `Clear goal` are
explicit; Clear does **not** abort a running turn. Native Pause is **monitoring pause only** and
never claims to pause Flux. Archive and ownership transfer refuse active/evaluating/unknown native
state and unresolved dispatch receipts; clear/inspect Flux first. Paused native goals must be
cleared before switching to another execution owner.

Immutable request receipts retain request ID, relationship/instance/session, exact caller condition,
SHA-256 digest, actor/time and baseline observation. Mutable dispatch/activation observations are
stored separately. Terminal history fingerprints are retained during active/paused state; unchanged
or off-condition clear rows cannot resolve a request. Session or goal identity changes are recorded without replay. A native terminal
outcome never automatically changes manual delivery status. Legacy local goal fields remain API
read-compatible/deprecated and only have an authoring editor in legacy mode.

Provisioning first reconciles `mazz-flux-project` / `mazz-flux-relationship` labels. Once a
create has an ambiguous transport/5xx outcome, a retry only reconciles; it does not blindly
create another instance. An uncertain request remains visible as a pending relationship.
Definitive pre-send/auth/configuration failures and confirmed HTTP rejections remain retryable
on the same relationship after correction. Every persisted attempt/error advances the project
revision, so refresh the revision before correcting or retrying. Constellation/model edits describe launch configuration;
they do not rebuild an already provisioned pod or switch its active model.

## API v2

**Hard cut:** project list/detail/mutation routes now live at `/api/v2/projects`.
The only retained old project route is GET-only `/api/projects/{id}/agent-context`, an alias
for workers whose original launch instructions contain that URL.

Contract: [`docs/openapi.json`](docs/openapi.json), also `GET /api/v2/openapi.json`.
Runbook: [`docs/multi-instance-projects.md`](docs/multi-instance-projects.md).

```sh
# Create a bootstrap-only project without automation prompts.
curl -X POST localhost:4270/api/v2/projects -H 'content-type: application/json' \
  -d '{"name":"My feature","description":"Delivery context","status":"pending"}'

# Read the latest revision before every domain mutation.
curl localhost:4270/api/v2/projects/PROJECT_ID
curl -X PATCH localhost:4270/api/v2/projects/PROJECT_ID -H 'content-type: application/json' \
  -d '{"expected_revision":2,"status":"review","heartbeat_prompt":null}'

# Attach an existing instance (does not start/stop/message it).
curl -X POST localhost:4270/api/v2/projects/PROJECT_ID/instances -H 'content-type: application/json' \
  -d '{"expected_revision":3,"mode":"attach","vape_instance_id":"INSTANCE_ID","label":"Reviewer","role":"reviewer","is_primary":true}'

# All manual actions name a relationship, not an implicit project instance.
curl -X POST localhost:4270/api/v2/projects/PROJECT_ID/instances/RELATIONSHIP_ID/message \
  -H 'content-type: application/json' -d '{"expected_revision":4,"message":"Review this change"}'
```

| Resource | Operations |
|---|---|
| `/api/v2/projects` | GET list (`?archived=true` includes archived), POST create |
| `/api/v2/projects/{id}` | GET aggregate, PATCH metadata, DELETE confirmed archived-local purge |
| `.../archive`, `.../restore`, `.../start`, `.../pause`, `.../heartbeat/force` | POST with `expected_revision` |
| `.../instances` | GET, POST attach or configure pending provision |
| `.../instances/{relationship_id}` | GET, PATCH configuration/primary, DELETE detach |
| `.../instances/{relationship_id}/{provision,start,stop,rename,message}` | Explicit POST actions |
| `.../instances/{relationship_id}/{status,session}` | GET on demand; session bounded to last 20 messages |
| `.../{notes,jira-tickets,ec-tickets,vape-pages,launchdarkly-flags}` | GET/POST collection, GET/PATCH/DELETE by stable resource ID |
| `.../notes/reorder` | POST `{expected_revision, ids:[...]}` (each note ID exactly once) |
| `.../kanban`, `.../kanban/{task_id}` | GET/POST board/create, POST/DELETE task with revision |
| `.../memory`, `.../activity-notes`, `.../agent-context` | GET conductor memory, history and worker context |
| `.../execution` | GET mode, current native observation and request/progress receipts |
| `.../execution/mode` | POST `{expected_revision, execution_mode}`; launches nothing |
| `.../execution/goals/{start,resume,clear}` | POST `{expected_revision, request_id, expected_session_id, condition?}`; condition required only on start |

PATCH omitted fields stay unchanged; explicit null clears nullable fields, and empty arrays
clear collections. Replacing a whole link/flag array assigns new resource IDs; use individual
PATCH to retain identity. IDs, timestamps and live provider fields are not writable. All domain
writes except create require `expected_revision`; stale writes return `409 stale_revision`.
A successful mutation may advance more than one revision and returns its acknowledged
`revision`. Use that revision to rebase local sibling drafts; a later unrelated external change
still produces a conflict. Fetch the current project when starting a new edit.

Preserved unversioned utilities: archetypes, settings, raw instance reads, constellations,
human tasks, global action log, state snapshots and the raw file browser. The file browser is
an operator escape hatch, not a substitute for revision-checked domain writes. executions/ and
bootstrap/ remain browsable but raw Files write/delete requests return 403, including symlink
aliases, so replay guards cannot be erased through that API. Other managed JSON edits still bypass
domain validation; stop the service and validate migration before boot.

## Configuration

| Variable | Default / behavior |
|---|---|
| `PORT` | `4270` |
| `MAZZ_FLUX_DATA_DIR` | Sibling `../mazz-flux-bot-state` |
| `OPENROUTER_API_KEY` | Optional environment-only conductor credential; absent = observe without steering |
| `OPENROUTER_MODEL` | `openai/gpt-5.6-sol`; settings override supported |
| `MAZZ_FLUX_INSTANCE_MODEL` | Default spawned model; settings/relationship override supported |
| `MAZZ_FLUX_PUBLIC_BASE_URL` | Auto-detected when possible; settings override supported |
| `MAZZ_FLUX_LIVE` | Live by default; `0`/`false` blocks VAPE mutations |
| `HEARTBEAT_SCAN_INTERVAL_SECS` | `15` |
| `CADMIUM_VAPE_URL` / `VAPE_MANAGER_URL` | VAPE Manager endpoint |
| `LD_ACCESS_TOKEN` | Optional direct LaunchDarkly **read-only** access token |
| `MAZZ_FLUX_STATE_REPO_URL` | Optional state Git remote |
| `MAZZ_FLUX_STATE_BRANCH` | Snapshot push branch; defaults to `main` (set a feature branch where required) |

LD calls are GET-only, use a five-second timeout and a sixty-second cache. Up to 32 flags are
tracked per project. Missing credentials, inaccessible flags/environments, mixed defaults,
network errors, and provider failures yield explicit per-flag unavailable results without
blocking project reads. A matching default is not proof that all targeting rules/users have
completed rollout; the dashboard compares the environment default only.

## State migration

Startup preflights all projects before migrating to schema 3. Schema-v2 projects retain their
manual status, local goals, automation lifecycle, VAPE IDs and history, and explicitly become
legacy_conductor. Older pre-v2 records first receive the existing Pending/relationship migration.
An error prevents startup without partial preflight writes. No migration dispatches upstream work.

```sh
# Only against a copied state directory while validating:
MAZZ_FLUX_DATA_DIR=/root/workspace/mazz-flux-bot/target/state-copy \
  target/debug/mazz-flux-bot migrate-state           # dry-run; checksums and errors
MAZZ_FLUX_DATA_DIR=/root/workspace/mazz-flux-bot/target/state-copy \
  target/debug/mazz-flux-bot migrate-state --apply
```

Before deployment snapshot state and stop the old writer. Roll back **binary and state together**;
an old binary can otherwise drop new fields when it rewrites JSON. Only one bot process may
write a state directory. See the runbook for the full cutover and recovery sequence.

## Project workspace

The show page has remembered Overview, Instances, Work, and Activity tabs with keyboard navigation.
Overview groups metadata, rollout flags, related links, and notes; Instances keeps targeted controls
and lazy-loaded sessions together; Work contains Kanban, human tasks, and conductor context.
Activity separates the filtered audit log from archive/restore/purge. Delivery and automation
health remain visible in the header. Blank goals can be saved or cleared at any time; creating
a tracking project, attaching an existing instance, and editing tracking data do not need a goal.

In bootstrap/native modes, local legacy goal fields are neither required nor shown in the normal
editor. Start remains available without them. Only legacy conductor mode keeps the old goal
prerequisite and pause-on-cleared-direction behavior. Mode selection never starts work. Native
controls live in Work with explicit pending/unknown outcomes and immutable execution receipts.
