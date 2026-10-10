# Releasing

`.github/workflows/bundle.yml` builds the desktop bundles, starts each one once, and
publishes them.

| Trigger | Result |
| --- | --- |
| Tag `v<version>` pushed | GitHub release `v<version>` with all bundles and generated notes |
| Run workflow (Actions tab) | Bundles kept as run artifacts, versioned `<version>-dev.<sha>` |
| Pull request touching the workflow, `scripts/bundle-*.sh` or `assets/icon.svg` | Same as a manual run |

To release, bump `version` in the root `Cargo.toml` (the app shows it in Settings > About),
commit, then tag and push:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

The workflow refuses a tag that doesn't match the workspace version.

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

Each job starts the bundle it built (macOS: from the mounted disk image; Linux: the
AppImage's contents under Xvfb with Mesa's software renderer), waits 20 seconds, checks
that the app is still running (and, on Linux, that it has a window), and keeps a
screenshot as the `smoke-macos` / `smoke-linux` artifact.

## Cost

macOS minutes count ten times on private repositories, and the macOS job compiles the
app twice (one per architecture). That's why bundles build on tags, on demand, and for
bundling changes only, not on every push.
