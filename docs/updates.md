# Updates

A copy of Rushes installed from a release keeps itself up to date with the newest
[GitHub release](releasing.md). `crates/update` (`rushes-update`) checks, downloads,
verifies and installs; the app's `update` module (`UpdateService`) decides when, and
shows where it stands.

## What the creator sees

- A minute after Rushes starts, and every four hours after, it asks GitHub for the newest
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
| `MacApp` | `<bundle>.app/Contents/MacOS/<exe>` | `Rushes-<version>-macos-universal.dmg` | Mounting it, `ditto`-copying `Rushes.app` beside the bundle, `codesign --verify`, then swapping the two with `renamex_np(RENAME_SWAP)` |
| `AppImage` | `$APPIMAGE`, when the executable is inside `$APPDIR` | `Rushes-<version>-<arch>.AppImage` | Renaming the download over the AppImage file |
| `Prefix` | `<prefix>/bin/rushes` with `<prefix>/share/applications/rushes.desktop`, or VideoCRM's `bin/videocrm` with `videocrm.desktop` | `rushes-<version>-linux-<arch>.tar.gz` | Unpacking it and renaming each file into the prefix, `bin/rushes` last |

Anything else (a `cargo` build, `cargo install`) has no installation: it never checks on
its own, and a check by hand links to the release page. The release file names are the
ones `scripts/bundle-*.sh` produce; renaming them breaks updates.

A copy also downloads by hand when its user can't change the installation's folder
(`/usr/local`, `/Applications` on a managed Mac), or when macOS runs it from the disk image
or from a translocated (quarantined) location; Settings then says to move it to
Applications.

## Installing

1. `GET https://api.github.com/repos/gregor-tokarev/rushes/releases/latest` (drafts and
   pre-releases excluded; `RUSHES_UPDATE_URL` replaces the URL for tests or a mirror).
   A tag newer than the running version (or than the version this session installed) is
   an update.
2. The release file for the installation downloads into `.rushes-update/` beside what it
   replaces (same volume, so the last step is a rename). Its size and the SHA-256 GitHub
   lists for it (`digest`) must match; an AppImage must also carry the AppImage magic bytes.
3. It's put in place as the table above says. Every step before the last works in the
   staging folder, so a failure leaves the installation as it was. The staging folder is
   removed afterwards, and before the next update if the app quit midway.
4. **Restart** starts a detached `/bin/sh` helper that waits for this process to exit (or
   60 s), then opens the installation (`open <bundle>` on macOS, the AppImage or
   `bin/rushes` on Linux). Waiting matters: the running app holds the data folder's lock
   until it exits.

Updates trust GitHub over HTTPS: the release files are checked against GitHub's own digest,
not against a separate signing key.

## From VideoCRM

Rushes used to be called VideoCRM. A VideoCRM copy's updater still asks
`gregor-tokarev/videocrm` (GitHub redirects the renamed repository) and looks for
VideoCRM's file names, holding VideoCRM's layout. So every release also carries them
([releasing.md](releasing.md#bundles)), with Rushes inside:

| VideoCRM copy | Release file | What it installs |
| --- | --- | --- |
| `VideoCRM.app` | `VideoCRM-<version>-macos-universal.dmg` | Rushes, as `VideoCRM.app` |
| AppImage | `VideoCRM-<version>-<arch>.AppImage` | The Rushes AppImage |
| Tarball | `videocrm-<version>-linux-<arch>.tar.gz` | Rushes as `bin/videocrm`, `videocrm.desktop` (named Rushes) and `videocrm.png` icons |

macOS calls an app by its bundle's name, so Rushes started from `VideoCRM.app` renames
the bundle to `Rushes.app` (unless a `Rushes.app` is there already), quits, and opens
again from there, before the data folder moves. A bundle it can't rename, such as one on
a disk image, keeps running under the old name. The Dock keeps an app by its path and
name, and shows a renamed one as a second icon: Rushes started from `Rushes.app` points
a Dock icon for `VideoCRM.app` beside it at itself, under its name, and restarts the Dock
to show that (`crates/update/src/dock.rs`).

On Linux, Rushes under VideoCRM's file names detects its installation like any other, and
its next update puts Rushes's names in place: in a prefix `bin/videocrm`,
`videocrm.desktop` and the `videocrm.png` icons are removed once Rushes's files are in.
Restart opens `bin/rushes`. An update of a `VideoCRM.app` still there renames it like the
first start does.

The data folder moves at the first start of Rushes (see
[ARCHITECTURE.md](ARCHITECTURE.md#data-locations)). Once no VideoCRM copies are left,
drop the VideoCRM files from `scripts/bundle-*.sh`, their checks in
`crates/update/tests/updater.rs`, and the `LEGACY_*` names.

## Tests

- `cargo test -p rushes-update`: version order, release parsing, installation
  detection, and the whole update against a local stand-in for GitHub (AppImage and
  tarball installs, damaged downloads, read-only folders, rate limits, a VideoCRM
  tarball's names replaced), plus the relaunch helper.
- `cargo test -p rushes --test update`: the service and its UI (Settings, title bar,
  notifications, automatic schedule) in a real `Workspace`.
- The Bundle workflow runs the crate's tests on macOS and Linux with
  `RUSHES_UPDATE_TEST_BUNDLES` and `RUSHES_UPDATE_TEST_VERSION` set, which installs the
  bundles it just built over older copies (VideoCRM's names among them) and checks the
  VideoCRM files' layout, so a release whose files the updater can't install fails before
  it's published.
