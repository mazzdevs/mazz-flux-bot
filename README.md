# 🔥 mazz-flux-bot

**An autonomous orchestrator that drives coding-agent instances toward a goal — heartbeat by heartbeat, forever, until it's done.**


<img width="1144" height="765" alt="image" src="https://github.com/user-attachments/assets/b1bdbe28-8ea6-43b0-b9ac-2d177143f756" />

You describe what you want built or fixed. mazz-flux-bot spins up a live [VAPE](#what-is-vape) instance running the [`pida`](#what-is-pida) coding-agent harness, gives it a real, LLM-composed opening directive, then checks in on a schedule you control — reading its progress, deciding whether to steer it, mark it done, flag an error, or raise a human task — compacting everything it learns into a running memory so it never needs to re-read the whole history to know what's going on.

It runs *inside* a VAPE instance itself, using that instance's own inference credentials — no external database, no cloud service, just flat files and a single static binary.

```
┌─────────────┐   heartbeat    ┌──────────────────┐   pida chat    ┌─────────────────┐
│  Project     │ ─────────────▶│    Conductor      │───────────────▶│  vape instance   │
│  (your goal) │   every N min │  (LLM decision)    │   send_message  │  running `pida`  │
└─────────────┘                └──────────────────┘                 └─────────────────┘
       ▲                                │                                     │
       │                        writes memory.md                     reports status/
       │                     (compacted, every tick)                  session messages
       └───────────────────────────────┴─────────────────────────────────────┘
```

## Why this exists

Coding agents are great at *doing* work but bad at *staying on task* unattended. Point one at a big goal, walk away, and it either finishes, drifts, gets stuck, or asks a question nobody's there to answer. mazz-flux-bot is the thing that stays there — polling, deciding, nudging, and knowing when to tap a human on the shoulder instead of guessing.

## Features

- **One instance per project.** Give it a goal, it spins up a real `pida`-harness VAPE instance and manages the whole lifecycle.
- **LLM-composed prompts, not templates.** The conductor writes the actual opening message to the agent *in its own words* from your goal — and composes each heartbeat's steering message the same way, informed by your optional **heartbeat prompt** plus its own running memory. Nothing is ever sent to the agent verbatim.
- **Persistent, compacted memory.** Every tick, the conductor rewrites a single memory file summarizing everything worth remembering — no unbounded context growth, no re-reading full history every time.
- **Human-in-the-loop, properly.** When the conductor hits a blocker only a person can resolve, it raises one or more discrete **human tasks** — even splitting a multi-item reply from the agent into separate, independently-resolvable tasks instead of one wall of text.
- **Per-project heartbeat cadence**, editable in seconds/minutes/hours right from the UI (default 15 minutes) — plus a **Force heartbeat** button when you don't want to wait.
- **LLM-assisted naming.** Leave the project name blank and it'll name itself from the goal. Vape instance names are readable slugs, not `mfb-a1b2c3d4`.
- **File-backed state, human-readable.** Every project, note, human task, and memory file is a plain JSON or Markdown file on disk — no database, browsable and editable directly through the built-in **Files** tab.
- **Own git history for your state.** The state directory lives as its own sibling git repo, independently committable/pushable — your projects and their history survive the bot's own repo (or the whole pod) being thrown away.
- **Built to run inside VAPE itself**, using the instance's own `OPENROUTER_API_KEY` and `gh` credentials — zero extra secrets to configure.
- **Light/dark UI**, real buttons, a proper icon set, and a live countdown to the next heartbeat. Because a tool you're going to stare at all day should not look like `curl | jq`.

## What is VAPE / pida?

This tool was built to run inside **Dutchie's VAPE** ("Virtual AI Programming Environment") platform — isolated, disposable cloud dev pods that clone repos and run whatever dev servers/tools a project needs. **`pida`** is one of VAPE's supported coding-agent harnesses (the other is `bilda`) — a long-running, chat-driven agent session you can poll, message, and interrupt over HTTP.

mazz-flux-bot talks to VAPE's REST API (`vape-manager`) to create/inspect/message instances, and only knows how to drive the `pida` harness today.

**If you don't have access to a VAPE-like platform, this tool isn't directly useful to you as-is** — but the orchestration pattern (heartbeat loop + LLM conductor + compacted memory + file-backed state) is a clean reference if you're building something similar against a different agent platform.

## Architecture

```
src/
  main.rs           — HTTP server bootstrap, routing, CLI (`commit-state`)
  models.rs         — Project/HumanTask/ProjectNote + vape API request/response shapes
  store.rs          — file-backed persistence (JSON + Markdown, no database)
  state_repo.rs      — git init/commit/push wrapper for the state directory
  conductor.rs       — the LLM ("conductor") that makes every decision, via OpenRouter
  heartbeat.rs        — the orchestration graph (metalcraft): route → create/fetch → decide → act
  vape_client.rs      — REST client for vape-manager (create/start/stop/rename/chat/...)
  api.rs             — axum HTTP handlers, one per route

static/              — vanilla HTML/CSS/JS frontend, no build step, no framework
tests/               — unit tests + a spice_framework behavioral suite for the conductor
```

**No sqlite, no Postgres.** State is plain files:

```
<data-dir>/
  projects/<id>.json           # one file per project
  notes/<id>/<ts>__<uuid>.md    # append-only, timestamped
  memory/<id>.md                 # conductor-authored, overwritten every tick
  human_tasks/<id>/<id>.json
  agent_prompts/validation.md   # optional, user-authored — extra mark_done criteria
  action_log/<yyyy-mm-dd>.ndjson
  settings.json
```

By default this lives in a **sibling directory** next to the bot's own checkout (`../mazz-flux-bot-state`) — deliberately outside this repo's working tree, so it's a separate git repo you can push independently (see [Persisting state](#persisting-state)).

## The heartbeat loop

Each tick, for each running project whose interval has elapsed:

1. **No instance yet?** Create one — conductor composes the opening prompt from your goal, picks a readable name (LLM-assisted, deterministic fallback), spawns a real `pida` instance.
2. **Instance exists?** Fetch its live status + last 6 chat messages.
3. **Decide.** The conductor sees `goal`, your optional `heartbeat_prompt`, its own `memory` from last time, and the current status — and returns one of:
   - `wait` — nothing to do this tick
   - `send_message` — compose and send a steering message (in its own words)
   - `mark_done` — the goal is achieved (optionally gated by `agent_prompts/validation.md`, if you've written one)
   - `mark_error` — stuck in a way a message can't fix
   - `create_human_task` — raise one or more discrete blockers for a person
   - always: a rewritten `memory` summary, replacing the last one
4. **Act + persist.** Whatever it decided gets executed and logged.

## Quick start

Requires Rust (pinned via [`bolt`](https://dutchie.roadie.so/docs/default/component/bolt/), or your own toolchain — no C dependencies, pure Rust).

```bash
git clone <this-repo>
cd mazz-flux-bot
export OPENROUTER_API_KEY=sk-or-v1-...   # the only secret this needs
cargo run
# → mazz-flux-bot listening on http://0.0.0.0:4270
```

Open `http://localhost:4270`, click **New project**, describe a goal (name optional — it'll pick one), hit **Start**. That's it.

### Configuration

All environment variables, all optional except the API key:

| Variable | Default | Purpose |
|---|---|---|
| `OPENROUTER_API_KEY` | *(none — conductor disabled)* | The only required secret. Without it, the bot still creates/observes instances but never steers, marks done, or raises tasks. |
| `OPENROUTER_MODEL` | `openai/gpt-5.6-sol` | Conductor's own decision-making model. Overridable per-run in Settings. |
| `MAZZ_FLUX_INSTANCE_MODEL` | `openai/gpt-5.6-sol` | Model used by spawned vape instances. Also overridable in Settings. |
| `PORT` | `4270` | HTTP port. |
| `MAZZ_FLUX_DATA_DIR` | `../mazz-flux-bot-state` | Where all project/note/memory files live. |
| `MAZZ_FLUX_LIVE` | live by default | Set `0`/`false` to dry-run every mutating vape call (logged, not fired). |
| `HEARTBEAT_SCAN_INTERVAL_SECS` | `15` | How often the outer loop checks which projects are due (each project's own cadence is separate, see below). |
| `CADMIUM_VAPE_URL` / `VAPE_MANAGER_URL` | `https://vape.stable.dexus.io` | vape-manager base URL — auto-detected from the in-cluster env var when running inside a VAPE instance. |
| `MAZZ_FLUX_STATE_REPO_URL` | *(none)* | Git remote for the state directory — enables `POST /api/state/commit` to push, not just commit locally. |
| `MAZZ_FLUX_STATE_BRANCH` | `main` | Branch pushed to `MAZZ_FLUX_STATE_REPO_URL`. |

### Persisting state

The state directory is its own git repo. Commit it manually from the UI (Files tab → the commit affordance) or on demand:

```bash
cargo run -- commit-state "end of day snapshot"
```

Set `MAZZ_FLUX_STATE_REPO_URL` to a real remote and every commit pushes too — your projects, notes, and memory survive the bot's own pod being deleted.

## The API

Everything the UI does, you can do directly — this is a first-class API, not an afterthought, and it's exactly what a `pida` instance running *inside* the same pod as this bot can use to manage its own projects.

```bash
# Create a project — name is optional, the conductor will name it from the goal
curl -X POST localhost:4270/api/projects \
  -H 'content-type: application/json' \
  -d '{"goal": "Add a health check endpoint and confirm it returns 200"}'

curl -X POST localhost:4270/api/projects/<id>/start
curl -X POST localhost:4270/api/projects/<id>/heartbeat/force   # don't wait for the timer
curl localhost:4270/api/projects/<id>/memory                     # what the conductor remembers right now
```

| Method | Path | What |
|---|---|---|
| `GET/POST` | `/api/projects` | List / create projects |
| `GET/DELETE` | `/api/projects/{id}` | Fetch / delete a project |
| `POST` | `/api/projects/{id}/start` \| `/pause` | Toggle the heartbeat |
| `POST` | `/api/projects/{id}/message` | Manually send a chat message, bypassing the conductor |
| `POST` | `/api/projects/{id}/goal` \| `/heartbeat-prompt` | Edit either prompt |
| `POST` | `/api/projects/{id}/heartbeat-interval` | Per-project cadence, in seconds |
| `POST` | `/api/projects/{id}/heartbeat/force` | Run one tick right now |
| `GET` | `/api/projects/{id}/notes` \| `/memory` | Conductor's notes / current memory |
| `POST` | `/api/projects/{id}/instance/rename` | Rename the underlying vape instance |
| `GET` | `/api/human-tasks` | Open (or all) human tasks, across every project |
| `POST` | `/api/human-tasks/{id}/resolve` | Resolve one |
| `GET` | `/api/log` | Action log, global or `?project_id=` scoped |
| `GET` | `/api/instances`, `/api/instances/{id}`, `/status`, `/session` | Read-through to vape-manager |
| `GET/PUT/DELETE` | `/api/files?path=` | Browse/edit/delete anything under the state directory |
| `GET/POST` | `/api/settings` | Conductor/instance model selection |
| `POST` | `/api/state/commit` | Snapshot (and push, if configured) the state repo |

## Development

```bash
cargo build
cargo test        # unit tests + a spice_framework behavioral suite for the conductor's decision parsing
```

No frontend build step — `static/` is served as-is by `tower-http`'s `ServeDir`. Edit, refresh.

## Status

A real, working tool that's been driving real PR/nitpick-resolution work against real repos from inside a real VAPE instance — not a demo. Still a young project: expect rough edges, and read `PLAN.md` for the full, unvarnished build history if you want to know exactly what's been verified live versus what's just wired up and untested.
