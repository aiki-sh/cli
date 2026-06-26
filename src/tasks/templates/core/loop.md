---
version: 2.1.0
type: orchestrator
---

# Loop: {{data.target}}

You are orchestrating the execution of subtasks under {{data.target}}.

## Step 1: Understand the work

    aiki task show {{data.target}}
    aiki task lane {{data.target}} --all

## Step 2: Execute the orchestration loop

Drive the loop below. It spawns each ready lane asynchronously, then waits in
**bounded** 30-second steps — a finished thread may unblock new lanes or the
next thread in a lane, so you re-check after every wait.

```bash
while true; do
  ready=$(aiki task lane {{data.target}} -o id)
  [ -z "$ready" ] && break

  sids=()
  for lane in $ready; do
    sid=$(aiki run {{data.target}} --next-thread --lane $lane --async -o id) || {
      rc=$?
      [ $rc -eq 2 ] && continue   # AllComplete for this lane
      exit $rc                     # real error
    }
    [ -n "$sid" ] && sids+=("$sid")
  done

  [ ${#sids[@]} -eq 0 ] && break

  # Bounded wait: returns within 30s. Exit 124 = timed out, still running
  # (re-loop and wait again); exit 0 = a thread finished (re-check lanes).
  echo "orchestrator: waiting up to 30s on ${#sids[@]} lane(s): ${sids[*]}"
  aiki session wait "${sids[@]}" --any --timeout 30 || true
done
echo "orchestrator: all lanes complete"
```

## CRITICAL: never end your turn while lanes are running

You run in headless mode. If you end your turn before the loop finishes, the
loop task is never closed and the build is reported as **failed** — even though
the subtasks may still be running. Rules:

- Keep each command short. The `--timeout 30` on `aiki session wait` returns
  control to you every 30 seconds; never run a command that blocks for the
  whole build.
- Do **not** end your turn and do **not** wait for a "background command
  completed" notification — it never arrives in headless mode. Keep driving the
  loop in this same turn until every lane shows done/closed.
- If the harness still moves a command to the background, do not stop: re-check
  `aiki task lane {{data.target}} --all` every few seconds (in this turn) until
  all lanes are complete.
- Only after every lane is complete do you run the Completion step.

## Failure handling

If a thread fails, its lane cannot proceed. Dependent lanes are also blocked;
independent lanes continue.

    aiki task lane {{data.target}} --all

If unrecoverable:

    aiki task stop {{id}} --reason "Failed: <reason>"

## Completion

When all lanes are complete:

    aiki task close {{data.target}} --confidence <1-4> --summary "All subtasks completed"
    aiki task close {{id}} --confidence <1-4> --summary "All lanes completed"
