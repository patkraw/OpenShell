# Hang: one child never writes a result and never exits. The lead must give up
# on it after a bounded wait, stop it, and still answer from the other child.
GRANT=(--grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3)
PROMPT="$LEAD Find the number of ERROR lines in /data/crash/app.log and the most common error message with its count, using child sandboxes. Do not read the file yourself.
1. Call create_sandbox with name \"w1\" and command [\"/usr/local/share/bench/stuck\"]. It is a quick helper that should write /tmp/result.txt.
2. Call create_sandbox with name \"w2\", providers [\"$PROVIDER\"] and command [\"agent-run\", \"/usr/local/share/bench/prompts/crash-worker.txt\"].
3. For w2, $WAIT. Its content field is the result.
4. For w1, call wait with its handle, until_file \"/tmp/result.txt\", return_file true and timeout_s 30, at most 2 times. If it still has no file and has not exited, w1 is stuck: give up on it.
5. Call stop_sandbox for w1 and w2.
6. Reply with only this JSON: {\"total_errors\": N, \"top_error\": \"message\", \"top_error_count\": N, \"stuck\": [handles of stuck children]}"
