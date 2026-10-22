import json, os
from scorelib import *
exp = json.load(open(os.path.join(HERE, "..", "split-count", "expected.json")))
ans = reply_json("total_errors")
checks = answer_checks(ans, exp)
checks["two_children"] = {"half1", "half2"} <= seen() and len(seen()) == 2
checks["none_left_running"] = not lines("left.txt")
finish(checks, answer=ans)
