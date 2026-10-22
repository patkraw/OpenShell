from scorelib import *
exp = expected("budget")
ans = reply_json("total_errors")
checks = answer_checks(ans, exp)
checks["within_budget"] = 1 <= len(seen()) <= 3
checks["none_left_running"] = not lines("left.txt")
finish(checks, answer=ans, children=sorted(seen()),
       limit_refusals=sum(1 for _, c in refusals() if c == "LIMIT_REACHED"))
