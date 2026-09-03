# Plan: goal/heartbeat prompts (LLM-composed) + persistent memory/compaction

Two related features landing together — the memory mechanism directly informs how the
heartbeat prompt gets composed, so they're one pass.

## Part A — Goal prompt + heartbeat prompt, both LLM-composed

1. **Goal prompt** (existing `goal` field, kept as the backend name; UI relabels it
   "Goal prompt") — the overall objective. Direction for the conductor to **compose the
   initial session prompt in its own words** when creating the vape instance — never
   sent to the instance verbatim.
2. **Heartbeat prompt** (new, optional field, `heartbeat_prompt`) — direction for what
   each periodic check-in should focus on. When the conductor chooses `send_message`, it
   composes that message **in its own words**, using `heartbeat_prompt` as guidance for
   *what this check-in is about*, while always keeping the overall `goal` in mind (fed
   into every tick's context regardless of which prompt is set) so a narrow per-tick
   focus never loses sight of the big picture.

Both are plain user-authored text, editable on the project detail page and settable at
creation time. Neither is ever sent to pida verbatim — the conductor always composes the
actual outgoing text, informed by memory (Part B) + goal + heartbeat_prompt.

### Data model
- `models::Project` / `CreateProjectRequest`: add `heartbeat_prompt: Option<String>`
  (`#[serde(default)]`, same pattern as `heartbeat_interval_secs`'s serde default).
- `Store`: `set_goal(id, &str)`, `set_heartbeat_prompt(id, Option<&str>)`.

### Conductor composition
- `Conductor::compose_initial_prompt(project_name, goal) -> Result<String>` — system
  prompt: "You are opening a new coding-agent session. Write the first message to the
  agent, in your own words, directing it toward this goal. Be concrete and actionable."
  Fallback (conductor disabled, call fails, empty response): send `goal` verbatim —
  today's behavior, never blocks instance creation.
- Heartbeat steering stays the existing `DecideNode` → `send_message` path — the change
  is prompt engineering: feed `heartbeat_prompt` (if set) + `goal` (always) + `memory`
  (Part B) into the per-tick context, and instruct the system prompt to compose
  `send_message` text itself rather than restate any field verbatim.

## Part B — Persistent memory / compaction

The conductor's own LLM call is already stateless per tick (fresh system+user each
time, no conversation history retained in the call itself) — the actual risk is that the
*only* cross-tick continuity today is `recent_messages` (last 6 raw pida messages,
bounded but low-signal) and the unbounded `project_notes` list (append-only, never
summarized). Over many ticks there's no compact, coherent "what's the state of this
project" the conductor can use without either re-reading raw history or nothing at all.

Fix: a single **memory file per project**, compacted (overwritten, not appended) each
tick — the conductor's own working summary of everything worth remembering, written
fresh every time it gets a response from pida.

- **Storage**: `Store::read_memory(project_id) -> Result<Option<String>>` /
  `write_memory(project_id, content) -> Result<()>`, file `memory/{project_id}.md`.
  Unlike `notes/` (append-only, timestamped, historical record) this is a single
  mutable file — each write *replaces* the previous content. Genuinely a new file
  category, not a repurposing of `project_notes`.
- **`Decision` gains `memory: Option<String>`** — the conductor's fully rewritten,
  self-contained compacted summary for this tick, replacing whatever was there before.
  Distinct from `add_note` (still a separate, append-only historical log — notes are
  "worth keeping a permanent record of," memory is "worth remembering right now").
- **`DecideNode::run`**: reads `memory/{project_id}.md` fresh every tick (same
  read-fresh-every-tick pattern as `agent_prompts/validation.md`), includes it in the
  `user` JSON blob as `"memory"` alongside `goal`, `heartbeat_prompt`, `pida_status`,
  `recent_messages`.
- **System prompt** gains: "You are given `memory` — your own compacted summary from
  the previous tick (empty on the first tick). Each response, include a `memory` field
  with a fully rewritten, self-contained summary of everything worth remembering about
  this project's progress, decisions, and state — this REPLACES the previous memory
  entirely, so carry forward anything still relevant rather than assuming it persists on
  its own. Keep it concise; this is what lets you avoid re-reading full history every
  tick. When composing `send_message`, use this memory (plus `goal` and
  `heartbeat_prompt`) to write a coherent, context-aware check-in in your own words."
- **`persist_tick`**: if `final_state.decision` (or a new `HeartbeatState.memory` update
  slot, following the same `Update`/reducer pattern as `add_note`) carries a non-empty
  `memory`, write it via `Store::write_memory` — independent of which `action` was taken,
  same as `add_note` today.
- **Not fed into `CreateInstanceNode`** — memory only exists once there's been at least
  one tick with a live instance; the very first tick's `create_instance` path has no
  memory yet, which is expected (`Option<String>` = `None` → omit from context / compose
  the initial prompt from `goal` alone, per Part A).

## `heartbeat.rs` wiring (combined)

- `HeartbeatState` gains `heartbeat_prompt: Option<String>` and `memory: Option<String>`
  (the latter populated by `FetchStatusNode` or a new small read step before `decide`,
  read from `Store::read_memory`).
- `Update` gains a `MemoryUpdated(String)` variant (or reuse `Decided` and extract
  `memory` out of it in `persist_tick` — simpler, avoids a new graph edge; decide during
  implementation based on which reads cleaner against the existing reducer).
- `CreateInstanceNode::run`: best-effort `conductor.compose_initial_prompt(...)` with
  verbatim-goal fallback, as in Part A. Logs `instance_prompt_composed` with the source
  (`llm_composed` vs `verbatim_goal_fallback`), mirroring the existing
  `instance_name_chosen` logging pattern.
- `DecideNode::run`: reads memory, builds `user` blob with
  `{goal, heartbeat_prompt, memory, pida_status, recent_messages}`, uses the updated
  `SYSTEM_PROMPT`.
- `persist_tick`: writes `final_state`'s `memory` (if present and non-empty) via
  `Store::write_memory`, unconditionally of which `TickOutcome` variant fired — same
  treatment as `add_note`.

## API + routes

- `POST /api/projects/{id}/goal` — `{"goal": "..."}`.
- `POST /api/projects/{id}/heartbeat-prompt` — `{"heartbeat_prompt": "..."}` (empty
  clears to `None`).
- `CreateProjectRequest` gets the new optional field so it can be set at creation time.
- `GET /api/projects/{id}/memory` — read-only surface for the UI to show current memory
  (no write endpoint needed beyond what the conductor itself writes each tick — this is
  conductor-authored, not user-authored, unlike `agent_prompts/`).

## Frontend

- **Create-project dialog**: "Goal" textarea relabeled "Goal prompt" + one-line
  explainer ("direction for the agent's own opening message — not sent verbatim"). New
  optional "Heartbeat prompt" textarea, same explainer style.
- **Project detail page**: both prompts become editable inline (textarea + Save,
  matching the existing heartbeat-interval/instance-rename inline-form pattern) instead
  of the current read-only `<p class="sub" id="project-goal">`. New read-only "Memory"
  section (like Notes, but single current value, not a list) showing the conductor's
  latest compacted summary — visibility into what it's "remembering," not editable by a
  human (it's the conductor's own scratch space, overwritten every tick regardless).

## Execution order

1. `src/models.rs`: `heartbeat_prompt` on `Project`/`CreateProjectRequest`.
2. `src/store.rs`: `set_goal`, `set_heartbeat_prompt`, `read_memory`, `write_memory`;
   thread `heartbeat_prompt` through `create_project`.
3. `src/conductor.rs`: `compose_initial_prompt`; update `SYSTEM_PROMPT`-adjacent doc
   comments only where relevant (the actual system prompt string lives in heartbeat.rs).
4. `src/heartbeat.rs`: `HeartbeatState.heartbeat_prompt`/`.memory`; memory read step
   before `decide`; `Decision.memory` field; updated `SYSTEM_PROMPT`; `CreateInstanceNode`
   LLM-compose + fallback + logging; `persist_tick` writes memory unconditionally.
5. `src/api.rs` + `src/main.rs`: goal/heartbeat-prompt update routes, memory read route.
6. `static/index.html`/`app.js`: create-dialog relabel + new field.
7. `static/project.html`/`project.js`: editable goal/heartbeat-prompt controls, read-only
   Memory section.
8. Build, `cargo test`:
   - `heartbeat_prompt`/`memory` optional-field serde defaults.
   - `compose_initial_prompt` fallback-on-disabled-conductor (no network needed).
   - `parse_conductor_response` handling a `memory` field (round-trips into `Decision`).
   - `Store::read_memory`/`write_memory` round-trip + overwrite-not-append semantics
     (write twice, confirm second write fully replaces the first, not appends).
9. Manual smoke test against the live proc: create a project with both prompts set,
   confirm the spawned instance's first session message is LLM-composed; force a
   heartbeat twice in a row and confirm memory persists/updates between ticks and the
   second tick's `send_message` (if any) reads as informed by the first tick's memory.
10. Commit + push to `main`.

## Executed (2026-09-03) — LLM-composed goal/heartbeat prompts + compacted memory

Landed exactly per the clarified requirements from the Q&A:

- **`Project.heartbeat_prompt: Option<String>`** (serde-default `None` for old project
  files), editable via `POST /api/projects/{id}/heartbeat-prompt` (empty string clears
  it), settable at creation time via `CreateProjectRequest`. `goal` gained a matching
  `POST /api/projects/{id}/goal` editor — both now editable inline on the project detail
  page's new "Prompts" section (textarea + Save, same pattern as the existing
  interval/rename inline forms), and both are exposed at project-creation time in the
  dashboard's New Project dialog with explainer text under each field.
- **Both prompts are LLM-composed, never sent verbatim**:
  - `Conductor::compose_initial_prompt(project_name, goal)` — one best-effort call asking
    the conductor to write the actual first message to a new pida session "in its own
    words, directing it toward this goal, concrete and actionable." `CreateInstanceNode`
    uses this with a hard fallback to `goal` verbatim on any failure/empty response
    (disabled conductor, network error, etc) \u2014 verified live: a test project's spawned
    instance's first session message was a genuinely composed, actionable paragraph, not
    the raw goal text.
  - Heartbeat steering (`send_message`) required no new conductor method \u2014 just prompt
    engineering: `heartbeat_prompt` (if set) + `goal` (always) + `memory` (see below) are
    fed into `DecideNode`'s `user` JSON blob, and `SYSTEM_PROMPT` now explicitly instructs
    composing `send_message` text in its own words using that context, never restating
    any field verbatim. Verified live: a project with both prompts set produced a
    `pida_send` whose message was clearly informed by (not a copy of) `heartbeat_prompt`
    and the agent's actual reported status.
- **Persistent memory / compaction**: new `memory/{project_id}.md` per-project file
  (`Store::read_memory`/`write_memory`) that the conductor **fully rewrites, never
  appends to**, every tick via a new `Decision.memory: Option<String>` field.
  `DecideNode` reads it fresh (same read-fresh-every-tick pattern as
  `agent_prompts/validation.md`) and includes it in the per-tick context; `persist_tick`
  writes `final_state.new_memory` unconditionally of which action fired, mirroring
  `add_note`'s treatment but as a full overwrite instead of an append. `GET
  /api/projects/{id}/memory` (read-only \u2014 conductor-authored, not user-editable) backs a
  new read-only "Memory" box on the project detail page's Prompts section. Verified
  live across two consecutive forced heartbeat ticks against a real instance: memory was
  written after the first successful tick, and the second tick's memory was a fresh,
  coherent rewrite that correctly carried forward the still-relevant state (not a
  restart from scratch, not an append).
- New/updated tests (17 total across the suite, all passing): `heartbeat_decisions.rs`
  gained `memory_field_round_trips`/`memory_field_absent_is_none`; `store_smoke.rs`
  gained `memory_overwrites_not_appends` and `goal_and_heartbeat_prompt_editable`;
  `models_serde.rs` gained `project_deserializes_without_heartbeat_prompt_field`
  (old-format JSON compat); `heartbeat_conductor.rs` gained a new `spice_framework` eval
  case (`wait-with-memory`) exercising the `memory` field through the same
  tool-call-shaped adapter as the rest of that suite, per the explicit ask to use spice
  for this kind of behavioral coverage.
- Smoke-tested live end-to-end on the running proc: created a project with both prompts
  set, forced three consecutive heartbeats against a real spawned instance (waiting out
  real pod-boot time with bounded polls, not blind sleeps), confirmed the LLM-composed
  initial session prompt, the context-aware `send_message`, and two-tick memory
  continuity, then cleaned up (paused + deleted the project, stopped + deleted both
  vape instances created during testing \u2014 a benign race between the periodic scan loop
  and a manually forced tick created two instances for one project; pre-existing
  behavior, not caused by this change, and not touched in this pass).
