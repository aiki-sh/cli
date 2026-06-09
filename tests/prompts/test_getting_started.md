You are testing that the aiki getting-started guide (`cli/docs/getting-started.md`) can be followed from start to finish on a fresh repository without errors. Each phase maps to a section in the guide. Run through ALL phases carefully and report results.

## Setup
- Run `aiki task start "Test getting-started guide: end-to-end walkthrough" --source prompt`
- Note the task ID as PARENT

---

## Phase 1: Prerequisites Installed

Verify the prerequisites listed in the guide are available on this system.

### 1.1 Check Git

    git --version
    # Should succeed and print a version string

### 1.2 Check jj (Jujutsu)

    jj --version
    # Should succeed and print a version string

### 1.3 Check Rust toolchain

    rustup --version
    cargo --version
    # Both should succeed and print version strings

### 1.4 Check Cargo bin is on PATH

    which aiki
    # Should resolve to a path (confirms cargo bin dir is on PATH)

### 1.5 Record phase 1 results

PASS if all four tools (git, jj, rustup, cargo) are available. FAIL with details about which tool is missing.

---

## Phase 2: Install Aiki (verify binary works)

The guide says to `cargo install --path cli`. Since aiki is already installed, just verify the binary is functional.

### 2.1 Check aiki version

    aiki --version
    # Should print a version string without errors

### 2.2 Check aiki help

    aiki --help
    # Should print usage information listing subcommands: init, task, plan, build, review, fix, doctor

### 2.3 Record phase 2 results

PASS if `aiki --version` and `aiki --help` both succeed. FAIL if either errors.

---

## Phase 3: Initialize Aiki in a Fresh Repo

This is Section 3 of the guide: `aiki init`.

### 3.0 Create a fresh test repository

    mkdir /tmp/test-getting-started && cd /tmp/test-getting-started
    git init
    git config user.email "test@example.com"
    git config user.name "Test User"
    echo "# Test Project" > README.md
    git add README.md
    git commit -m "Initial commit"
    jj git init --colocate

### 3.1 Run `aiki init`

    aiki init

**Verify:**
- Command exits with status 0 (no errors)
- Output does NOT contain "Error" or "error:" or "panic"

### 3.2 Verify init created expected artifacts

Check the artifacts mentioned in the guide and the ones `aiki init` is known to produce:

    ls .aiki/
    # Should contain: hooks.yml, tasks/, .manifest.json (at minimum)

    ls AGENTS.md
    # Should exist

    ls .aiki/tasks/
    # Should contain template files (plan.md, decompose.md, etc.)

### 3.3 Record phase 3 results

PASS if `aiki init` succeeds without errors and creates the expected directory structure. FAIL with the error output.

---

## Phase 4: Run `aiki doctor`

The guide mentions `aiki doctor` as a diagnostic tool. Verify it runs and reports a healthy state after init.

### 4.1 Run doctor

    aiki doctor

**Verify:**
- Command exits with status 0
- Output does NOT contain "FAIL" or "Error" (case-insensitive check for critical failures)
- Checks for prerequisites (git, jj, cargo) should pass

### 4.2 Record phase 4 results

PASS if `aiki doctor` runs successfully and reports no critical failures. FAIL with the full output if any check fails.

---

## Phase 5: Chat Mode — Task Lifecycle (Section 4 of the guide)

The guide describes a workflow where an agent starts a task, does work, and closes it. Replicate this end-to-end.

### 5.1 Start a task

The guide shows Claude automatically starting a task. Simulate this:

    aiki task start "Add comment to main function" --source prompt

**Verify:**
- Command succeeds and prints a task ID
- Output contains "Started"

Note the task ID as CHAT_TASK.

### 5.2 Show the task in progress

The guide says to check task status (Section 4.2):

    aiki task show <CHAT_TASK>

**Verify:**
- Command succeeds
- Output shows the task name "Add comment to main function"
- Task status is "in_progress" or equivalent

### 5.3 Add a progress comment

    aiki task comment add <CHAT_TASK> "Working on adding the comment to main"

**Verify:**
- Command succeeds
- Output contains "Commented"

### 5.4 Do some actual work

Create a simple file to simulate the agent's work:

    cat > main.py << 'PYEOF'
# main.py - Entry point for the application
# This function initializes and runs the main application loop.
def main():
    """Main entry point that starts the application."""
    print("Hello, world!")

if __name__ == "__main__":
    main()
PYEOF

### 5.5 Close the task with a summary

The guide shows Claude closing the task with a summary (Section 4.3):

    aiki task close <CHAT_TASK> --summary "Added comment to describe what the main function does"

**Verify:**
- Command succeeds
- Output contains "Closed"

### 5.6 View the closed task summary

The guide mentions running `aiki task show <task-id> --output summary`:

    aiki task show <CHAT_TASK> --output summary

**Verify:**
- Command succeeds
- Output contains the summary text "Added comment to describe what the main function does"

### 5.7 View task diff

The guide mentions `aiki task diff <task-id>`:

    aiki task diff <CHAT_TASK>

**Verify:**
- Command succeeds (exit status 0)
- Output shows the diff of changes made during the task (the main.py file)

### 5.8 Verify task list shows the completed task

    aiki task list

**Verify:**
- Command succeeds
- The task appears in the list as closed/completed

### 5.9 Record phase 5 results

PASS if the full task lifecycle (start → show → comment → close → show summary → diff → list) works end-to-end without errors. FAIL with details about which step failed.

---

## Phase 6: Cross-Agent Task Reference (Section 4.4 of the guide)

The guide describes a second agent referencing a task by ID. Test that a new agent session can read the previous task.

### 6.1 Show the task from a "different agent" perspective

    aiki task show <CHAT_TASK>

**Verify:**
- Command succeeds
- The task created in Phase 5 is fully visible with all its details
- The summary is present

### 6.2 Start a review-style task referencing the first

    aiki task start "Review <CHAT_TASK>" --source task:<CHAT_TASK>

**Verify:**
- Command succeeds and prints a new task ID

Note this as REVIEW_TASK.

### 6.3 Close the review task

    aiki task close <REVIEW_TASK> --summary "Reviewed task, code looks good"

**Verify:**
- Command succeeds

### 6.4 Record phase 6 results

PASS if tasks are readable and referenceable across "sessions" (simulated by separate commands). FAIL if task data is not visible.

---

## Phase 7: Headless Workflow — `aiki plan` (Section 5.1 of the guide)

The guide shows `aiki plan path/to/my-plan.md "description"`.

### 7.1 Run aiki plan with inline description

    aiki plan test-feature.md "Add a simple greeting utility function"

**Verify:**
- Command starts without errors
- A plan file is created at `test-feature.md` (or the agent creates it)
- The planning session produces output (does not hang or crash immediately)

Wait for the plan session to complete (or timeout after 5 minutes).

### 7.2 Verify the plan file exists

    ls test-feature.md
    cat test-feature.md

**Verify:**
- The file exists
- It contains markdown content describing the plan
- It has a YAML frontmatter block (delimited by `---`)

### 7.3 Verify plan tasks were tracked

    aiki task list

**Verify:**
- At least one task related to planning was created and closed
- No tasks are stuck in an error state

### 7.4 Record phase 7 results

PASS if `aiki plan` creates a valid plan file and completes without errors. FAIL with the error output. Note: if the plan agent times out but the plan file was created, record as PARTIAL PASS.

---

## Phase 8: Headless Workflow — `aiki build` (Section 5.2 of the guide)

The guide shows `aiki build path/to/my-feature.md --fix`.

### 8.0 Prepare a minimal plan for the build

If the plan from Phase 7 is not suitable for building, create a known-good plan:

```bash
cat > build-test-plan.md << 'PLANEOF'
---
title: Add greeting utility
scope: greetings/
---

# Add greeting utility

Create a simple greeting module.

## Tasks

1. Create `greetings/hello.py` with a function `greet(name: str) -> str` that returns `"Hello, {name}!"`
PLANEOF
```

Commit the plan so the build can see it:

    jj commit -m "Add build-test-plan.md"

### 8.1 Run aiki build (without --fix first, to test baseline)

    aiki build build-test-plan.md

**Verify:**
- Command starts without errors
- The build progresses through stages (decompose → loop)
- Build completes successfully

Wait for the build to complete (or timeout after 5 minutes).

### 8.2 Verify build output

    ls greetings/hello.py

**Verify:**
- The file was created as specified in the plan
- The file contains a `greet` function

    cat greetings/hello.py

### 8.3 Verify build tasks were tracked

    aiki task list

**Verify:**
- An epic task was created for the build
- Subtasks were created and completed
- No tasks are stuck in error state

### 8.4 Run aiki build with --fix (the full pipeline from the guide)

Create a second plan to test the full `--fix` pipeline:

```bash
cat > build-fix-plan.md << 'PLANEOF'
---
title: Add farewell utility
scope: farewells/
---

# Add farewell utility

Create a simple farewell module.

## Tasks

1. Create `farewells/goodbye.py` with a function `farewell(name: str) -> str` that returns `"Goodbye, {name}!"`
PLANEOF
```

Commit and run:

    jj commit -m "Add build-fix-plan.md"
    aiki build build-fix-plan.md --fix

**Verify:**
- Command starts without errors
- Build completes
- Review phase runs (or is attempted)
- Fix phase runs if review found issues (or is skipped cleanly if no issues)

Wait for the full pipeline to complete (or timeout after 10 minutes).

### 8.5 Verify --fix output

    ls farewells/goodbye.py
    cat farewells/goodbye.py

**Verify:**
- The file was created
- It contains a `farewell` function

### 8.6 Record phase 8 results

PASS if both `aiki build` and `aiki build --fix` complete successfully, creating the expected files. FAIL with details about which stage failed. Record whether the review and fix stages ran.

---

## Cleanup

Remove the test repository:

    rm -rf /tmp/test-getting-started

---

## Final Summary

Close the parent task with results:

    aiki task close <PARENT> --summary "Results: Phase 1 (prerequisites): PASS/FAIL. Phase 2 (binary check): PASS/FAIL. Phase 3 (aiki init): PASS/FAIL. Phase 4 (aiki doctor): PASS/FAIL. Phase 5 (task lifecycle): PASS/FAIL. Phase 6 (cross-agent reference): PASS/FAIL. Phase 7 (aiki plan): PASS/FAIL. Phase 8 (aiki build): PASS/FAIL. Details: ..."

**Report format:** For each phase and sub-check, state PASS or FAIL with details. Include any error output verbatim. Pay special attention to:

1. **Can a new user follow the guide without errors?** Every command in the guide should work as documented. If a command fails or produces unexpected output, report the exact error.

2. **Does `aiki init` produce a working setup?** The initialized repo should pass `aiki doctor` and support the full task lifecycle.

3. **Does the task lifecycle work end-to-end?** Start → show → comment → close → show summary → diff → list should all work without errors.

4. **Do tasks persist across "sessions"?** A task created in one command invocation should be fully visible in subsequent invocations.

5. **Does `aiki plan` create a valid plan file?** The plan should have frontmatter and content.

6. **Does `aiki build` produce the expected output?** Files specified in the plan should be created. The `--fix` flag should trigger the review+fix pipeline.

7. **Are there any undocumented prerequisites or steps?** If a command fails because of a missing step not in the guide, this is a documentation bug — report it.
