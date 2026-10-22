#!/usr/bin/env python3
"""Write app.log (10,000 lines) and expected.json for the split-count benchmark.

Seeded, so the file and the answer are the same on every run. The overall most
common error is only the second most common in each half, so a parent must add
up both children's counts; taking either child's top error gives the wrong
answer. Some WARN lines mention "ERROR" in their text; only lines whose level
is ERROR count.
"""
import json
import os
import random

LINES = 10_000
HALF = LINES // 2
# Error kinds and how many of each go into each half.
ERRORS = {
    "database timeout": (150, 50),
    "cache unavailable": (140, 145),
    "authentication failed": (60, 155),
    "disk full": (25, 20),
    "upstream 502": (15, 30),
}
SERVICES = ["api", "billing", "search", "auth", "worker"]
INFO = ["request served", "user logged in", "job queued", "job finished", "health check ok"]
WARN = ["slow query", "retrying request", "high memory", "deprecated endpoint used"]
DECOYS = 40  # WARN lines whose text contains "ERROR"


def half(rng, which):
    errors = [kind for kind, counts in ERRORS.items() for _ in range(counts[which])]
    decoys = DECOYS // 2
    rest = HALF - len(errors) - decoys
    lines = [("ERROR", kind) for kind in errors]
    lines += [("WARN", "retrying after upstream ERROR response")] * decoys
    for _ in range(rest):
        if rng.random() < 0.15:
            lines.append(("WARN", rng.choice(WARN)))
        else:
            lines.append(("INFO", rng.choice(INFO)))
    rng.shuffle(lines)
    return lines


def main():
    os.chdir(os.path.dirname(os.path.abspath(__file__)))
    rng = random.Random(4721)
    lines = half(rng, 0) + half(rng, 1)
    with open("app.log", "w") as f:
        for i, (level, text) in enumerate(lines):
            ts = f"2026-10-21T{8 + i // 3600:02d}:{i // 60 % 60:02d}:{i % 60:02d}Z"
            f.write(f"{ts} {level} [{rng.choice(SERVICES)}] {text} (req={rng.getrandbits(32):08x})\n")
    totals = {kind: a + b for kind, (a, b) in ERRORS.items()}
    top = max(totals, key=totals.get)
    expected = {
        "total_errors": sum(totals.values()),
        "top_error": top,
        "top_error_count": totals[top],
        "per_half": [sum(a for a, _ in ERRORS.values()), sum(b for _, b in ERRORS.values())],
        "by_kind": totals,
    }
    with open("expected.json", "w") as f:
        json.dump(expected, f, indent=2)
        f.write("\n")
    print(json.dumps(expected))


if __name__ == "__main__":
    main()
