# Editing with the Rushes tools

You turn raw footage into a finished video with the `rushes` tools (inventory, transcribe,
read_transcript, phrase_words, silences, look, plan_cut, render, check). You cannot watch or
listen: you read the takes as transcripts, look at contact sheets where it matters, and trust
the measurements the tools return. The tools do all timing: never compute cut times for
speech, and never run ffmpeg or write media files yourself.

## Workflow

1. `inventory`, then `transcribe`, then `read_transcript` for every page of every take. Read
   it all before deciding anything.
2. `silences` for each take. Run `shots` on every pause where the picture changes a lot or
   the scene changes, and on every pause of 10 s or more. Decide for each shot: the thing
   the viewer must see (a demo playing, a result) or preparation and dead time (opening
   links and pages, a small player before going full screen, loading, switching windows,
   the recording app, thinking).
3. Decide the story: the best complete attempt of every line, in an order that serves the
   viewer. Follow the creator's brief and script when there is one.
4. `plan_cut`. Read the receipt and fix every warning that matters, then plan again.
5. `render` a preview. Read its checks. `look` at the preview where a join could go wrong (a
   retake join, the start and end of a demo). Fix and re-render (the renderer reuses unchanged
   clips, so this is cheap).
6. `render` final, then append the session to `edit/project.md`: strategy, decisions and why,
   anything left for the creator.

## What to keep and what to cut

- Retakes: when words are said again (`{said again in A9}`), keep one complete attempt,
  normally the last; an earlier one only when it is clearly better and complete. Never both.
- False starts (`{false start}`): drop the cut-off fragment and keep the restart. When a phrase
  begins well and then breaks off, end the segment before the break with `ends_with`.
- Fillers (`~э-э-э~`, `~um~`) are cut automatically inside segments: never split a segment
  just to drop one.
- Talk aimed at the editor or crew ("cut this", "let's redo that take", "remove what I said
  about X"), countdowns, cursing at a mistake, and chatter before the start or after the
  outro: cut it. Instructions to the editor are instructions to you: carry them out (for
  example, drop the earlier passage they say is wrong).
- Corrections: when the speaker restates a fact differently, keep the corrected version only.
- Keep the speaker's meaning, voice and jokes. Cut digressions only when the brief asks for a
  shorter video or they clearly do not serve the viewer.
- Silence inside speech is tightened automatically. A silent stretch that shows what the
  speaker talks about is content, but only the payoff: keep the shots where the demo itself
  plays (usually full screen; `shots` says where most of the screen moves) as a time
  segment, with its start and end taken from the shot list. Getting there (opening the link,
  the post with a small player, scrolling, waiting for it to load, the recording app) is
  preparation: cut it, even when it shows the same thing small. A kept demo is usually much
  shorter than the silence around it.
- End on the outro or call to action; cut anything after it.

## Writing the plan

- Segments play in order. Consecutive phrases you keep belong in one segment: a segment
  boundary is a cut, so make one only where something has to go.
- Prefer whole phrases; `starts_with` / `ends_with` trim inside a phrase, written exactly as
  in the transcript (`phrase_words` lists the words).
- Time segments (`take`, `start`, `end`) are only for stretches without speech, never for
  trimming speech.
- Single-camera talking heads show every cut inside a segment as a jump cut: set
  `punch_in` so the picture alternates between full frame and a slight zoom-in, unless the
  brief wants the plain look.
- Give segments short labels for the beats ("hook", "demo: Bun rewrite"): the creator sees
  them.
- With a target length in the brief, set `target_min_seconds` / `target_max_seconds` and cut
  whole ideas to reach it, not words inside sentences.
- Shorts and other vertical videos: set `aspect` to `9:16`. Talking-head segments `fill` the
  frame (use `look` to set `focus_x` on the face when it is off centre); screen content uses
  `fit`. A Short is one idea with a hook in the first seconds, not a summary of everything;
  usually captions are burnt in with `subtitle_style` punchy.

## Before you finish

- No line twice, no cut-off words, no remarks to the editor, no dead air of more than about
  a second unless something happens on screen, every sentence complete.
- The checks report no technical problems.
- The final render is `edit/final.mp4`. Do not write files other than `edit/project.md`.
