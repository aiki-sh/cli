---
version: 1.0.0
---

# Plan Fix: {{data.target}}

**Goal**: Read the issues from review `{{data.review}}` and produce a fix plan that `decompose` can consume.

## Instructions

1. Read the review issues to understand what needs fixing:
   ```bash
   aiki review issue list {{data.review}}
   ```

2. For each issue, gather context:
   - What is the issue and its severity?
   - Where is it located (file, line range)?
   - Read the relevant code to understand the current state

3. Write a fix plan to `/tmp/aiki/plans/{{id}}.md` with the following structure:

   ```markdown
   # Fix Plan: Review {{data.review}}

   **Date**: <today>
   **Status**: Draft
   **Purpose**: Fix issues found in review {{data.review}} (target: {{data.target}})

   **Related**:
   - Review task: {{data.review}}
   - Fix target: {{data.target}}

   ---

   ## Summary

   <Brief overview of what the review found and what needs fixing>

   ---

   ## Issues

   For each issue, document:

   ### Issue N: <title>
   - **Severity**: high | medium | low
   - **Location**: <file:line>
   - **Problem**: <what is wrong>
   - **Fix**: <how to fix it>

   ---

   ## Dependencies

   <Note any dependencies between fixes, e.g.:
   - Issues in the same file that must be fixed together
   - Fixes that change interfaces other fixes depend on
   - Ordering constraints>

   ---

   ## Implementation Plan

   <Ordered list of fix steps, grouped by dependency>

   ### Step 1: <description>
   - Files: <list of files to modify>
   - Changes: <what to change>
   - Depends on: <nothing | step N>

   ### Step 2: <description>
   ...
   ```

4. Ensure the plan:
   - Covers every issue from the review
   - Groups related fixes (e.g., multiple issues in the same file)
   - Orders steps so dependencies are resolved first
   - Has enough detail for `decompose` to create implementation subtasks

5. Close this task when the plan is written:
   ```bash
   aiki task close {{id}} --confidence <1-4> --summary "Fix plan written to /tmp/aiki/plans/{{id}}.md — N issues, M steps"
   ```
