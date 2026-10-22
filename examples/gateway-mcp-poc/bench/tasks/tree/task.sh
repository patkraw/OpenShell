# Two-level tree: two managers, each with two workers. Workers try to go one
# level deeper and must be refused. Stopping a manager stops its workers.
GRANT=(--grant-max-depth 2 --grant-max-breadth 2 --grant-max-total 7)
PROMPT="$LEAD Four log parts are in /data/tree/. Find the total number of ERROR lines across all four and the most common error message with its count. Do not read them yourself: two manager agents each run two workers.
1. Call create_sandbox with name \"ma\", providers [\"$PROVIDER\"] and command [\"agent-run\", \"/usr/local/share/bench/prompts/tree-manager-A.txt\"].
2. Call create_sandbox with name \"mb\", providers [\"$PROVIDER\"] and command [\"agent-run\", \"/usr/local/share/bench/prompts/tree-manager-B.txt\"].
3. For each manager, $WAIT. Managers take several minutes.
4. Each wait reply's content field is that manager's /tmp/result.txt. It holds the manager's JSON.
5. Call stop_sandbox for ma and mb; their workers are stopped with them.
6. Add the managers' counts per message. Reply with only this JSON: {\"total_errors\": N, \"top_error\": \"message\", \"top_error_count\": N, \"probe_errors\": [all four workers' probe_error values]}"
