# Releasing

`.github/workflows/bundle.yml` builds the desktop bundles, starts each one once, and
publishes them. `.github/workflows/daily-release.yml` releases `main` every day it
changed.

| Trigger | Result |
| --- | --- |
| Daily, 03:30 UTC, when `main` has commits since the newest `v*` tag | Patch version bumped and tagged on `main`, then released like a pushed tag |
| Run workflow on Daily release (Actions tab) | Same as the daily run, now |
| Tag `v<version>` pushed | GitHub release `v<version>` with all bundles and generated notes |
| Run workflow on Bundle (Actions tab) | Bundles kept as run artifacts, versioned `<version>-dev.<sha>` |
| Pull request touching the workflow, `scripts/bundle-*.sh` or `assets/icon.svg` | Same as a manual Bundle run |

The Bundle workflow refuses a tag that doesn't match the workspace version in the root
`Cargo.toml` (the app shows it in Settings > About).

## Daily release

The daily run checks out `main` and runs `scripts/daily-release.sh`:

1. No commits since the newest `v*` tag: nothing to release, the run ends.
2. The workspace version is released already (`v<version>` exists): it bumps the patch
   number in `Cargo.toml` and `Cargo.lock` and commits `chore(release): <version>` as
   `github-actions[bot]`. A version bumped by hand and not tagged yet is kept.
3. It tags the commit `v<version>`, pushes the commit and the tag to `main` together, and
   calls the Bundle workflow with the tag, which builds, smoke-tests and publishes the
   release.

The push fails, and nothing is released, if `main` moved during the run; the next run
picks it up. A bundle that fails leaves its tag without a release; the next day's release
bumps past it. Daily releases only bump the patch number, and only of a plain
`<major>.<minor>.<patch>` version.

For a minor or major release, bump `version` in the root `Cargo.toml`, run
`cargo update --workspace` and commit; the next daily run (or a manual run) releases it.
To release right away by hand, tag and push:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

## Bundles

| File | Built by | Contents |
| --- | --- | --- |
| `VideoCRM-<version>-macos-universal.dmg` | `scripts/bundle-macos.sh` on `macos-15` | `VideoCRM.app` (arm64 + x86_64, macOS 10.15.7+), link to Applications |
| `VideoCRM-<version>-x86_64.AppImage` | `scripts/bundle-linux.sh` on `ubuntu-22.04` | The binary, desktop entry and icons |
| `videocrm-<version>-linux-x86_64.tar.gz` | same | `bin/`, `share/applications/`, `share/icons/` for `~/.local` or `/usr/local` |

The scripts also run locally (`scripts/bundle-macos.sh <version> <binary>...`,
`scripts/bundle-linux.sh <version> <binary>`); see their headers for the tools they need.

- **macOS**: the app is signed ad hoc and not notarized (no Developer ID), so Gatekeeper
  asks before the first start of a downloaded copy. Signing and notarizing need an Apple
  Developer account: `codesign --options runtime` with a Developer ID certificate, then
  `xcrun notarytool submit --wait` and `xcrun stapler staple` on the `.dmg`.
- **Linux**: the binary links only glibc, xcb and xkbcommon and loads Wayland, X11 and
  Vulkan (or GL) at run time, so building on Ubuntu 22.04 sets the glibc floor (2.35).
  The window's app id `videocrm` matches the desktop entry's `StartupWMClass`.
- Bundled apps start with a launcher's bare `PATH`; the app adds the usual tool folders
  itself (see `docs/pipeline.md`, Requirements).

## Smoke test

Every bundle is started once before it's published. macOS: the app opens from the
mounted disk image, and the job checks it's still running 20 seconds later. Linux: a
separate job on Ubuntu 24.04 (newer than the build's 22.04) runs the AppImage's contents
under Xvfb with Mesa's software renderer and checks that the app is running, has a
window, and drew something (the screenshot isn't blank). Screenshots are kept as the
`smoke-macos` / `smoke-linux` artifacts.

## Cost

macOS minutes count ten times on private repositories, and the macOS job compiles the
app twice (one per architecture). That's why bundles build on tags (at most one daily
release), on demand, and for bundling changes only, not on every push.
