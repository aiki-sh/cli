You are testing aiki's workspace isolation system with concurrent agents. Run through ALL phases carefully and report results.

## Setup
- Run `aiki task start "Test workspace isolation: 10-agent stress test" --source prompt`
- Note the task ID as PARENT

---

## Phase 1: Separate files (10 agents)

Each agent creates its own unique file. This tests basic parallel isolation at scale.

### 1.1 Launch 10 agents

Create 10 tasks:

    aiki task add "Agent 01: create file agent-01.txt with contents 'Hello from agent 01 — written at <timestamp>'"
    aiki task add "Agent 02: create file agent-02.txt with contents 'Hello from agent 02 — written at <timestamp>'"
    aiki task add "Agent 03: create file agent-03.txt with contents 'Hello from agent 03 — written at <timestamp>'"
    aiki task add "Agent 04: create file agent-04.txt with contents 'Hello from agent 04 — written at <timestamp>'"
    aiki task add "Agent 05: create file agent-05.txt with contents 'Hello from agent 05 — written at <timestamp>'"
    aiki task add "Agent 06: create file agent-06.txt with contents 'Hello from agent 06 — written at <timestamp>'"
    aiki task add "Agent 07: create file agent-07.txt with contents 'Hello from agent 07 — written at <timestamp>'"
    aiki task add "Agent 08: create file agent-08.txt with contents 'Hello from agent 08 — written at <timestamp>'"
    aiki task add "Agent 09: create file agent-09.txt with contents 'Hello from agent 09 — written at <timestamp>'"
    aiki task add "Agent 10: create file agent-10.txt with contents 'Hello from agent 10 — written at <timestamp>'"

Launch all 10 with --async:

    aiki task run <id-01> --async
    aiki task run <id-02> --async
    aiki task run <id-03> --async
    aiki task run <id-04> --async
    aiki task run <id-05> --async
    aiki task run <id-06> --async
    aiki task run <id-07> --async
    aiki task run <id-08> --async
    aiki task run <id-09> --async
    aiki task run <id-10> --async

### 1.2 Monitor workspaces

While agents are running:

- `ls /tmp/aiki/` — verify workspace directories exist for each agent session
- `jj workspace list` — confirm agent workspaces are listed
- Spot-check 2-3 workspace dirs with `ls` to confirm agents are working in isolated copies

### 1.3 Wait and verify

    aiki task wait <id-01> <id-02> <id-03> <id-04> <id-05> <id-06> <id-07> <id-08> <id-09> <id-10>

After all 10 finish:

- `jj log -r ..@` — should show 10 separate absorbed changes
- For each change, run `jj show <change-id>` and confirm:
  - The file diff is correct (agent-NN.txt for the right agent)
  - The description has an `[aiki]` metadata block with author, session, tool, task fields
  - Each change references a DIFFERENT session ID (proves isolation)
  - Each change references the correct task ID
- Verify NO cross-contamination: each agent's file only appears in its own change
  - Spot-check at least 3 agents: `jj diff -r <change>` should show exactly one file

### 1.4 Verify cleanup

- `ls /tmp/aiki/` — workspace dirs should be gone (or only contain unrelated dirs)
- `jj workspace list` — only `default` should remain
- Check `~/.aiki/sessions/by-repo/` — agent session sidecars should be cleaned up

### 1.5 Cleanup phase 1

Delete all agent-NN.txt files:

    rm -f agent-01.txt agent-02.txt agent-03.txt agent-04.txt agent-05.txt agent-06.txt agent-07.txt agent-08.txt agent-09.txt agent-10.txt

Record phase 1 results before continuing.

---

## Phase 2: Same file, different locations (5 agents)

5 agents all edit a single shared file, but each edits a DIFFERENT clearly-separated section.
This tests JJ's ability to merge non-conflicting concurrent edits to the same file.

### 2.0 Setup the shared file

Create `shared-sections.txt` with this exact content (copy-paste or write it):

```
=== SECTION 1 ===
placeholder line 1

=== SECTION 2 ===
placeholder line 2

=== SECTION 3 ===
placeholder line 3

=== SECTION 4 ===
placeholder line 4

=== SECTION 5 ===
placeholder line 5
```

Commit this file so all agents see it:

    jj commit -m "Add shared-sections.txt for isolation test phase 2"

### 2.1 Launch 5 agents

Each agent replaces the placeholder in its assigned section:

    aiki task add "Agent S1: In shared-sections.txt, replace 'placeholder line 1' with 'Agent S1 was here at <timestamp>'. Do NOT touch any other section."
    aiki task add "Agent S2: In shared-sections.txt, replace 'placeholder line 2' with 'Agent S2 was here at <timestamp>'. Do NOT touch any other section."
    aiki task add "Agent S3: In shared-sections.txt, replace 'placeholder line 3' with 'Agent S3 was here at <timestamp>'. Do NOT touch any other section."
    aiki task add "Agent S4: In shared-sections.txt, replace 'placeholder line 4' with 'Agent S4 was here at <timestamp>'. Do NOT touch any other section."
    aiki task add "Agent S5: In shared-sections.txt, replace 'placeholder line 5' with 'Agent S5 was here at <timestamp>'. Do NOT touch any other section."

Launch all 5 with --async:

    aiki task run <id-s1> --async
    aiki task run <id-s2> --async
    aiki task run <id-s3> --async
    aiki task run <id-s4> --async
    aiki task run <id-s5> --async

### 2.2 Wait and verify merges

    aiki task wait <id-s1> <id-s2> <id-s3> <id-s4> <id-s5>

After all 5 finish:

- `jj log -r ..@` — should show 5 new absorbed changes (plus the setup commit)
- Check each change touched only shared-sections.txt
- **Critical check:** `cat shared-sections.txt` (from the working copy) — verify:
  - All 5 placeholder lines have been replaced with agent messages
  - No conflict markers (`<<<<<<<`, `>>>>>>>`, `%%%%%%%`) in the file
  - Section headers (`=== SECTION N ===`) are intact
  - Each agent's message appears in the correct section
- `jj status` — verify no conflicts reported

### 2.3 Cleanup phase 2

    rm -f shared-sections.txt

Record phase 2 results before continuing.

---

## Phase 3: Same file, same location (5 agents — conflict test)

5 agents all try to edit the SAME line in a file. This WILL produce conflicts.
The goal is to verify that:
- JJ detects the conflicts correctly
- Conflict markers are well-formed
- No data is silently lost

### 3.0 Setup the contested file

Create `contested.txt` with this exact content:

```
Line before
REPLACE THIS LINE
Line after
```

Commit it:

    jj commit -m "Add contested.txt for isolation test phase 3"

### 3.1 Launch 5 agents

Each agent tries to replace the same line with a different value:

    aiki task add "Agent C1: In contested.txt, replace 'REPLACE THIS LINE' with 'Agent C1 claims this line at <timestamp>'"
    aiki task add "Agent C2: In contested.txt, replace 'REPLACE THIS LINE' with 'Agent C2 claims this line at <timestamp>'"
    aiki task add "Agent C3: In contested.txt, replace 'REPLACE THIS LINE' with 'Agent C3 claims this line at <timestamp>'"
    aiki task add "Agent C4: In contested.txt, replace 'REPLACE THIS LINE' with 'Agent C4 claims this line at <timestamp>'"
    aiki task add "Agent C5: In contested.txt, replace 'REPLACE THIS LINE' with 'Agent C5 claims this line at <timestamp>'"

Launch all 5 with --async:

    aiki task run <id-c1> --async
    aiki task run <id-c2> --async
    aiki task run <id-c3> --async
    aiki task run <id-c4> --async
    aiki task run <id-c5> --async

### 3.2 Wait and verify conflict handling

    aiki task wait <id-c1> <id-c2> <id-c3> <id-c4> <id-c5>

After all 5 finish:

- `jj log -r ..@` — should show 5 absorbed changes
- `jj status` — check if conflicts are reported
- `cat contested.txt` — examine the file:
  - If JJ reports conflicts: verify conflict markers are present and well-formed
  - Verify ALL 5 agents' content appears somewhere in the conflict markers (no silent data loss)
  - The "Line before" and "Line after" sentinel lines should be intact (conflict is localized)
- For each change: `jj show <change-id>` — confirm each agent's edit is recorded independently
- Verify each change has a DIFFERENT session ID in its `[aiki]` metadata

### 3.3 Cleanup phase 3

Resolve any conflicts (pick any agent's version or delete the file):

    rm -f contested.txt

---

## Final Summary

Close the parent task with results:

    aiki task close <PARENT> --summary "Results: Phase 1 (10 separate files): PASS/FAIL. Phase 2 (same file, different sections): PASS/FAIL. Phase 3 (same file, same line — conflict): PASS/FAIL. Details: ..."

**Report format:** For each phase and sub-check, state PASS or FAIL with details. Include any error output verbatim. Pay special attention to:
- Were all workspaces created and cleaned up?
- Were all changes absorbed with correct metadata?
- Did non-conflicting same-file merges succeed cleanly?
- Were conflicts detected and reported correctly (no silent data loss)?
