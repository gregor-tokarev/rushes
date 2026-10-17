#!/usr/bin/env python3
"""One single-pass AI edit with the kinocut harness, for the benchmark.

    python3 scripts/bench/run_kinocut.py --footage <dir> --brief <file> --out <run.json>
        --kinocut <venv with kinocut installed> --skill <kinocut checkout>/skills/kinocut [--model <m>]

kinocut (github.com/KyaniteLabs/kinocut) is a guardrailed video-editing MCP server with an
agent skill. The agent here is the same headless Claude Code the pipeline drives, given
kinocut's MCP server, its `kino` CLI on PATH, its SKILL.md and the same kind of prompt the
pipeline gives video-use (single pass, pre-approved). Writes run.json (wall time, cost,
turns, outcome) and the raw stream-json lines next to it (events.jsonl).
"""

import argparse
import json
import os
import subprocess
import time
from pathlib import Path

PROMPT = """Rushes edit request (single pass, pre-approved).

Use the kinocut skill to edit the raw footage in the current directory. Read the skill first: {skill_md}. Its MCP tools are available to you (mcp__kinocut__*), and its `kino` CLI is on PATH.

Raw footage in the current directory:
{files}

Write every output to ./edit/. Never modify the source files or anything in the skill directory.

The creator's brief:

{brief}

The creator pre-approved this edit, so do not stop to confirm the strategy:
1. Inspect and transcribe the footage, decide the strategy and append it to ./edit/project.md.
2. Execute the edit with kinocut. Render the final cut to ./edit/final.mp4.
3. Check the rendered output with kinocut's quality checks; fix and re-render if needed (at most 3 passes).

You are running headless inside the Rushes app: nobody can answer questions mid-run, and no human can review before you finish. Where something is ambiguous, make the most sensible choice and say which choice you made.

Reply with a short summary: the strategy you chose and what was produced."""


def describe(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration:stream=width,height", "-of", "json", str(path)],
        capture_output=True, text=True,
    )
    info = json.loads(out.stdout or "{}")
    duration = float(info.get("format", {}).get("duration", 0))
    size = next((f"{s['width']}x{s['height']}" for s in info.get("streams", []) if s.get("width")), "audio only")
    return f"- {path.name} ({int(duration // 60)}:{int(duration % 60):02d}, {size})"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--footage", type=Path, required=True)
    parser.add_argument("--brief", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--kinocut", type=Path, required=True, help="venv with kinocut installed")
    parser.add_argument("--skill", type=Path, required=True)
    parser.add_argument("--model")
    args = parser.parse_args()
    footage = args.footage.resolve()
    files = "\n".join(describe(p) for p in sorted(footage.iterdir()) if p.suffix.lower() in (".mp4", ".mov", ".mkv"))
    skill = args.skill.resolve()
    prompt = PROMPT.format(skill_md=skill / "SKILL.md", files=files, brief=args.brief.read_text().strip())
    kino = args.kinocut.resolve() / "bin"
    server = {"type": "stdio", "command": str(kino / "kino"), "args": ["--mcp"]}
    cmd = [
        "claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
        "--permission-mode", "acceptEdits",
        "--allowedTools=mcp__kinocut,Bash,Read,Write,Edit,Glob,Grep,TodoWrite",
        f"--mcp-config={json.dumps({'mcpServers': {'kinocut': server}})}", "--strict-mcp-config",
        f"--add-dir={skill}", f"--add-dir={skill.parent.parent}",
    ]
    if args.model:
        cmd += ["--model", args.model]
    env = os.environ.copy()
    env["PATH"] = f"{kino}:{env['PATH']}"
    env["MCP_TOOL_TIMEOUT"] = "3600000"
    # `kino doctor` runs `npx hyperframes --version`, whose first run installs HyperFrames'
    # agent skills globally (~/.claude/skills); this is HyperFrames' own opt-out.
    env["HYPERFRAMES_SKIP_SKILLS"] = "1"
    started = time.time()
    result = None
    with open(args.out.with_name("events.jsonl"), "w") as events:
        proc = subprocess.Popen(cmd, cwd=footage, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in proc.stdout:
            events.write(line)
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("type") == "result":
                result = message
        proc.wait()
    final = footage / "edit" / "final.mp4"
    record = {
        "harness": "kinocut", "model": args.model, "footage": str(footage),
        "wall_seconds": time.time() - started, "exit_code": proc.returncode,
        "cost_usd": (result or {}).get("total_cost_usd"), "num_turns": (result or {}).get("num_turns"),
        "agent_ms": (result or {}).get("duration_ms"), "is_error": (result or {}).get("is_error"),
        "final_cut": str(final) if final.is_file() else None, "summary": (result or {}).get("result"),
    }
    args.out.write_text(json.dumps(record, indent=2))
    print(json.dumps({k: v for k, v in record.items() if k != "summary"}, indent=2))


if __name__ == "__main__":
    main()
