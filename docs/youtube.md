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
emulators with the "Emulator" YouTube backend in settings, or with environment variables:

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

- Every YouTube request needs `Authorization: Bearer <token>`; the token is checked
  against the Google emulator's userinfo endpoint (cached for 5 s). Errors use Google's
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

1. `begin_authorization(&config)` binds `127.0.0.1:<random port>` (it refuses a Google
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
   and `CredentialStore::save` persist the login (`credentials.json`, mode 0600).
4. `upload_video(path, &metadata, &mut progress, &cancel)`: store the `session_url` from
   `UploadProgress::Started`. After a cancel, crash or network failure,
   `resume_upload(session_url, path, ...)` continues from the bytes YouTube already has
   (only for session URLs on the configured upload server). Transient failures (5xx, 408,
   429, 403 rate limits, dropped or stalled connections) are retried with exponential
   backoff from the server's offset; the retry budget resets whenever the server confirms
   new bytes. Cancel takes effect at the next read of the chunk being sent.
   `Error::SessionExpired` means start over.
5. `Error::needs_sign_in()` tells when to ask the user to reconnect (revoked or expired
   grant, or a `403 insufficientPermissions` because a YouTube scope was unticked);
   `quotaExceeded`, `uploadLimitExceeded` and `insufficientPermissions` errors have
   user-facing messages.

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
7. **Connect in VideoCRM.** Open **Settings → YouTube**, choose the Google backend, paste
   the client ID and client secret, and click **Connect**. Your browser opens Google's
   sign-in page:
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
