#!/usr/bin/env python3
"""One edit with a reimplementation of RankCut, for the benchmark.

    python3 scripts/bench/run_rankcut.py --footage <dir> --brief <file> --out <run.json>
        --tools <rushes-edit binary> [--model <m>]

RankCut (Shah et al., "RankCut: A Ranking-Based LLM Approach to Extractive Summarization for
Transcript-Based Video Editing", IUI '26) ranks transcript sentences with an LLM and lets a
deterministic selector assemble the cut. No code was released; this follows the paper:

1. Stage 1, coverage: an abstractive summary of the whole transcript (summary, tone, roles).
2. Stage 2, rules: a RULE_OBJECT (intent, keywords, rule text, budget instructions) from the
   summary and the editor's brief.
3. Stage 3, ranking: windows of about 50 sentences ranked under the RULE_OBJECT, with a
   GREEN/YELLOW/RED priority per sentence; local rank r of N becomes s = 1 - (r-1)/(N-1).
4. Redundancy: non-maximum suppression over sentences, (priority, score) first; a sentence is
   dropped when one already kept within 30 s is at least 0.8 similar.
5. Selector: deterministic and source-ordered. With a target length, sentences are admitted
   by (priority, score) up to the target (2% overshoot); without one, GREEN and YELLOW are
   kept. Neighbours closer than 2 s merge into one span, trimmed to sentence limits.

Differences from the paper, so the comparison is about the idea: the same model as the other
harnesses (Claude, via `claude -p` with no tools) instead of GPT-4.1 nano, no temperature
control, character-trigram cosine instead of MiniLM embeddings for redundancy, and the
transcript (ElevenLabs Scribe v2) and renderer of rushes-edit, so transcription and
rendering are the same as the Rushes harness.
"""

import argparse
import json
import math
import os
import re
import subprocess
import time
from collections import Counter
from pathlib import Path

WINDOW = 50
NMS_SIMILARITY = 0.8
NMS_SECONDS = 30.0
MERGE_GAP = 2.0
OVERSHOOT = 0.02
# Silence kept around each span: 0.08 s before, 0.12 s after.
PAD = 0.2
PRIORITY = {"GREEN": 0, "YELLOW": 1, "RED": 2}

STAGE1 = """You are preparing a long recording for an editor. Read the whole transcript and write a
high-recall coverage summary: every topic, claim, example, number and demo the speaker covers,
in order, so that later steps do not lose anything salient. Do not judge length yet.

Answer with JSON only:
{{"summary": "...", "dominant_tone": "...", "story_roles": {{"<speaker>": "<role>"}}}}

TRANSCRIPT (one sentence per line: id, start time, text)
{transcript}"""

STAGE2 = """From this coverage summary and the editor's brief, write the selection rules for an
extractive cut of the recording: which sentences belong in the edit and which do not.

Editor's brief:
{brief}

Coverage summary:
{summary}

Answer with JSON only, a RULE_OBJECT:
{{"intent": "...", "keywords": ["..."], "speaker_roles_priority": ["..."],
  "rule_text": "...", "budget_instructions": "..."}}"""

STAGE3 = """Rank transcript sentences for inclusion in an extractive video edit, following these rules:

{rules}

Below are sentences {first} to {last} of the transcript (local ids 0-{n_minus_1}, with their
start times). Rank ALL of them from most to least important to include, give each a priority
(GREEN: include, YELLOW: borderline, RED: leave out) and a relevance between 0 and 1.

Answer with JSON only:
{{"ranking": [<local ids, best first, every id once>],
  "sentence_colors": {{"<id>": "GREEN|YELLOW|RED"}},
  "sentence_relevance": {{"<id>": 0.0}}}}

SENTENCES
{window}"""


class Llm:
    def __init__(self, model, cwd):
        self.model, self.cwd, self.cost, self.calls = model, cwd, 0.0, 0

    def ask(self, prompt):
        cmd = ["claude", "-p", prompt, "--output-format", "json", "--tools", "", "--safe-mode"]
        if self.model:
            cmd += ["--model", self.model]
        for _attempt in range(3):
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=1800, cwd=self.cwd)
            try:
                reply = json.loads(out.stdout)
            except json.JSONDecodeError:
                continue
            self.calls += 1
            self.cost += reply.get("total_cost_usd") or 0.0
            text = reply.get("result", "")
            start, end = text.find("{"), text.rfind("}")
            try:
                return json.loads(text[start : end + 1])
            except json.JSONDecodeError:
                continue
        raise RuntimeError("the model did not answer with JSON")


def clock(t):
    return f"{int(t // 60)}:{t % 60:05.2f}"


def sentences(words):
    """Sentence-like units from word timings: end at . ? ! or a pause of 1.5 s or more."""
    out, current = [], []
    for i, word in enumerate(words):
        current.append(word)
        nxt = words[i + 1] if i + 1 < len(words) else None
        text = word["text"].rstrip("\"'»)")
        end = text.endswith((".", "?", "!", "…")) or nxt is None or nxt["start"] - word["end"] >= 1.5
        if end or current[-1]["end"] - current[0]["start"] > 25:
            out.append({
                "id": len(out), "start": current[0]["start"], "end": current[-1]["end"],
                "text": " ".join(w["text"] for w in current),
            })
            current = []
    return out


def trigrams(text):
    norm = re.sub(r"[^\w]+", " ", text.lower()).strip()
    return Counter(norm[i : i + 3] for i in range(max(0, len(norm) - 2)))


def cosine(a, b):
    dot = sum(v * b.get(k, 0) for k, v in a.items())
    na, nb = math.sqrt(sum(v * v for v in a.values())), math.sqrt(sum(v * v for v in b.values()))
    return dot / (na * nb) if na and nb else 0.0


def target_seconds(brief):
    m = re.search(r"Target length:\s*(\d+)\s*[–-]\s*(\d+)\s*(seconds|s|minutes|min)", brief)
    if not m:
        return None
    scale = 60 if m.group(3).startswith("min") else 1
    return int(m.group(2)) * scale


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--footage", type=Path, required=True)
    parser.add_argument("--brief", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--tools", type=Path, required=True, help="rushes-edit binary")
    parser.add_argument("--model")
    args = parser.parse_args()
    footage = args.footage.resolve()
    brief = args.brief.read_text().strip()
    started = time.time()
    tools = str(args.tools.resolve())

    def call(tool, payload):
        out = subprocess.run([tools, "call", tool, json.dumps(payload), "--footage", str(footage)], capture_output=True, text=True)
        if out.returncode != 0:
            raise RuntimeError(f"{tool}: {out.stdout[-2000:]} {out.stderr[-2000:]}")
        return out.stdout

    call("transcribe", {})
    transcript = next(p for p in (footage / "edit" / "transcripts").glob("*.json") if not p.name.endswith(".rushes.json"))
    take = "A"
    words = [w for w in json.loads(transcript.read_text())["words"] if w.get("type") == "word"]
    units = sentences(words)
    llm = Llm(args.model, footage)
    lines = "\n".join(f"[{u['id']}] {clock(u['start'])} {u['text']}" for u in units)

    stage1 = llm.ask(STAGE1.format(transcript=lines))
    rules = llm.ask(STAGE2.format(brief=brief, summary=json.dumps(stage1, ensure_ascii=False)))
    rules_text = json.dumps(rules, ensure_ascii=False, indent=1)
    for first in range(0, len(units), WINDOW):
        window = units[first : first + WINDOW]
        n = len(window)
        text = "\n".join(f"{i} {clock(u['start'])} {u['text']}" for i, u in enumerate(window))
        answer = llm.ask(STAGE3.format(rules=rules_text, first=first, last=first + n - 1, n_minus_1=n - 1, window=text))
        ranking = [int(i) for i in answer.get("ranking", []) if str(i).isdigit() and int(i) < n]
        ranking += [i for i in range(n) if i not in ranking]
        colors = answer.get("sentence_colors", {})
        for rank, local in enumerate(dict.fromkeys(ranking)):
            unit = window[local]
            unit["score"] = 1.0 - rank / (n - 1) if n > 1 else 1.0
            unit["priority"] = str(colors.get(str(local), "YELLOW")).upper()
            if unit["priority"] not in PRIORITY:
                unit["priority"] = "YELLOW"

    # Redundancy: keep the better of two similar sentences close in time.
    order = sorted(units, key=lambda u: (PRIORITY[u["priority"]], -u["score"], u["start"]))
    kept = []
    grams = {u["id"]: trigrams(u["text"]) for u in units}
    for unit in order:
        similar = any(
            abs(unit["start"] - k["start"]) <= NMS_SECONDS and cosine(grams[unit["id"]], grams[k["id"]]) >= NMS_SIMILARITY
            for k in kept
        )
        unit["suppressed"] = similar
        if not similar:
            kept.append(unit)

    def assemble(units_):
        spans_ = []
        for unit in sorted(units_, key=lambda u: u["start"]):
            if spans_ and unit["start"] - spans_[-1][1] <= MERGE_GAP:
                spans_[-1][1] = unit["end"]
            else:
                spans_.append([unit["start"], unit["end"]])
        return spans_

    target = target_seconds(brief)
    if target:
        # Admit by (priority, score) while the assembled cut, merged gaps included, stays
        # within the target plus the paper's 2% overshoot.
        chosen = []
        for unit in kept:
            if sum(b - a + PAD for a, b in assemble(chosen + [unit])) <= target * (1 + OVERSHOOT):
                chosen.append(unit)
    else:
        chosen = [u for u in kept if u["priority"] in ("GREEN", "YELLOW")]
    chosen.sort(key=lambda u: u["start"])
    spans = assemble(chosen)
    starts = sorted(w["start"] for w in words)
    ends = sorted(w["end"] for w in words)

    def pad(start, end):
        before = max((e for e in ends if e <= start - 0.001), default=0.0)
        after = min((s for s in starts if s >= end + 0.001), default=end + 1.0)
        return max(start - 0.08, (before + start) / 2), min(end + 0.12, (end + after) / 2)

    segments = [{"take": take, "start": round(a, 3), "end": round(b, 3)} for a, b in (pad(*s) for s in spans)]
    plan = {"segments": segments}
    vertical = "vertical" in brief.lower()
    if vertical:
        plan["aspect"] = "9:16"
    call("plan_cut", plan)
    render = {"quality": "final"}
    if "Burn subtitles" in brief:
        render.update({"burn_subtitles": True, "subtitle_style": "punchy" if vertical else "sentence"})
    call("render", render)

    (footage / "edit" / "rankcut.json").write_text(json.dumps(
        {"stage1": stage1, "rules": rules, "units": units, "spans": spans}, ensure_ascii=False, indent=1))
    final = footage / "edit" / "final.mp4"
    record = {
        "harness": "rankcut", "model": args.model, "footage": str(footage),
        "wall_seconds": time.time() - started, "cost_usd": llm.cost, "num_turns": llm.calls,
        "sentences": len(units), "kept_sentences": len(chosen), "spans": len(spans),
        "final_cut": str(final) if final.is_file() else None,
    }
    args.out.write_text(json.dumps(record, indent=2))
    print(json.dumps(record, indent=2))


if __name__ == "__main__":
    main()
