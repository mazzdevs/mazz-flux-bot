# Plan: Archetypes — a new primitive, third dashboard tab

## What it is

**Agent archetype**: a reusable persona/role definition — name, description, preferred
model — stored as a single markdown file. Not yet wired into project creation/heartbeat
behavior in this pass (that's a natural follow-up: "run this project as a Reviewer" or
similar) — this pass is the primitive itself: storage, API, and a new dashboard tab to
browse/create/edit them, plus five starter archetypes.

## Storage format

One file per archetype: `archetypes/{slug}.md`, front-matter + body, mirroring the
existing `<!-- created_at: ... -->` convention already used for project notes (simple,
human-readable, editable directly through the Files tab too):

```markdown
<!-- name: Coder -->
<!-- preferred_model: openai/gpt-5.6-sol-pro -->
Implements features and fixes with working, tested code. Reads existing conventions
before writing new code, keeps changes scoped to what was asked, and verifies its own
work runs before calling something done.
```

- `name`: display name (also used to derive the filename slug on create).
- `preferred_model`: an OpenRouter model id string. **Default `openai/gpt-5.6-sol-pro`**
  (confirmed live on this environment's OpenRouter account) if omitted.
- `description`: everything after the front-matter comment lines — free text, no length
  cap, rendered as-is (same "plain text in a `<pre>`" treatment as notes today, no
  markdown-to-HTML rendering added).
- Filename is a slug of `name` (reuse `heartbeat::slugify`, already exists) — human
  readable in the Files tab, e.g. `archetypes/coder.md`, `archetypes/researcher.md`.

## Backend

- `models.rs`: new `Archetype { slug: String, name: String, description: String,
  preferred_model: String }`.
- `store.rs`: new methods, same shape as the existing project-notes/agent-prompts
  primitives:
  - `list_archetypes() -> Vec<Archetype>` — reads every `archetypes/*.md`, parses
    front-matter, sorts by name.
  - `get_archetype(slug) -> Option<Archetype>`.
  - `create_archetype(name, description, preferred_model) -> Archetype` — slugifies
    `name`, writes the file, errors on slug collision (don't silently overwrite).
  - `update_archetype(slug, name?, description?, preferred_model?) -> Archetype`.
  - `delete_archetype(slug) -> ()`.
  - A small front-matter parse/serialize helper (two comment lines + body), private to
    `store.rs`, following the existing `split_created_at_comment` pattern already there.
- `api.rs` + `main.rs`: `GET/POST /api/archetypes`, `GET/POST/DELETE
  /api/archetypes/{slug}`.
- Seed the five starter archetypes on first boot if `archetypes/` is empty (idempotent —
  checked at `Store::open` time or lazily on first `list_archetypes` call, whichever
  reads cleaner) so a fresh install isn't an empty tab: **Coder, Researcher, Planner,
  Reviewer, Designer** (content drafted below).

## Frontend — new "Archetypes" tab

Between Dashboard and Files in the tab strip (`index.html`'s `.tab-strip` +
`.tab-panel`s, same pattern as the existing two tabs). List/tile view toggle mirroring
the existing Projects section (reuse the same view-toggle localStorage pattern, separate
key). Each card/row shows name, preferred model, description preview, edit/delete
actions. A "New archetype" button (styled like the existing prominent blue "New
project" button) opens a small dialog: Name, Preferred model (text input, placeholder
`openai/gpt-5.6-sol-pro`), Description (textarea).

New `static/archetypes.js` (kept separate, same reasoning as `files.js`) for
load/create/edit/delete + the tab's `onShow` lazy-load hook (same lazy-load pattern
`files.js` already uses via `window.filesTab`).

## The five starter archetypes

1. **Coder** — implements features/fixes with working, tested code; reads existing
   conventions first; keeps changes scoped; verifies before calling done.
   `openai/gpt-5.6-sol-pro`.
2. **Researcher** — investigates and reports findings without changing code; cites
   sources/file locations for every claim; explicitly separates "confirmed" from
   "inferred"; produces a structured writeup, not just an answer.
   `openai/gpt-5.6-sol-pro`.
3. **Planner** — breaks a goal into a concrete, ordered, reviewable plan before any
   execution; calls out risks/unknowns/decisions that need a human; doesn't start
   implementing without explicit go-ahead.
   `openai/gpt-5.6-sol-pro`.
4. **Reviewer** — critiques existing work (code, PRs, plans) for correctness,
   completeness, and adherence to stated requirements; flags what's wrong or risky
   without necessarily fixing it; prioritizes findings by severity.
   `openai/gpt-5.6-sol-pro`.
5. **Designer** — focuses on UX/UI and information architecture; proposes concrete,
   comparable options (not just one answer) with tradeoffs; considers accessibility and
   consistency with existing patterns before novelty.
   `openai/gpt-5.6-sol-pro`.

(Full description text drafted directly into each seed file during implementation —
above is the gist, not verbatim.)

## Execution order

1. `src/models.rs`: `Archetype` struct.
2. `src/store.rs`: front-matter parse/serialize helper + list/get/create/update/delete +
   first-boot seeding of the five starters.
3. `src/api.rs` + `src/main.rs`: routes.
4. `static/index.html`: third tab + panel markup + create-archetype dialog.
5. `static/archetypes.js`: list/tile rendering, create/edit/delete, lazy-load hook.
6. `static/style.css`: minor additions if the existing tile/dialog/button classes don't
   already cover everything (they mostly should — reuse, don't reinvent).
7. Build, add tests (`store_smoke.rs`-style: create/list/get/update/delete round-trip,
   front-matter parse edge cases, slug-collision rejection, default-model-when-omitted).
8. Manual smoke test against the live proc: confirm the five seeds appear on first load
   (or after clearing `archetypes/` in a scratch data dir), create/edit/delete one via
   the UI, confirm files land correctly under `archetypes/` and are editable via the
   existing Files tab too (same directory, no special-casing needed there).
9. Commit + push to `main`.

## Conductor awareness (in scope for this pass)

The conductor needs to know archetypes exist so it can reference them when directing
sub-agent spin-up — e.g. a goal/plan that says "spin up a sub_agent to validate this"
should let the conductor tell the pida instance *which* archetype to use and what that
archetype means, not just say "sub_agent" generically.

- `DecideNode::run` (heartbeat.rs) reads `Store::list_archetypes()` fresh every tick
  (same read-fresh pattern as `agent_prompts/validation.md` and `memory`) and includes a
  compact catalog in the `user` JSON blob: `"archetypes": [{"name", "description",
  "preferred_model"}, ...]`. Cheap — just the list, not full file contents beyond what's
  already parsed.
- `SYSTEM_PROMPT` gains guidance: "You have a catalog of `archetypes` — reusable
  agent personas (name, description, preferred model). When your goal, heartbeat_prompt,
  or memory references spinning up a sub-agent for a specific kind of work (e.g.
  'spin up a sub_agent to validate the implementation', 'resolve this nitpick with a
  sub_agent'), pick the archetype whose description best matches that kind of work and
  tell the pida instance, in your `send_message` text, to use that archetype — name it
  explicitly and summarize its description/preferred model so pida has enough to act on
  without needing to look it up itself. If no archetype fits, proceed without one."
- This is deliberately *advisory*, not a hard mechanism: the conductor composes a
  message that *tells* pida which archetype/persona to adopt for a sub-agent (pida's own
  actual subagent-spawning tool call is outside this bot's control — mazz-flux-bot only
  ever talks to pida over chat). No new vape API call, no new action type needed for
  this pass — it rides entirely on the existing `send_message` path now being
  archetype-aware.
- Also feed the archetype catalog into `Conductor::compose_initial_prompt` (used by
  `CreateInstanceNode` when spawning the project's own instance) — if the goal itself
  implies a primary role (e.g. a goal that's fundamentally a research task), the initial
  session prompt can open by establishing that persona too. Same advisory framing.

## Explicitly out of scope for this pass

- Auto-selecting `preferred_model` to override `instance_model` for a given project
  based on its dominant archetype — the conductor can *mention* a preferred model in
  its message today, but actually switching the project's own instance model based on
  archetype inference is a separate, larger change (would need to happen at
  create-instance time, before any archetype-relevant context exists yet) and isn't
  part of this pass.

## Executed (2026-09-03) — Archetypes primitive, third dashboard tab, conductor awareness

- **New primitive: `Archetype`** (`models.rs`) \u2014 name, description, preferred model,
  stored one-per-file at `archetypes/{slug}.md` with a 2-line comment front-matter
  (`<!-- name: ... -->`, `<!-- preferred_model: ... -->`) followed by the description as
  plain body text, mirroring the existing `<!-- created_at: ... -->` convention already
  used for project notes. Default `preferred_model` is `openai/gpt-5.6-sol-pro` ("Sol
  Pro" \u2014 confirmed live on this environment's OpenRouter account).
- `Store` gained full CRUD (`list/get/create/update/delete_archetype`) plus
  `seed_default_archetypes()`, called unconditionally on every boot but **idempotent
  per-slug** (not gated on whether `archetypes/` exists as a whole) \u2014 per explicit
  follow-up request: editing a seeded archetype and restarting leaves the edit intact;
  fully deleting one and restarting recreates it, since the guard is "does an archetype
  with this exact slug exist," checked independently for each of the five starters.
  Verified live against the running proc: edited `reviewer`'s description, deleted
  `planner` entirely, restarted \u2014 `reviewer` kept the edit, `planner` came back.
- **Five starter archetypes**: Coder, Researcher, Planner, Reviewer, Designer, each with
  a real, specific description (not placeholder text) \u2014 seeded automatically on first
  boot into any fresh `MAZZ_FLUX_DATA_DIR`.
- **Conductor awareness (the actual point of this primitive)**: the full archetype
  catalog is read fresh every heartbeat tick and fed into both decision-making paths:
  - `DecideNode`'s `user` context blob now includes `archetypes` alongside
    `goal`/`heartbeat_prompt`/`memory`/status \u2014 available for both the `send_message`
    heartbeat-steering composition AND informing `create_human_task`/`mark_done` calls,
    per explicit follow-up that archetypes needed to be available for heartbeat-prompt
    composition too (not just initial-prompt composition).
  - `Conductor::compose_initial_prompt` gained an optional `archetypes_json` parameter,
    threaded from `CreateInstanceNode` \u2014 the initial session prompt can establish a
    primary persona if the goal clearly implies one.
  - `SYSTEM_PROMPT` updated: when a goal/heartbeat_prompt/memory implies spinning up a
    sub-agent for specific work, pick the best-matching archetype and name it explicitly
    in the composed `send_message` text (with a summary of its description/model) so
    pida has enough to act on. Deliberately advisory, not a new mechanism \u2014 this bot
    only ever talks to pida over chat, so it can't directly invoke pida's own
    sub-agent-spawning tool itself; it can only *tell* pida which persona to use.
- **New "Archetypes" tab** between Dashboard and Files (`index.html`, new
  `static/archetypes.js`, new `icon-users` SVG symbol) \u2014 list/tile view toggle (own
  localStorage key, same pattern as Projects'), a "New archetype" dialog
  (name/preferred-model/description), inline edit (reuses the same dialog, pre-filled)
  and delete. Archetype `.md` files are also directly browsable/editable through the
  existing Files tab \u2014 no special-casing needed there, same directory.
- New tests: `archetype_crud_and_defaults` (create/get/update/delete round-trip,
  slug-collision rejection, default-model-when-omitted) and
  `archetype_seeding_is_idempotent_per_slug` (edit-survives-reseed,
  delete-triggers-recreate). 20 tests total, all passing.
- Verified live end-to-end: all 5 defaults seeded on a fresh boot, full CRUD via the API,
  the Files tab correctly listing all 5 `archetypes/*.md` files, and the tab itself
  rendering correctly in the browser (screenshot-confirmed).

## Executed (2026-09-03) — `worker` label on every spawned instance

- `CreateInstanceRequest` gained a `labels: HashMap<String, String>` field (documented
  as a real field in this repo's own earlier research but never actually wired into the
  struct until now). `CreateInstanceNode` sets `{"worker": "true"}` on every instance
  this bot creates, so it's identifiable in the vape dashboard/instance list as
  bot-managed rather than a person's own interactive session.
- Verified live: created a throwaway project, forced instance creation, confirmed via
  `GET /api/v1/instances/{id}` that the real instance carries `vape.io/worker: true`
  (vape-manager namespaces labels under `vape.io/`, consistent with its other
  auto-applied labels like `vape.io/owner`/`vape.io/constellation`) \u2014 then cleaned up
  (paused/deleted the project, stopped/deleted the instance).
- 20 tests still pass (`models_serde.rs`'s `job_config_model_field_serializes_when_present`
  test updated to also assert the label round-trips).
