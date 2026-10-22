# Crash and retry: the first child crashes without a result; the lead must see
# that, start a replacement, and a repeated create with the same request_id
# must return the same child instead of a third. A third child fails after
# printing a partial result: its result file says exit=4, and the lead must
# report that and not use its numbers.
GRANT=(--grant-max-depth 1 --grant-max-breadth 3 --grant-max-total 4)
PROMPT="$LEAD Find the number of ERROR lines in /data/crash/app.log and the most common error message with its count, using child sandboxes. Do not read the file yourself.
1. Call create_sandbox with name \"w1\" and command [\"/usr/local/share/bench/fast-count\", \"/data/crash/app.log\"]. It is a fast counter that writes /tmp/result.txt.
2. For w1, $WAIT. If wait returns file_ready false with an exit code, w1 failed; note its exit code.
2b. Call create_sandbox with name \"w3\" and command [\"run-child\", \"/usr/local/share/bench/partial-count\", \"/data/crash/app.log\"]. It is another fast counter. For w3, $WAIT, and note the exit code on the first line of its result.
3. Start a replacement: call create_sandbox with name \"w2\", providers [\"$PROVIDER\"], command [\"agent-run\", \"/usr/local/share/bench/prompts/crash-worker.txt\"] and request_id \"retry-w2\".
4. Call create_sandbox again with exactly the same arguments, including request_id \"retry-w2\", as if the first reply had been lost. Note whether it returns the same handle.
5. For w2, $WAIT. Its content field is the result. Only then call stop_sandbox for w1, w2 and w3.
6. Reply with only this JSON: {\"total_errors\": N, \"top_error\": \"message\", \"top_error_count\": N, \"first_exit_code\": N, \"retry_handle_same\": true|false, \"w3_exit_code\": N}"
