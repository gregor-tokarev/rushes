# VideoCRM — agent notes

Read `docs/ARCHITECTURE.md` first: crate boundaries, threading model, data locations and
test commands live there.

## GPUI Kit

- The app uses `gpui-kit = "0.7.1"` (GPUI snapshot `gpui-pre 0.3.8`). The authoritative API
  is the exact published source in the cargo registry:
  `~/.cargo/registry/src/index.crates.io-*/{gpui-kit,gpui-component,gpui-base}-0.7.1` and
  `gpui-pre-0.3.8`. Search it for real signatures; never invent an API or port one from
  React/CSS/older GPUI by analogy.
- Load the vendored skills before UI work: `.claude/skills/gpui-kit` (coding guides, GPUI
  mechanics, testing) and `.claude/skills/gpui-kit-design-guides` (visual and interaction rules).
- Only `crates/app` depends on `gpui-kit`. `core`, `youtube` and `pipeline` stay UI-free and
  synchronous.

## Conventions

- README.md is product-only (what it is, why use it). Technical docs go in `docs/`.
- Conventional commit messages (`feat(core): ...`). No co-author trailers.
- Branch names follow conventional branch (`feature/...`, `fix/...`).
- Visual verification: the machine has no display; run the app under Xvfb
  (`scripts/screenshot.sh`) and inspect PNGs. Drive input with `xdotool`.
- Cargo builds are serialized machine-wide; a build waiting on
  `/run/user/$(id -u)/cargo-build-slot.log` is queued, not stuck.
