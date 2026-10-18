# Edit benchmark: evaluator

`evaluate.py` measures one edited video against the raw source footage, a word-level reference
transcript (Scribe JSON) and a gold annotation. It works only from the files: if the harness
wrote an EDL, that EDL seeds the search and is reported next to the measured cuts, but it is
never trusted. `compare.py` turns many result files into markdown tables. `doubles.py` counts
the double takes a viewer hears in each edit (a line said twice because it was tried again):
it reads the words inside each range of the harness's edit list and asks a fresh model,
twice per edit by default.

Requirements: Python 3, numpy, ffmpeg/ffprobe (tested with 8.0). No other packages.

## Run

```sh
python3 scripts/bench/evaluate.py \
  --source  SRC.mp4 --reference SRC.scribe.json --gold GOLD.json \
  --output  run/edit/final.mp4 [--edl run/edit/edl.json] [--run-meta run/run.json] \
  --out     results/<harness>/<task>/<run>/result.json \
  --cache-dir /path/to/cache

python3 scripts/bench/compare.py 'results/**/*.json' --out results/table.md
```

The first evaluation against a given source decodes the whole source once and caches it in
`--cache-dir` (default `$RUSHES_BENCH_CACHE` or `~/.cache/rushes-bench-eval`): every video
frame as a 64x36 grey thumbnail, plus the audio. Later runs reuse the cache. The script prints
the `summary` block. The full detail goes to `--out`.

`compare.py` reads `harness`, `task` and `run` from each result's `run_meta`. When those are
missing it parses the path with `--path-regex` (default `<harness>/<task>/<run>/<file>.json`).
For each task it writes one table: one column per harness, and cells showing the mean over
runs with the (min..max) range.

Useful options of `evaluate.py`:

| option | default | meaning |
|---|---|---|
| `--fillers` | Russian and English hesitation patterns | Comma-separated regexes matched against lower-cased tokens with punctuation stripped. |
| `--sync-points` | 48 | Number of A/V sync sample points. |
| `--sync-crop-bottom` | 0 | Ignore this fraction of the frame bottom in video matching. Blocks that never match (captions, overlays) are already dropped automatically. |
| `--silence-db` | integrated LUFS − 25, clamped to [−60, −30] | Threshold for silencedetect. |
| `--reference-timeline` | `plain` | Clock of the reference and gold times; see below. |
| `--output-audio-clock` | `player` | How output audio is placed in time; see below. |
| `--no-video` | off | Audio-only mapping, no sync measurement. |

## How the timeline is recovered

1. **Audio.** Output and source are decoded to 16 kHz mono and turned into 10 ms log-energy
   envelopes. Each envelope is floored 45 dB below that file's speech level, and the gain
   difference is removed.
2. **Candidate lags.** A lag is source time minus output time. Candidates come from the EDL
   hint and from a global search: 3 s output windows are matched against the whole source by
   FFT normalised cross-correlation on a 20 ms envelope. A second pass with 1 s windows covers
   output speech the first pass did not explain.
3. **Segmentation.** A Viterbi pass over the candidate lags, plus an "unmatched" state with
   switch penalties, gives a 10 ms segmentation. This handles reordering and source spans used
   more than once. Silence costs nothing, so a pause inherits the lag of the speech around it.
4. **Sample-level refinement.** For each segment, waveform cross-correlation refines the lag
   overall, near the start and near the end (`lag_start_ms`, `lag_end_ms`, `lag_drift_ms`).
   Short runs whose waveform does not confirm the lag are discarded.
5. **Boundaries.** Each boundary is placed by minimising the residual of "predict with
   segment A" against "predict with segment B" around the cut. If that is ambiguous because
   the cut sits in silence, the boundary snaps to the picture cut (the largest frame change)
   when there is one. Each boundary reports its uncertainty and basis.
6. **Framing census.** The picture is compared at about 150 audio-located frames under a
   set of framings. The result is reported in `video_framing`, and later picture matching
   uses only the framings that were actually found. The framings are:
   - the same framing with a centred punch-in zoom of 1.0, 1.06, 1.12 or 1.2;
   - for an output with a different aspect (for example vertical 9:16):
     - `fill`, a centre crop of the source scaled to cover the frame. This decodes and caches
       a cropped set of source thumbnails.
     - `fit`, the whole source letterboxed in the middle band.

   If nothing matches, the picture is not used for mapping at all. The sync points come out
   unusable with a note, and the audio mapping is unchanged.
7. **Picture checks on the audio mapping.**
   - Short audio runs that the waveform or the picture contradicts are dropped.
   - Low-speech parts of every audio run are checked against the picture. A lag that was only
     inherited across a near-silent stretch is taken out where a unique source match elsewhere
     explains the picture much better for 1 s or more (`mapping.picture_verification`). An
     example is a silent screen demo followed by speech.
8. **Stretches without sustained speech, 1 s or longer,** are placed by the picture. The
   output is sampled every 0.2 s, and a small Viterbi chooses between the neighbouring audio
   lags and the source positions found by unique global nearest-neighbour matching of 32x18
   thumbnails. Such segments have `basis: video`. When the lag is the same on both sides and
   the picture agrees, the stretch is the source's own pause.
9. **Merging.** Neighbouring segments that continue the same source at the same lag
   (within 10 ms, with an output gap of up to 1 s) are merged. They are not a join and
   produce no cut edges.

### Clocks

- **Source audio** follows the container clock: timestamp gaps are filled. This is the clock
  that `trim` and `-ss` cut by.
- **Output audio** is placed the way players play it (`player`): every decoded sample back
  to back from the first audio timestamp. This is the case where `concat -c copy` of AAC
  segments makes audio drift. Pass `--output-audio-clock pts` to follow the packet
  timestamps instead. `validity.audio_samples_minus_timestamps_ms` shows how far the two
  disagree.
- **Reference and gold times** are usually made from a plain decode of the source audio,
  which ignores timestamp gaps. The evaluator measures where the source's container clock and
  that plain decode differ (`mapping.reference_clock`) and converts before comparing words
  and gold units. Use `--reference-timeline pts` if your reference is on the container clock.

## Metrics (result.json)

**`validity`**
- Codecs, width x height, `r_frame_rate` / `avg_frame_rate`, durations, and audio stream
  duration minus video stream duration in ms.
- Audio packet timestamp continuity.
- `audio_samples_minus_timestamps_ms`.
- `decode_check`: every video frame and the whole audio stream are decoded in one pass, and
  ffmpeg error lines are counted. `decodes_end_to_end` is true when that pass had no errors.

**`mapping`**
- `segments[]`: for each segment, output start/end, source start/end (container clock),
  `basis` (`audio` or `video`), lag overall / at start / at end, waveform correlation peak,
  and boundary uncertainty and basis.
- `output_unexplained_speech_s`: output speech that matches nothing in the source.
- `video_fill`: the stretches that were placed by picture.
- `edl`: for each declared range, its declared output position, the measured output position
  of its source start and end, and `residual_start_ms` / `residual_end_ms`
  (measured − declared). Also the median / p95 / max of |residual|.

**`sync`**
- Definition: error = audio source time − video source time at the same output instant, in
  ms. Negative means the sound is late relative to the picture.
- How a point is measured: points are spread over segments whose lag was confirmed on the
  waveform, staying 0.3 s away from the segment edges. At each point, 0.5 s of output frames is
  slid over source frames within ±1.5 s of the position the audio was located at, at 2 ms
  steps. The search is centred on the offset seen at the previous usable point, with a ±4 s
  fallback, and each framing found by the census is tried; the best one is reported per point
  as `view`. Source frames are interpolated between neighbours; matching uses per-block normalised
  features, and blocks that never match (captions, overlays) are dropped.
- When a point is rejected: low contrast, a second distinct minimum, or a too-wide valley
  (static picture).
- Reported: `median_ms`, `p95_abs_ms`, `max_abs_ms`, `first_ms`, `last_ms`, and the drift
  slopes `drift_ms_per_min` and `drift_ms_per_join` (OLS plus Theil–Sen). The join index counts
  real joins only, meaning a source discontinuity or an output gap.

**`gold`**
- A reference word counts as kept when at least half of its duration lies inside a kept
  source span.
- Drop unit: counted as removed when at most 20% of its duration is present. Reported per
  category and overall, by count and by duration.
- Keep unit: counted as kept when at least 80% is present. Presence is weighted by word
  duration, so trimmed inner pauses do not count. Reported for `core` and `normal`.
- `visual_demo`: the fraction of demo time present; a demo counts as kept at 50% or more.
- `one_of` (retakes): an alternative counts as present at 50% or more. The group outcome is
  exactly one, duplicate or none.
- `duplicated_source_s`: source time used more than once.

**`cut_craft`**
- Joins, and segment length min / median / count under 0.5 s.
- `clipped_words`: segment edges that keep between 10% and 90% of a reference word.
- `edges_inside_words`: every edge that falls inside a word, with the word and the kept and
  cut ms.
- `clicks`: the click statistic, with a ratio for each join. The statistic is the maximum
  2 ms first-difference energy within ±10 ms of the join, divided by the median of the same
  quantity over the surrounding ±100 ms. Its threshold is the 99.5th percentile of the same
  statistic at 4000 random points of the uncut source, so a join is flagged only when it is
  rarer than natural speech transients.

**`fillers`**
- Filler tokens kept against the total inside the source span the output draws from.
- False-start tokens (ending in `-` or `--`) kept.

**`pacing`**
- Kept words per output minute, fillers excluded.
- Silences from ffmpeg `silencedetect` (threshold above, d ≥ 1.0 s): count, total and longest.
  The same figures are repeated without the output time that maps into gold `visual_demo`
  spans (`dead_air_s` in the summary).

**`loudness`**
- ebur128 integrated LUFS, true peak in dBTP, and LRA.

**`run_meta`**
- Copied verbatim from `--run-meta`.

**`summary`**
- The flat set of key numbers that `compare.py` uses.

**`timing_s`**
- Evaluator runtime by stage.

## Validation

Every synthetic output has a known truth. Most are built from the 560 s excerpt using 21
ranges. Some ranges include gold drop units and some exclude them; one pair is reordered; one
range is a silent screen demo; one retake is kept twice. Each output has its EDL next to it.
The validation scripts are kept outside this repository because their ranges are specific to
the footage.

Two independent checks are used below:
- **Pixel identity**: for every usable sync point, the output frames are matched to source
  frames by exact thumbnail identity, and the output audio is cross-correlated directly
  against the known source position.
- **Container truth**: the per-segment truth is rebuilt from container timestamps plus pixel
  identity of each segment file.

| output | how it was made | truth | evaluator |
|---|---|---|---|
| clean, 288.7 s | one `trim`/`atrim` + `concat` filtergraph, frame-snapped ranges | EDL; sync 0 within one frame | 21/21 segments. EDL residual \|ms\| median 2.4, p95 5.6, max 6.0. Sync 36/48 usable: median −0.4, p95 8.5, max 9.2 ms; drift 0.2 ms/join. Points vs pixel identity: mean \|Δ\| 4.2 ms. Gold equals the expectation computed straight from the EDL. |
| buggy concat, 288.7 s | per-segment MP4 (AAC, 30 ms fades, `-r 60`) + `concat -c copy`, as video-use renders | container truth: drift −29.0 ms/join, last segment −579 ms | Drift −29.0 ms/join (OLS), −29.2 (Theil–Sen), r² ≈ 1. Points vs truth: median −1.0, mean \|Δ\| 4.5 ms. `audio_samples_minus_timestamps_ms` = +601. Gold unchanged. |
| injected drift, 288.7 s | like clean, but the audio of range k starts 30·k ms early | −30.0 ms/join | Drift −29.9 ms/join. Points vs truth: median −0.3, mean \|Δ\| 3.3 ms. |
| vertical fill 9:16 | centre crop of the clean output, 540x960 | as clean | Census picks `fill`. Sync 40/48 usable, median 0.9, max 7.9 ms. Mapping and gold identical to clean. |
| vertical fit 9:16 | clean output letterboxed over a blurred copy | as clean | Census picks `fit`. Sync 33/48 usable, median −0.1, max 21.8 ms. Mapping and gold identical to clean. |
| unknown framing | top-left quarter, mirrored | as clean | Census reports no known framing and sync has 0 usable points with a note. Mapping (21/21) and gold identical to clean. |
| long clean, 28.0 min | full source minus fumbles of 15 s or more, 13 ranges, one filtergraph | EDL | EDL residual median 0.8, max 9.9 ms. Sync vs pixel identity: median Δ 1.8, mean \|Δ\| 2.5, max 6.3 ms. (This render really is 10–29 ms off: VFR to CFR.) Gold: all 86 keeps kept, 12/48 fumbles removed, as constructed. |
| oracle, 17.8 min | gold keep units minus gold drop units, 86 ranges, buggy concat | gold should score about perfect | Drops 74/75 removed (one 3.3 s fumble at 22% present, borderline), core 43/43, normal 43/43, retakes exactly one 4/4, demo time 0.997. Drift −21.6 ms/join, r² ≈ 1, up to −1.93 s. |

The validation renders show what each mechanism catches:
- The silent demo in the drift renders is placed by picture.
- Picture checks, framing views and merging were validated on a real harness output
  (excerpt task, rendered frame and sample exact with alternating 1.12x punch-ins; its
  `cut.json` gives the truth). The evaluator recovers all 40 spans, places the silent demo at
  the right source position, and finds both zoom factors in use (17 and 18 usable points).

**Runtime.** The machine was shared with other jobs (load average 15–40 on 12 cores), so these
are upper bounds:
- excerpt outputs (4.8 min): 20–65 s;
- the 17.8 min output: 173 s;
- the 28 min output: 351 s, of which 294 s is the full decode of the output.

The first evaluation against a new source also decodes and caches it: about 6 min for the
33 min source under that load.


## Limitations

- **Constant tempo assumed.** Within a segment the lag is taken as constant. Speed ramps or
  time-stretching show up as `lag_drift_ms` and as extra segments; they are not modelled.
- **Framings it knows.** Video matching handles:
  - the source framing with a centred zoom of 1.0 to 1.2;
  - a centre-crop `fill` to another aspect, or a letterboxed `fit`.

  Off-centre crops, pans, zooms above 1.2, mirrored or composited pictures are not modelled.
  For those, the census reports `status` as not matched, sync points are unusable, and silent
  stretches cannot be placed by picture, but the audio mapping still works. Colour grading is
  tolerated because the features are block-normalised.
- **Audio from elsewhere.** Audio that is not from the source (music beds, generated speech)
  is reported as unexplained. Loud music under the speech degrades the audio mapping.
- **Silent segments.** Segments placed by picture take the picture's lag as their audio lag,
  because nothing in them can be heard.
- **Word timings.** They come from the reference transcript (about ±20–50 ms), so words near
  the 10% / 90% clipping bounds are noisy.
- **Sync precision per point.** It is limited by the render's own frame quantization, ±½ to 1
  frame. Medians and slopes over many points are far more precise than any single point.
- **Gold coverage.** Gold scoring covers only what the gold annotation contains. Fillers and
  pauses under 4 s are measured separately, not through gold.
