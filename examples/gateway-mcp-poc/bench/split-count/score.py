#!/usr/bin/env python3
"""Score a lead's reply against expected.json: total, top error and its count.

Usage: score.py expected.json reply.txt  ->  prints one JSON line, exit 0 on pass.
The reply may contain other text; the last JSON object with "total_errors" counts.
"""
import json
import re
import sys

expected = json.load(open(sys.argv[1]))
text = open(sys.argv[2], errors="replace").read()
answer = None
for m in re.finditer(r"\{[^{}]*\}", text):
    try:
        obj = json.loads(m.group(0))
    except ValueError:
        continue
    if "total_errors" in obj:
        answer = obj
checks = {
    "found_answer": answer is not None,
    "total_errors": bool(answer) and answer.get("total_errors") == expected["total_errors"],
    "top_error": bool(answer)
    and str(answer.get("top_error", "")).strip().lower() == expected["top_error"],
    "top_error_count": bool(answer) and answer.get("top_error_count") == expected["top_error_count"],
}
print(json.dumps({"pass": all(checks.values()), "checks": checks, "answer": answer}))
sys.exit(0 if all(checks.values()) else 1)
