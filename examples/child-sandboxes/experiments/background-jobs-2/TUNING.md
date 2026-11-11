# Text tuning record (stage S1)

Each round of S1 runs the four approaches with the current T1 text on the
practice tasks with the tuning models (PLAN.md, Section 9). After a round,
every run's trace is read; text changes follow the T1 rules (PLAN.md,
Section 5) and apply to every approach they concern. The final text is the
one in PLAN.md and `mcp-proxy.mjs`.

## Round 1 (stage s1-1)

**Runs:** 32 of 32 successful in the end, but one plain run (Nemotron 3
Ultra, single) first **failed at the 25-minute run limit**. It was wrongly
marked as an infrastructure failure, because a token count of 429 matched
the rate-limit check (since fixed), and was run again. That failed try
counts as a failure: **31 of 32 first tries succeeded.**

**The failed try:** the lead ran `process-batch` in the foreground (timed
out), then ran the plain check loop on `/tmp/jobs/process-batch/exit` 53
times for 24 minutes, for a job it had never started detached, so the exit
file could never appear. Change B below addresses this. A further option,
kept for round 3 if needed: make the plain check loop print `no such job`
when the job's folder does not exist.

**Findings:**

| Finding | managed | plain | pid | auto |
|---|---|---|---|---|
| First command run in the foreground, which timed out after 30 s | 6 of 6 runs | 7 of 7 | 7 of 7 | not possible |
| Timed-out `exec` calls, all runs together | 7 | 28 | 31 | 0 |
| `wait` called after a foreground timeout, with no job (refused) | 4 runs | — | — | — |
| Check loop lengthened past 25 s (30, 45, 60), so it timed out | — | 12 calls | 26 calls | — |
| `sleep` in the lead's own shell | 0 | 13 calls | 0 | 0 |
| Polling with bare `sleep` and `cat` in the child | — | some | 1 run | — |
| A timed-out command started again in the foreground | — | some | some | — |

The rule "if you are not sure, use the background" was not followed: no
lead put an unknown command in the background first.

**Changes for round 2:**

- **A (managed, plain, pid):** the foreground is only for commands known to
  finish within a few seconds, such as reading or listing files; every
  other command, including any not run before, goes to the background.
- **B (managed, plain, pid):** a foreground command that times out is not a
  background job (with managed: `wait` cannot collect it) and may still be
  running; check with `ps` before starting it again.
- **C (plain, pid):** keep the check loop at 25 seconds, since `exec` stops
  after `timeout_s` (30 by default); do not wait with `sleep` alone, in the
  child or in the lead's own shell.
- auto: no change.

## Round 2 (stage s1-2)

**Runs:** 32 of 32 successful on the first try.

**Findings**, 8 runs per approach, round 1 → round 2:

| Finding | managed | plain | pid | auto |
|---|---|---|---|---|
| First command run in the foreground, which timed out | 8 → 8 | 8 → 8 | 8 → 8 | 0 → 0 |
| Timed-out `exec` calls | 10 → 10 | 33 → 30 | 33 → 33 | 0 → 0 |
| Refused calls (`wait` with no job) | 5 → 3 | — | — | 0 → 0 |
| Check loop lengthened past 25 s | — | 17 → 24 | 27 → 20 | — |
| `sleep` in the lead's own shell | 0 → 0 | 13 → 4 | 0 → 0 | 0 → 0 |
| Bare `sleep` polling in the child | 0 → 0 | 8 → 2 | 15 → 8 | 0 → 0 |

(Round 1 counts here include all 32 runs; the round 1 section above
counted the 28 read first.)

**What the traces show:**

- **Change A had no effect.** The leads never weigh foreground against
  background: their reasoning goes straight from "now I need to run the
  command" to a foreground `exec`. The guidance is in the tool text, but
  nothing prompts a decision at that moment.
- **Change B did not remove the belief it targets.** After a foreground
  timeout, leads still assume the command now runs in the background, for
  example: "The command timed out, meaning it's still running in the
  background. Let me wait for it by checking the exit file."
- **Change C helped only with sleeping:** `sleep` in the lead's own shell
  and bare `sleep` polling fell, but leads still lengthened the check loop
  to wait longer and then hit `exec`'s limit.
- **Auto avoided all of it**, because it has no foreground or background
  choice to make.

**Conclusion:** further wording is unlikely to change the foreground-first
habit and risks tuning to the practice tasks. Round 3 makes only the
mechanical changes below, then T1 is frozen. A design idea to note for the
report: a foreground `exec` that times out could return a job instead of a
dead end, which is what auto does.

## Round 3 (stage s1-3)

**Changes** (prepared from round 1's failed try):

- **D (plain, pid): `no such job`.** The check loop first checks that the
  job exists (plain: its folder; pid: its `out` file) and otherwise prints
  `no such job` at once, instead of `running` until the run limit. Managed
  and auto already refuse `wait` with an unknown job, so this gives every
  approach the same honest answer.
- **E (plain): start recipe fix.** `mkdir … && setsid … &` sent the whole
  line, `mkdir` included, to the background, so the start returned before
  the job's folder existed. Now `mkdir … || exit 1; setsid … &`. In rounds
  1 and 2 this only mattered for a check made within a moment of a start.
- **pid (proxy, not text):** the job's `out` file is created before the
  paths are returned, so a check right after a start finds it.

All three were tested in Debian: a check right after a start prints
`running`, a check on a job never started (plain) or on made-up paths
(pid) prints `no such job` at once, and a finished job prints its exit
code.

**Runs read:** 27 of 32 (5 were still running when the traces were
copied).

**Findings**, round 2 → round 3 (round 3 counts cover fewer runs: managed
7, plain 8, pid 5, auto 7):

| Finding | managed | plain | pid | auto |
|---|---|---|---|---|
| First command run in the foreground, which timed out | 8 → 7 of 7 | 8 → 8 of 8 | 8 → 4 of 5 | 0 → 0 |
| Timed-out `exec` calls | 10 → 7 | 30 → 28 | 33 → 10 | 0 → 0 |
| Refused calls (`wait` with no job) | 3 → 2 | — | — | 0 → 0 |
| Check loop lengthened past 25 s | — | 24 → 8 | 20 → 9 | — |
| `no such job` replies | — | 9 | 11 | — |

- **One failure, unrelated to waiting:** a plain run (Qwen3.6,
  build-verify) collected the right code, `ddfd46e5`, then wrote
  `dfd46e5` to the result file (a copy error).
- **`no such job` worked as meant after a foreground timeout:** leads that
  checked a job they had never started (made-up paths such as
  `/tmp/build-app.out`, or a different name than the start) got the answer
  at once and started the command properly. Round 1's 24-minute loop no
  longer happens.
- **But pid's two placeholders misled:** leads put the `exit` path where
  `<OUT>` belonged, so the check answered `no such job` for a job that was
  running, and the lead restarted the command or lost track.

**All 32 runs of round 3** (read after the stage finished): 30 of 32
successful. The second failure, also in an approach without `wait`:

- **pid, Nemotron 3 Ultra, single:** after the foreground timeout, the lead
  wrote its own check, `[ -f /tmp/exec/exit ] && cat /tmp/exec/exit || echo
  running`, on a made-up path, and ran it 467 times, every 2 seconds, for
  24 minutes, until the run limit. The recipe's `no such job` guard did not
  help, because the lead never used the recipe.

**What this shows:** a guard in a recipe protects only the agents that
copy the recipe, and agents improvise. Managed's `wait` refuses an unknown
job whatever the agent improvises, so the protection lives in the tool.
Round 4 cannot change this; it is a property of the approaches.

Full round 3 counts, all 32 runs:

| Finding | managed | plain | pid | auto |
|---|---|---|---|---|
| First command run in the foreground, which timed out | 8 of 8 | 8 of 8 | 7 of 8 | 0 |
| Timed-out `exec` calls | 8 | 28 | 22 | 0 |
| Refused calls (`wait` with no job) | 3 | — | — | 0 |
| Check loop lengthened past 25 s | — | 8 | 20 | — |
| `no such job` replies | — | 9 | 17 | — |

## Round 4 (stage s1-4): repair of round 3's pid change

Under the exception in PLAN.md (Section 9): round 3's change caused a
defect, so one more round runs, with every approach again and only the
defective text changed.

- **pid:** the check takes one placeholder again, `<EXIT>`, and finds the
  `out` file next to it: `d=$(dirname <EXIT>); [ -f $d/out ] || { echo no
  such job; exit 0; }; …`. Without quotes inside, so it copies cleanly
  into the `exec` command array; job paths have no spaces.
- managed, plain, auto: unchanged from round 3.

Tested in Debian: made-up paths print `no such job` at once; a real job
right after its start prints `running`, then its exit code.

**Runs:** 31 of 32 successful.

| Finding | managed | plain | pid | auto |
|---|---|---|---|---|
| First command run in the foreground, which timed out | 8 of 8 | 8 of 8 | 8 of 8 | 0 |
| Timed-out `exec` calls | 9 | 27 | 22 | 0 |
| Refused calls (`wait` with no job) | 3 | — | — | 0 |
| Check loop lengthened past 25 s | — | 14 | 19 | — |
| `sleep` in the lead's own shell | 0 | 4 | 17 | 0 |
| Bare `sleep` polling in the child | 0 | 41 | 3 | 0 |
| `no such job` on a job that was running (pid) | — | — | 6 → **0** | — |

- **The pid repair worked:** `no such job` now answers only for jobs that
  were never started.
- **One failure, plain (Qwen3.6, single):** after the foreground timeout,
  the lead wrote its own check on a job it had never started,
  `/tmp/jobs/_process-batch/exit`, and polled it with `sleep 30` for 23
  minutes until the run limit. The same improvised-check pattern as round
  3's pid failure.

## Conclusion and freeze

Over four rounds (128 runs, the two tuning models, the two practice
tasks):

| | managed | plain | pid | auto |
|---|---|---|---|---|
| Failures | **0** | 3 (two loops to the run limit, one copy error) | 1 (a loop to the run limit) | **0** |
| First command in the foreground | every run | every run | almost every run | never (no choice) |

- **Text could not change the foreground-first habit.** Leads run an
  unknown command in the foreground whatever the text says, then mistake
  the timed-out command for a job.
- **What protects leads is the tool, not the text.** Managed's `wait`
  refuses an unknown job however the lead improvises; the plain and pid
  check loops protect only leads that copy them, and leads improvise.
- **Auto avoided every one of these problems,** because it has no
  foreground or background choice.

**T1 is frozen** as it stands in PLAN.md (Section 5) and `mcp-proxy.mjs`
at the commit that adds this section; the report cites that commit. S2
runs with this text.
