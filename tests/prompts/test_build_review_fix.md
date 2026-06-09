You are testing aiki's `build --fix` pipeline end-to-end. The goal is to verify that `aiki build <plan> --fix` correctly chains build → review → fix, and to diagnose exactly where the pipeline breaks if it does.

## Setup
- Run `aiki task start "Test build --fix pipeline: task graph verification" --source prompt`
- Note the task ID as PARENT

---

## Phase 1: `aiki build <plan.md>` (baseline — no --fix)

Establish that a plain build works before testing the full pipeline.

### 1.0 Create plan file

Create `test-plan.md`:

```markdown
---
title: Add greeting utilities
scope: test-greetings/
---

# Add greeting utilities

Create a small greeting utility module for testing purposes.

## Tasks

1. Create `test-greetings/hello.py` with a function `greet(name: str) -> str` that returns `"Hello, {name}!"`
2. Create `test-greetings/farewell.py` with a function `farewell(name: str) -> str` that returns `"Goodbye, {name}!"`
```

Commit it:

    jj commit -m "Add test-plan.md for build pipeline test"

### 1.1 Run the build

    aiki build test-plan.md

Wait for it to complete.

### 1.2 Verify build results

**Check epic was created:**
- `aiki task` — should show the epic task
- `aiki task show <epic-id>` — should have subtasks, all closed

**Check files exist:**
- `test-greetings/hello.py` — should contain a `greet()` function
- `test-greetings/farewell.py` — should contain a `farewell()` function

### 1.3 Record phase 1 results

PASS if build completed, epic created with subtasks, subtasks executed, output files exist. FAIL otherwise with details.

---

## Phase 2: `aiki build <plan.md> --fix` — the main event

This is the critical test. We need to verify that `--fix` triggers the full pipeline: build → review → fix, and trace it through the task graph.

### 2.0 Create plan that will produce reviewable code

Create `test-plan-fixable.md` with intentionally simple/buggy code so the reviewer has something to find:

```markdown
---
title: Create data utilities
scope: test-datautil/
---

# Create data utilities

Build a small data utility module. Keep implementations deliberately simple.

## Tasks

1. Create `test-datautil/parser.py` with a function `parse_csv(text: str) -> list[dict]` that splits a CSV string into rows and returns a list of dicts. Use basic string splitting (not the csv module). Do not handle edge cases like quoted fields or empty lines.
2. Create `test-datautil/stats.py` with functions `mean(values)` and `median(values)`. The mean function should just do `sum(values) / len(values)` without handling empty lists. The median function should sort and return the middle element without handling even-length lists correctly.
3. Create `test-datautil/transform.py` with a function `normalize(values: list) -> list` that normalizes numeric values to 0-1 range. Use `(v - min) / (max - min)` without handling the case where all values are equal (division by zero).
```

Commit it:

    jj commit -m "Add test-plan-fixable.md for build --fix pipeline test"

### 2.1 Run build --fix

    aiki build test-plan-fixable.md --fix

This should run the full pipeline synchronously:
1. **Build phase:** create epic → decompose → loop (execute subtasks)
2. **Review phase:** create review task → run review agent → record issues
3. **Fix phase:** if issues found → create fix-parent → plan-fix → decompose → loop → post-fix review

Wait for it to complete. Note any errors or early exits.

### 2.2 Verify the task graph — build phase

Run `aiki task` to see the full task list.

**Find the epic:**
- Look for a task named something like "Create data utilities" (from the plan title)
- Run `aiki task show <epic-id>`
- **Check:** Epic should have `data.plan` field pointing to `test-plan-fixable.md`
- **Check:** Epic should have subtasks (the decomposed implementation tasks)
- **Check:** All build subtasks should be closed/completed
- **Check:** `aiki task show <epic-id>` should show subtask details

**Find the build/loop task:**
- Should be a subtask or linked task with the build execution
- Should be closed

### 2.3 Verify the task graph — review phase

This is the critical checkpoint. The review MUST exist as a task in the graph.

**Find the review task:**
- Run `aiki task list --source file:test-plan-fixable.md` to find all tasks linked to the plan
- Look for a task with "review" or "Review" in the name
- The review task should have a `validates` edge pointing to the epic

**If NO review task exists:** This is the main failure mode we're testing for. Record:
- The exact command output from `aiki build test-plan-fixable.md --fix`
- The full `aiki task` listing
- `aiki task show <epic-id>` output
- Whether the build completed synchronously or spawned an async process
- Any error messages during the review creation phase

**If a review task DOES exist, verify its structure:**
- `aiki task show <review-task-id>` — examine the full task data
- **Check `data.scope_kind`:** Should be `code`
- **Check `data.scope_id`:** Should be `test-plan-fixable.md` (the plan path)
- **Check `data.issue_count`:** Should be set (even if "0")
- **Check `data.options.fix`:** Should be `true`
- **Check `data.options.fix_template`:** Should be `fix`
- **Check task status:** Should be closed (review completed)
- `aiki review issue list <review-task-id>` — list any issues found

### 2.4 Verify the task graph — fix phase

**If the review found issues (issue_count > 0):**

Find the fix-parent task:
- Look for a task named "Fix: ..." in the task list
- Run `aiki task show <fix-parent-id>`
- **Check `data.review`:** Should point to the review task ID
- **Check `data.scope_kind`:** Should match the review's scope
- **Check source:** Should include `task:<review-task-id>`
- **Check subtasks:** Should have plan-fix and implementation subtasks
- **Check status:** All fix subtasks should be closed

**If the review found NO issues (issue_count = 0 or not set):**
- This is acceptable — the code may have been good enough
- But check: is `data.issue_count` actually set on the review task? If it's missing (not "0", but absent), this exposes Bug 2 from ops/now/fix-flag-issues.md — the `has_issues` check in `build.rs` returns `false` when the field is missing, even though issues may exist as comments
- Run `aiki task show <review-task-id>` and check if there are comments that look like issues

### 2.5 Verify the full pipeline chain

Reconstruct the chain from the task graph:

```
Epic (build target)
  ├── subtask: implementation task 1 (closed)
  ├── subtask: implementation task 2 (closed)
  ├── subtask: implementation task 3 (closed)
  └── validates ← Review task (closed)
                     ├── data.issue_count = N
                     └── source: task:<review-id> ← Fix-parent (if issues found)
                          ├── subtask: plan-fix (closed)
                          ├── subtask: fix implementation tasks (closed)
                          └── Post-fix review (if quality loop ran)
```

**Verify:**
1. Epic exists and all build subtasks are closed
2. Review task exists with `validates` edge to epic
3. Review task has `data.scope_kind=code` and `data.scope_id=<plan_path>`
4. Review task has `data.issue_count` set
5. If issues > 0: fix-parent exists with `data.review=<review_id>`
6. If fix ran: fix subtasks are closed
7. If quality loop ran: post-fix review task exists

Record the FULL task graph for the report. Use `aiki task show` for each task in the chain.

### 2.6 Verify code files

Regardless of whether fix ran:
- `test-datautil/parser.py` should exist with a `parse_csv` function
- `test-datautil/stats.py` should exist with `mean` and `median` functions
- `test-datautil/transform.py` should exist with a `normalize` function

If fix ran, check whether the code was actually improved (e.g., empty list handling added, division by zero guarded).

### 2.7 Record phase 2 results

For each sub-check, state PASS or FAIL:
- [ ] Build phase completed (epic + subtasks closed)
- [ ] Review task was created (exists in task graph)
- [ ] Review task has `validates` edge to epic
- [ ] Review task has correct scope data (`scope_kind=code`, `scope_id=<plan>`)
- [ ] Review task has `data.issue_count` set
- [ ] If issues found: fix pipeline ran (fix-parent + subtasks exist)
- [ ] If fix ran: fix subtasks all closed
- [ ] Code files exist and are functional

---

## Phase 3: `aiki build <plan.md> --fix` — Ctrl+C resilience (Bug 1 check)

Test what happens if the review agent is interrupted.

### 3.0 Setup

Create `test-plan-interrupt.md`:

```markdown
---
title: Add string utilities
scope: test-stringutil/
---

# Add string utilities

## Tasks

1. Create `test-stringutil/case.py` with functions `to_snake_case(s: str) -> str` and `to_camel_case(s: str) -> str`
```

Commit it:

    jj commit -m "Add test-plan-interrupt.md for interrupt test"

### 3.1 Run build --fix and interrupt during review

    aiki build test-plan-interrupt.md --fix

When you see the review loading screen (after build completes, before review finishes), press Ctrl+C.

### 3.2 Verify behavior after interrupt

- **Check:** Did the command exit cleanly or error?
- **Check:** `aiki task` — what's the state of the task graph?
- **Check:** Was a review task created (even if incomplete)?
- **Check:** Did the CLI print a success message ("Build Completed" / "Review completed") despite the interrupt?

**Known Bug 1:** If Ctrl+C during review, `handle_session_result` returns `Ok(())` for the `Stopped` variant, and the pipeline continues — potentially printing success and skipping fix even though the review never completed.

### 3.3 Record phase 3 results

PASS if interrupted build does NOT falsely report success. FAIL if the pipeline continues after Ctrl+C and reports success/skips fix.

---

## Phase 4: `aiki review <task-id> --fix` (standalone review + fix)

Test the review+fix flow independently from build.

### 4.0 Setup

Create a task with deliberately bad code:

    aiki task start "Create code with bugs for review --fix test"

Create `buggy.py`:

```python
import subprocess

def run_command(user_input):
    subprocess.call("echo " + user_input, shell=True)  # command injection

def average(numbers):
    return sum(numbers) / len(numbers)  # crashes on empty list

def read_file(path):
    f = open(path)  # no context manager
    return f.read()
```

Close the task:

    aiki task close <task-id> --summary "Created buggy.py with deliberate issues"

### 4.1 Run review --fix

    aiki review <task-id> --fix

Wait for it to complete.

### 4.2 Verify through task graph

**Review task:**
- `aiki task show <review-task-id>` — should have `data.scope_kind=task`, `data.scope_id=<task-id>`
- `data.options.fix` should be `true`
- `data.issue_count` should be > 0

**Fix pipeline:**
- Fix-parent task should exist with `data.review=<review-task-id>`
- Fix subtasks should be closed
- Post-fix review should exist (quality loop)

### 4.3 Record phase 4 results

PASS if review found issues AND fix pipeline ran end-to-end through the task graph. FAIL otherwise.

---

## Phase 5: `aiki fix <review-id> --once` (single pass, no quality loop)

### 5.0 Setup

Create a new task, write some code, close it, and run a standalone review:

    aiki task start "Create code for fix --once test"

Create `messy.py`:

```python
def   calculateTotal(  items ,tax_rate):
    t = 0
    for i in items:
        t = t + i
    t = t + t * tax_rate
    return t
```

Close and review:

    aiki task close <task-id> --summary "Created messy.py"
    aiki review <task-id>

Note the review task ID.

### 5.1 Run fix --once

    aiki fix <review-task-id> --once

### 5.2 Verify through task graph

**Check fix pipeline ran:**
- Fix-parent task exists
- Fix subtasks all closed

**Critical — verify NO post-fix review:**
- There should be NO review task created after the fix
- The fix-parent should NOT have a `validates` edge pointing to it
- This is the key difference from `aiki fix` (without `--once`)

### 5.3 Record phase 5 results

PASS if fix ran single pass and stopped without post-fix review. FAIL otherwise.

---

## Cleanup

Remove all test artifacts:

    rm -rf test-greetings/ test-datautil/ test-stringutil/
    rm -f test-plan.md test-plan-fixable.md test-plan-interrupt.md buggy.py messy.py

---

## Final Summary

Close the parent task with results:

    aiki task close <PARENT> --summary "Results: Phase 1 (build): PASS/FAIL. Phase 2 (build --fix): PASS/FAIL. Phase 3 (interrupt): PASS/FAIL. Phase 4 (review --fix): PASS/FAIL. Phase 5 (fix --once): PASS/FAIL. Details: ..."

**Report format:** For each phase and sub-check, state PASS or FAIL with details. Include task IDs and data field values. Pay special attention to:

1. **Did `build --fix` create a review task?** (Phase 2.3) — This is the PRIMARY check. The previous run of `build --fix` failed to create a review at all. If the review task is missing, report everything you can find about why.

2. **Does the review task have correct data fields?** (Phase 2.3) — `scope_kind`, `scope_id`, `issue_count`, `options.fix`, `options.fix_template`

3. **Does the `has_issues` check work correctly?** (Phase 2.4) — Known Bug 2: `build.rs` only checks `data.issue_count` and returns false if missing, while `fix.rs` falls back to checking comments. If `issue_count` is missing but issues exist as comments, the fix will be silently skipped.

4. **Does the pipeline handle interrupts correctly?** (Phase 3) — Known Bug 1: `handle_session_result` returns `Ok(())` for Stopped/Detached, causing the pipeline to continue.

5. **Task graph completeness:** For each pipeline phase, the full chain of tasks should be reconstructable from the task graph. Missing edges or tasks indicate a bug.
