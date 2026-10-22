# Big result: a child's result file is bigger than the 256 KiB that wait and
# get_file return. The lead must take the summary from the start of the file
# and notice that the rest was cut.
GRANT=(--grant-max-depth 1 --grant-max-breadth 1 --grant-max-total 2)
PROMPT="$LEAD A child sandbox runs a test suite and writes its report.
1. Call create_sandbox with name \"tests\" and command [\"/usr/local/share/bench/big-report\"]. It writes its report to /tmp/result.txt.
2. For tests, $WAIT. Its content field is the report: after the exit= line, the next line is a JSON summary. Note whether the reply says the content was truncated.
3. Call stop_sandbox for tests.
4. Reply with only this JSON: {\"tests\": N, \"failed\": N, \"slowest_test\": \"name\", \"truncated\": true|false, \"saw_end_line\": true|false}"
