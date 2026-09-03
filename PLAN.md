# Plan: finish rename/instance-links, LLM-assisted instance naming, model-selection settings

Continuing directly on top of in-flight work this session (settings UI removed,
`agent_prompts/validation.md` injection landed, slugify-based naming landed but not yet
built due to the still-missing `rename_instance` handler). This pass finishes what's
outstanding and adds two new asks.

## Outstanding from previous plan (still to do)

- **`VapeClient::rename_instance`** (`POST /api/v1/instances/{id}/rename`, body
  `{"name": "..."}`) — referenced by `main.rs`'s route but not yet implemented, currently
  a compile error blocking everything else.
- **`api::rename_instance` handler** + route wiring.
- **Instance links on the project detail page** — `GET /api/instances/{id}` already
  returns `urls: Vec<String>`; not yet surfaced in `project.js`/`project.html`.
- **Rename control on the project detail page** — small inline form next to the new
  instance-links block.

## New ask A: LLM-assisted instance naming (optional upgrade over pure slugify)

Current (already implemented, not yet built): `instance_name()` deterministically
slugifies the project's own name + a short id suffix — no LLM call, fast, free, always
works. Per request, offer an *optional* upgrade: ask the conductor LLM to propose a
short, clean instance-name slug from the project's goal text, for cases where the raw
project name doesn't compress well (e.g. name is generic like "test" but the goal is
specific).

- New `Conductor::suggest_instance_slug(&self, project_name: &str, goal: &str) ->
  Result<String>` in `conductor.rs` — one cheap `decide()`-style call with a tight system
  prompt ("respond with ONLY a lowercase-hyphenated slug, 2-4 words, no punctuation
  besides hyphens, summarizing this project for a Kubernetes-safe resource name"), reusing
  the same OpenRouter client machinery.
- `CreateInstanceNode::run` tries the LLM suggestion first (best-effort — a failure here
  must never block instance creation): if the conductor is enabled, ask for a slug: run
  it through the *same* `slugify()`/length-clamp safety net as the deterministic path
  (never trust raw LLM output as a k8s name unsanitized), and use it if non-empty;
  otherwise fall straight to the existing deterministic `instance_name()` fallback — so
  this degrades gracefully with zero conductor configured, exactly like today.
- Log which path was used (`llm_slug` vs `deterministic_slug`) in the action log for
  visibility/debugging, since this adds a small amount of nondeterminism to instance
  creation.

## New ask B: bring back a Settings UI — but only for model selection, not API keys

Per the explicit walk-back: keep `OPENROUTER_API_KEY` env-var-only (no key management in
the UI — that part stays removed), but add back a Settings dialog scoped to **which
model** is used, for two separate things:

1. **The conductor's own model** (`OPENROUTER_MODEL` env var today, default
   `openai/gpt-5.6-sol`) — the model that makes wait/send_message/mark_done/etc
   decisions each heartbeat tick.
2. **The model used for vape instances this bot spawns** — `JobConfig` (the body sent to
   `POST /api/v1/instances`) needs a new `model` field (confirmed real via this
   environment's own `BILDA_AUTO_JOB` env var, which has a sibling `model`/`effort`
   shape) alongside the existing `prompt`/`harness` fields. Default: `openai/gpt-5.6-sol`
   (mirrors this environment's own default install convention, e.g.
   `BILDA_INITIAL_MODEL`/`PIDA_MODEL` env vars already seen in this pod's environment).

Storage: back in `settings.json` via the `Store::get_setting`/`set_setting` primitives
that were kept (not deleted) in the sqlite-removal pass — just two keys this time,
`conductor_model` and `instance_model`, no API-key fields at all. Precedence: DB
setting → env var (`OPENROUTER_MODEL` / a new `MAZZ_FLUX_INSTANCE_MODEL`) → hardcoded
default `openai/gpt-5.6-sol`.

- `src/conductor.rs`: `OpenRouterClient::new()`'s `model` resolution needs to become
  async (DB-aware) OR `Conductor::from_env()` gets a sibling `Conductor::from_sources(&Store)`
  reintroduced — **but scoped to model only**, no key logic. Simplest: keep API key
  strictly env-only (`OPENROUTER_API_KEY`), but resolve the *model string* fresh each
  tick from `Store` with env fallback, same pattern as the old settings code minus the
  key half.
- `src/heartbeat.rs`: `CreateInstanceNode` needs the resolved instance-model string —
  threaded in similarly (read from `Store` fresh per creation, falls back to
  `MAZZ_FLUX_INSTANCE_MODEL` env var, then the hardcoded default).
- `src/models.rs`: `JobConfig` gains `#[serde(skip_serializing_if = "Option::is_none")]
  pub model: Option<String>`.
- `src/api.rs`: reintroduce a **minimal** settings endpoint —
  `GET /api/settings` returns `{conductor_model, instance_model}` (both resolved values,
  no secrets involved so no masking needed), `POST /api/settings` accepts
  `{conductor_model?, instance_model?}` and writes through `Store::set_setting`.
- `static/index.html`/`app.js`: reintroduce a **much smaller** Settings dialog — two
  labeled text inputs (Conductor model, Instance model), each with the current effective
  value as placeholder, Save button. No API-key fields, no fieldsets-per-backend, no
  clear-key buttons — this is a 2-field form now.

## Execution order

1. `src/vape_client.rs`: `rename_instance`.
2. `src/api.rs` + `src/main.rs`: `rename_instance` handler + route (unblocks the build).
3. `static/project.html`/`project.js`: instance links (from `GET /api/instances/{id}`'s
   `urls[]` + a vape-dashboard link) and the rename inline-form.
4. `src/models.rs`: `JobConfig.model`.
5. `src/conductor.rs`: model-only settings resolution (`resolve_model(store, db_key,
   env_key, default) -> String`, sync-free helper reused by both conductor-model and
   instance-model lookups).
6. `src/heartbeat.rs`: `CreateInstanceNode` reads instance-model from `Store` + env,
   passes into `JobConfig.model`; `Conductor::from_env()` callers become
   `Conductor::from_sources(&Store)` again (model-only now, key still env-only inside
   that fn).
7. LLM-assisted slug: `Conductor::suggest_instance_slug`, wired into
   `CreateInstanceNode::run` with the deterministic path as a hard fallback.
8. `src/api.rs`/`src/main.rs`: minimal 2-field settings endpoints.
9. `static/index.html`/`app.js`: minimal 2-field Settings dialog.
10. Build, fix errors, `cargo test` (add coverage for the new `JobConfig.model`
    serialization and the LLM-slug-falls-back-safely path using a mock/stubbed
    conductor, mirroring the existing mock-server pattern from earlier PLAN.md history).
11. Manual smoke test against the live proc: change instance model via Settings, create
    a throwaway project, confirm the create-instance call carries the new model
    (check action log detail or a dry-run log line); confirm rename + instance links
    work against the real live project/instance already in this session.
12. Commit + push to `main`.

## Executed (2026-09-03) — UI overhaul, settings simplification, naming, links, rename, multi-task human tasks

Full session summary of everything landed in this pass:

- **UI overhaul**: new `static/icons.svg` sprite (SVG `<symbol>` defs, ~20 stroke icons,
  no emoji anywhere), full `style.css` rewrite with CSS-variable design tokens
  (spacing/type/radius scales), light+dark theme via `data-theme` on `<html>` +
  `prefers-color-scheme` fallback, `static/theme.js` toggle shared by both pages, real
  button system (`.btn`/`.btn-secondary`/`.btn-danger`/`.btn-icon`/`.btn-group`) replacing
  plain links and flat `button.mini`, dialogs/tabs/tables/lists restyled to match.
  Human tasks section moved below Projects on the dashboard per request.
- **Settings simplified to model-only**: removed all API-key management from the UI —
  `OPENROUTER_API_KEY` is env-var-only again, `src/anthropic_client.rs` deleted entirely,
  `Conductor` collapsed to OpenRouter-only. Settings dialog now has exactly two fields
  (Conductor model, Instance model), backed by `settings.json` via the `Store::get_setting`/
  `set_setting` primitives that were kept from the earlier sqlite-removal pass.
  `conductor::resolve_model` is the shared DB-then-env-then-default lookup used by both.
- **`agent_prompts/validation.md`**: new `Store::read_agent_prompt`, read fresh every
  tick by `DecideNode` and appended to `SYSTEM_PROMPT` (framed as extra criteria the
  conductor must satisfy before choosing `mark_done`) when the file exists and isn't
  empty. Edited directly through the existing Files tab — no new write endpoint needed.
- **Human-readable + LLM-assisted instance names**: `heartbeat::slugify`/`instance_name`
  give a deterministic `{project-name-slug}-{short-id}` scheme; `CreateInstanceNode` first
  tries `Conductor::suggest_instance_slug` (one cheap LLM call framed as "name this k8s
  resource"), runs whatever comes back through the same `slugify` safety net, and only
  falls back to the deterministic scheme if the conductor is disabled/fails/returns
  nothing usable — verified live: a test project "Fix login bug" got instance name
  `fix-login-bug-506e9e` via the LLM path (logged as `instance_name_chosen` with
  `source: "llm_slug"`), and a real spawned instance carried that name in the live vape
  API response.
- **Vape instance rename**: `VapeClient::rename_instance` (`POST
  /api/v1/instances/{id}/rename`) is now live-fired for the first time in this repo's
  history — confirmed working end to end (renamed a real running instance, verified via
  a follow-up `GET /api/v1/instances/{id}`). Exposed as `POST
  /api/projects/{id}/instance/rename` + an inline rename form on the project page.
- **Instance links on project page**: `GET /api/instances/{id}`'s existing `urls[]` field
  plus a constructed `https://vape.stable.dexus.io/instances/{id}` dashboard link render
  as a list under the Instance row — verified against the real `test-chat` project,
  showing the vape dashboard link plus all ~15 real app endpoint URLs for that instance.
- **Heartbeat interval as seconds/minutes/hours**: the interval editor on the project page
  gained a unit `<select>` alongside the number input; storage is still always seconds
  server-side (`Project.heartbeat_interval_secs` unchanged), the UI just displays/accepts
  whichever unit divides evenly (e.g. 900s shows as "15 minutes").
- **Multi-task `create_human_task`**: `Decision` gained a `tasks: Option<Vec<String>>`
  field alongside the existing single-`message` shape. When the conductor sees a pida
  reply enumerating several distinct blockers (e.g. a numbered list), it can now emit
  `{"action": "create_human_task", "tasks": [...]}` and each entry becomes its own
  independently-resolvable `HumanTask` row instead of one monolithic paragraph. The
  legacy single-`message` shape still works unchanged (falls back to a one-element task
  list). System prompt updated to explicitly instruct the split. Covered by both a new
  `spice_framework` eval case (`tests/heartbeat_conductor.rs`) and plain unit tests
  (`tests/heartbeat_decisions.rs`).
- New test files: `tests/heartbeat_decisions.rs` (slugify/instance_name/multi-task
  parsing, renamed from `heartbeat_naming.rs`), `tests/models_serde.rs` (`JobConfig.model`
  serialization). All 12 tests across the full suite pass.
- Smoke-tested live against the running proc on this instance: settings get/update,
  project creation → LLM-assisted naming → real instance creation → rename → cleanup
  (stopped + deleted the throwaway instance), `agent_prompts/validation.md` write/read
  round-trip via the Files API, dark/light theme toggle, and the redesigned project page
  (screenshots confirmed instance links, rename form, and the minutes-denominated
  heartbeat interval all render correctly).
