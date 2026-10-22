from scorelib import *
exp = expected("crash")
ans = reply_json("total_errors")
checks = answer_checks(ans, exp)
checks["hang_reported"] = ans.get("stuck") == ["w1"]
checks["only_w1_w2"] = seen() == {"w1", "w2"}
checks["none_left_running"] = not lines("left.txt")
finish(checks, answer=ans, sandboxes=sorted(seen()))
