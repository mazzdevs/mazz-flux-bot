# mazz-flux-bot

A file-backed project dashboard and heartbeat conductor for Flux/VAPE instances.
Projects can group multiple instances, with **one explicitly selected primary** receiving
heartbeat automation. Related instances remain manually controlled.

## Project tracking

- Manual delivery status: **Pending, In Progress, Review, Merged, Flags Enabled, Flags Cleaned**.
- Separate `automation.status`: Draft, Running, Paused, Done, Error, Blocked.
- Description, optional automation goal, and optional heartbeat guidance. Projects can be tracking-only.
- Original Jira tickets, EC tickets, VAPE pages, stable editable notes.
- LaunchDarkly flag references with live, read-only default value / targeting / target match.
- Multiple Flux relationships with labels, roles, launch constellation/model, and primary-only goal/prompt overrides.
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

## Automation behavior

1. With a nonblank effective goal, Start on an empty project creates and provisions a default primary. Without a goal, Start/Force and new-instance provisioning return `422 validation_error` before external calls. A primary goal override can supply the direction; heartbeat guidance alone is not a goal.
2. Existing relationships without a primary produce `409 missing_primary`; select one explicitly.
3. A project heartbeat reads the primary's status/transcript, composes a steering decision,
   and acts only on that primary. Related instance transcripts are not polled by heartbeat.
4. Per-project exclusion serializes forced/scheduled ticks, lifecycle changes and promotion.
5. Promotion keeps shared memory with a transition marker, clears last-heartbeat time,
   and makes the new primary due now. A paused project remains paused until Start/Force.
6. Done/Error/Blocked pause automation, not delivery tracking.

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
# Create tracking metadata without automation prompts. Name is optional; LLM naming only runs when a nonblank goal is present.
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

PATCH omitted fields stay unchanged; explicit null clears nullable fields, and empty arrays
clear collections. Replacing a whole link/flag array assigns new resource IDs; use individual
PATCH to retain identity. IDs, timestamps and live provider fields are not writable. All domain
writes except create require `expected_revision`; stale writes return `409 stale_revision`.
A successful mutation may advance more than one revision and returns its acknowledged
`revision`. Use that revision to rebase local sibling drafts; a later unrelated external change
still produces a conflict. Fetch the current project when starting a new edit.

Preserved unversioned utilities: archetypes, settings, raw instance reads, constellations,
human tasks, global action log, state snapshots and the raw file browser. The file browser is
an operator escape hatch, not a substitute for revision-checked domain writes. Editing managed
JSON directly bypasses domain validation; stop the service and validate migration before boot.

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

Startup automatically preflights all projects and migrates legacy records to schema 2 before
starting HTTP/heartbeat. An error prevents startup without partial preflight writes. Every old
project becomes manual Pending while its old automation lifecycle, VAPE ID and history remain.

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

Clearing the last effective goal pauses heartbeat automation in the same project update. A
nonblank primary override can retain direction when the project goal is cleared. Clearing that
override pauses if no project goal remains. Saving direction again never resumes automation
automatically: Start is always explicit. The show-page prerequisite control opens and focuses
the goal editor; tracking-only projects keep Start and Force disabled until direction is saved.
