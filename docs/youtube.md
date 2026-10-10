# YouTube integration

VideoCRM signs in with Google (installed-app OAuth: loopback redirect + PKCE) and uploads
through the YouTube Data API v3 resumable upload protocol. The code lives in
`crates/youtube` (`videocrm-youtube`); local emulators for both services live in
`emulators/`.

## Developing against the emulators

Requires Node.js 24 or newer.

```bash
scripts/emulators.sh
```

The script installs `emulators/node_modules` on first run (`npm ci`) and starts, in the
foreground:

| Service | URL | What it is |
|---|---|---|
| Google | `http://127.0.0.1:4002` | Built-in Google emulator from [`emulate`](https://github.com/vercel-labs/emulate): OAuth 2.0 / OIDC consent page, token endpoint, userinfo, revoke |
| YouTube | `http://127.0.0.1:4020` | Custom emulator (`emulators/youtube.ts`): channels, resumable video uploads, videos, thumbnails, uploads playlist |

Ports come from `GOOGLE_EMULATOR_PORT` and `YOUTUBE_EMULATOR_PORT`. Point the app at the
emulators with **Settings → YouTube → Developer: local emulator** (a switch, folded away by
default; a fresh install connects to Google), or with environment variables, which win over
the settings:

```bash
VIDEOCRM_GOOGLE_URL=http://127.0.0.1:4002 VIDEOCRM_YOUTUBE_URL=http://127.0.0.1:4020 cargo run
```

Signing in opens the emulator's account picker in the browser. Seeded accounts:

| Account | Name | Channel |
|---|---|---|
| `creator@example.com` | Demo Creator | `@creator` |
| `editor@example.com` | Second Channel | `@editor` |
| `testuser@gmail.com` | Test User | built into the Google emulator |

A channel is created the first time an account calls the YouTube emulator. State lives in
memory and resets when the emulators stop.

### Emulator behaviour

- Every YouTube API request needs `Authorization: Bearer <token>`; the token is checked
  against the Google emulator's userinfo endpoint (cached for 5 s). As on Google, PUTs to
  an upload session URL need no token: the session URL itself is the credential. Errors use Google's
  shape: `{"error": {"code", "message", "errors": [{"reason", "domain", ...}], "status"}}`.
- Uploads follow the resumable protocol: `POST /upload/youtube/v3/videos?uploadType=resumable`
  returns the session URL in `Location`; `PUT` chunks with `Content-Range`, `308` +
  `Range: bytes=0-N` while incomplete, `Content-Range: bytes */total` asks for the
  status. Non-final data is persisted in multiples of 256 KiB, like Google's servers.
  Video bytes are spooled to a temp directory; the state only keeps size and SHA-256.
- Processing: a finished upload reports `processingStatus: processing` and
  `uploadStatus: uploaded`, then `succeeded` / `processed` after
  `YOUTUBE_EMULATOR_PROCESSING_MS` (default 3000).
- Quota: the documented costs (`videos.insert` 1600, update/delete/thumbnails 50, lists 1)
  count against `YOUTUBE_EMULATOR_DAILY_QUOTA` (default 10,000, reset at midnight Pacific
  time) and fail with `quotaExceeded` once used up.
- Scheduled publishing: a private video with `publishAt` turns public when that time passes.

Test helpers on the YouTube emulator (no auth):

- `GET /_emulator/uploads`: completed uploads with byte count, SHA-256, content type and
  metadata.
- `POST /_emulator/faults`: make the next matching requests fail. Examples:
  - `{"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 2}`:
    fail two chunk PUTs.
  - `{"method": "POST", "path": "/upload/youtube/v3/videos", "status": 403, "reason": "quotaExceeded"}`
  - `{"path": "/upload/youtube/v3/videos", "persistBytes": 262144}`: the next chunk keeps
    only 256 KiB and answers 308, as if the connection dropped.
- `DELETE /_emulator/faults`: clear pending faults.
- `GET /_emulate`: the emulate inspector (requests, routes, state, reset).

Known differences from real Google:

- The Google emulator maps any unknown bearer token to its first seeded user. The YouTube
  emulator therefore also requires tokens to look like the ones the Google emulator mints
  (`google_…`), so garbage tokens get `401`, but a revoked access token keeps working there.
  Revoking does delete the refresh token, so the next refresh fails with `invalid_grant`.
- Emulator access tokens never expire; tests force a refresh by setting `expires_at` in
  the past.
- Scopes are not enforced, and custom thumbnails work without channel verification.

### Tests

```bash
cargo test -p videocrm-youtube          # unit tests + end-to-end tests that start the emulators
cargo clippy -p videocrm-youtube --all-targets
cd emulators && npm test                # node:test suite for the YouTube emulator
cd emulators && npm run typecheck
```

The end-to-end tests (`crates/youtube/tests/emulator.rs`) start their own emulator pair on
free ports for every test, drive the consent page like a browser, and stop the emulators
when the test ends. Without Node or `emulators/node_modules` they print a notice and pass
without running.

## Using the client from the app

All calls block; run them on a background thread and stream progress back to the UI.

1. Optionally `check_client(&config)` first: it posts a made-up authorization code to the
   token endpoint, so an unknown client ID or a wrong secret fails with
   `Error::is_invalid_client()` before any browser opens (a known client gets
   `invalid_grant`, which counts as fine), and an unreachable server with
   `Error::is_network()`.
   `begin_authorization(&config)` binds `127.0.0.1:<random port>` (it refuses a Google
   config without client secret); open `pending.authorize_url()` in the browser;
   `pending.wait(timeout, &cancel)` serves the redirect, shows a "You're signed in" page
   and returns `Credentials`. Requests without this sign-in's `state` get an error page
   and are ignored. Check
   `credentials.has_scope("https://www.googleapis.com/auth/youtube.upload")`: Google lets
   the user untick individual scopes.
2. `YoutubeClient::new(config, credentials, on_refresh)`: pass an `on_refresh` that calls
   `CredentialStore::save_credentials` on a clone of the app's one store, so refreshed
   tokens survive restarts. The client refreshes 60 s before expiry and once after a
   `401`.
3. `Account::new(&client.userinfo()?, client.my_channel().ok().as_ref(), client.config())`
   and `CredentialStore::save` persist the login (`credentials.json`, mode 0600). The
   account carries the email, name and picture plus the channel's id, title, handle,
   thumbnail, subscriber count (`None` when hidden) and public video count; files saved
   before those fields existed still load.
4. `upload_video(path, &metadata, &mut progress, &cancel)`: store the `session_url` from
   `UploadProgress::Started`. After a cancel, crash or network failure,
   `resume_upload(session_url, path, ...)` continues from the bytes YouTube already has
   (only for session URLs on the configured upload server). Transient failures (5xx, 408,
   429, 403 rate limits, dropped or stalled connections) are retried with exponential
   backoff from the server's offset (2, 4, 8, 16 and 32 s by default, about a minute in
   all); the retry budget resets whenever the server confirms new bytes. Each retry is
   reported before its wait as `UploadProgress::Retrying { attempt, max_retries, wait,
   cause }`, where `cause` is `Network`, `Server` or `RateLimited`. Cancel takes effect at
   the next read of the chunk being sent. `Error::SessionExpired` means start over.
5. `Error::needs_sign_in()` tells when to ask the user to reconnect (revoked or expired
   grant, or a `403 insufficientPermissions` because a YouTube scope was unticked);
   `quotaExceeded`, `uploadLimitExceeded`, `insufficientPermissions`, rate limits and server
   errors (5xx) have user-facing messages; `Error::is_network()` tells a missing connection.

## In the app

`crates/app/src/publish/` owns everything YouTube in the UI. Other modules use its public
views and the `YoutubeService` global.

| Piece | File | What it does |
|---|---|---|
| `YoutubeService` | `service.rs`, `upload.rs` | Global entity (`YoutubeService::init(state, cx)`, `YoutubeService::global(cx)`): connection, credentials, upload jobs |
| `PublishPanel::new(video_id, window, cx)` | `panel.rs` | Publish tab of a video page |
| `UploadsPage::new(window, cx)` | `uploads_page.rs` | All upload jobs, newest first |
| `YoutubeSettings::new(window, cx)` | `settings.rs` | YouTube section of Settings and onboarding |
| `AccountBadge::new(window, cx)` | `badge.rs` | Sidebar footer account; click opens `Route::Settings` |
| rules, chapters, progress maths | `rules.rs`, `chapters.rs`, `transfer.rs` | Pure functions, unit tested |

The views draw Lucide icons outside GPUI Kit's default set (`Film`, `Image`, `ListOrdered`,
`CloudUpload`, `CirclePlay`, `CirclePause`, ...): the application registers
`gpui_kit::assets::AllAssets`, which serves the whole Lucide catalog.

### Connection

- The backend comes from `AppSettings.youtube` through `publish::backend_config`. The
  default is Google with empty client fields; Google needs both fields and a client ID
  ending in `.apps.googleusercontent.com` (otherwise Connect stays disabled and says why);
  the emulator falls back to the default URLs for empty fields; `VIDEOCRM_GOOGLE_URL` /
  `VIDEOCRM_YOUTUBE_URL` override both (the settings section says so).
- The settings section (Settings → YouTube and onboarding step 3) reads top to bottom: the
  OAuth client (setup steps with console links, folded; client ID and secret), the
  connection with Connect, and what Google will show once Connect is usable (unverified-app
  warning, permission boxes, 7-day reconnect in Testing, private uploads until the audit).
  The emulator is a switch under the folded **Developer: local emulator**; turning it on
  puts the emulator addresses where the client fields were. Signed in, the section shows
  the account first. The service re-reads it on `AppEvent::SettingsChanged`; a different backend signs
  out, because tokens belong to the backend that issued them (`Account::backend`).
- `connection()` is `Disconnected | Connecting { authorize_url } | Connected(Account) |
  NeedsSignIn(Account) | Failed(message)`; the service emits `YoutubeEvent::ConnectionChanged` and notifies on
  every change. `connect` runs the sign-in on a `youtube-sign-in` thread. Before the
  browser opens it checks that the sign-in can work: for the emulators both must answer
  (the same probe as **Check**), and `check_client` must accept the OAuth client. Either
  failure ends the sign-in at once with a sentence saying what to fix ("Google doesn't
  recognise this client ID and secret…", "Can't reach Google…", "Neither emulator is
  answering…"). Then `begin_authorization` and the wait (up to 5 minutes) run, the consent
  page opens with `cx.open_url` and its URL is exposed for **Copy link**; `cancel` sets the
  wait's cancel flag. While waiting, the panel says to finish in the browser and, for
  Google, that "Error 401: invalid_client" there means a wrong client ID or secret.
- The sign-in thread only produces the client and account. The service saves the login
  when it accepts the result, so a cancelled or superseded sign-in leaves no file behind.
  Refreshed tokens go to the file only while that sign-in lasts: a request still running
  after **Disconnect** cannot write the tokens back.
- At startup the saved login is restored without network access, then the account is read
  again in the background (it is also re-read after each upload finishes processing) so
  the subscriber and video counts stay current. Rejected tokens (`needs_sign_in`, such as
  the weekly expiry of apps in Testing) turn the connection into `NeedsSignIn(account)`:
  the settings, the Publish tab and the sidebar badge keep naming the account ("Sign in
  again · Reconnect to keep uploading as Demo Creator (@creator)") with a **Reconnect**
  button, and the badge's dot turns amber.
- The channel picture is downloaded once per URL into `<data dir>/avatars/` for the
  account card and the sidebar badge; without it the avatar shows initials.
- `disconnect` cancels running uploads (they stay resumable), removes the credentials file
  and revokes the grant in the background.

### Uploads

- `start_upload(video_id, cx)` checks `blocking_reasons` first: connected, a video file that
  exists, title 1–100 characters without `<` or `>` (empty falls back to the video
  title), description up to 5,000 bytes, tags up to 500 characters counted the way
  YouTube does (commas between tags, quotes around tags with spaces), and a schedule only
  for private videos and in the future. The Publish tab lists the same reasons under the
  disabled **Upload to YouTube** button.
- Each attempt is an `UploadJob` in the store: `Pending` → `Uploading { sent, total }`
  (written in 10 % steps, so the videos table and a crash show roughly how far it got) →
  `Processing { video_id }` → `Done`, or `Failed` / `Cancelled`. The session URL is stored
  as soon as YouTube opens the session.
- The transfer runs on a `youtube-upload` thread. Progress reaches the UI at most ten
  times a second (`transfer::Throttle`); `Transfer` averages the speed over the last five
  seconds and derives the time left.
- After the bytes arrive the video gets `video.youtube = YoutubeLink { .. }` and its status
  becomes `Scheduled` (with a publish time), `Published` (public or unlisted) or stays as
  it was (private). A scheduled upload also sets the video's Publish date to the local
  date of the publish time: that is when YouTube publishes it, so the planner follows. The custom thumbnail is set next; a failure there is a warning
  notification, not a failed upload, and its reason stays on the Upload card and the
  Uploads page for the rest of the session ("Thumbnail not set"; `403 forbidden` becomes
  `Error::ThumbnailsNotAllowed`: custom thumbnails need a channel verified at
  youtube.com/verify). `get_video` is polled every 5 s for about two minutes to report
  processing → ready.
- The Upload card of an uploaded video says who can watch it as YouTube reports it
  (`refresh_youtube_video`, one `videos.list` when the tab opens or connects, and every
  processing poll): "Private · goes public Mon, 12 Oct at 09:00", "Unlisted", ... It links
  to the video and to YouTube Studio, and the form says that its changes apply to the next
  upload. When YouTube answers that it has no such video (deleted in YouTube Studio, or
  removed by YouTube) and the connected channel is the one it was uploaded to, the video's
  link is dropped (`is_off_youtube`): the card says "Not on YouTube anymore" and offers a
  plain **Upload to YouTube**, the Uploads row stops linking to it and the videos table
  shows no YouTube state. A failed lookup (offline) keeps the stored link. There is no "update on YouTube": `videos.update` needs the broader `youtube`
  scope, which VideoCRM does not ask for.
- While the worker retries a passing failure, the Upload card and the Uploads page say so
  ("YouTube isn’t responding · Retrying (2 of 5)…", or "Can’t reach YouTube" when offline)
  and the sidebar badge reads "Retrying the upload…", until the upload gets further than
  before. Meanwhile the progress shows what YouTube confirmed (`YoutubeService::progress`):
  after a failed request the worker reports `Bytes` back at the server's offset before the
  retry, and bytes in flight during a retry don't count. A failure reads as a
  plain sentence (`describe_error`; the technical error goes to stderr) and stays
  resumable when YouTube had opened a session.
- Deleting a video cancels its running upload (the delete confirmations say so, through
  `publish::delete_upload_notice`) and drops its jobs from the service, as the store
  deletes them with the video. If the last chunk was already on its way and YouTube
  finishes the upload anyway, a notice links to the video so it can be removed in Studio.
- `cancel_upload` stops at the next chunk read and keeps the session; `resume_upload`
  continues from the offset YouTube reports. A session continues the bytes it was opened
  for, so each job records its file's size and modification time (`UploadJob::file_stamp`,
  taken when the upload starts). Resume is offered and allowed only while the job's file
  is still the video's chosen file and its stamp is unchanged: choosing another file
  (also AI edit's "Use for upload") or re-exporting the file drops the session
  (`check_stopped_uploads` runs on data changes, when the window is activated and before
  every resume), and the Upload card offers a fresh upload with a note about the stopped
  one. A session YouTube refuses on resume (`400`, such as `badContent` for a file of
  another length, or `404`/`410`), or one the current connection can't use (another
  backend's after a switch), is dropped as well: "YouTube can’t continue this upload.
  Start over…". Starting a new upload of a video (**Start over**, **Upload again…**)
  drops the sessions of its stopped uploads, and `resume_upload` refuses while another
  upload of the same video runs, so one video never goes up twice by accident. The
  video file can't be changed while it uploads. At startup, jobs a previous run left
  `Pending` or `Uploading` become `Failed { "Interrupted — you can resume" }` (resumable
  when a session URL was stored) and jobs left `Processing` become `Done`.

### Publish tab

- Form edits are saved into `video.publish` 500 ms after the last change (and when the tab
  closes or the app quits). Only the fields the form changed are written, so changes made
  elsewhere meanwhile, such as the final cut AI edit picks or "Use for upload", stay; they
  show up in the form at once, except in fields with edits of its own waiting.
- **Insert chapters from script** turns the Script document's Heading 2 blocks into a
  `Chapters:` list with timestamps from the spoken words before each heading
  (`videocrm_core::document`, 150 wpm). YouTube wants the first chapter at 0:00 and none
  shorter than 10 s, so spoken words before the first heading become "Intro" and shorter
  sections give their slot to the next heading. Inserting again replaces the earlier list.
  The timestamps are estimates; the button says so. YouTube ignores lists of fewer than
  three chapters and Shorts have none, so the button is disabled then and says why; when
  the chosen file is shorter than the list (its last chapter needs 10 s before the end),
  a warning says so.
- **Schedule** forces private (YouTube makes the video public at the publish time) and
  restores the earlier visibility when turned off.
- The Upload card lists what blocks an upload in every state, so a disabled **Upload to
  YouTube**, **Upload again** or **Start over** always says why. A chosen file counts as
  missing only once the background check finds it gone.
- The file menu offers the AI edit outputs (`final.mp4`, then `preview.mp4`), then the
  footage videos, then **Choose file…** (`cx.prompt_for_paths`; on Linux that needs the
  desktop portal). When no file chooser opens, "Couldn't open the file chooser" asks for
  the path instead (`~` expands; the file must exist, and be a JPG, PNG, GIF or BMP for the
  thumbnail). Files can also be dropped on the Video file and Thumbnail cards. The list refreshes when the window is
  activated again. Size, duration and resolution come from `videocrm_pipeline::probe_media`
  on the background executor.

### Trying it

```bash
scripts/emulators.sh &        # Google on :4002, YouTube on :4020
VIDEOCRM_DATA_DIR=$(mktemp -d) cargo run -p videocrm --example publish_demo [-- --dark] [--video <title>]
```

`publish_demo` mounts the Publish tab, the Uploads page, the YouTube settings and the badge
on a sample workspace (the emulator backend, unless a Google client is set up there).
Without a browser, use **Copy link** and finish the consent with any HTTP client: fetch
the link, submit the account's form (`email` field) and follow the redirect to `127.0.0.1`.

### App tests

```bash
cargo test -p videocrm --lib publish        # validation, chapters, progress and formatting
cargo test -p videocrm --test publish       # YoutubeService against the emulators
cargo test -p videocrm --test publish_ui    # the views in headless windows, clicks and typing
```

`tests/publish.rs` covers sign-in (consent posted like a browser), restart, disconnect,
cancelling a sign-in, an upload verified byte for byte through `/_emulator/uploads`, a
scheduled upload, cancel and resume, validation, interrupted-job recovery and backend
switches, and deleting a video while it uploads. `tests/publish_ui.rs` drives the form,
chapters, Uploads page, settings and badge, connect-and-upload from the Publish tab, the
retry state during an outage, the done card, an expired sign-in, file drops, and a stopped
emulator failing fast. Both start their own emulators and skip with
a notice without Node.

## Connecting a real YouTube account

VideoCRM does not ship shared Google credentials: every user creates their own OAuth
client in Google Cloud (free) and pastes it into VideoCRM. It takes about ten minutes.

1. **Create a project.** Open <https://console.cloud.google.com/>, use the project picker
   at the top, choose **New project**, name it (for example "VideoCRM") and create it.
   Make sure it is selected.
2. **Enable the YouTube Data API v3.** Go to **APIs & Services → Library**, search for
   "YouTube Data API v3", open it and click **Enable**
   (<https://console.cloud.google.com/apis/library/youtube.googleapis.com>).
3. **Configure the consent screen.** Open **Google Auth Platform**
   (<https://console.cloud.google.com/auth/overview>) and click **Get started**:
   - App name "VideoCRM", your email as user support email.
   - Audience: **External**.
   - Contact information: your email. Accept the policy and click **Create**.
4. **Add the scopes** (recommended). In **Data Access → Add or remove scopes**, add
   `https://www.googleapis.com/auth/youtube.upload` and
   `https://www.googleapis.com/auth/youtube.readonly` (`openid`, `email` and `profile` need
   no setup). Save.
5. **Add yourself as a test user.** In **Audience → Test users → Add users**, enter the
   Google account that owns your YouTube channel. Leave the publishing status on
   **Testing**: only test users can sign in, which is all you need.
6. **Create the OAuth client.** In **Clients → Create client**, choose application type
   **Desktop app**, name it "VideoCRM desktop" and click **Create**. Copy the **Client ID**
   and **Client secret** (or download the JSON) right away: the console may not show the
   secret again.
7. **Connect in VideoCRM.** Open **Settings → YouTube**, paste the client ID and client
   secret, and click **Connect**. VideoCRM first checks the client with Google: a typo in
   either value is reported right away. Your browser
   opens Google's sign-in page (if it does not, for example on a remote machine, use
   **Copy link** and open the link in a browser on the same computer):
   - Pick the test-user account.
   - Google warns "Google hasn't verified this app". Click **Continue**. This is expected
     for a personal OAuth client.
   - Leave all permission boxes ticked, especially "Manage your YouTube videos".
   - When the page says "You're signed in", return to VideoCRM.

### Limits to know about

- **Quota.** A project gets 10,000 units per day by default, reset at midnight Pacific
  time. Each upload (`videos.insert`) costs about 1,600 units, so roughly six uploads a
  day; setting a thumbnail costs 50, reading channel or video data costs 1. More quota
  needs the YouTube API Services audit and quota extension form.
- **Unverified projects upload private.** Videos uploaded through API projects created
  after 28 July 2020 that have not passed YouTube's API compliance audit are locked to
  private, whatever privacy you choose, and the lock cannot be lifted in YouTube Studio
  or appealed. Only uploads made after the project passes the audit
  (<https://support.google.com/youtube/contact/yt_api_form>) keep their privacy setting.
  Until then, treat VideoCRM uploads as private drafts and upload the public version
  through YouTube Studio.
- **Weekly reconnect while in Testing.** Google expires refresh tokens of "Testing"
  projects with an external audience after 7 days. VideoCRM then asks you to connect again.
  Publishing the app (Audience → **Publish app**) removes the 7-day limit; the unverified
  warning stays.
- **Custom thumbnails** require a verified YouTube channel (phone verification at
  <https://www.youtube.com/verify>); otherwise YouTube rejects `thumbnails.set` with
  `403 forbidden`.
- **Upload limits.** YouTube caps how many videos a channel can upload per day
  (`uploadLimitExceeded`); the cap is lower for new or unverified channels.
