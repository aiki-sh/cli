You are testing aiki's conflict resolution system. Run through ALL phases carefully and report results.

## Setup
- Run `aiki task start "Test conflict resolution: comprehensive stress test" --source prompt`
- Note the task ID as PARENT

---

## Phase 1: Conflict Detection After Absorption

Two agents edit the SAME line in the same file. Verify that conflicts are correctly detected post-absorption and the autoreply message fires.

### 1.0 Setup the contested file

Create `conflict-test.txt` with this exact content:

```
Header line
TARGET LINE
Footer line
```

Commit it:

    jj commit -m "Add conflict-test.txt for conflict resolution test phase 1"

### 1.1 Launch 2 agents that will conflict

    aiki task add "Agent A1: In conflict-test.txt, replace 'TARGET LINE' with 'Agent A1 wrote this at <timestamp>'"
    aiki task add "Agent A2: In conflict-test.txt, replace 'TARGET LINE' with 'Agent A2 wrote this at <timestamp>'"

Launch both with --async:

    aiki run <id-a1> --async
    aiki run <id-a2> --async

### 1.2 Wait and verify conflict detection

    aiki task wait <id-a1> <id-a2>

After both finish:

- `jj status` — should report conflicts
- `jj log -r 'conflicts() & @'` — should show conflicted change(s) at working copy
- `cat conflict-test.txt` — examine the file for conflict markers

**Critical checks:**
- Conflict markers are present in the file
- The `Header line` and `Footer line` sentinel lines are INTACT (conflict is localized)
- Both agents' content appears somewhere in the conflict block (no silent data loss)
- Markers use JJ format (NOT Git format):
  - `<<<<<<< Conflict N of M` — start marker
  - `%%%%%%% Changes from base` — diff section (shows additions/removals, NOT literal content)
  - `+++++++ Contents of side #2` — literal content section
  - `>>>>>>> Conflict N of M ends` — end marker
- The `%%%%%%%` section contains diff lines (prefixed with `+` or `-`), NOT raw content
- Each absorbed change has a DIFFERENT session ID in its `[aiki]` metadata

### 1.3 Verify the autoreply fired

Check that at least one agent received a `CONFLICT RESOLUTION REQUIRED` autoreply. This can be verified by examining the agent session logs or task comments. The autoreply should mention:
- "CONFLICT RESOLUTION REQUIRED"
- The conflicted file name (`conflict-test.txt`)
- Instructions about JJ conflict marker format

### 1.4 Cleanup phase 1

Resolve the conflict by picking either agent's version (or squash both changes):

    jj resolve conflict-test.txt

Or manually edit the file to remove conflict markers, then verify:

    jj resolve --list
    # Should return empty (no remaining conflicts)

    rm -f conflict-test.txt

Record phase 1 results before continuing.

---

## Phase 2: JJ Conflict Marker Format Validation

This phase creates a controlled conflict and verifies the EXACT structure of JJ's conflict markers, which differ from Git's.

### 2.0 Setup

Create `marker-test.txt`:

```
Line 1: untouched
Line 2: untouched
Line 3: ORIGINAL CONTENT HERE
Line 4: untouched
Line 5: untouched
```

Commit it:

    jj commit -m "Add marker-test.txt for conflict marker format test"

### 2.1 Create a controlled conflict

    aiki task add "Agent M1: In marker-test.txt, replace 'ORIGINAL CONTENT HERE' with 'ALPHA modification by M1'. Do NOT touch any other lines."
    aiki task add "Agent M2: In marker-test.txt, replace 'ORIGINAL CONTENT HERE' with 'BETA modification by M2'. Do NOT touch any other lines."

Launch both:

    aiki run <id-m1> --async
    aiki run <id-m2> --async
    aiki task wait <id-m1> <id-m2>

### 2.2 Validate marker structure

Read `marker-test.txt` and verify:

1. **Surrounding lines are intact**: Lines 1, 2, 4, 5 are exactly as originally written
2. **Conflict block structure** (in order):
   - Line starting with `<<<<<<< Conflict 1 of 1`
   - Line starting with `%%%%%%%` — this is the DIFF section
   - One or more diff lines showing changes from base (with `+`/`-` prefixes)
   - Line starting with `+++++++` — this is the LITERAL CONTENT section
   - One or more content lines (the other side's actual text)
   - Line starting with `>>>>>>> Conflict 1 of 1 ends`
3. **The %%%%%%% section is a DIFF**: It should show something like:
   ```
   -ORIGINAL CONTENT HERE
   +ALPHA modification by M1
   ```
   (or the reverse, depending on which side is the diff)
4. **The +++++++ section is LITERAL CONTENT**: It should show:
   ```
   BETA modification by M2
   ```
   (or vice versa)
5. **There is exactly 1 conflict block** (agents only edited one line)

### 2.3 Cleanup phase 2

    rm -f marker-test.txt

Record phase 2 results before continuing.

---

## Phase 3: Manual Conflict Resolution (Edit-in-Place)

Test the primary resolution workflow: an agent receives the CONFLICT RESOLUTION REQUIRED autoreply, edits the file to remove markers, and the next absorption succeeds cleanly.

### 3.0 Setup

Create `resolve-manual.txt`:

```
=== HEADER ===
CONTESTED LINE
=== FOOTER ===
```

Commit it:

    jj commit -m "Add resolve-manual.txt for manual resolution test"

### 3.1 Create the conflict

    aiki task add "Agent R1: In resolve-manual.txt, replace 'CONTESTED LINE' with 'R1 version: implemented feature X'"
    aiki task add "Agent R2: In resolve-manual.txt, replace 'CONTESTED LINE' with 'R2 version: implemented feature Y'"

    aiki run <id-r1> --async
    aiki run <id-r2> --async
    aiki task wait <id-r1> <id-r2>

Verify conflicts exist:

    jj status
    # Should show conflicts

### 3.2 Resolve by editing the file directly

Now manually edit `resolve-manual.txt` to replace the entire conflict block with merged content:

```
=== HEADER ===
Merged: implemented both feature X and feature Y
=== FOOTER ===
```

The file must have NO conflict markers remaining.

### 3.3 Verify resolution succeeded

- `jj status` — should report NO conflicts
- `jj resolve --list` — should return empty
- `cat resolve-manual.txt` — should show the clean merged content
- The working copy should be a clean, non-conflicted change

### 3.4 Cleanup phase 3

    rm -f resolve-manual.txt

Record phase 3 results before continuing.

---

## Phase 4: `aiki resolve` Command Workflow

Test the `aiki resolve <change-id>` command that creates a structured resolve task from the template.

### 4.0 Setup

Create `resolve-cmd-test.txt`:

```
First section content
BATTLE LINE
Last section content
```

Commit it:

    jj commit -m "Add resolve-cmd-test.txt for aiki resolve command test"

### 4.1 Create the conflict

    aiki task add "Agent RC1: In resolve-cmd-test.txt, replace 'BATTLE LINE' with 'RC1 claims this'"
    aiki task add "Agent RC2: In resolve-cmd-test.txt, replace 'BATTLE LINE' with 'RC2 claims this'"

    aiki run <id-rc1> --async
    aiki run <id-rc2> --async
    aiki task wait <id-rc1> <id-rc2>

Verify conflicts:

    jj status

### 4.2 Get the conflicted change ID

    jj log -r 'conflicts() & @' --no-graph -T 'change_id ++ "\n"'

Note the change ID.

### 4.3 Run `aiki resolve`

    aiki resolve <change-id>

**Verify the resolve task was created correctly:**
- The task should have subtasks matching the resolve template:
  1. Understand What Conflicted
  2. Identify Conflicted Files
  3. Understand the Conflict Types
  4. Resolve Conflicts
  5. Verify Resolution
- The task source should be `conflict:<change-id>`
- The task description should reference the correct change ID

### 4.4 Work through the resolve task subtasks

Follow the template's guided workflow:

1. **Understand What Conflicted**: Run the jj commands to inspect both parents
   ```bash
   jj diff -r <change-id>
   jj log -r <change-id> --no-graph -T 'parents.map(|c| c.change_id() ++ " " ++ c.description().first_line()).join("\n")'
   ```

2. **Identify Conflicted Files**: Run `jj resolve --list -r <change-id>`
   - Should list `resolve-cmd-test.txt`

3. **Understand Conflict Types**: Determine this is a "competing modifications" conflict

4. **Resolve Conflicts**: Edit the file to remove markers

5. **Verify Resolution**: Run `jj resolve --list -r <change-id>` — should return empty

### 4.5 Close the resolve task

    aiki task close <resolve-task-id> --summary "Resolved conflict in resolve-cmd-test.txt: merged competing modifications"

### 4.6 Cleanup phase 4

    rm -f resolve-cmd-test.txt

Record phase 4 results before continuing.

---

## Phase 5: Multiple Conflicts in One File

Three agents all modify DIFFERENT lines in the same file, but two of those lines happen to be adjacent, creating overlapping conflicts.

### 5.0 Setup

Create `multi-conflict.txt`:

```
Line A: stable
Line B: REPLACE_B
Line C: REPLACE_C
Line D: REPLACE_D
Line E: stable
```

Commit it:

    jj commit -m "Add multi-conflict.txt for multi-conflict test"

### 5.1 Launch 3 agents

    aiki task add "Agent MC1: In multi-conflict.txt, replace 'REPLACE_B' with 'MC1 edited B'"
    aiki task add "Agent MC2: In multi-conflict.txt, replace 'REPLACE_C' with 'MC2 edited C'"
    aiki task add "Agent MC3: In multi-conflict.txt, replace 'REPLACE_D' with 'MC3 edited D'"

    aiki run <id-mc1> --async
    aiki run <id-mc2> --async
    aiki run <id-mc3> --async
    aiki task wait <id-mc1> <id-mc2> <id-mc3>

### 5.2 Examine the conflict structure

- `jj status` — check conflict status
- `cat multi-conflict.txt` — examine the file

**Verify:**
- Stable lines (`Line A: stable`, `Line E: stable`) are intact
- Either:
  - Multiple separate conflict blocks (one per contested line), OR
  - One merged conflict block covering the adjacent lines
- ALL three agents' modifications appear somewhere in the conflict markers
- No data is silently lost — each agent's change is recorded

**Count the conflicts:**
- `jj resolve --list` — note how many conflict blocks exist
- If JJ merges adjacent conflicts into one block, that's expected behavior (document it)

### 5.3 Resolve all conflicts

Edit the file to merge all three agents' changes:

```
Line A: stable
Line B: MC1 edited B
Line C: MC2 edited C
Line D: MC3 edited D
Line E: stable
```

Verify:

    jj resolve --list
    # Should return empty

### 5.4 Cleanup phase 5

    rm -f multi-conflict.txt

Record phase 5 results before continuing.

---

## Phase 6: Modification vs Deletion Conflict

One agent modifies a line while another agent deletes it entirely. This tests a different conflict type.

### 6.0 Setup

Create `mod-vs-delete.txt`:

```
Keep this line
DELETE OR MODIFY THIS LINE
Keep this line too
```

Commit it:

    jj commit -m "Add mod-vs-delete.txt for modification vs deletion test"

### 6.1 Launch 2 agents

    aiki task add "Agent MD1: In mod-vs-delete.txt, replace 'DELETE OR MODIFY THIS LINE' with 'MD1 modified this line with new content'"
    aiki task add "Agent MD2: In mod-vs-delete.txt, delete the line 'DELETE OR MODIFY THIS LINE' entirely (remove it, don't replace it)"

    aiki run <id-md1> --async
    aiki run <id-md2> --async
    aiki task wait <id-md1> <id-md2>

### 6.2 Examine the conflict

- `jj status` — should report conflicts
- `cat mod-vs-delete.txt` — examine the file

**Verify:**
- A conflict block exists showing the modification vs deletion
- The sentinel lines (`Keep this line`, `Keep this line too`) are intact
- The %%%%%%% section shows the deletion (removing the line)
- The +++++++ section shows the modification (or vice versa)
- Neither side's intent is silently dropped

### 6.3 Resolve the conflict

Choose one resolution strategy (either keep the modification or honor the deletion):

Edit the file to resolve, then verify:

    jj resolve --list
    # Should return empty

### 6.4 Cleanup phase 6

    rm -f mod-vs-delete.txt

Record phase 6 results before continuing.

---

## Phase 7: Conflict Resolution Preserves Provenance

Verify that after resolving a conflict, the original agents' changes still have correct `[aiki]` metadata in their change descriptions.

### 7.0 Setup

Create `provenance-test.txt`:

```
PROVENANCE LINE
```

Commit it:

    jj commit -m "Add provenance-test.txt for provenance preservation test"

### 7.1 Create conflict with known agents

    aiki task add "Agent PV1: In provenance-test.txt, replace 'PROVENANCE LINE' with 'PV1 was here'"
    aiki task add "Agent PV2: In provenance-test.txt, replace 'PROVENANCE LINE' with 'PV2 was here'"

    aiki run <id-pv1> --async
    aiki run <id-pv2> --async
    aiki task wait <id-pv1> <id-pv2>

### 7.2 Before resolving: check provenance

    jj log -r ..@ --no-graph -T 'change_id ++ "\n" ++ description ++ "\n---\n"'

**Verify for each agent's absorbed change:**
- Has an `[aiki]` metadata block in the description
- Contains `agent=` field
- Contains `session=` field (and the two sessions differ)
- Contains `task=` field (matching the respective task IDs)

### 7.3 Resolve the conflict

Edit `provenance-test.txt` to remove conflict markers (pick either version).

### 7.4 After resolving: verify provenance is preserved

    jj log -r ..@ --no-graph -T 'change_id ++ "\n" ++ description ++ "\n---\n"'

**Critical check:** The `[aiki]` metadata blocks from both agents' original changes are STILL present and unchanged. Conflict resolution should NOT strip or modify provenance metadata from the absorbed changes.

### 7.5 Cleanup phase 7

    rm -f provenance-test.txt

Record phase 7 results before continuing.

---

## Final Summary

Close the parent task with results:

    aiki task close <PARENT> --summary "Results: Phase 1 (conflict detection): PASS/FAIL. Phase 2 (marker format): PASS/FAIL. Phase 3 (manual resolution): PASS/FAIL. Phase 4 (aiki resolve command): PASS/FAIL. Phase 5 (multi-conflict): PASS/FAIL. Phase 6 (mod vs delete): PASS/FAIL. Phase 7 (provenance preservation): PASS/FAIL. Details: ..."

**Report format:** For each phase and sub-check, state PASS or FAIL with details. Include any error output verbatim. Pay special attention to:
- Were conflicts correctly detected post-absorption (not pre-absorption)?
- Did the autoreply message fire with correct content?
- Are JJ conflict markers in the correct format (%%%%%%% diff, NOT Git =======)?
- Did manual edit-in-place resolution work cleanly?
- Did `aiki resolve` create the correct task structure?
- Were multiple conflicts handled correctly (no silent data loss)?
- Was modification-vs-deletion conflict type handled properly?
- Was provenance metadata preserved through conflict resolution?
