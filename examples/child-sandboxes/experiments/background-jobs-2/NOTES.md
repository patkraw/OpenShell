# Notes from the runs

Observations made while reading the traces, kept for the report.

## S2a: Lightning did the work in its own sandbox

- **What happened:** of Nemotron 3.5 Lightning's 21 failures in S2a, 18
  were wrong answers, and in all 18 the lead ran or examined the task
  commands in its own sandbox with its own shell (for example
  `run-suite`, `shard … 1 &`, `cat /usr/local/bin/run-suite`), against the
  prompt's "Do this work inside a child sandbox, not in your own sandbox".
  The codes it wrote never appeared in any MCP reply; the right code did.
- **Why it was possible:** the task commands are in the image, and
  children copy their lead's image, so the lead has them too. Run in the
  lead, they draw different random values, so the code never matches the
  child's.
- **Not changed:** the commands still run in a lead, and the runs were not
  repeated. These failures say more about a weak model drifting into doing
  the work itself than about the waiting approaches, and should be read
  that way when comparing approaches for Lightning.
- Claude Haiku 5.5 and DeepSeek V4.1 Flash succeeded in all 84 of their
  S2a runs.
