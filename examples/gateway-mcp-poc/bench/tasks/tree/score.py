from scorelib import *
exp = expected("tree")
ans = reply_json("total_errors")
checks = answer_checks(ans, exp)
s = seen()
# Sandbox names are stored in lowercase.
check_names = {"ma", "mb", "wa1", "wa2", "wb3", "wb4"}
checks["managers_and_workers"] = check_names <= s
checks["depth_3_refused"] = not any(n.startswith("probe") for n in s) and \
    sum(1 for t, c in refusals() if t == "create_sandbox" and c == "LIMIT_REACHED") >= 1
checks["stop_cascades"] = not lines("left.txt")
finish(checks, answer=ans, sandboxes=sorted(s), refusals=refusals())
