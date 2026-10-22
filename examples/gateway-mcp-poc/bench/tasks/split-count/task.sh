# Split counting: fan out to two children, collect both results, combine them.
GRANT=(--grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3)
PROMPT="$LEAD The file /data/app.log has 10,000 log lines. Each line is: timestamp LEVEL [service] message (req=id). Find (1) how many lines have LEVEL ERROR, and (2) the most common error message across the whole file and how many times it occurs. Do not read /data/app.log yourself: two child agents count it.
1. Call create_sandbox with name \"half1\", providers [\"$PROVIDER\"] and command [\"agent-run\", \"/usr/local/share/bench/prompts/split-1.txt\"].
2. Call create_sandbox with name \"half2\", providers [\"$PROVIDER\"] and command [\"agent-run\", \"/usr/local/share/bench/prompts/split-2.txt\"].
3. For each child, $WAIT.
4. Each wait reply's content field is that child's /tmp/result.txt. It holds the child's JSON counts.
5. Call stop_sandbox for both children.
6. Add the counts per message across both halves. Reply with only this JSON: {\"total_errors\": N, \"top_error\": \"message\", \"top_error_count\": N}"
