<p align="center">
  <img src="assets/icon.svg" width="96" alt="VideoCRM icon">
</p>

<h1 align="center">VideoCRM</h1>

<p align="center">A desktop production home for YouTube creators: plan videos, write scripts, edit with AI and upload.</p>

<p align="center">
  <a href="docs/youtube.md#connecting-a-real-youtube-account">Connect YouTube</a>
  ·
  <a href="docs/pipeline.md#requirements">Set up AI editing</a>
  ·
  <a href="docs/ARCHITECTURE.md">Architecture</a>
</p>

- **Plan** every video in a Notion-style database, as a table or a board.
- **Write** scripts in rich documents with B-roll and on-screen cues, word count and runtime.
- **Edit with AI**: hand your raw takes and script to [video-use](https://github.com/browser-use/video-use), approve its plan, get a cut.
- **Upload** to your YouTube channel from the video's page, with resumable uploads.

Everything stays on your computer: one local database and a folder per video.

## Build from source

Install Rust. On Linux, also install a C/C++ toolchain, CMake, `pkg-config`, and the
development libraries for Fontconfig, FreeType, X11, Wayland and xkbcommon.

```sh
cargo run --release
```
