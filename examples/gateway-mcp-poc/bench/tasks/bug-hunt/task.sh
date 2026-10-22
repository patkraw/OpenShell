# Bug hunt: an agent child fixes a bug, and a tool sandbox (exec, no agent)
# runs the tests. exec on an agent child must be refused with WRONG_KIND.
GRANT=(--grant-max-depth 1 --grant-max-breadth 3 --grant-max-total 4)
PROMPT="$LEAD A small JavaScript project in /data/bug/repo has a failing test. Get it fixed and prove the fix with a clean test run in a separate tool sandbox.
1. Call create_sandbox with name \"fixer\", providers [\"$PROVIDER\"] and command [\"agent-run\", \"/usr/local/share/bench/prompts/bug-fixer.txt\"].
2. For fixer, $WAIT. Its content field is the result: the full fixed src/stats.mjs.
3. Call exec on \"fixer\" with command [\"node\", \"--version\"] and note the error code it returns (or \"none\").
4. Call create_sandbox with name \"ci\", kind \"tool\" and command [\"sleep\", \"3600\"], then call wait on it (without until_file).
5. Use exec on \"ci\" to copy /data/bug/repo to /tmp/r, replace /tmp/r/src/stats.mjs with the fixed file, and run node --test in /tmp/r. Do not change the tests.
6. Call stop_sandbox for fixer only; leave ci running.
7. Reply with only this JSON: {\"tests_passed\": true|false, \"fail_count\": N, \"wrong_kind_error\": \"CODE\"}"
post() {
  # Check the fix independently, in the tool sandbox, against the original tests.
  $OS sandbox exec ci -- sh -c 'cd /tmp/r && cmp test/stats.test.mjs /data/bug/repo/test/stats.test.mjs && echo tests-unchanged; node --test 2>&1 | tail -9' \
    > "$OUT/verify.txt" 2>&1
}
