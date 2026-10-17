#!/usr/bin/env python3
"""Blind editorial comparison of two harnesses' cuts by a fresh model.

    python3 scripts/bench/judge.py --bench <dir> --task <task> [--a rushes --b video-use] [--runs 1,2]
        [--model <m>] [--out <file.json>]

For each run number, both outputs are turned into what a viewer hears: the reference
transcript's words inside each measured output segment (result.json, written by
evaluate.py), in output order, with a mark at every cut and a line for every stretch
without speech. A fresh `claude -p` (no tools) gets the source transcript, the brief and
the two edits as "Edit 1" and "Edit 2", and answers in JSON. Each pair is judged twice
with the order swapped, so a preference for a position cancels out.
"""

import argparse
import json
import random
import subprocess
from pathlib import Path

RUBRIC = """You are reviewing two edits of the same raw recording for a YouTube creator. You
cannot see the video: you get what the viewer hears, as transcripts. The source transcript
shows everything that was recorded, with times in seconds. Each edit lists its segments in
output order: the output time, the source range it comes from, and the words; "‖" marks a
cut, and "[no speech …]" is a stretch kept for what is on screen.

The creator's brief:
{brief}

Judge each edit on:
1. completeness: every point of the argument is kept, nothing the viewer needs is lost
   (including screen demos the speech refers to);
2. cleanliness: no retake or repeated line, no false starts, no remarks to the editor or
   off-topic outtakes, no corrected wrong claim kept, no long dead air;
3. flow: the joins read naturally, sentences are complete, the order makes sense;
4. brief: follows the brief (length, format).

Score each 1-10 for each edit, then pick the better edit overall (or "tie" only if they are
truly equivalent). Answer with JSON only:
{{"edit1": {{"completeness": n, "cleanliness": n, "flow": n, "brief": n}},
  "edit2": {{"completeness": n, "cleanliness": n, "flow": n, "brief": n}},
  "winner": "edit1" | "edit2" | "tie",
  "reasons": "<3-6 sentences naming concrete moments>"}}

SOURCE TRANSCRIPT
{source}

EDIT 1 ({len1})
{edit1}

EDIT 2 ({len2})
{edit2}
"""


def clock(t):
    t = max(0.0, t)
    return f"{int(t // 60)}:{t % 60:05.2f}"


def load_words(reference):
    data = json.loads(Path(reference).read_text())
    return [w for w in data.get("words", []) if w.get("type") in ("word", "audio_event") and w.get("start") is not None]


def source_text(words):
    lines, current, last_end = [], [], None
    for w in words:
        if current and (w["start"] - last_end >= 0.5):
            lines.append(f"[{current[0]['start']:.2f}-{last_end:.2f}] " + " ".join(x["text"] for x in current))
            current = []
        current.append(w)
        last_end = w["end"]
    if current:
        lines.append(f"[{current[0]['start']:.2f}-{last_end:.2f}] " + " ".join(x["text"] for x in current))
    return "\n".join(lines)


def edit_text(result, words):
    segments = (result.get("mapping") or {}).get("segments") or []
    lines = []
    for seg in segments:
        a, b = seg["src_start"], seg["src_end"]
        kept = [w for w in words if w.get("type") == "word" and a <= (w["start"] + w["end"]) / 2 < b]
        head = f"{clock(seg['out_start'])}-{clock(seg['out_end'])} (source {a:.2f}-{b:.2f})"
        if kept:
            lines.append(f"{head}: " + " ".join(w["text"] for w in kept) + " ‖")
        else:
            lines.append(f"{head}: [no speech, {b - a:.1f} s of picture]")
    duration = (result.get("validity") or {}).get("duration_s") or 0
    return "\n".join(lines), f"{clock(duration)} long, {len(segments)} segments"


def ask(prompt, model, cwd):
    # --safe-mode: no CLAUDE.md, skills or plugins colouring the judgement; no tools at all.
    cmd = ["claude", "-p", prompt, "--output-format", "json", "--tools", "", "--safe-mode"]
    if model:
        cmd += ["--model", model]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=1800, cwd=cwd)
    reply = json.loads(out.stdout)
    text = reply.get("result", "")
    start, end = text.find("{"), text.rfind("}")
    return json.loads(text[start : end + 1]), reply.get("total_cost_usd")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bench", type=Path, required=True)
    parser.add_argument("--task", required=True)
    parser.add_argument("--a", default="rushes")
    parser.add_argument("--b", default="video-use")
    parser.add_argument("--runs", default="1,2,3")
    parser.add_argument("--model")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    tasks = json.loads((args.bench / "tasks.json").read_text())
    brief = (args.bench / tasks[args.task]["brief"]).read_text().strip()
    words = load_words(args.bench / "reference" / "full.scribe_v2.json")
    source_name = tasks[args.task]["source"]
    if source_name == "excerpt.mp4":
        words = [w for w in words if w["start"] < 560.0]
    source = source_text(words)
    verdicts = []
    rng = random.Random(7)
    for run in args.runs.split(","):
        results = {}
        for harness in (args.a, args.b):
            path = args.bench / "runs" / args.task / harness / run / "result.json"
            if path.is_file():
                results[harness] = json.loads(path.read_text())
        if len(results) < 2:
            print(f"run {run}: missing a result, skipped")
            continue
        texts = {h: edit_text(r, words) for h, r in results.items()}
        orders = [(args.a, args.b), (args.b, args.a)]
        rng.shuffle(orders)
        for first, second in orders:
            prompt = RUBRIC.format(
                brief=brief, source=source,
                edit1=texts[first][0], len1=texts[first][1],
                edit2=texts[second][0], len2=texts[second][1],
            )
            verdict, cost = ask(prompt, args.model, args.bench)
            winner = {"edit1": first, "edit2": second}.get(verdict.get("winner"), "tie")
            record = {
                "run": run, "order": [first, second], "winner": winner,
                "scores": {first: verdict.get("edit1"), second: verdict.get("edit2")},
                "reasons": verdict.get("reasons"), "cost_usd": cost,
            }
            verdicts.append(record)
            print(json.dumps(record, ensure_ascii=False), flush=True)
    wins = {h: sum(1 for v in verdicts if v["winner"] == h) for h in (args.a, args.b)}
    wins["tie"] = sum(1 for v in verdicts if v["winner"] == "tie")
    summary = {"task": args.task, "comparisons": len(verdicts), "wins": wins, "verdicts": verdicts}
    out = args.out or args.bench / "results" / f"judge-{args.task}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(summary, ensure_ascii=False, indent=2))
    print(json.dumps({"task": args.task, "wins": wins}))


if __name__ == "__main__":
    main()
