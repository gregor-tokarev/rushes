#!/usr/bin/env python3
"""Double takes left in an edit: places where the viewer hears the speaker say a line (or
its start) twice because it was tried again, found by a fresh model.

    python3 scripts/bench/doubles.py --reference <full.scribe_v2.json> [--reads 2]
        [--model <m>] [--out <file.json>] <result.json>...

What the viewer hears comes from the edit list the harness wrote (edl.json, given to
evaluate.py and named in result.json): the reference transcript's words inside each source
range, in output order, with ‖ at every cut. The measured mapping is not used for this: it
cannot place sub-second segments, so a repeat cut out with a short cut would read as heard.
Every edit is read `--reads` times by a fresh `claude -p` without tools; its count is the
mean over the reads.
"""

import argparse
import json
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

PROMPT = """Below is the transcript of an edited talking-head video: what the viewer hears, in
order, with output times in brackets and ‖ where the editor made a cut.

Find every DOUBLE TAKE the editor left in: places where the viewer hears the speaker say the
same line (or the start of the same line) twice because the speaker tried it again: a retake,
a restart after stumbling, or an abandoned attempt followed by the full one. Include partial
ones ("I want to show ‖ I want to show you..."). Do NOT count deliberate repetition for
emphasis, lists or rhetorical parallels, or the same topic discussed again in other words.

Answer with JSON only, no prose: {"doubles": [{"time": "m:ss", "first": "<the first attempt,
quoted>", "second": "<the repeat, quoted>"}]}. An empty list if there are none.

TRANSCRIPT:
"""

# The reference is on the plain-decode timeline; edit lists are on the container clock.
CLOCK_OFFSET = 0.022


def heard(result, words, every=20.0):
    """The words inside the edit's source ranges, as lines of about `every` seconds."""
    ranges = json.loads(Path(result["inputs"]["edl"]).read_text())["ranges"]
    lines, line, mark, out, previous = [], [], 0.0, 0.0, None
    for r in ranges:
        for index, word in enumerate(words):
            middle = (word["start"] + word["end"]) / 2 + CLOCK_OFFSET
            if not r["start"] <= middle <= r["end"]:
                continue
            at = out + middle - r["start"]
            if line and at - mark >= every and word["text"][:1].isupper():
                lines.append(f"[{int(mark // 60)}:{mark % 60:04.1f}] " + " ".join(line))
                line = []
            if not line:
                mark = at
            elif previous is not None and index != previous + 1:
                line.append("‖")
            line.append(word["text"])
            previous = index
        out += r["end"] - r["start"]
    if line:
        lines.append(f"[{int(mark // 60)}:{mark % 60:04.1f}] " + " ".join(line))
    return "\n".join(lines)


def read(text, model):
    out = subprocess.run(
        ["claude", "-p", "--model", model, "--output-format", "json", "--tools", ""],
        input=PROMPT + text, capture_output=True, text=True, timeout=1800,
    )
    reply = json.loads(out.stdout)["result"]
    return json.loads(re.search(r"\{.*\}", reply, re.S).group(0))["doubles"]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("results", nargs="+", type=Path)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--reads", type=int, default=2)
    parser.add_argument("--model", default="claude-opus-5-5")
    parser.add_argument("--parallel", type=int, default=6)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    words = [w for w in json.loads(args.reference.read_text())["words"] if w["type"] == "word"]

    edits = []
    for path in args.results:
        result = json.loads(path.read_text())
        meta = result.get("run_meta") or {}
        edits.append({"result": str(path), "harness": meta.get("harness"), "task": meta.get("task"),
                      "run": meta.get("run"), "text": heard(result, words)})
    jobs = [(edit, k) for edit in edits for k in range(args.reads)]
    with ThreadPoolExecutor(args.parallel) as pool:
        found = list(pool.map(lambda job: read(job[0]["text"], args.model), jobs))
    for edit in edits:
        edit["reads"] = [doubles for (e, _), doubles in zip(jobs, found) if e is edit]
        edit["mean"] = sum(map(len, edit["reads"])) / len(edit["reads"])
        del edit["text"]
        print(f"{edit['task']}/{edit['harness']}/{edit['run']}: {edit['mean']:.1f} "
              f"({', '.join(str(len(r)) for r in edit['reads'])})")
    if args.out:
        args.out.write_text(json.dumps(edits, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
