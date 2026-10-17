#!/usr/bin/env python3
"""Runs the AI-edit benchmark: every task x harness x run, a few at a time, then scores
each output with evaluate.py.

    python3 scripts/bench/run_edits.py --bench <dir> [--tasks a,b] [--harnesses rushes,video-use]
        [--runs 2] [--parallel 2] [--model opus] [--skill <video-use dir>] [--no-eval]

<dir>/tasks.json names each task's source (in <dir>/sources/), brief and gold file:

    {"excerpt-longform": {"source": "excerpt.mp4", "brief": "briefs/longform.txt", "gold": "gold/excerpt.json"}}

Each run gets its own footage folder, <dir>/runs/<task>/<harness>/<n>/footage, with the
source linked in, and is driven through the real pipeline (the bench_run example of
rushes-pipeline). Finished runs (run.json present) are skipped, so the script resumes.
Runs are started in pairs of harnesses where possible, so both see the same machine load.
"""

import argparse
import json
import os
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent


def run_one(bench, task, spec, harness, n, args, lock):
    run_dir = bench / "runs" / task / harness / str(n)
    footage = run_dir / "footage"
    footage.mkdir(parents=True, exist_ok=True)
    source = bench / "sources" / spec["source"]
    link = footage / spec["source"]
    if not link.exists():
        link.symlink_to(source.resolve())
    run_json = run_dir / "run.json"
    if not run_json.exists():
        cmd = [
            str(args.runner), "--harness", harness, "--footage", str(footage),
            "--brief", str(bench / spec["brief"]), "--out", str(run_json),
            "--label", f"{task}/{harness}/{n}",
        ]
        if harness == "rushes":
            cmd += ["--tools", str(args.tools)]
        else:
            cmd += ["--skill", str(args.skill)]
        if args.model:
            cmd += ["--model", args.model]
        with lock:
            print(f"start {task}/{harness}/{n}", flush=True)
        with open(run_dir / "stdout.log", "w") as log:
            subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, env=os.environ.copy())
    meta = json.loads(run_json.read_text())
    meta.update({"task": task, "harness": harness, "run": n})
    run_json.write_text(json.dumps(meta, indent=2))
    result = run_dir / "result.json"
    final = footage / "edit" / "final.mp4"
    if not args.no_eval and final.is_file() and not result.exists():
        edl = footage / "edit" / "edl.json"
        cmd = [
            sys.executable, str(HERE / "evaluate.py"), "--source", str(source.resolve()),
            "--reference", str(bench / "reference" / "full.scribe_v2.json"),
            "--gold", str(bench / spec["gold"]), "--output", str(final),
            "--run-meta", str(run_json), "--out", str(result),
        ]
        if edl.is_file():
            cmd += ["--edl", str(edl)]
        with open(run_dir / "evaluate.log", "w") as log:
            subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT)
    with lock:
        if result.exists():
            status = "scored"
        elif not final.is_file():
            status = "no final.mp4"
        else:
            status = "not scored" if args.no_eval else "evaluation failed"
        print(f"done  {task}/{harness}/{n}: {status}", flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bench", type=Path, required=True)
    parser.add_argument("--tasks")
    parser.add_argument("--harnesses", default="rushes,video-use")
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--parallel", type=int, default=2)
    parser.add_argument("--model")
    parser.add_argument("--runner", type=Path, help="bench_run binary")
    parser.add_argument("--tools", type=Path, help="rushes-edit binary")
    parser.add_argument("--skill", type=Path, default=Path.home() / ".claude/skills/video-use")
    parser.add_argument("--no-eval", action="store_true")
    args = parser.parse_args()
    bench = args.bench.resolve()
    target = HERE.parent.parent / "target" / "release"
    args.runner = args.runner or target / "examples" / "bench_run"
    args.tools = args.tools or target / "rushes-edit"
    tasks = json.loads((bench / "tasks.json").read_text())
    wanted = args.tasks.split(",") if args.tasks else list(tasks)
    harnesses = args.harnesses.split(",")
    # Interleave harnesses so concurrent runs are one of each.
    jobs = [(task, harness, n) for task in wanted for n in range(1, args.runs + 1) for harness in harnesses]
    lock = threading.Lock()
    with ThreadPoolExecutor(max_workers=args.parallel) as pool:
        futures = [pool.submit(run_one, bench, task, tasks[task], harness, n, args, lock) for task, harness, n in jobs]
        for future in futures:
            future.result()


if __name__ == "__main__":
    main()
