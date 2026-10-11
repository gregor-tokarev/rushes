# Updates

A VideoCRM installed from a release keeps itself up to date with the newest
[GitHub release](releasing.md). `crates/update` (`videocrm-update`) checks, downloads,
verifies and installs; the app's `update` module (`UpdateService`) decides when, and
shows where it stands.

## What the creator sees

- A minute after VideoCRM starts, and every four hours after, it asks GitHub for the newest
  release, while **Settings › About › Update automatically** is on (the default).
- A newer release this copy can install downloads and replaces it in the background. The
  running app keeps working; a notification and a **Restart to update** button in the
  title bar offer the restart, which saves pending edits like Quit does and opens the new
  version. Restarting while an upload or an AI edit runs asks first, since it stops them.
  Not restarting is fine too: the next start is the new version.
- A newer release this copy can't install (see below) is announced once, with a
  **Download…** button that opens the release page.
- **Settings › About › Updates** shows the status (up to date, downloading with progress,
  ready, available, or why a check failed) and **Check for updates**, which works in every
  copy, including one built from source.

Automatic checks that fail stay quiet (Settings shows why); an automatic installation that
fails is announced once per version, with the download page.

## Installations

`Installation::detect` looks at the running executable:

| Installation | Detected by | Release file | Replaced by |
| --- | --- | --- | --- |
| `MacApp` | `<bundle>.app/Contents/MacOS/<exe>` | `VideoCRM-<version>-macos-universal.dmg` | Mounting it, `ditto`-copying `VideoCRM.app` beside the bundle, `codesign --verify`, then swapping the two with `renamex_np(RENAME_SWAP)` |
| `AppImage` | `$APPIMAGE`, when the executable is inside `$APPDIR` | `VideoCRM-<version>-<arch>.AppImage` | Renaming the download over the AppImage file |
| `Prefix` | `<prefix>/bin/videocrm` with `<prefix>/share/applications/videocrm.desktop` | `videocrm-<version>-linux-<arch>.tar.gz` | Unpacking it and renaming each file into the prefix, `bin/videocrm` last |

Anything else (a `cargo` build, `cargo install`) has no installation: it never checks on
its own, and a check by hand links to the release page. The release file names are the
ones `scripts/bundle-*.sh` produce; renaming them breaks updates.

A copy also downloads by hand when its user can't change the installation's folder
(`/usr/local`, `/Applications` on a managed Mac), or when macOS runs it from the disk image
or from a translocated (quarantined) location; Settings then says to move it to
Applications.

## Installing

1. `GET https://api.github.com/repos/gregor-tokarev/videocrm/releases/latest` (drafts and
   pre-releases excluded; `VIDEOCRM_UPDATE_URL` replaces the URL for tests or a mirror).
   A tag newer than the running version (or than the version this session installed) is
   an update.
2. The release file for the installation downloads into `.videocrm-update/` beside what it
   replaces (same volume, so the last step is a rename). Its size and the SHA-256 GitHub
   lists for it (`digest`) must match; an AppImage must also carry the AppImage magic bytes.
3. It's put in place as the table above says. Every step before the last works in the
   staging folder, so a failure leaves the installation as it was. The staging folder is
   removed afterwards, and before the next update if the app quit midway.
4. **Restart** starts a detached `/bin/sh` helper that waits for this process to exit (or
   60 s), then opens the installation (`open <bundle>` on macOS, the AppImage or
   `bin/videocrm` on Linux). Waiting matters: the running app holds the data folder's lock
   until it exits.

Updates trust GitHub over HTTPS: the release files are checked against GitHub's own digest,
not against a separate signing key.

## Tests

- `cargo test -p videocrm-update`: version order, release parsing, installation
  detection, and the whole update against a local stand-in for GitHub (AppImage and
  tarball installs, damaged downloads, read-only folders, rate limits), plus the relaunch
  helper.
- `cargo test -p videocrm --test update`: the service and its UI (Settings, title bar,
  notifications, automatic schedule) in a real `Workspace`.
- The Bundle workflow runs the crate's tests on macOS and Linux with
  `VIDEOCRM_UPDATE_TEST_BUNDLES` and `VIDEOCRM_UPDATE_TEST_VERSION` set, which installs the
  bundles it just built over older copies, so a release whose files the updater can't
  install fails before it's published.
