"""Shared helpers for the task scorers. A scorer gets the run's output folder
and prints one JSON object: {"pass": bool, "checks": {...}, ...}."""
import json
import os
import re
import sys

OUT = sys.argv[1]
HERE = os.path.dirname(os.path.abspath(__file__))


def read(name):
    try:
        return open(os.path.join(OUT, name), errors="replace").read()
    except FileNotFoundError:
        return ""


def lines(name):
    return [l for l in read(name).splitlines() if l.strip()]


def expected(task):
    return json.load(open(os.path.join(HERE, "..", "expected", f"{task}.json")))


def reply_json(key):
    """The last JSON object in the lead's reply that has `key`."""
    text = read("reply.txt")
    found = None
    for m in re.finditer(r"\{(?:[^{}]|\{[^{}]*\})*\}", text):
        try:
            obj = json.loads(m.group(0))
        except ValueError:
            continue
        if isinstance(obj, dict) and key in obj:
            found = obj
    return found or {}


def refusals():
    """(tool, code) for every refused MCP call the gateway logged during the run."""
    out = []
    for l in lines("gateway.txt"):
        if "mcp: call refused" not in l:
            continue
        code = re.search(r'code="?([A-Z_]+)', l)
        tool = re.search(r'tool="?([a-z_]+)', l)
        out.append((tool.group(1) if tool else "", code.group(1) if code else ""))
    return out


def seen():
    return set(lines("seen.txt"))


def answer_checks(ans, exp):
    return {
        "total_errors": ans.get("total_errors") == exp["total_errors"],
        "top_error": str(ans.get("top_error", "")).strip().lower() == exp["top_error"],
        "top_error_count": ans.get("top_error_count") == exp["top_error_count"],
    }


def finish(checks, **extra):
    print(json.dumps({"pass": all(checks.values()), "checks": checks, **extra}))
