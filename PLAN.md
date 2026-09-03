# Plan: file browser tab for the state directory

Continues on branch `pida/run-in-flux-inference` (file store already landed — see
earlier sections of this file for that work). Two asks:

1. **Project-scoped + global action logs** — already done. `GET /api/log?project_id=X`
   (used on the project detail page) and `GET /api/log` with no filter (dashboard-wide,
   already on `index.html`) both exist today. No new work needed here; will just confirm
   both still work after other changes in this pass and call it out to the user rather
   than silently skip it.

2. **A file-browser tab for the state directory** — read/edit every `.md`/`.json` under
   `mazz-flux-bot-state/` (projects, notes, human_tasks, action_log, settings.json,
   cache/) directly from the web UI, as a new top-level tab alongside the existing
   Projects dashboard.

## API additions (`src/api.rs`, new `files` module or just more handlers)

All paths are scoped to `state.store.root()` and defended against traversal — reject any
requested path that escapes the root after normalization (`..`, absolute paths, symlink
escape). This is a local single-user dev tool but the store now literally holds an
editable filesystem window, so path-escape is the one thing worth being careful about.

- `GET /api/files?path=<relative>` — if `path` is a directory (or omitted, meaning root):
  return `{"type":"dir","entries":[{"name","path","is_dir","size","modified_at"}]}`
  sorted directories-first then name. If `path` is a file: return
  `{"type":"file","path","content","size","modified_at"}` — content read as UTF-8 (files
  in this store are always JSON or markdown, so this is safe); non-UTF8 or oversized
  (>2MB, to stop someone pointing this at something huge) returns an error instead of
  garbling/hanging the UI.
- `PUT /api/files?path=<relative>` body `{"content": "..."}` — overwrites the file via
  the same atomic write-then-rename helper already in `store.rs` (exposed as a small pub
  fn, e.g. `store::write_file_atomic`, reused here instead of duplicating the tmp-rename
  dance). Creates parent directories if missing (lets the file browser also be used to
  add new ad-hoc notes by typing a path that doesn't exist yet). Rejects writes outside
  the root the same way GET does.
- `DELETE /api/files?path=<relative>` — removes a file (not a whole directory, to keep
  this low-risk — deleting a project's whole folder isn't a feature this adds).
- No new create-project-note-via-file-browser affordance beyond "PUT a new path" — good
  enough, no special-cased "new file" button needed beyond a text input for the path.

These live directly on `Store` as thin wrappers (`Store::browse(rel_path)`,
`Store::read_file(rel_path)`, `Store::write_file(rel_path, content)`,
`Store::delete_file(rel_path)`) so the path-safety check has one implementation, reused
by both the HTTP handlers and (later, if wanted) the CLI.

## Frontend: new "Files" tab

`index.html` gets a simple tab strip at the top of `<main>` — "Projects" (existing
content, default) / "Files" (new). Vanilla show/hide via a `data-tab` attribute + one
small JS toggle, no router needed (single page, no navigation state worth persisting
beyond `localStorage` like the existing list/tile toggle already does).

Files tab layout: a breadcrumb-style path header, a directory listing (click a row to
navigate into a dir or open a file), and a simple textarea-based editor pane for the
currently open file with Save/Delete/Revert buttons — no need for a rich markdown/JSON
editor, this is an ops tool not an IDE. JSON files get a "format" button that
round-trips through `JSON.parse`/`JSON.stringify(…, null, 2)` client-side before saving
(catches typos before they hit disk) but the raw textarea content is always what's
authoritative if that button isn't used.

New `static/files.js` (kept separate from `app.js` to avoid one more giant file) wires:
`loadDir(path)`, `openFile(path)`, `saveFile()`, `deleteFile()`, breadcrumb click
navigation, and an "up one level" control.

## Execution order

1. `src/store.rs`: add `browse`/`read_file`/`write_file`/`delete_file` with the shared
   path-safety normalization helper.
2. `src/api.rs`: `GET/PUT/DELETE /api/files` handlers; register routes in `main.rs`.
3. `static/index.html`: tab strip, new `<section id="files">` markup (breadcrumb + listing
   + editor pane), confirm existing Action log section markup/copy still makes sense
   labeled as "global" now that Files exists as a sibling concept.
4. `static/files.js`: directory/file fetch+render+edit logic.
5. `static/style.css`: minimal styling for tabs, breadcrumbs, file listing rows, editor
   textarea.
6. Manual smoke test: browse into `projects/`, open a project json, edit+save, confirm
   change round-trips via `GET /api/projects/:id`; open a note `.md`, confirm the leading
   `<!-- created_at: ... -->` comment is visible (expected — the browser shows the raw
   file, not the parsed `ProjectNote.content`); attempt a path-escape (`../../etc/passwd`
   style) and confirm it's rejected.
7. Re-verify both action-log surfaces (global on dashboard, project-scoped on project
   detail page) still return correct data — no code change expected there, just a
   confirmation pass.

## Executed (2026-09-03) — file browser tab + live-by-default + confirmed logs

- **Action logs were already project-scoped and global** (`GET /api/log?project_id=X` on
  the project detail page, `GET /api/log` unfiltered on the dashboard) — no change
  needed, confirmed working end to end against a real running project.
- **File browser**: `Store::browse`/`write_file`/`delete_file` (src/store.rs) with a
  shared `resolve_path` helper that rejects `..`/absolute paths and any dotfile/dotdir
  component (blocks `.git` specifically — the state repo's own git metadata must never
  be reachable through this API) plus a canonicalize-based symlink-escape check.
  `GET/PUT/DELETE /api/files?path=...` wired in `api.rs`/`main.rs`. New "Files" tab on the
  dashboard (`static/files.js`, tab-strip markup in `index.html`, styling in
  `style.css`) — breadcrumb navigation, directory listing, textarea editor with
  Save/Revert/Delete and a JSON-format helper. Verified live: browsed into `projects/`,
  read a real running project's JSON, edited it via `PUT`, confirmed the change through
  the normal `/api/projects/:id` endpoint, then restored it.
- **`MAZZ_FLUX_LIVE` is now live by default** — flipped from opt-in (`=1` to go live) to
  opt-out (`=0` to force dry-run). No env var needs to be set at all for the bot to
  actually create/steer real vape instances now; confirmed live in this session (project
  `test-chat` got a real instance, `obbpisa2`, created without `MAZZ_FLUX_LIVE` set
  anywhere in the environment).
- Added `tests/store_smoke.rs::file_browser_reads_writes_and_blocks_escapes` covering
  write/read/edit/delete round-trips plus both escape classes (`../`, `.git`).

## Executed (2026-09-03) — force-heartbeat, per-project interval, relative times

- **Force heartbeat button** (project detail page): `POST /api/projects/{id}/heartbeat/force`
  → `heartbeat::force_tick`, which runs one full graph tick for that single project
  immediately, bypassing both the `heartbeat_enabled`/`status == running` filter and the
  per-project due-check. Verified live against the real running project — correctly ran
  the conductor and raised a second human task.
- **Per-project heartbeat interval, default 15 minutes**: `Project.heartbeat_interval_secs`
  (new field, serde-defaults to 900 for old project files with no such field, so no
  migration needed). Editable via `POST /api/projects/{id}/heartbeat-interval` (min
  5s, clamps rather than rejects). The periodic loop's own scan cadence
  (`HEARTBEAT_SCAN_INTERVAL_SECS`, default 15s) is now a separate concept from any one
  project's interval — the scan loop wakes up frequently and only actually ticks a
  project once `is_due()` says its own interval has elapsed since `last_heartbeat_at`.
- **Countdown timer** on the project detail page: computed client-side from
  `last_heartbeat_at` (or `created_at` if never ticked) + `heartbeat_interval_secs`,
  re-rendered every second independent of the 5s data poll.
- **Relative time formatting** (`formatRelative`, "4 minutes ago" / "in 3m 20s" style) —
  added to both `app.js` (dashboard action log) and `project.js` (overview table's
  created/last-heartbeat fields, heartbeat activity log). Raw ISO timestamp kept in a
  `title` attribute for hover.
- Store test coverage extended: default interval, custom interval round-trip, and the
  5s-minimum clamp.
