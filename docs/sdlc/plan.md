# Plan

`aiki plan` starts an interactive session where you and an AI agent collaborate on a specification. The result is a plan file that can be fed into `aiki build`.

## Usage

```bash
# Create a new plan (prompts for guidance)
aiki plan ops/now/user-auth.md

# Create with inline guidance
aiki plan ops/now/user-auth.md "JWT-based auth with refresh tokens"

# Edit an existing plan
aiki plan ops/now/user-auth.md

# Auto-generate filename from description
aiki plan Add user authentication
# → creates ops/now/add-user-authentication.md
```

## How It Works

1. **Aiki creates a plan task** with subtasks that guide the conversation:
   - Clarify user intent and requirements
   - Draft initial plan structure
   - Resolve open questions
   - Validate completeness
   - Confirm with user

2. **A Claude session starts** with the plan task loaded. The agent walks through each subtask interactively, asking questions and writing to the plan file incrementally.

3. **When the plan is finalized**, the agent removes the `draft: true` frontmatter and closes the task. The plan is ready for `aiki build`.

## Modes

| Mode | Trigger | Behavior |
|------|---------|----------|
| **Create at path** | `aiki plan new-feature.md` | Creates file, derives topic from filename ("New Feature") |
| **Create with guidance** | `aiki plan new-feature.md "details..."` | Creates file with your guidance as context |
| **Edit** | `aiki plan existing.md` | Opens existing plan, resumes from current state |
| **Auto-generate** | `aiki plan Add dark mode` | Creates `ops/now/add-dark-mode.md` automatically |

When no inline guidance is provided (except in auto-generate mode), you get an interactive prompt where you can type multi-line input (Shift+Enter for newlines, Enter to submit, Esc to skip).

## Plan Structure

Plans follow a suggested structure:

```markdown
# Title

**Date**: 2025-01-15
**Status**: Draft
**Purpose**: One-line summary

---

## Executive Summary
## User Experience
## How It Works
## Use Cases
## Implementation Plan
## Error Handling
## Open Questions
```

Not all sections are required — the agent adapts the structure to fit the topic.

## Options

| Flag | Effect |
|------|--------|
| `--template <name>` | Use a custom plan template (default: `plan`) |
| `--agent <type>` | Choose which agent runs the session (default: `claude-code`) |

## Tips

- Plan files live in `ops/now/` by convention, but you can put them anywhere
- The plan task is linked to the file via `source: file:<path>`, so running `aiki plan` on the same file resumes the existing task
- Plans with `draft: true` in frontmatter are considered work-in-progress
