# Aiki on a team

Aiki is opt-in **per user, per repo**. Initializing aiki checks a small amount
of state into the repo (so the project declares "this repo uses aiki"), but each
teammate still decides for themselves whether aiki runs on their machine. This
page explains what gets shared, what stays local, and what every developer (and
their agent) sees.

## Checked-in vs. per-user state

| State | Scope | Lives in | Created by |
|---|---|---|---|
| `.aiki/` directory | **Checked in** | the repo | `aiki init` |
| `<aiki>` block in `AGENTS.md` / `CLAUDE.md` | **Checked in** | the repo | `aiki init` |
| Instruction symlink (`CLAUDE.md` ↔ `AGENTS.md`) | **Checked in** | the repo | `aiki init` |
| Per-user enable marker | **Per user** | `~/.aiki/.init/repos/<repo>/enabled` | `aiki init` (only) |
| Editor hook configs (`~/.claude`, `~/.cursor`, `~/.codex`, Zed) | **Per user** | your home dir | `aiki init` |
| OTel receiver, `~/.aiki/githooks/` | **Per user / machine** | your machine | `aiki init` |

The key idea: a teammate cloning a repo that contains `.aiki/` is **not** silently
enrolled. The checked-in artifacts declare intent; the per-user **marker** is what
actually turns aiki on for a given person. Without the marker, aiki stays dormant.

## The four states

Aiki resolves every directory to one of four states (`.aiki/` present? marker present?):

| `.aiki/` | marker | State | What happens |
|---|---|---|---|
| no | no | **NotAikiRepo** | aiki is silent; nothing runs |
| yes | no | **Dormant** | aiki is installed but not enabled for you; SessionStart shows a "not active" notice |
| yes | yes | **Active** | aiki runs normally |
| no | yes | **OrphanedMarker** | a stale marker (the repo's `.aiki/` is gone); reaped automatically |

## Onboarding scenarios

### Dev A — the lead who adds aiki

```bash
aiki init        # creates .aiki/, the <aiki> block, the symlink, and YOUR marker
git add .aiki AGENTS.md CLAUDE.md .gitignore
git commit -m "Adopt aiki"
git push
```

Dev A is **Active**. The commit shares the `.aiki/` directory and the `<aiki>`
block with the team, but **not** Dev A's marker (it lives in `~/.aiki`, outside
the repo).

### Dev B — a teammate who uses aiki

After pulling Dev A's commit, Dev B has `.aiki/` but no marker → **Dormant**.
Their agent's first `SessionStart` shows:

> 合 aiki not active. Run `aiki init` to enable

To opt in, Dev B runs:

```bash
aiki init        # .aiki/ already exists, so this just writes Dev B's marker
                 # (and installs their editor hooks). No working-tree churn.
```

Now Dev B is **Active**.

### Dev B — a teammate who does NOT use aiki

They still get the checked-in `.aiki/` and `<aiki>` block. Two things keep their
agent from misfiring:

1. **The dormancy preamble** at the top of the `<aiki>` block tells the agent the
   instructions are dormant unless `SessionStart` says "Aiki is active." A Dormant
   repo's SessionStart says it is *not* active, so the agent ignores the block.
2. **The binary-missing clause**: if aiki isn't installed at all, no SessionStart
   context mentions aiki, which the preamble also treats as dormant.

So a teammate with no aiki binary, or one who simply never runs `aiki init`, gets
an agent that follows its native workflow and never tries to run `aiki` commands.

### CI environments

CI usually has neither the aiki binary nor a marker → **NotAikiRepo** or
**Dormant**. The hook gate exits silently before aiki loads, and the CLI gate
refuses non-allowlisted `aiki` commands. CI does not need any special handling.

## Turning aiki off: `aiki remove`

`aiki remove` is the symmetric teardown. Scope is controlled by two orthogonal
flags:

| Command | Removes | Team impact |
|---|---|---|
| `aiki remove` | Just **your** marker for this repo | None — local only, reversible with `aiki init` |
| `aiki remove --shared` | This repo's `.aiki/`, `<aiki>` block, symlink, git config | **Working-tree changes** — committing them affects teammates |
| `aiki remove --global` | Editor hooks, OTel receiver, `~/.aiki/` (all your markers) | None to repos — your checked-in `.aiki/` directories are left alone |
| `aiki remove --shared --global` | Every enabled repo's checked-in integration **and** the machine-wide setup | Scorched earth |

- Bare `aiki remove` never prompts (local-only, reversible).
- `--shared` and `--global` prompt for confirmation and refuse to run
  non-interactively without `--force`.
- `.jj/` is intentionally left in place by `--shared` (aiki cannot yet tell its
  own JJ repo apart from one you own, and losing version-control history is
  unrecoverable).

## Cleaning up an old install

Aiki does not ship a dedicated migration tool for pre-gate setups. If an older
install needs refreshing (for example, to pick up the inline hook gate):

```bash
aiki doctor --fix          # re-installs hooks in the current (gated) form
# or, for a clean slate:
aiki remove --global       # wipe the machine-wide setup
aiki init                  # re-enroll
```
