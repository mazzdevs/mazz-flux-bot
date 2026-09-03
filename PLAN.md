# mazz-flux-bot — plan

A Rust tool that manages VAPE ("flux") dev instances, with a local HTML frontend and a
local SQLite cache/history DB. Evolved from "cadmium vape but in Rust" into a small
autonomous orchestrator: you create a **Project** with a natural-language **goal**, and a
heartbeat loop drives one `pida`-harness vape instance toward that goal — creating it,
polling its status/session, and (when `ANTHROPIC_API_KEY` is set) asking Claude what to
do next each tick: wait, steer with a message, or declare the goal done/stuck.

**Status: scaffolded and compiles/runs (2026-09-03).** See "Built" section near the
bottom for what's real vs. still dry-run-only.

## Confirmed facts (verified live against the real API today, 2026-09-02)

- **Base URL:** `https://vape.stable.dexus.io` (default baked into cadmium; override via
  `CADMIUM_VAPE_URL`). Requires being on Cloudflare WARP.
- **Auth:** `Authorization: Bearer <token>` where `<token>` = `gh auth token` (GitHub CLI
  token). Verified: `GET /api/v1/me` returns `{"username":"mazzdevs", ...}` with this token.
  No separate credential storage needed — shell out to `gh auth token` at request time.
- **Reads require no extra scopes** beyond what `gh auth token` already has (repo, read:org, gist).
- Full REST surface documented in `~/dutchie/vape/CLAUDE.md` (`### REST API v1` section) —
  this is the source of truth for endpoint list, kept in the vape repo itself.

### Verified-live endpoints (curl'd successfully, 2026-09-02/03, on WARP)

| Method | Path | Notes |
|---|---|---|
| GET | `/api/v1/me` | current user |
| GET | `/api/v1/instances` | list my instances — array with id, name, status, owner, constellation, created_at, urls, pod_ip, ready, labels, bilda/pr info |
| GET | `/api/v1/instances/{id}` | single instance detail (rich: sidecars, cloudbeaver creds, pr, context_summary) |
| GET | `/api/v1/instances/{id}/agent-status` | **unified** status: `{"state","active_harness","harnesses":{"<name>":{"state","model",...}}}` — tells you which harness (`bilda` or `pida`) is live, no need to guess |
| GET | `/api/v1/instances/{id}/{harness}/api/status` | harness-specific detailed status (ready, isStreaming, pendingAsk, planMode, turnLiveness, model, autoMode, ...) |
| GET | `/api/v1/instances/{id}/{harness}/api/session` | `{"messages":[...], "todos":[...]}` — full chat transcript, confirmed working (returned real conversation history) |
| GET | `/api/v1/constellations` | list constellations: id, name, description, repos, resources, endpoints, procs, prompt |
| GET | `/api/v1/constellations/{id}` | single constellation detail (404 with `{"error":{"message","code"}}` shape if unknown id) |

**Key correction from the first pass:** the chat proxy path is **harness-specific**, not
always `/bilda/...`. An instance can run harness `bilda` *or* `pida` (`cadmium vape create
--harness bilda|pida`) and the proxy segment must match: `/api/v1/instances/{id}/bilda/api/...`
for bilda instances, `/api/v1/instances/{id}/pida/api/...` for pida ones. That's why the
first attempt 404'd — the test instance (`e2nwvslk`) turned out to be running `pida`, and
`/pida/api/status` + `/pida/api/session` both returned 200 once corrected. **Always call
`/agent-status` first to learn `active_harness`, then build the harness-specific path
from that** — don't hardcode `bilda`.

### Endpoints from cadmium `--help` / vape source structs — path confirmed via binary
### string extraction, body shape from source, NOT live-fired (mutating, so not tested
### during research to avoid side effects on real instances)

| Method | Path | Purpose | Confidence |
|---|---|---|---|
| POST | `/api/v1/instances` | create instance | body shape below, from `internal/handlers/api.go: CreateInstanceRequest` — high confidence, not fired live |
| POST | `/api/v1/instances/{id}/stop` | stop | path string extracted from cadmium binary itself — high confidence |
| POST | `/api/v1/instances/{id}/start` | start | same — high confidence |
| DELETE | `/api/v1/instances/{id}` | delete | same pattern as GET, standard REST — high confidence |
| POST | `/api/v1/instances/{id}/rename` | rename display name | path string extracted from binary — high confidence |
| POST | `/api/v1/instances/{id}/{harness}/api/chat` | send a chat message, body `{"message": "...", "files"?: [...]}` | path pattern confirmed generic (`/api/v1/instances/%s/%s/api%s` template found in binary strings); body shape from `bilda/server/index.ts` `POST /api/chat` handler — high confidence, not fired live to avoid injecting a message into a real session |
| POST | `/api/v1/instances/{id}/{harness}/api/answer` | answer a pending question from the agent | same source, not fired |
| POST | `/api/v1/instances/{id}/{harness}/api/plan-approval` | approve/reject a pending plan | same, not fired |
| GET | `/api/v1/instances/{id}/{harness}/api/job/result` | structured result of an autonomous job | GET, safe, just not tried against a completed job yet |
| GET | `/api/v1/instances/{id}/{harness}/api/stream` | SSE — live event stream | matches binary string `/api/v1/instances/%s/%s/api/stream` |

**Create instance request body** (from `internal/handlers/api.go:217`, `CreateInstanceRequest`):

```json
{
  "name": "my-instance",
  "constellation": "back-office",
  "repos": [{"owner": "GetDutchie", "name": "back-office", "branch": "main"}],
  "sidecars": ["postgres", "redis"],
  "labels": {},
  "subdomain": "my-stable-slug",
  "prompt": "markdown handed to CLAUDE.md",
  "ticket": "DEVX-1234"
}
```
Only `name` + `constellation` are required for the basic case; everything else is
`omitempty`. `Job` (autonomous job config), `resources`, `env_vars`, `endpoints`, `procs`
overrides exist too but aren't needed for a v1 create form.

**Fallback if a specific mutating endpoint turns out wrong once tried for real:** shell
out to the `cadmium` binary itself for that one operation (e.g. `cadmium vape send <id>
"<msg>"`) rather than blocking the whole tool on it — everything else (list/status/session
reads, create/lifecycle once confirmed) stays direct HTTP.

## Architecture

- **Language:** Rust.
- **Web server:** `axum`, serving:
  - A small JSON API (`/api/instances`, `/api/instances/:id/start` etc. — mirrors vape's
    own shape so the frontend JS is simple).
  - Static HTML/vanilla-JS frontend (no build step) from `static/` via `tower-http`'s
    `ServeDir`.
- **Local DB:** SQLite via `rusqlite` (or `sqlx` with the `sqlite` feature — decide at
  implementation time; `rusqlite` is simpler for a single-user local tool with no async
  driver needed, but the rest of the app is async/axum so `sqlx` may fit better — lean
  `sqlx` unless it's annoying).
  - **Cache table** `instance_cache`: last-fetched snapshot of `/api/v1/instances` (id,
    name, status, constellation, owner, created_at, urls_json, pod_ip, ready, raw_json,
    fetched_at). Lets the UI render instantly on load before the live fetch resolves, and
    gives an offline/degraded view if the API or WARP tunnel is down.
  - **Action log table** `action_log`: every mutating action taken through the tool
    (action type: create/start/stop/delete/send_message, instance id, timestamp, request
    payload, result/error). Simple audit trail, also handy for "what did I just do".
  - **Bilda messages cache** (once chat endpoints are confirmed): mirror of `/api/session`
    messages per instance, so the frontend can show history without re-fetching everything.
- **Auth:** shell out to `gh auth token` per request — exactly how cadmium itself
  authenticates (verified: no separate credential store, no config file token; `gh auth
  status` on this machine shows an active `gho_...` token with `repo`/`read:org`/`gist`
  scopes, and that token round-tripped fine against `/api/v1/me`). **No `.env` file
  needed** as long as `gh auth login` is already done on the machine running the tool —
  which it is here. Only fall back to an env var (e.g. `MAZZ_FLUX_VAPE_TOKEN`) if `gh` is
  missing or unauthenticated; surface a clear error telling the user to run `gh auth
  login` first rather than silently prompting for a token.
- **HTTP client:** `reqwest`.
- **Base URL override:** support `CADMIUM_VAPE_URL` env var same as cadmium (default
  `https://vape.stable.dexus.io`), so switching to sandbox/dev is a one-line env change,
  no code change.
- **Network dependency:** all of this requires being on Cloudflare WARP (confirmed: calls
  only succeeded once connected). Tool should give a clear error (not a generic timeout)
  when the manager is unreachable — e.g. detect connect-timeout/DNS failure and point at
  `/cf-warp` skill or "connect to WARP" rather than a raw reqwest error.

## Frontend (local HTML)

- Single static page, vanilla JS (`fetch`), talking to the local axum JSON API — not
  directly to vape (keeps the GitHub token server-side only, never shipped to the browser).
- Views:
  1. **Instance list** — table: name, status, constellation, age, ready, urls, PR link if
     present. Actions per row: start / stop / delete / open chat.
  2. **Create instance** — pick constellation (from `/api/v1/constellations`), name,
     submit.
  3. **Chat panel** — per instance: transcript (from cache + live refresh), a textbox to
     send a message (`POST .../bilda/api/chat`), status strip (harness/model/cost/todos).
  4. **Action history** — recent entries from `action_log`.
- Poll on an interval (e.g. 5s for list, 3s for an open chat panel) — mirrors vape's own
  frontend polling cadence noted in its CLAUDE.md.

## Project layout (planned)

```
mazz-flux-bot/
  Cargo.toml
  src/
    main.rs          # axum app wiring, routes
    vape_client.rs   # reqwest wrapper for vape-manager API + gh-token auth
    db.rs            # sqlite schema/migrations + cache/log read-write
    api.rs           # local JSON API handlers (proxy-ish, + DB-backed history)
    models.rs        # shared structs (Instance, Constellation, ChatMessage, ActionLogEntry)
  static/
    index.html
    app.js
    style.css
  mazz-flux-bot.db    # sqlite file (gitignored)
  PLAN.md             # this file
```

## Open questions / next steps (in order)

All read endpoints needed for list + status + chat-read are now live-confirmed (see
table above). Remaining unknowns are only on the **mutating** side, deliberately not
fired during research:

1. Fire one real `POST /api/v1/instances/{id}/{harness}/api/chat` against an instance you
   don't mind steering (or a fresh throwaway one) to confirm the response shape exactly
   matches `{"ok": true}` and that it actually appears in the target's session.
2. Fire one real `POST /api/v1/instances` create call to confirm the minimal
   `{"name","constellation"}` body is sufficient and see the actual response shape
   (`CreateInstanceResponse` — not yet inspected, only the request side).
3. Confirm `stop`/`start`/`delete`/`rename` response shapes (likely just updated instance
   JSON or `{"ok":true}` — cheap to confirm on an instance you own once the tool exists
   and you're using it for real, no need to pre-test blind).
4. Decide `rusqlite` vs `sqlx` (lean `sqlx` for async consistency with axum unless setup
   friction argues otherwise).
5. Scaffold `cargo init`, add deps (`axum`, `tokio`, `reqwest`, `serde`, `sqlx` or
   `rusqlite`, `tower-http`). Build `vape_client.rs` against the confirmed read endpoints
   first (me / list / detail / agent-status / session), get the list + chat-read view
   rendering end-to-end before touching create/lifecycle/chat-send.
6. Add start/stop/delete/create/chat-send once each is confirmed live (or wire them
   directly since paths+bodies are already high-confidence — just watch the first real
   response closely).
7. Add SQLite cache + action log wiring throughout.

## Answered

- **"Do I need to add tokens to an env file?"** — No. Auth mirrors cadmium exactly: shell
  out to `gh auth token` at request time, using your existing `gh auth login` session.
  Nothing to configure unless `gh` itself isn't authenticated on the machine running the
  tool, in which case the fix is `gh auth login`, not an env file.

## Built (2026-09-03)

Scaffolded, compiles clean, smoke-tested end to end in dry-run mode: create a project →
start its heartbeat → loop wakes up on its interval → attempts to create a `pida`
instance for the project's goal → logs the (dry-run) call → project shows the note.
Real code, real run — not just a plan.

### Layout

```
mazz-flux-bot/
  Cargo.toml
  src/
    main.rs             # AppState, axum router + static file serving, spawns heartbeat
    models.rs            # Project/ActionLogEntry + vape API response/request shapes
    db.rs                 # sqlite (sqlx): projects, action_log, instance_list_cache
    vape_client.rs        # reqwest wrapper: gh-token auth, confirmed reads + dry-run-gated mutations
    anthropic_client.rs   # direct api.anthropic.com Messages call, the "brain"
    heartbeat.rs           # tokio interval loop: create-or-evaluate each running project
    api.rs                  # axum JSON handlers backing the frontend
  static/
    index.html, app.js, style.css   # single-page dashboard, vanilla JS, polls every 5s
```

### The project/goal/heartbeat model

- **Project** = `{name, goal, constellation, status, vape_instance_id, heartbeat_enabled}`.
  One row per goal, one vape instance per project (by design — this tool does not fan a
  project out across multiple instances).
- **Draft → Running**: creating a project leaves it in `draft` (no instance, no
  heartbeat). `POST /api/projects/{id}/start` flips `heartbeat_enabled=1` and
  `status=running` — only then does the loop touch it. `pause` reverses this without
  losing the instance link.
- **Heartbeat tick** (`heartbeat.rs`, interval via `HEARTBEAT_INTERVAL_SECS`, default 60s):
  for every `running` project —
  1. No `vape_instance_id` yet → `POST /api/v1/instances` with `job.prompt = goal`,
     `job.harness = "pida"` (see confidence note on that field below). On success, stores
     the returned id.
  2. Has an instance → `GET .../agent-status` to confirm harness is actually `pida` (skips
     — logs a note, doesn't error — if someone's instance ended up on `bilda` instead),
     then `GET .../pida/api/status` + `.../pida/api/session` (last 6 messages) and hands
     that plus the goal to the brain.
  3. **Brain** (`anthropic_client.rs` + `decide_next_action` in `heartbeat.rs`): one
     Messages API call, system prompt fixes the JSON response shape
     (`{"action": wait|send_message|mark_done|mark_error, "message"?, "note"?}`). No
     `ANTHROPIC_API_KEY` → always `wait`, tool only observes. Unparseable model response →
     also `wait` (never act on a response we can't validate). `send_message` calls
     `.../pida/api/chat`; `mark_done`/`mark_error` update project status and turn the
     heartbeat back off (a finished/stuck project shouldn't keep ticking).
  4. Every tick writes to `action_log` and updates `last_heartbeat_at`/`last_note` — this
     is the audit trail the dashboard's "Action log" panel tails.

### Safety defaults (read before flipping either of these on)

- **`MAZZ_FLUX_LIVE`** (default unset = off): gates every *mutating* vape call
  (create/start/stop/delete/`pida_send`). Off by default, every mutating call just logs
  `[dry-run] would POST ...` and returns a `{"dry_run": true, ...}` marker instead of
  firing. **Reads always fire live** (list/detail/agent-status/pida status/session) — those
  are safe and are how the plan's "confirmed live" facts were established. Turn this on
  (`MAZZ_FLUX_LIVE=1`) only once you're ready for the tool to actually create/steer/delete
  real cloud instances.
- **`ANTHROPIC_API_KEY`** (default unset): without it the brain never runs — heartbeat
  ticks are pure observation (status fetch + log entry), nothing is ever sent to an
  instance and no Anthropic spend happens. This is a normal env var read directly by
  `anthropic_client.rs`; no internal WARP-gated LLM gateway was found (checked vape's own
  `internal/llm/classify.go` — it also just calls `api.anthropic.com` directly with this
  same env var), so "via Cloudflare WARP" turned out not to apply here — WARP is only
  needed for the vape-manager calls, not the Anthropic ones.
- **`CADMIUM_VAPE_URL`** (default `https://vape.stable.dexus.io`) and
  **`MAZZ_FLUX_DB_PATH`** (default `mazz-flux-bot.db`, relative to cwd) round out the env
  vars. `PORT` (default `4270`) for the local web UI. `HEARTBEAT_INTERVAL_SECS` (default
  `60`) for tick cadence.

### Confidence notes carried into the code (see comments at the source)

- `job.harness` on the create-instance body is a **best-effort placement**, not confirmed.
  The local `vape` checkout's Go structs (`internal/handlers/api.go`) don't have a
  `harness` field anywhere — that source is stale relative to what's actually deployed.
  The only evidence `harness` is a real field at all is the literal string `"harness"`
  found via `strings` on the live `cadmium` binary. Before relying on this, flip
  `MAZZ_FLUX_LIVE=1` for one real create call and check whether the resulting instance
  actually comes up on `pida` (via `agent-status`) — if it lands on `bilda` instead, the
  field needs to move (top-level on `CreateInstanceRequest` instead of nested in `job`, or
  a different key entirely).
- `pida_send`'s path/body (`POST .../pida/api/chat`, `{"message": "..."}`) is inferred from
  the generic `/api/v1/instances/%s/%s/api%s` template found in the cadmium binary plus
  the `bilda` server's `/api/chat` handler shape (assumed mirrored for `pida` — not
  independently confirmed, since firing it would inject a real message into a real
  session). First real `MAZZ_FLUX_LIVE=1` send is the confirmation step.
- Everything under "Verified-live endpoints" above (agent-status, pida status/session,
  instances list/detail, constellations) is real — curled and read successfully, and the
  models in `models.rs` were shaped directly off those real responses.

### Next steps

1. Flip `MAZZ_FLUX_LIVE=1` on a throwaway project and watch one real create → confirm
   harness lands on `pida` and fix the `job.harness` placement if not.
2. Set `ANTHROPIC_API_KEY` and watch one real brain tick's `note` in the action log —
   sanity check the JSON parses and the decision is reasonable before trusting `mark_done`.
3. Consider a per-project max-heartbeat-ticks or max-spend guard once live — right now a
   `running` project ticks forever until it hits `mark_done`/`mark_error` or a human pauses
   it.
4. `cargo clippy` pass (not run yet) and trim the two dead-code warnings
   (`ProjectStatus::parse`, `CreateInstanceResponse`) once they're wired to something (e.g.
   parsing `status` back out of the DB row instead of reading the raw TEXT column, and
   using the typed create response instead of a raw `serde_json::Value`).

## Metalcraft + spice_framework refactor (2026-09-03)

Per project decision, `heartbeat.rs` was refactored from a hand-rolled if/else tick into a
[`metalcraft`](https://github.com/rust4ai/metalcraft) (v0.11.0, real published crate)
typed-state graph, with [`spice_framework`](https://crates.io/crates/spice-framework)
(v0.2.0) behavioral tests for the decision-parsing logic. Both are legitimate published
crates from the same small `rust4ai` GitHub org — **naming gotcha for anyone touching this
later**: crates.io also has an unrelated, empty placeholder crate literally named `spice`
(a "SPICE protocol" stub, not an AI test harness — its entire content is a default `cargo
new` unit test). Do not add `spice = "..."`; the real one is `spice-framework`, imported
in code as `spice_framework` (that's what metalcraft's own examples use too). Also: the
`rig` feature on `metalcraft` pulls ~600 extra transitive crates (a second `reqwest` major
version, image/video codecs) for a multi-provider LLM framework we don't need — we kept our
own minimal `anthropic_client.rs` and use plain `metalcraft` (no `rig` feature).

### The graph (`src/heartbeat.rs`)

```
route --(no instance)--> create_instance --> END
  \--(has instance)--> fetch_status --(harness != pida)--> END
                            \--(pida)--> decide --> act --> END
```

- **Nodes** (`RouteNode`, `CreateInstanceNode`, `FetchStatusNode`, `DecideNode`, `ActNode`)
  each hold only the `Arc<VapeClient>`/`Arc<AnthropicClient>` they need — no DB access.
  This keeps the graph itself unit/spice-testable without a database.
- **State** (`HeartbeatState`, `pub(crate)`) is built fresh from a `Project` DB row at the
  start of every tick and thrown away at the end. Durability lives in sqlite
  (`persist_tick`, the only place `heartbeat.rs` touches the DB), not in the graph — each
  tick's `Executor::run()` is a fresh, ephemeral run, not a checkpointed long-lived one.
- **Human-in-the-loop, for real** (not just decorative): `FetchStatusNode` calls
  `NodeOutcome::interrupt_with(...)` — metalcraft's actual interrupt mechanism — when the
  instance has a pending question and no `ANTHROPIC_API_KEY` is configured to safely
  answer it. The executor returns `RunOutcome::Interrupted{reason, ..}`, which
  `persist_tick` logs as `heartbeat_interrupted` and surfaces as the project's `last_note`.
  Functionally equivalent to the old "wait" fallback, but now expressed through the
  library's real interrupt semantics instead of a magic string.
- **StepGuard** (`loop_guard`) is wired in as defence-in-depth against a future edit
  accidentally introducing a cycle — the graph is acyclic by construction today (every
  path terminates at `END` within 4 steps), so this should never actually fire. It does
  *not* address the separate "a project could tick forever" concern from the original
  plan (that's cross-tick, at the outer `tokio::interval` level, not intra-tick) — still an
  open item, see Next Steps below.
- **Errors**: a node returning `Err` produces `RunOutcome::Failed{state, node, error}`,
  logged as `heartbeat_node_failed` with the partial state preserved (metalcraft hands back
  accumulated state on failure rather than dropping it) — nothing crashes the outer loop.

### spice_framework tests (`tests/heartbeat_brain.rs`)

Targets `parse_brain_response` — the safety-critical seam between "whatever text Anthropic
returned" and "is it safe to act on". No live LLM call, no API key, no network: spice's
`AgentUnderTest` contract is chat-shaped (`run(user_message, config) -> AgentOutput`), so
`BrainAdapter` treats `user_message` as the raw brain text and reports the resulting
`Decision.action` as if it were a tool call — an honest fit since the thing under test
really is "text in, one bounded/validated decision out."

**Writing the tests caught two real gaps, fixed in the same pass** (this is the point of
writing behavioral tests before/alongside the code, not after):
1. A hallucinated action string outside `{wait, send_message, mark_done, mark_error}`
   parsed "successfully" (non-empty `action` field) and would have passed through
   unvalidated — `ActNode`'s catch-all happened to treat it as `wait` anyway, but
   `parse_brain_response` itself wasn't the one guaranteeing that. Fixed: added
   `KNOWN_ACTIONS` validation directly in the parser.
2. Markdown-fenced JSON (` ```json\n{...}\n``` `) failed to parse at all despite the
   system prompt asking the model not to fence it — models don't always comply. Fixed:
   `strip_markdown_fence` runs before the `serde_json::from_str` attempt.

Run with `cargo test` (both the lib and the spice suite run as part of the normal test
target — no separate invocation needed).

### Dependency footprint

`cargo tree` sanity check: with `rig` removed, `metalcraft` + `spice-framework` (dev-only)
add a reasonable, justified set of crates — nothing like the `rig`-feature explosion. Full
`cargo build` (clean) finishes in ~2-5s incrementally; the one-time cold build after adding
both crates was ~40s+ (mostly `rig`'s tree before it was removed — plain `metalcraft` alone
is fast).

## Live spike confirmation (2026-09-03)

Ran a real end-to-end spike: `MAZZ_FLUX_LIVE=1`, project `mfb-spike`
(constellation=`back-office`, goal="reply hello and report your status, then stop"),
heartbeat left to run for real against `https://vape.stable.dexus.io`. Both previously
"not yet fired live" guesses are now **confirmed correct**:

- **`job.harness: "pida"` placement is right.** `POST /api/v1/instances` with our exact
  body shape created instance `w682hq9a`; `GET .../agent-status` came back
  `active_harness: "pida"`. No field-placement fix needed after all.
- **`job.prompt` correctly seeds the autonomous job's first user turn.** The goal text
  appeared verbatim as the first `user` message in `.../pida/api/session`, and the agent
  actually completed it correctly ("Hello! Status: ready and idle...").
- **`pida_send` (`POST .../pida/api/chat`, `{"message": "..."}`) is confirmed.** Sent via
  our own `/api/projects/{id}/message` manual-override endpoint; the message and the
  agent's reply ("OK") both showed up in the real session transcript afterward.
- **Our own heartbeat graph handled the boot window correctly, live.** The instance
  returned `503 Instance not ready` for ~10-15s after creation; `FetchStatusNode`'s error
  surfaced as `RunOutcome::Failed` → logged as `heartbeat_node_failed` → retried
  automatically next tick with no crash. Once ready, the very next tick logged the correct
  `ANTHROPIC_API_KEY not set` observation-only note.

Every "high confidence, not fired live" item in the endpoint tables above should now be
read as **confirmed live**, not just high-confidence-from-source. The only remaining
untested mutating calls are `start`/`stop`/`rename`/`delete` on an instance (create and
send are now both proven).

Spike instance `w682hq9a` (`mfb-942cf462`) was left running per request:
https://back-office--w682hq9a.stable.dexus.io — visible in `cadmium vape ls` /
the vape dashboard under owner `mazzdevs`. Delete it via `cadmium vape delete w682hq9a`
(or through mazz-flux-bot itself once the UI's delete action is wired to a project) when
done looking at it — it is a real running pod and will otherwise sit until the reaper's
warn→grace window (see vape's own auto-cleanup docs) eventually reaps it.

## Second brain backend: OpenRouter (2026-09-03)

`src/brain.rs` adds `Brain`, an enum dispatching to whichever LLM backend is configured:

- `ANTHROPIC_API_KEY` set → direct Anthropic call (unchanged `anthropic_client.rs`),
  checked first so existing setups keep working with no config change.
- else `OPENROUTER_API_KEY` set → OpenRouter's OpenAI-compatible `/chat/completions`
  (`OpenRouterClient`, also in `brain.rs`), model via `OPENROUTER_MODEL` (default
  `openai/gpt-5.6-sol` — verified live on OpenRouter's `/api/v1/models` today, not
  assumed). Any OpenRouter model id works, including routing Anthropic models through
  OpenRouter instead of direct.
- neither → `Brain::Disabled`, identical "observe only" behavior as before.

`AppState.anthropic: Arc<AnthropicClient>` was renamed to `AppState.brain: Arc<Brain>`
(and the same rename through `heartbeat.rs`'s node structs) since the field is no longer
Anthropic-specific. `parse_brain_response` and the spice tests were untouched — they
operate on the brain's raw text response regardless of which backend produced it.

Not yet live-fired against OpenRouter (only startup backend-selection was smoke-tested).
Next step if picking this up: set a real `OPENROUTER_API_KEY`, rerun the same live-spike
pattern from the earlier section against a throwaway project, and confirm the
`choices[0].message.content` parse actually matches what `openai/gpt-5.6-sol` returns
(OpenAI-family models are usually well-behaved chat-completion responders, but this
hasn't been checked against this specific model).
