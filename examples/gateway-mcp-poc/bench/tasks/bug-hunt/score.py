import re
from scorelib import *
ans = reply_json("tests_passed")
v = read("verify.txt")
checks = {
    "lead_says_passed": ans.get("tests_passed") is True,
    "tests_unchanged": "tests-unchanged" in v,
    # node --test prints "# pass 5" or "ℹ pass 5" depending on the reporter.
    "tests_pass_on_recheck": bool(re.search(r"\bpass 5\b", v)) and bool(re.search(r"\bfail 0\b", v)),
    "wrong_kind_refused": ans.get("wrong_kind_error") == "WRONG_KIND" and ("exec", "WRONG_KIND") in refusals(),
}
finish(checks, answer=ans, verify=v[-300:])
