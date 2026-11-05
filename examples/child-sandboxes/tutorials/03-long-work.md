# 03 · Long work

**Goal:** run work that takes longer than one call: start background jobs,
wait for them, watch their progress, and run several workers in parallel.

Setup: [02](02-mcp-inside-a-root.md) (`lead`, `w1` and `a1` running), plus
the shell setup from the [index](README.md).

`exec` stops waiting after 45 seconds. A harness turn or a build takes
longer, so it runs as a **background job**: `exec` with `"background": true`
starts it and returns at once, and `wait` collects the result.

## 1. Start a job

```shell
r=$(mcp lead exec '{"name":"a1","command":["sh","-c","for i in 1 2 3 4 5 6; do echo step $i; sleep 2; done; echo done"],"background":true}')
echo "$r"
JOB=$(jq -r .job <<<"$r")
```

```json
{"job":"1-2679e226","state":"running"}
```

`JOB` keeps the job id; `wait` needs all of it.

A background job has no `timeout_s`. It runs until it ends or the child is
deleted.

## 2. One job per child

A child runs one background job at a time:

```shell
mcp lead exec '{"name":"a1","command":["true"],"background":true}'
```

```json
{"error":"failed_precondition","message":"a job is already running in 'a1' (job 1-2679e226); wait for it, or delete the child to stop it","retryable":false}
```

For parallel work, use one child per job (step 5). Foreground `exec` still
works while a job runs.

## 3. Watch progress

The job writes its output inside the child, under
`/tmp/openshell/jobs/<n>/`, where `<n>` is the job id before the dash
(`${JOB%%-*}`):

```shell
mcp lead exec "{\"name\":\"a1\",\"command\":[\"tail\",\"-n\",\"2\",\"/tmp/openshell/jobs/${JOB%%-*}/out\"]}" | jq -r .stdout
```

```text
step 1
step 2
```

The folder also holds `err`, `exit`, `pid` and `stdin`. Output stays there
until the next job starts in that child, so a job that writes a lot can fill
its disk.

## 4. Wait for the result

`wait` holds the call for up to `timeout_s` seconds (default 30, at most 40)
and returns as soon as the job ends. Ask for 1 second while the job runs:

```shell
mcp lead wait "{\"name\":\"a1\",\"job\":\"$JOB\",\"timeout_s\":1}"
```

```json
{"job":"1-2679e226","message":"the job is still running; call wait again","state":"running"}
```

Call it again until the state changes:

```shell
mcp lead wait "{\"name\":\"a1\",\"job\":\"$JOB\",\"timeout_s\":40}"
```

```json
{"binary_output":false,"exit_code":0,"job":"1-2679e226","state":"completed","stderr":"","stderr_truncated":false,"stdout":"step 1\nstep 2\nstep 3\nstep 4\nstep 5\nstep 6\ndone\n","stdout_truncated":false}
```

The result can be read again until the next job starts in that child. The
third state, `lost`, means the job stopped without an exit code: it was
killed, or the child restarted.

Wrong arguments are refused before anything runs:

```shell
mcp lead wait '{"name":"a1","job":"1","timeout_s":40}'
mcp lead wait "{\"name\":\"a1\",\"job\":\"$JOB\",\"timeout_s\":45}"
```

```json
{"error":"invalid_argument","message":"'job' must be a job id returned by exec","retryable":false}
{"error":"invalid_argument","message":"'timeout_s' must be between 1 and 40","retryable":false}
```

## 5. Workers in parallel

Start one job in each child, then wait for each:

```shell
for c in a1 w1; do
  mcp lead exec "{\"name\":\"$c\",\"command\":[\"sh\",\"-c\",\"sleep 5; echo $c done\"],\"background\":true}" \
    | jq -r .job > "/tmp/$c.job"
done
for c in a1 w1; do
  until r=$(mcp lead wait "{\"name\":\"$c\",\"job\":\"$(cat /tmp/$c.job)\",\"timeout_s\":40}") \
        && [ "$(jq -r .state <<<"$r")" != running ]; do :; done
  jq -c '{job, state, exit_code, stdout}' <<<"$r"
done
```

```json
{"job":"3-ffef5209","state":"completed","exit_code":0,"stdout":"a1 done\n"}
{"job":"1-6a555b98","state":"completed","exit_code":0,"stdout":"w1 done\n"}
```

The jobs ran side by side, so both finished in about 7 seconds, not 10. Job
numbers count per child.

## 6. Stop a job

There is no cancel. To stop a job, delete its child:

```shell
mcp lead delete '{"name":"a1"}'
```

Create `a1` again if you go on to 05: `mcp lead create_sandbox '{"name":"a1"}'`.

## What you learned

- `background: true` starts a job; `wait` collects it, 40 seconds per call.
- One job per child at a time; use more children for parallel work.
- Progress is in `/tmp/openshell/jobs/<n>/out` inside the child.
- Deleting a child is how you stop its job.
