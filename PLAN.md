# Plan: file-backed storage (markdown + JSON) + state-repo persistence

Continues on branch `pida/run-in-flux-inference`. Two problems this solves at once:

1. **Build blocker in this environment**: `sqlx@0.9` requires `rustc 1.94`, bolt's newest
   pinned Rust is `1.89.0`, and building sqlite support also needs a C toolchain
   (`gcc`/`pkg-config`/`libsqlite3-dev`) that isn't present by default on a fresh flux
   instance. Dropping sqlx removes both problems — no C compiler, no rustc version fight.
2. **User ask**: replace sqlite with plain markdown/JSON files on disk, gitignored, with a
   way to snapshot that state into its own separate git repo (so a flux instance's
   accumulated project state — notes, logs, settings — survives instance teardown instead
   of dying with the pod's sqlite file).

## 1. Storage layout (sibling directory, own git repo — not nested/gitignored)

A gitignored subfolder *inside* the bot repo would mean nesting a second `.git` inside
the first — git tooling tolerates this but it's a foot-gun (`git add -A` at the repo
root silently ignores it, `git clean -fdx` can nuke it, an IDE or script that doesn't
respect `.gitignore` can wreck it). Instead, state lives in a **sibling directory next
to the bot repo**, entirely outside `mazz-flux-bot`'s working tree:

```
/root/workspace/mazz-flux-bot/           # the bot's own repo (this checkout)
/root/workspace/mazz-flux-bot-state/     # separate directory, separate git repo
```

Default path is computed as `../mazz-flux-bot-state` relative to the bot's own repo
root, overridable via `MAZZ_FLUX_DATA_DIR` (absolute path recommended if overriding —
e.g. pointing it outside `/root/workspace` entirely on a persistent volume). Nothing to
add to `.gitignore` in the bot repo since the directory isn't under it at all. Layout —
one file per record where practical, so diffs in the state repo are meaningful:

```
mazz-flux-bot-state/
  projects/
    <project_id>.json          # Project struct, pretty-printed
  notes/
    <project_id>/
      <rfc3339-ts>__<uuid8>.md # one note = one markdown file, content verbatim
  human_tasks/
    <project_id>/
      <task_id>.json           # HumanTask struct
  action_log/
    <yyyy-mm-dd>.ndjson        # append-only, one JSON object per line, day-sharded
  settings.json                 # flat {key: value} map (conductor API keys/models)
  cache/
    instances.json              # {"raw_json": "...", "fetched_at": "..."}
```

- `HumanTask.id` / `ProjectNote` filename / action-log entries move from sqlite
  AUTOINCREMENT integers to **uuid v4 strings** — no shared counter file needed, no race
  between concurrent writers. `api.rs` path extractors (`Path<i64>`) become `Path<String>`;
  update `static/*.js` callers that treat task ids as numbers.
- Action log stays day-sharded ndjson (not one-file-per-entry) since it's high-volume
  (every heartbeat tick) and per-entry files would be noisy in the state repo's history —
  ndjson append is still a clean, mergeable diff.

## 2. `Store` replaces `SqlitePool`

New `src/store.rs` (replaces `src/db.rs`) with the **same function signatures** used
today (`create_project`, `get_project`, `set_project_status`, `add_project_note`,
`create_human_task`, `log_action`, `get_setting`/`set_setting`, `cache_instance_list`,
etc.) so `api.rs`, `heartbeat.rs`, and `main.rs` need only a type swap
(`SqlitePool` → `Arc<Store>`), not a rewrite of call sites.

- `Store { root: PathBuf, lock: tokio::sync::Mutex<()> }`. All writes take the lock and do
  read-modify-write-rename (write to `*.tmp`, `fs::rename` over the target) for
  crash-safety; reads are lock-free direct file reads.
- `list_projects` / `list_running_projects`: read every `projects/*.json`, filter/sort in
  memory (fine at this tool's scale — dozens of projects, not thousands).
- Drop the `instance_cache` table entirely — grep confirms it's dead code (only
  `instance_list_cache` is actually read/written).
- `sqlx`, `libsqlite3-sys` deps removed from `Cargo.toml`; add nothing new for JSON (already
  have `serde_json`), and nothing new for markdown (notes are just written as raw `.md`
  files, no parsing needed on the way in — only shown as preformatted text today anyway).
- `bolt.yaml` no longer needs a C toolchain — drop that setup step; `cargo build` becomes
  a pure-Rust build again.

## 3. State-repo persistence (new `src/state_repo.rs`)

`data/` is its own **separate, nested git repo** — not part of `mazz-flux-bot`'s repo.
Add `/data/` to `mazz-flux-bot/.gitignore` so the outer repo never sees it.

- On first use, `state_repo::ensure_init(&root)` runs `git init` inside `data/` if
  `data/.git` doesn't exist, sets `user.name`/`user.email` from `GIT_USER_NAME`/
  `GIT_USER_EMAIL` (already present in this environment) if unset, and adds a remote
  named `origin` from `MAZZ_FLUX_STATE_REPO_URL` if that env var is set and no remote
  exists yet.
- `state_repo::commit(&root, message) -> Result<CommitSummary>`: `git add -A && git
  commit -m <message>` inside `data/` (no-op / `Ok(None)` if nothing changed). Pushes to
  `origin <branch>` (default `main`, override `MAZZ_FLUX_STATE_BRANCH`) only if a remote
  is configured — local-only commits still work with no remote, so this is useful even
  before a state repo exists on GitHub.
- **Trigger points**:
  - `POST /api/state/commit` — manual button, returns the commit sha/summary or "nothing
    to commit". Settings dialog gets a "Commit state" button + last-commit-info display.
  - Optional auto-commit from the heartbeat loop every `MAZZ_FLUX_STATE_AUTO_COMMIT_TICKS`
    ticks (unset = disabled, opt-in) — commits after each full tick pass, message
    `"heartbeat snapshot <rfc3339>"`.
  - CLI escape hatch for cron/manual use without booting the HTTP server: `cargo run --
    commit-state` (checked in `main()` before starting axum — if `argv[1] ==
    "commit-state"`, init the store, run one commit, print result, exit).

## 4. Docs

Update this PLAN.md's existing "run inside a flux instance" section stays as-is (still
accurate — `0.0.0.0` bind + `VAPE_MANAGER_URL` fallback are orthogonal to storage). Add a
new section (this one) documenting the file layout, uuid id change, and state-repo
commit/push workflow, plus a one-line `README`-style note: **first time setup needs
`MAZZ_FLUX_STATE_REPO_URL` set (e.g. `git@github.com:mazzdevs/mazz-flux-bot-state.git`,
repo created ahead of time via `gh repo create mazzdevs/mazz-flux-bot-state --private`)
for pushes to work; without it, commits are still made locally in `data/.git`.**

## 5. Migration/compat notes

- No migration path from the old `mazz-flux-bot.db` sqlite file — this is a dev tool with
  throwaway state (confirmed by the "spike instance" language already in this file), so a
  clean cutover is fine. Delete `MAZZ_FLUX_DB_PATH` env references.
- Existing tests under `tests/heartbeat_conductor.rs` operate on the graph's pure
  `parse_conductor_response` logic, not the DB — unaffected by this change (verify after
  the fact, don't need to touch them).

## Execution order

1. `src/store.rs` + delete `src/db.rs`, update `src/lib.rs` module list.
2. `Cargo.toml`: remove `sqlx`; keep `uuid` (now used for note filenames + human task /
   log entry ids too, not just project ids).
3. `models.rs`: `HumanTask.id`/`ProjectNote.id` → `String`; `ActionLogEntry.id` → `String`.
4. `api.rs`: swap `Path<i64>` → `Path<String>` on the resolve-human-task route; `AppState`
   field type.
5. `main.rs`: `AppState { db: Arc<Store>, .. }`, data-dir init, `commit-state` CLI branch.
6. `src/state_repo.rs`: init/commit/push.
7. New `/api/state/commit` handler + wire into router.
8. `static/app.js` + `settings` dialog: "Commit state" button, human-task id handling as
   strings.
9. `.gitignore`: add `/data/`.
10. `bolt.yaml`: drop C-toolchain step (was never actually added as a step — just drop the
    plan to add one).
11. Build (`bolt build` / `cargo build`), fix compile errors, smoke-test the API surface
    manually (create project, add note, create+resolve human task, settings save,
    `/api/state/commit` with and without a remote configured).

## Executed (2026-09-03) — file store landed, sqlite fully removed

Implemented as planned above, with a couple of refinements made during the build:

- **Note ids/filenames use epoch-millis + a short uuid** (`<millis>__<uuid8>.md`), not a
  sanitized rfc3339 string — simpler and avoids a fragile colon-restoration step. The real
  rfc3339 `created_at` is embedded as a leading `<!-- created_at: ... -->` line in the file
  and stripped back out on read, so it round-trips exactly.
- `HumanTask`/`ProjectNote`/`ActionLogEntry` ids are now uuid v4 **strings**, not
  sqlite-autoincrement integers — no shared counter file, no cross-writer races.
  `resolve_human_task` takes `&str` now; `api.rs`'s route changed `Path<i64>` → `Path<String>`.
- `Store` (src/store.rs) has the exact same function surface as the old `db.rs` module had
  (as methods instead of free functions taking a pool) — `api.rs`/`heartbeat.rs` changes
  were almost entirely `db::fn(&state.db, ...)` → `state.store.fn(...)` mechanical swaps.
- **Data dir default is a sibling of this repo** (`../mazz-flux-bot-state`), computed from
  `<parent of cwd>/<repo-dir-name>-state` — not a subdirectory, not gitignored, genuinely
  outside this git repo's working tree. Override with `MAZZ_FLUX_DATA_DIR` for anything
  else (e.g. a persistent volume path). `mazz-flux-bot.db`/`MAZZ_FLUX_DB_PATH` are gone.
- `state_repo.rs` shells out to `git` (init/add/commit/push) inside the data dir — verified
  live against a tempdir: `git init` on first commit, identity picked up from
  `GIT_USER_NAME`/`GIT_USER_EMAIL` (already in this environment), commits correctly report
  `committed: false` when there's nothing new, real commits get real shas. Push is
  attempted only if `MAZZ_FLUX_STATE_REPO_URL` was set at `ensure_init` time (adds an
  `origin` remote); not exercised against a real remote in this session (no such
  throwaway repo was created) — `pushed: false` with a logged warning is the expected,
  tested-for behavior when no remote exists.
- `POST /api/state/commit` (optional `{"message": "..."}` body) added and wired into the
  router; `cargo run -- commit-state [message]` is the equivalent CLI path for cron/manual
  use without booting the HTTP server.
- **Build no longer needs a C toolchain or a newer-than-bolt-pinned rustc.** Removing sqlx
  removed both blockers from earlier in this session (sqlx@0.9 required rustc 1.94, and
  building sqlite support needed gcc/pkg-config/libsqlite3-dev that a fresh flux instance
  doesn't have by default). `bolt.yaml`'s `rust: 1.89.0` pin is enough on its own now —
  verified with a clean `rm -rf target && bolt build` and `bolt test`, no manual rustup or
  apt-get install needed.
- Added `tests/store_smoke.rs` — exercises the whole `Store` surface (project
  create/status/note/human-task/setting/action-log) against a real tempdir, no mocks.
  Existing `tests/heartbeat_conductor.rs` was untouched and still passes (it only tests
  pure JSON-parsing logic, never touched the DB).
- Smoke-tested the full HTTP API by hand against a running instance
  (`MAZZ_FLUX_DATA_DIR=/tmp/mfb-state`): create/list/get project, start/pause (status +
  heartbeat_enabled flip correctly), notes list, human-tasks list, action log, settings
  get/update, and `/api/state/commit` (first commit real, second no-op, `git log` in the
  tempdir showing the expected commits) — all confirmed working end to end.
