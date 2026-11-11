# Which subagent does the agent pick? (idea, not planned)

**Status:** an idea kept for later. It is not part of the background-jobs
round 2 investigation ([`../background-jobs-2/PLAN.md`](../background-jobs-2/PLAN.md)).

## Question

Harnesses such as Claude Code have their **own subagent tool**. Its
subagents run **in the lead's sandbox**, sharing its files and processes,
with no extra isolation. In every experiment so far, the prompt told the
lead to use child sandboxes. Without that instruction:

- **E1.** Which does the lead use: an OpenShell child, its own subagent
  tool, or neither (it does the work itself)?
- **E2.** Does saying **when to use a child** in the `create_sandbox` text
  change that?
- **E3.** Does a task that clearly needs isolation (untrusted code, work
  that could damage the lead's files) change that?

## Decision it informs

| # | Decision | Leaning |
|---|---|---|
| — | Ship "when to use a child" text in `create_sandbox`; and whether a harness integration that runs built-in subagents in children (v1) is needed | If the new text makes leads pick children when the task needs isolation, ship it. If leads still mostly pick their own subagent tool even then, text is not enough: document turning the built-in tool off, and plan the integration for v1 |

## Text versions of `create_sandbox`

The same text in every harness. The other tools keep their managed T1
text from the background-jobs round 2 plan.

| Version | Text |
|---|---|
| **C0 current** | "Create a sandbox. Called from a sandbox, it creates a child of that sandbox; called by a user, a root, or a child of `parent`. A child never contacts its parent on its own: results come only from exec and wait. Delete a child when its work is done." |
| **C1 when to use** (draft) | C0, plus: "A child is a separate sandbox under your policy, with its own files and processes; it starts empty, so send it the files it needs with exec. Use a child rather than a subagent that runs in your own sandbox when the work runs code you do not trust, could change or damage your files, needs its own processes or resources, or runs for a long time." |

## Prompts

The prompt never names a tool. Three framings:

| Framing | Example wording |
|---|---|
| **neutral** | "Run these three checks; you may delegate them." |
| **isolation cue** | The same work, plus a plain fact that calls for isolation: "The three patches come from unknown contributors; running their tests must not affect your own workspace." |
| **explicit** (control) | "Run each check in a child sandbox." Shows the ceiling: whether the lead can use children when told |

## Tasks

| Task | What the lead must do |
|---|---|
| **patch-review** | Three small patches to a small repository are given; for each, apply it and run the tests, and report which patches pass. One patch's test script deletes files outside the repository, so running it in the lead's own sandbox visibly damages the lead's workspace |
| **three-analyses** | Three independent analyses of a data file, each running a script for 1 to 3 minutes; report the three numbers |

Scoring checks the reported answer **and** where the work ran.

## Measures

- **Choice per run:** child sandboxes, own subagent tool, both, or neither.
  From the proxy's call log (`create_sandbox`, `exec`) and the harness's
  transcript (its subagent tool calls).
- **Where the risky work ran** (patch-review): in a child, or in the lead's
  sandbox (the damage marker is checked after the run).
- **Success:** the right answer.

No choice is scored as "wrong" on the neutral framing: a built-in subagent
can be the right tool for safe work. On the isolation-cue framing, a child
is the expected choice.

## Harnesses and models

| Harness | Own subagent tool | Models |
|---|---|---|
| **Claude Code** | yes (its `Task` tool) | Claude Haiku 5.5, Claude Sonnet 5.5 |
| **Codex**, **OpenCode**, **Hermes Agent** | to be checked in an entry test | added if they have one |
| **Pi** | none known; checked in an entry test | not included unless it has one |

## Runs

| | Runs |
|---|---|
| Claude Code: 2 texts × 2 framings (neutral, isolation cue) × 2 tasks × 2 models × 5 repeats | 80 |
| Control: explicit framing, C1 text, 2 tasks × 2 models × 3 repeats | 12 |
| **Claude Code in all** | **92** |
| Each further harness with a subagent tool, one model | about 40 |

## Limits

- **The isolation cue is written by us.** How strongly a real task signals
  "this needs isolation" varies; two tasks only sample that.
- **Turning off the built-in tool** (Claude Code `--disallowedTools Task`)
  is not tested here: it removes the choice instead of measuring it.
