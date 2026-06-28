# Harness capture rig (Apple `container`)

Per-harness, throwaway-`$HOME` containers for (1) capturing each harness's native
hook payloads into `cli/tests/fixtures/<harness>/`, and (2) running gated e2e
runs in isolation. Uses Apple's `container` runtime (no Docker Desktop). The same
Dockerfile + run flags work with `podman` or `docker` if you prefer.

## Why this exists

The adapters are built and CI-tested against **captured fixtures** (the
deterministic hook-replay tests in `cli/tests/e2e/<harness>.rs`, modeled on
`cli/tests/herdr_plugin_tests.rs`). The live binary is needed only:
1. **once** per harness to capture each event's native payload, and
2. for the occasional `#[ignore]` real-agent e2e.

So you never install all the agent CLIs on your daily machine. The isolation unit
is `$HOME` (every harness writes config/sessions/auth there, and `aiki init`
injects hooks into the same dirs), so each capture runs in a container with its
own throwaway `$HOME`.

## Validated end-to-end (2026-06-26, container 1.0.0)

`./capture.sh claude` and `./capture.sh codex` both captured real native payloads
into `cli/tests/fixtures/{claude,codex}/` (claude: SessionStart, UserPromptSubmit,
PreToolUse, PostToolUse, Stop, SessionEnd; codex: SessionStart, UserPromptSubmit,
PreToolUse, Stop). Three harness-specific gotchas the live run surfaced and the
rig now handles:

1. Run as **non-root** - Claude Code (and others) refuse `--dangerously-skip-permissions` as root; the image drops to the base `node` user.
2. **Pre-create the config dir** - mounting a credential file into `~/.claude` makes that dir root-owned; the image pre-creates `~/.claude`/`~/.codex` as `node` so the file-mount overlays only the file and the dir stays writable. Apple `container`'s virtio-fs lets the non-root user read the read-only-mounted cred.
3. **Codex hook trust** - codex hides hooks behind `[features].codex_hooks = true`, uses PascalCase `hooks.json` keys, and headless `codex exec` silently skips *untrusted* hooks; the profile enables the feature and passes `--dangerously-bypass-hook-trust`.

## Prerequisites (one time)

```sh
# Apple container (https://github.com/apple/container), macOS 15+/Apple Silicon:
#   brew install --cask container   (or download the signed pkg)
container system start --enable-kernel-install   # first run downloads the kata kernel
container --version
```

## Capture native payloads

```sh
cd cli/tests/harness-rig
./capture.sh claude     # -> cli/tests/fixtures/claude/*.jsonl
./capture.sh codex      # -> cli/tests/fixtures/codex/*.jsonl
```

`capture.sh <harness>`:
1. `container build`s a per-harness image (base + that one CLI + the capture shim).
2. `container run`s it with your **credentials mounted read-only** (or an API key
   if set), wires the harness's hooks to point at the capture shim, and runs one
   trivial prompt.
3. The shim writes each event's raw stdin payload to the mounted output dir, which
   lands in `cli/tests/fixtures/<harness>/<Event>.jsonl`.

## Auth

Each harness profile (`harnesses/<name>.sh`) declares how it authenticates:

- **Mount existing credentials (default here).** claude and codex store auth in
  files (`~/.claude/.credentials.json`, `~/.codex/auth.json`), so the rig mounts
  them read-only and reuses your subscription login. No new keys.
- **API key env var (fallback / CI).** If `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`
  (etc.) is set in your shell, `capture.sh` passes it through with `-e` instead of
  mounting. Use this for headless CI and for the env-var-keyed harnesses
  (kiro=`KIRO_API_KEY`, amp, grok).

Caveat: a mounted OAuth token can be device-bound or expire. If a run fails to
authenticate, regenerate (`claude setup-token`) or switch to an API key.

## Adding a harness

Drop a `harnesses/<name>.sh` defining four functions (`harness_install`,
`harness_cred_mount`, `harness_wire_hooks`, `harness_run`) and an
`harness_events` list. The hook-wiring differs per harness (settings.json,
hooks.json, a per-agent JSON config, a JS/TS plugin, an executable shim) - mirror
what that harness's plan in `ops/now/harnesses/<name>.md` describes, and the
ground truth for the already-wired ones is `cli/src/config.rs`.

## Phase 2: in-container e2e (later)

A heavier `Dockerfile.e2e` adds an aiki Linux build (builder stage, `cargo build`
with the `time = 0.3.47` pin) so `aiki init` + `aiki run <task>` run fully inside
the container against the real agent, asserting provenance. The capture rig above
is the prerequisite and the more frequently used piece.

## Notes

- Images are `linux/arm64` on Apple Silicon; the npm CLIs fetch arm64 binaries.
- Verify `container run` flag names (`-v`, `-e`, `--rm`, `--build-arg`) against
  your installed `container` version; they track Docker but may differ.
- Nothing here is committed with secrets: creds are mounted at run time, never
  baked into an image.
