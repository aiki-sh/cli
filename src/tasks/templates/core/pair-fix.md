---
version: 3.0.0
type: fix
---

# Pair Fix: {{data.scope.name}} (iteration {{data.iteration}})

You are pair-programming with the user to address review issues from review **{{data.review}}**.

**Before doing anything else**, tell the user which quality-loop iteration this is (iteration {{data.iteration}}). If this is iteration 2 or later, explain that the previous fixes were re-reviewed and new issues were found — these are **not** the same issues from before.

Walk through each issue below **one at a time**, starting with the highest severity. For each:

1. **Present** the issue clearly (what, where, why it matters)
2. **Show** the relevant code by reading the file at the specified location
3. **Assess the evidence**:
   - If the cited file/location clearly supports the issue, say so
   - If the cited file/location does **not** support the issue well, say that clearly and distinguish the underlying concern from the weak citation
4. **Present approaches** (when multiple paths exist):
   - If you identify more than one reasonable way to address the issue, present them as lettered options (**A**, **B**, **C**, …) before asking what action to take
   - Each option: one-line summary, then a brief note on trade-offs (scope, risk, complexity)
   - If there is only one obvious approach, skip this step
5. **Ask** the user what they'd like to do, using numbered options:
   - **1. Fix**: Delegate the fix to a background subagent so we can keep moving (if approaches were presented, confirm which approach to use — e.g. "1A" means Fix with approach A)
   - **2. Plan**: Write a fix plan for this issue instead of implementing it now
   - **3. Skip**: Mark as won't-do (ask for a brief reason)
   - **4. Discuss**: Talk it through before deciding
   - Keep this prompt short so the user can reply with a single number (or number+letter when approaches are listed)
6. **Act immediately** on the user's choice:
   - For **Fix**:
     - If approaches were presented, include the chosen approach in the subtask description so the subagent knows which path to take
     - Always create a subtask and delegate it via `aiki run <subtask-id> --async` so the conversation continues while the fix runs in the background
     - Briefly report that the fix was delegated, then move directly to the next issue without asking for another confirmation
   - For **Plan**:
     - Write a concise fix plan for the issue
     - The plan should cover the problem, affected files, intended change, dependencies, and verification steps
     - Save the plan in a repo-appropriate plan location if this workflow has one; otherwise include the plan in the task summary/comment in a structured way
     - Briefly report that the issue was planned, then move directly to the next issue
   - For **Skip**:
     - Record the reason as a comment
     - Briefly confirm the skip, then move directly to the next issue
   - For **Discuss**:
     - Discuss the tradeoff
     - End with the same numbered options
     - Once the user chooses, act and move on without re-asking
7. **Track status accurately**:
   - Do not claim an issue is completed unless the fix is actually done
   - If a fix was delegated and is still running, report it as in progress
   - Do not imply the whole pair-fix task is complete while delegated subtasks are still in flight
8. **Wait for delegated fixes to complete**:
   If any fixes were delegated to background agents, wait for them actively:
   1. Start a background heartbeat that alerts you every 30 seconds:
      ```bash
      (sleep 30 && echo "HEARTBEAT: Give status update on in-flight fixes") &
      ```
   2. Wait for the next completion:
      ```bash
      aiki session wait <id1> <id2> <id3> --any
      ```
   3. When one returns, report its result to the user, restart the heartbeat, and repeat with the remaining IDs
   4. On each heartbeat, give a brief status update (e.g., "Still waiting on 2 fixes: `<id1>`, `<id2>`")
   5. Repeat until all delegated fixes have completed or failed

When all issues are addressed, summarize:
- N issues fixed
- N issues planned
- N issues skipped (with reasons)

Then close this task with the summary **only when every issue is fixed, planned, or skipped**.

## Issues

{{data.issues_md}}
