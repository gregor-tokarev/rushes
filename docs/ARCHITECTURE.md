# VideoCRM architecture

Desktop app (Rust, [GPUI Kit](https://gpui-kit.com) 0.7.1) for planning, scripting,
AI-editing and uploading YouTube videos. Everything is local: one SQLite database plus
a media folder per video.

## Workspace layout

```text
crates/
  core/       videocrm-core      domain model, SQLite store, documents, templates, media folders
  youtube/    videocrm-youtube   Google OAuth (loopback + PKCE), YouTube Data API v3 client, resumable upload
  pipeline/   videocrm-pipeline  AI edit pipelines: drives a coding agent (Claude Code) with the video-use skill
  app/        videocrm           GPUI Kit desktop app (the only crate that depends on gpui-kit)
emulators/    local API emulation for dev/tests: vercel-labs/emulate (Google OAuth) + custom YouTube emulator
scripts/      dev helpers (run emulators, fake agent for pipeline tests, Xvfb screenshots)
docs/         technical docs (README.md stays product-only)
```

Dependency direction: `app -> core, youtube, pipeline`. `youtube` and `pipeline` do NOT
depend on `core`; they expose their own small types and the app maps between them. This
keeps them independently testable and lets them be built in parallel.

## Threading model

`core`, `youtube` and `pipeline` are synchronous, blocking libraries (std threads, `ureq`
for HTTP, `std::process` for child processes). They never require an async runtime.
The app runs them on GPUI's background executor (`cx.background_spawn`) and streams
progress back to the UI through channels (`smol::channel` / `futures::channel::mpsc`),
consumed by a foreground task (`cx.spawn`) that updates entities and calls `cx.notify()`.
Long operations take a cancellation flag (`Arc<AtomicBool>`) and a progress callback.

### Database access

Another program (a second VideoCRM, a backup or sync tool, `sqlite3`) may hold the
database's write lock for seconds. The UI thread never waits for it:

- The database is in WAL mode and the app opens three connections to it: the UI
  thread's, the background writer's and a spare one for code outside the write queue
  (`AppState::store()`, used by tests; it waits up to 5 s for a lock, so never on the
  UI thread). WAL readers never wait for a writer, so reading rows, documents, runs and
  uploads on the UI thread stays instant during a lock.
- Every change goes through `AppState` (`state/writes.rs`): videos, properties, views,
  settings, documents, new, copied and deleted videos. While nothing is queued a change
  is written at once on the UI connection, which gives up on a lock after 100 ms.
  Otherwise it joins one ordered write queue: the cached rows show it at once (reloads
  replay the queue over what they read), and a background task writes the queue in order
  on the writer's connection, retrying every 2 s until it lands. The queue keeps only the
  newest value per video, document, view and the settings; writes whose effect depends on
  order (creating, deleting, property definitions) are never overtaken. A busy database
  ends a pass. Another failure (a full disk) holds back what depends on the failing
  write: the later writes of a video it creates or deletes, of a run or upload it
  inserts, or every later write after a property definition.
- A write that keeps failing is never dropped: the video page shows "Not saved —
  retrying" and the creator is told once. Only writes that can never succeed (the video
  was deleted) leave the queue, with a notification.
- Quitting (Ctrl+Q, closing the window, SIGTERM/SIGINT/SIGHUP) writes the queue on the
  UI thread, waiting at most 5 s in all for a lock. What still can't be written is kept
  in `unsaved-changes.json` next to the database (mode 0600) and written at the next
  start, before the settings are read (kept settings that still can't be written are
  used as queued), which says so.
- The background services' records take the same queue (`state/records.rs`): AI
  edit runs, every entry of their logs (numbered by the pipeline service), upload jobs
  and their progress, kept brief forms. The services keep them in memory, which is
  what the UI shows; the queue keeps the newest state per run and upload and every
  log entry, in order after the run's insert. A running edit or upload therefore
  never makes the UI wait for another program's lock. The rows' latest run and
  upload status follow once those writes land.

## Data locations

- Database and settings: `dirs::data_dir()/videocrm/` (`videocrm.db`, `credentials.json`).
  Overridable with env `VIDEOCRM_DATA_DIR` (used by tests and screenshots).
  The running app holds an exclusive lock on `videocrm.lock` there; a second copy on the
  same data dir shows "VideoCRM is already open" and quits without opening the database.
  `unsaved-changes.json` appears there only when the app quit while another program
  kept the database locked; the next start writes it to the database and removes it.
- Media root (default `dirs::video_dir()/VideoCRM`, fallback `~/Videos/VideoCRM`):
  one folder per video `<slug>-<short id>/` containing `footage/` (raw takes).
  video-use writes all outputs to `footage/edit/` (`final.mp4`, `project.md`, ...).
- OAuth credentials: `credentials.json`, file mode 0600. Never logged.

## core (`videocrm-core`)

- `model.rs` — the shared contract (already written; extend, do not break).
- `store.rs` — `Store` over `rusqlite` (bundled). Schema migrations via `PRAGMA user_version`.
  Videos, property defs, documents (blocks as JSON), database views, pipeline runs
  (+ append-only run events), uploads, key/value settings (JSON).
  `Store::list_video_rows()` returns `VideoRow`s with derived script stats and the
  latest run/upload status. First open seeds a default "All videos" table view and a
  "Board" view.
- `document.rs` — document logic: spoken word count, estimated runtime (150 wpm default),
  outline (headings), Markdown export/import (round-trippable for all `BlockKind`s;
  B-roll / on-screen cues use a `> [!broll]` / `> [!onscreen]` callout syntax), teleprompter
  text (spoken blocks only, inline markdown stripped).
- `templates.rs` — script templates (Tutorial, Review, Vlog, Short, Blank) producing
  `Vec<Block>` personalised with title/hook/format.
- `media.rs` — media root, per-video folder creation, footage listing (video/audio
  extensions, size, mtime), finding edit outputs (`footage/edit/final.mp4`, `preview.mp4`).
- `settings.rs` — typed `AppSettings` stored in the settings table: media root,
  YouTube backend (`Google { client_id, client_secret }` | `Emulator { google_url, youtube_url }`),
  pipeline config (agent command, video-use skill dir, extra instructions, auto-approve),
  `onboarding_completed`.

## youtube (`videocrm-youtube`)

- `YoutubeConfig` — endpoints + OAuth client. `YoutubeConfig::google(client_id, secret)`
  points at real Google; `YoutubeConfig::emulator(google_url, youtube_url)` points at the
  local emulators. Env overrides: `VIDEOCRM_GOOGLE_URL`, `VIDEOCRM_YOUTUBE_URL`.
- OAuth: installed-app flow. `begin_authorization(&config) -> PendingAuthorization`
  binds `127.0.0.1:0`, builds the consent URL (scopes: `openid email profile
  https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly`,
  `access_type=offline`, `prompt=consent`, PKCE S256, random `state`). The app opens the URL
  in the browser; `PendingAuthorization::wait(timeout, cancel)` serves the redirect, shows a
  small "you can close this tab" page, validates `state`, exchanges the code.
- `Credentials` (access/refresh token, expiry, scope) + `CredentialStore` (JSON file, 0600).
- `YoutubeClient` — refreshes the access token when expired or on 401 and reports
  refreshed credentials through a callback so the app can persist them.
  `userinfo()`, `my_channel()`, `upload_video(path, &VideoMetadata, progress, cancel)`
  (resumable protocol, chunked, resumable after interruption via the session URL),
  `resume_upload(session_url, path, ...)`, `set_thumbnail(video_id, path)`,
  `get_video(id)`, `list_my_uploads(max)`, `revoke()`.

## pipeline (`videocrm-pipeline`)

Drives [video-use](https://github.com/browser-use/video-use) through a headless coding
agent (`claude -p --output-format stream-json --verbose`). video-use is conversational:
it proposes a strategy and waits for approval, so a run has two phases:

1. **Plan** — agent inventories footage, transcribes, proposes a strategy, stops.
   Run status `Planning -> AwaitingApproval`; the strategy text is the run summary.
2. **Render** — user approves (optionally with notes) or sends feedback; the app resumes
   the same agent session (`--resume <session_id>`). Status `Rendering -> Succeeded`
   once `footage/edit/final.mp4` exists. Further feedback resumes again.

`auto_approve` collapses both phases into one prompt. `doctor()` reports whether the agent
CLI, the video-use skill, ffmpeg/ffprobe and the ElevenLabs key are available.
`scripts/fake-agent` imitates the agent's stream-json output for tests.

## app (`videocrm`)

GPUI Kit app. Main pieces:

- `AppState` entity: owns the database connections (see "Database access"), the write
  queue, cached rows/properties/views, settings, connected account; all mutations go
  through it and emit events.
- Shell: title bar, sidebar (Videos views, Pipelines, Uploads, recent pages, account,
  settings), routed main area.
- `database/` — Notion-like videos database: table view (inline cell editing, column
  menu, add property, sort/filter/search) and board view (kanban by status, drag cards).
- `editor/` — block-based rich document editor (slash menu, markdown shortcuts,
  keyboard navigation between blocks, drag to reorder, inline formatting, live stats).
- Video page: icon + title, properties panel, tabs Script / Notes / Footage & AI edit / Publish.
- Onboarding (first run) and New-video wizard (templates).
- Publish: YouTube account connect, metadata form, upload with progress, resume.
- AI edit: footage list, edit brief, run timeline (agent messages/tool calls), approve /
  feedback, output preview and "use for upload".

## Testing

- `cargo test -p videocrm-core` — store, documents, templates.
- `cargo test -p videocrm-youtube` — unit tests + integration tests against the emulators
  (`emulators/`; skipped with a message when Node is unavailable).
- `cargo test -p videocrm-pipeline` — with `scripts/fake-agent`.
- `cargo test -p videocrm` — GPUI UI integration tests (`#[gpui_kit::test]`).
- Visual checks: `scripts/screenshot.sh` runs the app under Xvfb and captures PNGs.
