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
  in an isolated container. A green run is a **capability certification**: every
  aiki capability the harness declares in its `HarnessDefinition` (drive/isolation,
  `[aiki]` provenance, policy gating, token attribution, context injection) is
  exercised end-to-end. See [Live e2e rig](#live-e2e-rig).

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
| `claude` | claude only | `e2e_claude_*` — the full certification: provenance, task-diff, session/thread, lifecycle, isolation-recovery, **gate**, **tokens**, **context injection** |
| `codex`  | codex only  | `e2e_codex_*` (same set; codex acts as its own coder/reviewer) |
| `multi`  | claude **and** codex | `e2e_multi_*` cross-agent handoffs (e.g. claude builds, codex reviews) |

`e2e.sh <config>` runs the WHOLE `e2e_<config>_*` set by default, because the
certification only holds when every declared capability is exercised. The
non-live guard `capability_coverage_is_complete` (in
`cli/tests/e2e/certification.rs`, runs under a plain `cargo test`) fails the build
if a drivable harness declares a capability that no live test certifies — so
"green rig run ⇒ fully supported" cannot silently regress.

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

### What it certifies (per capability)

Each capability the harness's `HarnessDefinition` declares maps to a live test
(see `cli/tests/e2e/certification.rs`, and the two-axis model in
`ops/now/harnesses/00-overview.md`):

1. **Drive + per-change provenance** (`e2e_<h>_provenance_*`): `aiki run <task>`
   spawns the real agent, which creates a file and closes the task; the session
   UUID is discovered (the `SessionStart` hook fired); the file is in jj history;
   an `[aiki]` change carries `task=<id>`.
2. **Workspace isolation + recovery** (`e2e_<h>_*` in `isolation_recovery.rs`):
   crash recovery, concurrent absorption, stale-worker watchdog.
3. **Gate** (`e2e_<h>_gate_blocks_protected_change`): a `.aiki/hooks.yml` deny
   policy actually STOPS the real agent from writing a protected path — the first
   live proof `supports_blocking: true` is enforced. Codex normalizes every
   `PreToolUse` (incl. `apply_patch`) to a shell ask, so the policy denies on both
   the `change` and `shell` channels.
4. **Tokens** (`e2e_<h>_tokens_attributed_to_task`): after a run, the task's
   rolled-up `data["tokens"]` (via `aiki task show -o tokens`) is non-zero.
5. **Context injection** (`e2e_<h>_context_injected`): a marker aiki injects at
   `session.started` — present nowhere in the task prompt — ends up in a file the
   agent wrote, proving the injected context reached and was consumed.

The gate and injection tests each include a deterministic self-check (they drive
`aiki hooks stdin` with a synthetic native payload and assert the policy fires)
so a silently-broken policy fails loudly instead of masquerading as a pass.

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
