---
version: 2.0.0
type: orchestrator
---

# Loop: {{data.target}}

You are orchestrating the execution of subtasks under {{data.target}}.

## Step 1: Understand the work

    aiki task show {{data.target}}
    aiki task lane {{data.target}} --all

## Step 2: Execute

Loop until all lanes are complete:

1. Get ready lanes via `aiki task lane {{data.target}} -o id`
2. For each ready lane, start it with `aiki run {{data.target}} --next-thread --lane <lane-id> --async -o id`
3. Collect the session IDs from started threads
4. Wait for any to finish with `aiki session wait <sid1> <sid2> ... --any`
5. Loop back — finished thread may have unblocked new lanes or the next thread in a lane

```bash
while true; do
  ready=$(aiki task lane {{data.target}} -o id)
  [ -z "$ready" ] && break

  sids=()
  for lane in $ready; do
    sid=$(aiki run {{data.target}} --next-thread --lane $lane --async -o id) || {
      rc=$?
      [ $rc -eq 2 ] && continue  # AllComplete for this lane
      exit $rc                    # real error
    }
    [ -n "$sid" ] && sids+=("$sid")
  done

  [ ${#sids[@]} -eq 0 ] && break
  aiki session wait "${sids[@]}" --any
done
```

**How it works:**

1. Get ready lanes (may be empty if all lanes are blocked or running)
2. Start threads for ready lanes, collecting session IDs
3. Handle exit code 2 (AllComplete) per lane — skip that lane
4. Wait for any running thread to finish via `aiki session wait`
5. Loop back - finished thread may have unblocked new lanes
6. Exit when no ready lanes remain

## Failure handling

If a thread fails, its lane cannot proceed. Dependent lanes
are also blocked. Independent lanes continue.

    aiki task lane {{data.target}} --all

If unrecoverable:

    aiki task stop {{id}} --reason "Failed: <reason>"

## Completion

When all lanes are complete:

    aiki task close {{data.target}} --confidence <1-4> --summary "All subtasks completed"
    aiki task close {{id}} --confidence <1-4> --summary "All lanes completed"
