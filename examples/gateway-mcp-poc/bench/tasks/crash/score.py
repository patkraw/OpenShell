from scorelib import *
exp = expected("crash")
ans = reply_json("total_errors")
checks = answer_checks(ans, exp)
checks["crash_seen"] = ans.get("first_exit_code") == exp["first_exit_code"]
checks["retry_deduplicated"] = ans.get("retry_handle_same") is True and seen() == {"w1", "w2"}
checks["none_left_running"] = not lines("left.txt")
finish(checks, answer=ans, sandboxes=sorted(seen()))
