# Multi-instance projects: implementation and operator runbook

## Authority and boundaries

This implements the approved D1–D14 design from the planning session. One project groups many
Flux relationships, but only one primary receives automated conductor traffic. Delivery status
is manual. LD integration is read-only. Related instances are operated only by explicit actions.
All instance removal paths retain the upstream instance. No live migration or process restart
is part of the code implementation; the operator performs the cutover after review.

## Storage

- `projects/<id>.json`: schema 2, revision, delivery status, description, links, flag tracking,
  note ordering, nested automation state, timestamps and default launch constellation.
- `project_instances/<id>.json`: complete relationship collection and optional primary ID.
- `user_notes/<project>/<note>.json`: stable user note content. Order is stored atomically on
  the project, and returned positions are calculated from it.
- Existing conductor notes, memory, human tasks, Kanban, settings, archetypes and action logs
  remain in their existing locations.

User metadata writes and the relationship collection are individually atomic, serialized within
one process. They are not cross-file database transactions. Revision updates follow collection
writes; disk failures can leave a saved collection without its revision increment. Storage errors
are surfaced as 500 and need operator inspection. Keep backups; do not run multiple writers.
Migration writes relationships before rewriting a legacy project, allowing safe retry after a
process interruption. Unknown future schemas, invalid delivery status, conflicting ownership,
and malformed records block startup before migration writes. Preflight inventories every
relationship collection, including orphan and misnamed files. A resumed legacy collection must
preserve exactly its original primary VAPE ID and project identity.

## Migration dry-run and cutover

1. Stop or maintenance-lock the old managed process via VAPE lifecycle tools. Do not run an old
   writer against schema 2 state.
2. Snapshot the complete state directory, including uncommitted files. Record code SHA, state
   SHA and backup path. A Git commit alone is insufficient if untracked state was not staged.
3. Copy state to a scratch directory, preserving all project/history files. Do not point a second
   running server at the live state directory.
4. Run `target/debug/mazz-flux-bot migrate-state` with `MAZZ_FLUX_DATA_DIR` set to the copy.
   Dry-run changes nothing and reports every record's before/after SHA256 plus validation errors.
5. Review every error and ownership conflict. Legacy `vape_instance_id` becomes a deterministic
   `legacy-<project-id>` primary relationship. Every existing delivery status starts Pending;
   old Blocked/Done/etc. remains under automation. No external calls occur during migration.
6. Run `migrate-state --apply` against the copy. Rerun dry-run; all records should be schema 2
   with `migrated:false`. Verify IDs, lifecycle, history, cadence and counts.
7. Build and deploy the new binary/UI together. After stopping the old writer and backing up,
   apply migration to the live directory or let startup perform the same preflight/application.
8. Restart only the managed process through VAPE. Inspect logs and status, then exercise a
   specific `/api/v2/projects` response and the UI.
9. Existing workers keep their old GET-only context URL; it aliases the v2 handler. Every other
   old project route is removed (old list/detail return 410). The alias accepts no writes.

## Rollback

Stop the new managed writer. Restore the pre-migration directory and the old binary/checkout
as a matched pair. Do not simply switch back to the old binary while keeping schema 2 state.
Retain the failed migrated directory for inspection. No VAPE instance is removed by rollback.

## Provisioning and concurrency

Every API lifecycle operation and heartbeat acquires the same project lock. A queued scheduled
tick rechecks enabled/due/archive state after acquiring it. A Force request validates its revision
inside the lock and can intentionally act while paused. Archive prevents both automatic and
forced ticks. Promotion waits for any already executing tick, then updates the primary; no old
target action is allowed after a completed promotion.

New instances are labeled `worker=true`, `mazz-flux-project=<id>`, and
`mazz-flux-relationship=<id>`. Provisioning reconciles those labels first. The sent-attempt marker is written only after request/authentication preparation succeeds.
Ambiguous transport/5xx responses leave reconciliation-only state; confirmed no-create failures
clear that state and allow correction/retry on the same relationship. Every persisted provisioning
transition increments aggregate revision, invalidating pre-attempt edits and detach requests.
A missing/ambiguous response stays visible rather than producing duplicates.

Prompt overrides belong only to primary. Clear them before demoting/promoting away from a
primary that has overrides; the API rejects silent loss of primary-only direction. The former
primary remains related and receives no later heartbeat traffic. Shared memory is retained
with a transition marker and the new primary is due immediately. Paused remains paused.

## Archive, restore, purge

Archive pauses automation, hides the project from default listing, stops provider polling,
and retains its relationship ownership. Restore leaves automation paused. Purge requires the
project to be archived, the exact project name, and the current revision. It removes all local
project/relationship/note/memory/Kanban/human-task files, but preserves global audit history.
It never calls VAPE stop/delete. Detach is the explicit way to release an instance for reuse
without purging project history.

## Safety and access

This remains a trusted-network, single-user app. It does not add tenant authentication. The
raw file browser remains powerful and bypasses domain revisions; use it only as an operator.
The agent-context endpoint excludes global settings, other projects, logs and transcripts.
Never store tokens in project notes or links. LD credentials come only from `LD_ACCESS_TOKEN`;
provider errors are sanitized and the LD client has no mutation interface.

## Verification contract

- `bolt test`: legacy migration/preflight, delivery enum, relationship uniqueness, note CRUD,
  API revisions/errors/clear behavior, GET-only context alias, archive/restore/purge, primary
  targeting and static UI route usage.
- `tests/primary_heartbeat.rs`: spawns a short-lived test server with loopback-only VAPE/LLM
  providers and a test-only gh executable. Confirms automatic reads/sends target the primary
  and never the secondary. All fake data remains in tests.
- `node --check static/app.js` and `node --check static/project.js`.
- OpenAPI document at `docs/openapi.json` and `/api/v2/openapi.json`.
- Before live enablement: scratch project with one primary/two related instances; verify exact
  targeting, promotion, no-primary conflict, note/link/flag CRUD, archive/restore/purge retention.
  Real provisioning requires explicit user approval and must not be run by unit tests.

## Observability

The action log records project/resource edits, lifecycle actions, targeted manual operations,
provisioned IDs, primary changes, heartbeat decisions, and errors. Process logs expose startup
migration failures and provider/storage failures. API enrichment reports per-instance and
per-flag unavailable states without converting provider failure into false/disabled state.
Inspect durations and errors before lowering polling/cadence or increasing tracked counts.

## Browser reconciliation and contract details

Project mutation responses acknowledge the revision while holding the project guard. The UI
rebases retained sibling drafts only when they were based on the mutation's expected revision;
it does not rebase drafts across unrelated external changes. Stable keyed DOM reconciliation
preserves open disclosures, loaded sessions, clean keyboard focus and unrelated dirty forms.
Non-nullable PATCH fields reject explicit null; nullable prompts/model/labels/titles retain
null-as-clear semantics. Create and patch both enforce a 5..=31536000 second interval.
The instance-status contract returns `instance`, nullable `agent_status`, and nullable
`pida_status`; it does not wrap them in a fictitious `status` object. Percentage/mixed LD defaults
retain known targeting and the provider/cache fetch time even when the default value is unknown.
