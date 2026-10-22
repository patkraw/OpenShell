#!/usr/bin/env python3
"""Re-score every saved run with the current scorers and print summary tables.

Reads results/round*/<task>/<harness>/ (and each run's summary.json for the exit
code and time), writes results/rescored.jsonl and prints Markdown tables:
passes per task and harness, median seconds, and the failed checks.
Usage: summarize.py
"""
import glob
import json
import os
import statistics
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
TASKS = ["split-count", "budget", "tree", "least-privilege", "injection", "crash", "bug-hunt", "hang", "big-result"]
HARNESSES = ["pi", "opencode", "qwen", "codex", "deepagents", "hermes"]
rows = []
for run in sorted(glob.glob(os.path.join(HERE, "results", "round*", "*", "*"))):
    rnd, task, harness = run.split(os.sep)[-3:]
    try:
        old = json.load(open(os.path.join(run, "summary.json")))
    except (FileNotFoundError, ValueError):
        old = {}
    out = subprocess.run([sys.executable, os.path.join(HERE, "tasks", task, "score.py"), run],
                         capture_output=True, text=True,
                         env={**os.environ, "PYTHONPATH": os.path.join(HERE, "tasks")})
    try:
        score = json.loads(out.stdout)
    except ValueError:
        score = {"pass": False, "checks": {"scorer_error": False}, "error": out.stderr[-300:]}
    rows.append({"round": int(rnd[5:]), "task": task, "harness": harness,
                 "seconds": old.get("seconds"), "exit": old.get("exit"), "score": score})
with open(os.path.join(HERE, "results", "rescored.jsonl"), "w") as f:
    for r in rows:
        f.write(json.dumps(r) + "\n")

def cell(task, h):
    rs = [r for r in rows if r["task"] == task and r["harness"] == h]
    if not rs:
        return "–"
    ok = sum(r["score"].get("pass") for r in rs)
    secs = [r["seconds"] for r in rs if r["seconds"] is not None]
    med = f" · {statistics.median(secs):.0f}s" if secs else ""
    return f"{'✅' if ok == len(rs) else '⚠️' if ok else '❌'} {ok}/{len(rs)}{med}"

print("| Task | " + " | ".join(HARNESSES) + " |")
print("|---|" + "---|" * len(HARNESSES))
for t in TASKS:
    print(f"| {t} | " + " | ".join(cell(t, h) for h in HARNESSES) + " |")
tot = [r for r in rows]
print(f"\nPassed {sum(r['score'].get('pass') for r in tot)} of {len(tot)} runs.\n")
print("Failed checks:")
for r in rows:
    if not r["score"].get("pass"):
        bad = [k for k, v in (r["score"].get("checks") or {}).items() if not v]
        print(f"- round {r['round']} {r['harness']} {r['task']}: {', '.join(bad)}")
