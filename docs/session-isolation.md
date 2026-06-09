# Session Isolation & Workspace Absorption Workflow

## Overview

When multiple agent sessions run concurrently in the same repo, aiki creates
**isolated JJ workspaces** per session so agents don't stomp on each other's
files. Changes are automatically **absorbed** (rebased) back into the main
workspace at the end of each turn and when the session ends.

Key design principle: **unconditional isolation**. Every session gets its own
workspace, regardless of how many sessions are active. This eliminates race
conditions that would arise if isolation were conditional on session count.

---

## Architecture Diagram

```
 ~/.aiki/sessions/                      /tmp/aiki/
 ├── {uuid-A}          (session file)   ├── {repo-id}/
 └── {uuid-B}          (session file)   │   ├── {uuid-A}/        (isolated workspace A)
                                        │   │   ├── .jj/repo → ~/.../repo/.jj/repo
                                        │   │   └── (working tree copy)
                                        │   ├── {uuid-B}/        (isolated workspace B)
                                        │   │   └── ...
                                        │   └── .absorb.lock     (serializes absorptions)
                                        │
                                        └── {other-repo-id}/
                                            └── ...
```

---

## Complete Lifecycle

### Phase 1: Session Registration

```
                        ┌─────────────────────┐
                        │   Agent starts       │
                        │   (claude, cursor,   │
                        │    codex, etc.)       │
                        └─────────┬───────────┘
                                  │
                                  ▼
                  ┌───────────────────────────────┐
                  │   session.started event fires  │
                  │   (session_started.rs:20)       │
                  └───────────────┬───────────────┘
                                  │
               ┌──────────────────┼──────────────────┐
               ▼                  ▼                   ▼
   ┌───────────────────┐ ┌──────────────┐ ┌─────────────────────┐
   │ Prune dead PIDs   │ │ Create       │ │ Execute core hook   │
   │ (crashed agents)  │ │ session file │ │ (hooks.yaml:6-41)   │
   │                   │ │ ~/.aiki/     │ │  • jj new           │
   │                   │ │ sessions/    │ │  • aiki init --quiet│
   │                   │ │ {uuid}       │ │  • inject context   │
   └───────────────────┘ └──────────────┘ └─────────────────────┘
```

**Source:** `cli/src/events/session_started.rs` handles the event.

Session file at `~/.aiki/sessions/{uuid}` contains:
```
[aiki]
agent=claude
external_session_id=...
session_id={uuid}
started_at=2026-02-25T...
mode=interactive
parent_pid=12345
[/aiki]
```

---

### Phase 2: Workspace Creation

Triggered on every **turn start** and several other events:

```
Events that trigger isolation:
  • turn.started          (hooks.yaml:128)
  • session.resumed       (hooks.yaml:45)
  • session.compacted     (hooks.yaml:68)
  • session.cleared       (hooks.yaml:101)
  • repo.changed          (hooks.yaml:312)
```

```
 ┌──────────────────────────────────────────────────────────────┐
 │              workspace_ensure_isolated()                      │
 │              (functions.rs:959)                               │
 └──────────────────────────────┬───────────────────────────────┘
                                │
                                ▼
                ┌───────────────────────────────┐
                │ find_jj_root(cwd)             │
                │ → walk up from cwd for .jj/   │
                └───────────────┬───────────────┘
                                │
                                ▼
                ┌───────────────────────────────┐
                │ create_isolated_workspace()   │
                │ (isolation.rs:56)             │
                │                               │
                │ 1. Check if exists (reuse)    │
                │    • If exists: rebase to     │
                │      current @- if lineage    │
                │      still valid              │
                │    • If lineage diverged:     │
                │      destroy + recreate       │
                │ 2. Resolve @- change_id       │
                │    explicitly via jj log      │
                │ 3. jj workspace add           │
                │    /tmp/aiki/{repo}/{uuid}    │
                │    --name aiki-{uuid}         │
                │    -r {parent_change_id}      │
                └───────────────┬───────────────┘
                                │
                                ▼
                ┌───────────────────────────────┐
                │ Inject context to agent:      │
                │ (hooks.yaml:135-141)          │
                │                               │
                │ "WORKSPACE ISOLATION: ...     │
                │  You MUST cd {ws_path} ..."   │
                └───────────────────────────────┘
```

**Reuse behavior:** If workspace already exists at `/tmp/aiki/{repo-id}/{uuid}/`,
it is rebased to the current `@-` (if the workspace's parent is still in the
current lineage). If the lineage has diverged, the workspace is destroyed and
recreated. (isolation.rs:79-129)

---

### Phase 3: Agent Works in Isolated Workspace

```
                    Main Workspace (repo root)
                    ┌─────────────────────────┐
                    │  @- ─── @ (working copy) │
                    │   │                       │
                    │   │   (human or other     │
                    │   │    agent's changes)   │
                    └───┼───────────────────────┘
                        │
                  fork point
                        │
         ┌──────────────┼──────────────┐
         ▼                             ▼
 Isolated WS A                  Isolated WS B
 /tmp/aiki/{repo}/{uuid-A}/    /tmp/aiki/{repo}/{uuid-B}/
 ┌──────────────────────┐      ┌──────────────────────┐
 │ Agent A edits files  │      │ Agent B edits files  │
 │ in this directory    │      │ in this directory    │
 │                      │      │                      │
 │ .jj/repo → real repo│      │ .jj/repo → real repo│
 └──────────────────────┘      └──────────────────────┘
```

Each workspace is a JJ workspace sharing the same underlying repo
(`.jj/repo` points back to the original). Agents `cd` into their workspace
and perform all file operations there.

**Sandbox implications:** JJ operations in an isolated workspace write to the
shared repo store (the path in `.jj/repo`), which lives outside the workspace
tree. Sandboxed agents (e.g. Codex with `--full-auto` / `workspace-write`
sandbox) must be told about this path via `--add-dir`, otherwise the sandbox
blocks writes to the JJ store and the agent silently fails to update task
state. See `codex.rs:apply_jj_flags()` which reads `.jj/repo` and passes
`--add-dir` pointing to the parent `.jj` directory when the store is external.

---

### Phase 4: Workspace Absorption (Turn Completion)

Triggered on:
- `turn.completed` (hooks.yaml:155) — after every agent turn
- `session.ended` (hooks.yaml:326) — when session terminates
- **Claude Code only**: `ExitPlanMode` PreToolUse (events.rs:190) — when agent requests to exit plan mode, absorbs workspace *before* showing user approval prompt so plan files are visible before user decides whether to approve

```
 ┌───────────────────────────────────────────────────────────────┐
 │                workspace_absorb_all()                         │
 │                (functions.rs:1003)                            │
 │                                                               │
 │  Scans /tmp/aiki/*/{session-uuid}/ for this session's        │
 │  workspaces. For each workspace found:                        │
 └───────────────────────────────┬───────────────────────────────┘
                                 │
                                 ▼
 ┌───────────────────────────────────────────────────────────────┐
 │                absorb_workspace()                             │
 │                (isolation.rs:343)                             │
 └───────────────────────────────┬───────────────────────────────┘
                                 │
                                 ▼
```

#### Snapshot + Two-Phase Rebase

```
 ┌──────────────────────────────────────────────────────────────┐
 │  SNAPSHOT WORKSPACE WORKING COPY                             │
 │  (isolation.rs:369-372)                                      │
 │                                                               │
 │  jj status  (in workspace_dir)                               │
 │                                                               │
 │  Captures any files written since last implicit snapshot.    │
 │  All subsequent JJ commands use --ignore-working-copy, so    │
 │  without this, files written after the last snapshot would   │
 │  be lost.                                                     │
 └──────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  GUARD: Skip empty/root workspace heads                      │
 │  (isolation.rs:381-384)                                      │
 │                                                               │
 │  If ws_head is all zeros (JJ root change), skip absorption. │
 └──────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  ACQUIRE ABSORB LOCK                                         │
 │  (isolation.rs:249, called at 405)                           │
 │                                                               │
 │  Path: /tmp/aiki/{repo-id}/.absorb.lock                     │
 │  Mechanism: O_CREAT|O_EXCL (atomic file creation)            │
 │  Timeout: 30 seconds (stale lock removal)                    │
 │  Poll interval: 100ms                                        │
 └──────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  SNAPSHOT TARGET WORKING COPY (inside lock)                  │
 │  (isolation.rs:411-414)                                      │
 │                                                               │
 │  jj status  (in target_dir)                                  │
 │                                                               │
 │  Captures any changes made in the target workspace (e.g.,    │
 │  user deleting files in main while agent works in isolation)  │
 │  into @'s committed tree. Without this, the rebase computes  │
 │  an empty diff for @ and silently reverts the user's changes.│
 └──────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  STEP 1: Rebase workspace chain onto target @-               │
 │  (isolation.rs:426-445)                                      │
 │                                                               │
 │  jj rebase -b {ws_head} -d @- --ignore-working-copy         │
 │                                                               │
 │  Uses -b (branch): moves only workspace-specific commits,    │
 │  NOT shared ancestors. Avoids cascade rewrites to siblings.   │
 │                                                               │
 │  Before:  @- ─── @                                           │
 │            \                                                  │
 │             └─── ws_changes ─── ws_head                      │
 │                                                               │
 │  After:   @- ─── ws_changes ─── ws_head                     │
 │            \                                                  │
 │             └─── @  (still in old position)                  │
 └──────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  IDEMPOTENCY GUARD                                           │
 │  (isolation.rs:456-481)                                      │
 │                                                               │
 │  Check: is ws_head already an ancestor of @?                 │
 │  (revset: `{ws_head} & ::@`)                                │
 │                                                               │
 │  If yes → this workspace was already absorbed (e.g., from    │
 │  both turn.completed and session.ended). Skip Step 2 to      │
 │  avoid moving @ BACKWARD and orphaning changes absorbed      │
 │  between the two calls. Return Skipped.                      │
 └──────────────────────────┬───────────────────────────────────┘
                            │ (ws_head NOT in ::@)
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  STEP 2: Rebase @ onto workspace head                        │
 │  (isolation.rs:492-508)                                      │
 │                                                               │
 │  jj rebase -s @ -d {ws_head} --ignore-working-copy          │
 │                                                               │
 │  Uses --ignore-working-copy because JJ's working-copy        │
 │  tracking is stale after step 1's rebase. Filesystem sync    │
 │  is handled by `workspace update-stale` below.               │
 │                                                               │
 │  After:   @- ─── ws_changes ─── ws_head ─── @               │
 │                                                               │
 │  The workspace's changes are now ancestors of @.             │
 └──────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  SYNC FILESYSTEM                                             │
 │  (isolation.rs:514-537)                                      │
 │                                                               │
 │  jj workspace update-stale                                   │
 │                                                               │
 │  Updates the filesystem to match the rebased @. Without      │
 │  this, the next snapshot would see stale files.              │
 └──────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  RELEASE LOCK (RAII: fd-lock guard drops)                     │
 └──────────────────────────────────────────────────────────────┘
```

**Why two phases?** Workspaces fork at different times (different @-
ancestors). A single `jj rebase -b @ -d <ws_head>` drags intermediate
default-workspace ancestors along, cascading rewrites to sibling
workspaces and creating divergent changes.

**Why a lock?** Without serialization, concurrent step-2s each move @
to their own target, disconnecting from previous absorptions. The lock
ensures chaining: each absorption builds on the last.

---

### Phase 5: Post-Absorption Results

```
 ┌──────────────────────────────────────────────────────────────┐
 │              AbsorbResult enum                               │
 │              (isolation.rs:316)                              │
 └──────────────────────────┬───────────────────────────────────┘
                            │
              ┌─────────────┴─────────────┐
              ▼                           ▼
         Absorbed                      Skipped
              │                           │
              ▼                           ▼
     ┌──────────────┐           ┌──────────────┐
     │ Post-absorb  │           │ Cleanup:     │
     │ conflict     │           │ (same as     │
     │ check in     │           │  Absorbed)   │
     │ absorb_all() │           │              │
     │              │           │ (empty ws,   │
     │ (func:1108)  │           │  root change │
     └──────┬───────┘           │  or already  │
            │                   │  absorbed)   │
  ┌─────────┴─────────┐        └──────────────┘
  ▼                   ▼
No conflicts     Conflicts in @
  │                   │
  ▼                   ▼
Cleanup:        ┌──────────────┐
• jj forget     │ Emit auto-   │
• rm -rf ws     │ reply with   │
                │ conflict     │
                │ details      │
                │ (hooks.yaml  │
                │  :173-193)   │
                └──────────────┘
```

Conflict detection happens **after** absorption in `workspace_absorb_all()`
(functions.rs:1108-1161), not inside `absorb_workspace()`. It checks
`jj log -r 'conflicts() & @'` to see if the target's @ has conflicts after
the rebase. If so, it gets the conflicted file list via `jj resolve --list`
and the hook emits an `autoreply` that triggers another agent turn
(hooks.yaml:173-193):

```
CONFLICT RESOLUTION REQUIRED

Absorption introduced conflicts in the working copy.
Conflicted files:
{files}

To resolve: edit the conflicted files to remove JJ conflict markers...
```

---

### Phase 6: Session End & Final Cleanup

```
 ┌───────────────────────┐
 │ session.ended event   │
 │ (hooks.yaml:326)      │
 └───────────┬───────────┘
             │
             ▼
 ┌───────────────────────────────────────┐
 │ workspace_absorb_all()               │
 │ (final absorption — same as turn)     │
 └───────────┬───────────────────────────┘
             │
             ▼
 ┌───────────────────────────────────────┐
 │ Remove session file:                 │
 │ ~/.aiki/sessions/{uuid}             │
 └───────────┬───────────────────────────┘
             │
             ▼
 ┌───────────────────────────────────────┐
 │ Opportunistic orphan cleanup:        │
 │ cleanup_orphaned_workspaces()        │
 │ (isolation.rs:724)                   │
 │                                       │
 │ Scans jj workspace list for aiki-*   │
 │ entries with no active session →     │
 │ jj workspace forget + rm -rf         │
 └───────────────────────────────────────┘
```

---

### Crash Recovery

If an agent crashes before cleanup:

```
 ┌──────────────────────────────────────────────────────┐
 │ recover_orphaned_workspaces(session_uuid)            │
 │ (isolation.rs:613)                                    │
 │                                                       │
 │ Called during next session's prune_dead_pid_sessions  │
 └────────────────────────┬─────────────────────────────┘
                          │
                          ▼
 ┌──────────────────────────────────────────────────────┐
 │ Scan /tmp/aiki/*/{session_uuid}/ for dead sessions   │
 └────────────────────────┬─────────────────────────────┘
                          │
           For each orphaned workspace:
                          │
                          ▼
 ┌──────────────────────────────────────────────────────┐
 │ 1. find_repo_root_from_workspace()                   │
 │    (.jj/repo → text file → repo root)                │
 │    (isolation.rs:883)                                │
 │                                                       │
 │ 2. absorb_workspace() into main                      │
 │    ├─ Absorbed → cleanup                             │
 │    ├─ Skipped → cleanup                              │
 │    └─ Error → create recovery bookmark:              │
 │               aiki/recovered/{workspace_name}        │
 │               (preserves unabsorbed changes)         │
 │                                                       │
 │ 3. cleanup_workspace() (always, regardless)           │
 └──────────────────────────────────────────────────────┘
```

---

## Complete State Machine

```
                            ┌─────────┐
                            │  START  │
                            └────┬────┘
                                 │ session.started
                                 ▼
                     ┌───────────────────────┐
                     │    SESSION ACTIVE      │
                     │                        │
                     │  Session file created  │
                     └───────────┬────────────┘
                                 │ turn.started
                                 ▼
                     ┌───────────────────────┐
                     │  ENSURE ISOLATED      │
                     │  (unconditional)      │
                     └───────────┬───────────┘
                                 │
                                 ▼
                     ┌───────────────────────┐
                     │  ISOLATED MODE         │
                     │  (ws at /tmp/aiki)     │
                     └───────────┬───────────┘
                                 │
                                 │    agent works
                                 │
                                 ▼
                     ┌───────────────────────┐
                     │    turn.completed      │
                     └───────────┬───────────┘
                                 │
                    ┌────────────┴────────────┐
                    ▼                         ▼
                 Absorbed                  Skipped
                    │                         │
                    ▼                         │
         ┌──────────────────┐                 │
         │ Conflict check   │                 │
         │ (in absorb_all)  │                 │
         └────────┬─────────┘                 │
           ┌──────┴──────┐                    │
           ▼             ▼                    │
        Clean      Conflicts in @             │
           │             │                    │
           │             ▼                    │
           │    ┌──────────────┐              │
           │    │ auto-reply   │              │
           │    │ "CONFLICT    │              │
           │    │  RESOLUTION  │              │
           │    │  REQUIRED"   │              │
           │    └──────┬───────┘              │
           │           │ agent resolves       │
           │           ▼                      │
           │    ┌──────────────┐              │
           │    │ Next turn →  │              │
           │    │ re-absorb    │              │
           │    └──────────────┘              │
           │                                  │
           └────────────┬─────────────────────┘
                        │ session.ended
                        ▼
            ┌─────────────────────┐
            │  Final absorb_all   │
            │  Remove session file│
            │  Orphan cleanup     │
            └──────────┬──────────┘
                        │
                        ▼
                   ┌─────────┐
                   │   END   │
                   └─────────┘
```

---

## Known Issues

### Concurrent absorption can strand commits (P0)

When multiple sessions absorb sequentially (serialized by lock), later
absorptions can strand earlier ones' commits on side branches. The
`rebase -b` in Step 1 can create sibling branches instead of a linear
chain when two workspace chains share the same fork point.

**Symptoms:** Agent writes a file, absorption captures it into JJ, but the
commit ends up on a side branch not reachable from `@`. The file silently
disappears from the working copy.

**Plan:** `ops/now/bug-absorption-concurrency.md`

---

## Key Files

| File | Role |
|------|------|
| `cli/src/session/isolation.rs` | All workspace CRUD: create, absorb, cleanup, recovery |
| `cli/src/session/mod.rs` | AikiSession struct, session file creation, PID detection |
| `cli/src/flows/core/hooks.yaml` | Event handlers that wire everything together |
| `cli/src/flows/core/functions.rs` | Native functions: `workspace_ensure_isolated`, `workspace_absorb_all` |
| `cli/src/events/session_started.rs` | Session lifecycle: prune dead PIDs, create session file, run core hook |
| `cli/src/agents/runtime/codex.rs` | Codex agent spawning: `apply_jj_flags()` adds `--add-dir` for shared JJ store |

---

## Design Decisions

| Decision | Rationale |
|----------|-----------|
| Unconditional isolation | Every session gets a workspace — no session counting, no special-casing. Eliminates race conditions where a second session could start before the first creates its workspace |
| Two-phase rebase | Single rebase drags shared ancestors, cascading rewrites to sibling workspaces |
| File-based lock | Serializes concurrent absorptions so each builds on the last |
| `/tmp/aiki/` for workspaces | Ephemeral by nature; reboot cleans up naturally; crash recovery handles the rest |
| `jj workspace add -r @-` | Forks from parent of working copy so agent starts with a clean working copy |
| Target snapshot inside lock | `jj status` in target dir captures user's filesystem changes (e.g., file deletions) into @'s committed tree before rebasing. Done inside the lock to ensure consistent state. Without this, the rebase computes an empty diff and silently reverts user changes |
| Both steps use `--ignore-working-copy` | After step 1 rebases the workspace chain, JJ's working-copy tracking is stale. Step 2 also uses `--ignore-working-copy`, then `workspace update-stale` syncs the filesystem |
| `jj new --ignore-working-copy` in default workspace | `session.started` and `repo.changed` hooks run `jj new` in the default workspace, which can race with concurrent absorptions. Without `--ignore-working-copy`, `jj new` triggers a working-copy snapshot that diverges from an in-flight absorption's rebase, causing jj to reconcile divergent operations and silently revert user filesystem changes (e.g. files moved in Finder). User changes are captured later by the absorption's own `jj status` target snapshot |
| Codex `--add-dir` for shared JJ store | JJ workspaces share the repo store (`.jj/repo` is a text file pointing to the original repo's `.jj/repo` directory). Codex's `workspace-write` sandbox only allows writes under the cwd; `--add-dir` whitelists the shared store so `aiki task` commands can write JJ operations. Without this, the agent silently fails to update task state. `codex.rs:apply_jj_flags()` reads `.jj/repo` and adds `--add-dir` only when the store is external (safe no-op for non-workspace repos where `.jj/repo` is a directory) |
| Idempotency guard between Step 1 and 2 | Prevents double-absorption (e.g., from both `turn.completed` and `session.ended`) from moving @ backward, which would orphan changes absorbed between the two calls |

---

## JJ Conflict Model References

The absorption and conflict handling in this system relies on JJ's first-class conflict model.
Understanding these fundamentals is essential for working on the isolation code:

- **[Conflicts](https://docs.jj-vcs.dev/latest/conflicts/)** — How JJ stores conflicts as first-class objects (not text markers), materialization in working copies, resolution workflow, and propagation to descendants
- **[Technical: Conflicts](https://docs.jj-vcs.dev/latest/technical/conflicts/)** — Tree algebra internals (`A+(C-B)+(E-D)`), on-demand resolution, simplification during rebase
- **[Working Copy](https://docs.jj-vcs.dev/latest/working-copy/)** — Conflict marker round-trip: materialization (commit→files) and de-materialization (files→commit on snapshot)
- **[Steve's JJ Tutorial: Conflicts](https://steveklabnik.github.io/jujutsu-tutorial/branching-merging-and-conflicts/conflicts.html)** — Practical walkthrough of conflict resolution, propagation through descendants, `jj resolve`

Key design implications for aiki:

| JJ capability | Implication for isolation |
|--------------|--------------------------|
| Rebases always succeed (conflicts recorded, not rejected) | Absorption never fails — conflicts are detected post-absorption via `conflicts() & @` revset |
| `conflicts()` revset | Native conflict detection: `jj log -r 'conflicts() & @'` in `workspace_absorb_all()` |
| `jj resolve --list -r <rev>` | Query conflicted files in any revision, not just working copy |
| Resolution propagates to descendants | Fixing a conflict in one commit auto-resolves dependent commits |
| Conflict markers are round-trippable | Agents can edit markers in files; JJ parses resolution back on snapshot |
