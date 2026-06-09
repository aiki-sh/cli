---
version: 1.0.0
sources:
  - conflict:{{data.conflict_id}}
---

# Resolve Merge Conflict: {{data.conflict_id}}

A merge conflict was detected during workspace absorption that could not be auto-resolved.

Simple conflicts (append-only additions, non-overlapping changes in different parts of a file, and identical modifications) were already auto-resolved. The remaining conflicts require semantic understanding of both tasks' intent.

The conflicted change is `{{data.conflict_id}}`.

**Goal**: Preserve BOTH tasks' goals whenever possible - both the workspace task and target branch task succeeded at their objectives independently. The merge should honor both sets of intentions.

Please leave updates as you go using `aiki task comment`.

When done with all subtasks close this task with a summary:

```bash
aiki task close {{id}} --confidence <1-4> --summary "Resolved conflicts in <file1, file2>: <brief explanation of how you merged>"
```

# Subtasks

## Understand What Conflicted

First, inspect the conflicted change to understand what you were trying to do:

```bash
# See what changed in your conflicted change
jj diff -r {{data.conflict_id}}

# See the change description (may contain task ID)
jj log -r {{data.conflict_id}} --no-graph -T 'change_id ++ "\n" ++ description'
```

Now find out which changes you conflicted with by viewing BOTH parents:

```bash
# See both parents of the conflict (shows both sides with task provenance)
jj log -r {{data.conflict_id}} --no-graph -T 'parents.map(|c| c.change_id() ++ " " ++ c.description().first_line()).join("\n")'
```

This shows:
- **Parent 1**: The rebase destination (main branch's latest change)
- **Parent 2**: Your workspace change being rebased

Both parent descriptions should contain `task=<task-id>` showing which tasks created each side of the conflict.

### Extract Task IDs from Parent Changes

Parse the parent descriptions to find task IDs:

```bash
# Get parent 1 (target branch change) with full description
jj log -r '{{data.conflict_id}}~' --no-graph -T description

# Get parent 2 (your workspace change) with full description
jj log -r '{{data.conflict_id}}~2' --no-graph -T description
```

Look for `task=<32-char-id>` in each description. The task ID is the 32-character lowercase alphabetic string after `task=`.

### View Task Intent and Context

Once you've extracted the other task ID:

```bash
# Explore the other task to understand its intent and changes
aiki task explore <other-task-id>
```

This gives you the **intent** behind the other change:
- What was the task trying to accomplish?
- What files did it modify?
- Was it part of a larger effort (check parent task if it's a subtask)?

Understanding both tasks' goals helps you merge intelligently rather than mechanically.

When done, close this subtask.
{% endsubtask %}

{% subtask %}
## Identify Conflicted Files

List all files with conflicts:

```bash
# List all files with conflicts
jj resolve --list -r {{data.conflict_id}}
```

Each line shows: `filename    <conflict description>`

Leave a comment with the list of conflicted files, then close this subtask.
{% endsubtask %}

{% subtask %}
## Understand the Conflict Types

**Note**: Simple conflicts were auto-resolved during absorption. This includes append-only additions (both sides adding different content), non-overlapping changes in different parts of the file, and identical modifications. The conflicts below require semantic understanding.

For each conflicted file, determine:

- **Competing modifications**: Both changed the same lines differently
  - **Resolution**: Understand intent from both task contexts, merge semantically to preserve both goals

- **Modification vs deletion**: One side modified, other side deleted
  - **Resolution**: Review carefully - if deleted code is still needed by modifications, keep modified version; otherwise respect the deletion intent

- **Semantic conflict**: Changes don't overlap textually but conflict logically (e.g., one task renamed a function, another added calls to the old name)
  - **Resolution**: Apply both changes correctly - update references to use new names, preserve new functionality

**Default strategy: Preserve BOTH tasks' goals** - both tasks succeeded independently, find a way to make both sets of changes coexist.

Leave a comment describing the conflict types found, then close this subtask.
{% endsubtask %}

{% subtask %}
## Resolve Conflicts

For each conflicted file:

1. Open the file in your workspace (it contains JJ conflict markers)
2. Find conflict blocks:
   ```
   <<<<<<< Conflict N of M
   %%%%%%% Changes from base to side #1
   - old line
   + change from target branch
   +++++++ Contents of side #2
   + your changes
   >>>>>>> Conflict N of M ends
   ```

3. Replace the entire conflict block with the correct merged content that satisfies both tasks' goals
4. Remove all conflict markers (`<<<<<<<`, `%%%%%%%`, `+++++++`, `>>>>>>>`)

When all files are resolved, close this subtask.
{% endsubtask %}

{% subtask %}
## Verify Resolution

Check that no conflicts remain:

```bash
# Check that no conflicts remain
jj resolve --list -r {{data.conflict_id}}
```

Should return empty (no conflicts).

If conflicts remain, go back to the "Resolve Conflicts" subtask. Otherwise, close this subtask.
{% endsubtask %}

## Need Help?

If resolution is ambiguous or risky at any point:
```bash
aiki task comment {{id}} "Unable to resolve: <reason>"
```
The human can provide guidance or take over resolution.

## After Completion

After you close the parent task, workspace absorption will automatically retry on the next turn.
