# Harness rigs (Apple `container`)

Two throwaway-container rigs for exercising aiki's harness adapters without
installing every agent CLI on your daily machine. Both use Apple's `container`
runtime (no Docker Desktop); the same Dockerfiles + run flags also work with
`podman` or `docker`.

- **Capture rig** (`capture.sh` + `Dockerfile`): records each harness's NATIVE
  hook payloads into `cli/tests/fixtures/<harness>/`, for deterministic offline
  hook-replay tests. See [Capture rig](#capture-rig).
- **Live e2e rig** (`e2e.sh` + `Dockerfile.e2e`): compiles aiki for Linux and
  runs the real `#[ignore]` e2e tests (`cli/tests/e2e/`) against the actual agent
  in an isolated container, asserting end-to-end aiki behaviour (session
  discovery, task lifecycle, workspace isolation, `[aiki]` provenance). See
  [Live e2e rig](#live-e2e-rig).

The isolation unit is `$HOME`: every harness writes config/sessions/auth there
and `aiki init` injects hooks into the same dirs, so each run gets its own
throwaway `$HOME` inside the container. You never install the agent CLIs on your
host.

## Prerequisites (one time)

```sh
# Apple container (https://github.com/apple/container), macOS 15+/Apple Silicon:
#   brew install --cask container   (or download the signed pkg)
container system start --enable-kernel-install   # first run downloads the kata kernel
container --version

# The e2e rig COMPILES aiki (jj-lib + gix + ratatui) inside the builder VM. The
# default 2GB builder OOM-kills the linker; give it room (host permitting):
container builder start -c 8 -m 16g
```

Nothing here is committed with secrets: creds are mounted at run time, never
baked into an image. Images are `linux/arm64` on Apple Silicon.

---

## Live e2e rig

`./e2e.sh <config>` builds an image that compiles aiki for Linux, installs the
agent CLI(s), wires aiki's real global hooks (via a baked `aiki init`), then runs
the matching `#[ignore]` tests in `cli/tests/e2e/` against the live agent with
your credentials mounted read-only. Unlike the capture rig (which only records
payloads), this asserts actual aiki behaviour end to end.

### Configs

| Config | Agents installed | Runs |
|---|---|---|
| `claude` | claude only | `e2e_claude_*` (provenance, task-diff, session/thread, lifecycle) |
| `codex`  | codex only  | `e2e_codex_*` (same set; codex acts as its own coder/reviewer) |
| `multi`  | claude **and** codex | `e2e_multi_*` cross-agent handoffs (e.g. claude builds, codex reviews) |

Single-agent configs validate one harness in isolation. The `multi` config exists
for genuinely cross-agent workflows that a one-agent image structurally cannot
run (`cli/tests/e2e/multi_agent.rs`).

### Run

```sh
cd cli/tests/harness-rig
./e2e.sh claude                                   # full claude suite
./e2e.sh codex                                    # full codex suite
./e2e.sh multi                                    # cross-agent suite (needs both creds)

# Narrow to specific tests with TESTFILTER (a cargo test name substring):
TESTFILTER=e2e_claude_provenance ./e2e.sh claude
TESTFILTER=e2e_multi_claude_builds_codex_reviews ./e2e.sh multi
```

The first build per config compiles aiki and is slow (~5 min on the 16GB
builder); switching the agent CLI afterward only rebuilds the thin install layer.
`set -e` means a broken build never wastes a live API call.

### What it asserts (single-agent provenance test)

1. `aiki run <task>` spawns the real agent, which creates a file and closes the task.
2. The session UUID is discovered (the agent's `SessionStart` hook fired).
3. The task is closed, and the file is present in jj history.
4. An `[aiki]` provenance change carries `task=<id>`.

Note the provenance mechanism differs by harness: claude records it inline via a
`PostToolUse` hook; **codex** has no such hook and records out-of-band via an OTel
receiver (it exports `apply_patch` logs to `:19876`). In production that receiver
is a systemd/launchd socket service; a container has no systemd, so the codex
provenance test stands up `socat` as the inetd
(`TCP-LISTEN:19876,fork EXEC:"aiki hooks otel --agent codex"`) sharing the test's
`AIKI_HOME`. See `cli/tests/e2e/main.rs` `start_codex_otel_receiver`.

### Agent resolution (why some tests pass no `--agent`)

`aiki run <unassigned-task>` resolves the agent in order: explicit `--agent`,
then the task assignee, then the active session, then parent-process detection,
then **the sole installed agent**. The last step means a single-agent container
"just works" with no
`--agent`. So the claude tests omit `--agent` (covering that default path) while
the codex tests pass it (covering the explicit path). `--agent` is only required
when more than one agent is installed (the `multi` config, where tests name the
agent for each role).

### Auth

`claude` mounts `~/.claude/.credentials.json`; `codex` mounts `~/.codex/auth.json`
(read-only). `multi` mounts **both**. A single-agent config falls back to
`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` if set. A mounted OAuth token can expire or
be device-bound; if auth fails, regenerate (`claude setup-token`) or use a key.

### Fast iteration without a full rebuild

To test a code change without rebuilding the image, mount the host sources over
the image's and let cargo recompile incrementally against its cached deps. The
default run container is 1GB and OOM-kills the linker, so bump it to 8GB:

```sh
container run --rm -m 8g -c 6 \
  -v "$PWD/../../src":/src/src -v "$PWD/../../tests":/src/tests \
  -v "$HOME/.claude/.credentials.json:/home/node/.claude/.credentials.json:ro" \
  aiki-e2e-claude:latest \
  'export PATH=/usr/local/cargo/bin:$PATH; cd /src && cargo test --test e2e -- --ignored --nocapture --test-threads=1 e2e_claude_provenance'
```

(`e2e.sh` itself always does a clean image build; use the above only for tight
edit/run loops.)

---

## Capture rig

The adapters' deterministic tests replay **captured fixtures** (native hook JSON
mapped to a neutral `AikiEvent`), so the live binary is needed only (1) once per
harness to
capture each event's native payload, and (2) for the occasional live e2e above.

```sh
cd cli/tests/harness-rig
./capture.sh claude     # -> cli/tests/fixtures/claude/*.jsonl
./capture.sh codex      # -> cli/tests/fixtures/codex/*.jsonl
```

`capture.sh <harness>`:
1. `container build`s a per-harness image (base + that one CLI + the capture shim).
2. `container run`s it with your **credentials mounted read-only** (or an API key
   if set), wires the harness's hooks to point at the capture shim, runs one
   trivial prompt.
3. The shim writes each event's raw stdin payload to `cli/tests/fixtures/<harness>/<Event>.jsonl`.

### Validated end-to-end (2026-06-26, container 1.0.0)

`./capture.sh claude` and `./capture.sh codex` captured real native payloads
(claude: SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop,
SessionEnd; codex: SessionStart, UserPromptSubmit, PreToolUse, Stop). Three
gotchas the rig now handles (they also apply to the e2e rig):

1. Run as **non-root**: Claude Code refuses `--dangerously-skip-permissions` as root; the image drops to the base `node` user.
2. **Pre-create the config dir**: mounting a cred file into `~/.claude` makes that dir root-owned; the image pre-creates `~/.claude`/`~/.codex` as `node` so the mount overlays only the file. virtio-fs lets the non-root user read the read-only cred.
3. **Codex hook trust**: codex hides hooks behind `[features].codex_hooks = true`, uses PascalCase `hooks.json` keys, and headless `codex exec` silently skips *untrusted* hooks; the profile enables the feature and passes `--dangerously-bypass-hook-trust`.

---

## Adding a harness

Drop a `harnesses/<name>.sh`. For the **capture** rig it defines `harness_install`,
`harness_cred_mount`, `harness_wire_hooks`, `harness_run`, and `harness_events`.
The **e2e** rig only needs `harness_install` + `harness_cred_mount` (it uses
aiki's real hooks via the baked `aiki init`, not `harness_wire_hooks`); a
multi-agent profile (see `harnesses/multi.sh`) also defines `harness_cred_mounts`
(both creds) and `e2e.sh` special-cases its name. Mirror what the harness's plan
in `ops/now/harnesses/<name>.md` describes; the ground truth for already-wired
harnesses is `cli/src/config.rs`.

## Notes

- Verify `container run` flag names (`-v`, `-e`, `--rm`, `-m`, `--build-arg`)
  against your installed `container` version; they track Docker but may differ.
- `container builder start -m 16g` persists until you stop it; check with
  `container builder status`.
