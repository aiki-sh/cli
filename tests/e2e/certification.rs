//! Harness-support CERTIFICATION suite.
//!
//! The other e2e suites prove individual behaviours; this suite makes a green
//! run *mean something*: when the live tests pass for a harness, every aiki
//! capability that the harness DECLARES in its `HarnessDefinition`
//! (`cli/src/harnesses/<name>/mod.rs`) has been exercised end-to-end against the
//! real agent. A capability a harness claims but no live test proves is a
//! FAILURE ([`capability_coverage_is_complete`]), so a harness cannot silently
//! ship at a lower support level than it advertises.
//!
//! The pre-existing suites already certify the DRIVE axis (spawn + workspace
//! isolation + recovery: `isolation_recovery`, `task_lifecycle`) and the
//! per-change OBSERVE axis (`[aiki]` provenance: `provenance`). This suite adds
//! the capabilities a "Governable" harness declares but nothing proved live:
//!
//!   - **Gate** (`supports_blocking: true`) — aiki can BLOCK a real action.
//!   - **Tokens** (drivable + transcript) — per-turn tokens roll up to the task.
//!   - **Context injection** (observe) — aiki's injected context reaches the agent.
//!
//! Each capability has a per-harness `e2e_<harness>_*` wrapper so the container
//! rig's `TESTFILTER=e2e_claude` / `e2e_codex` picks them up. See
//! `cli/tests/harness-rig/README.md` and `ops/now/harnesses/00-overview.md`
//! (the two-axis capability model this suite certifies against).

use super::*;
use std::time::Duration;
use tempfile::tempdir;

// =============================================================================
// Capability model + coverage guard (the structural "green ⇒ fully supported")
// =============================================================================

/// An aiki capability a live certification run can prove for a harness.
///
/// Coarser than the internal event surface on purpose: these are the axes a
/// `HarnessDefinition` declares (via `runtime` and `hooks.supports_blocking`)
/// plus the two observe-side capabilities every hooked harness is expected to
/// deliver. `ops/now/harnesses/00-overview.md` is the source of the taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capability {
    /// aiki spawns + isolates the agent (hard workspace isolation, session-scoped
    /// provenance). Declared by `runtime.is_some()`.
    Drive,
    /// aiki records per-change `[aiki]` provenance from the agent's edits.
    /// Declared by having a hook surface at all.
    ObserveChange,
    /// aiki can BLOCK a real action before it happens. Declared by
    /// `hooks.supports_blocking == true`.
    Gate,
    /// per-turn token usage rolls up to the driving task. Declared by
    /// `runtime.is_some()` (a driven run yields a transcript / telemetry).
    Tokens,
    /// aiki's injected session/turn context reaches and is consumed by the agent.
    /// Declared by having a hook surface at all.
    ContextInjection,
}

/// Capabilities a harness's registered `HarnessDefinition` CLAIMS, derived
/// structurally so a new/edited definition can't drift away from what the live
/// suite must prove.
fn declared_capabilities(def: &aiki::harnesses::definition::HarnessDefinition) -> Vec<Capability> {
    let mut caps = Vec::new();
    // Any registered harness has a hook surface (Sdk or Vendor), so aiki can
    // observe changes and inject context.
    caps.push(Capability::ObserveChange);
    caps.push(Capability::ContextInjection);
    // A drivable harness (aiki can spawn it) unlocks isolation + token tracking.
    if def.runtime.is_some() {
        caps.push(Capability::Drive);
        caps.push(Capability::Tokens);
    }
    // A blocking hook protocol unlocks policy gating.
    if def.hooks.supports_blocking {
        caps.push(Capability::Gate);
    }
    caps
}

/// One live test that certifies a `(harness, capability)` pair.
///
/// `test` is referenced by value so a renamed/removed test fails to COMPILE;
/// [`capability_coverage_is_complete`] then proves every DECLARED capability of
/// every drivable harness appears here — closing the gap between what a harness
/// advertises and what a green run actually exercised.
struct Proof {
    harness_id: &'static str,
    capability: Capability,
    #[allow(dead_code)] // Referenced for its compile-time existence guarantee.
    test: fn(),
}

fn proofs() -> Vec<Proof> {
    use Capability::*;
    let p = |harness_id, capability, test: fn()| Proof {
        harness_id,
        capability,
        test,
    };
    vec![
        // ---- claude-code ----
        // Drive + ObserveChange are both proven by the provenance test (it spawns
        // the agent AND asserts a per-change `[aiki]` commit).
        p(
            "claude-code",
            Drive,
            crate::provenance::e2e_claude_provenance_on_trivial_change,
        ),
        p(
            "claude-code",
            ObserveChange,
            crate::provenance::e2e_claude_provenance_on_trivial_change,
        ),
        p("claude-code", Gate, e2e_claude_gate_blocks_protected_change),
        p("claude-code", Tokens, e2e_claude_tokens_attributed_to_task),
        p("claude-code", ContextInjection, e2e_claude_context_injected),
        // ---- codex ----
        p(
            "codex",
            Drive,
            crate::provenance::e2e_codex_provenance_on_trivial_change,
        ),
        p(
            "codex",
            ObserveChange,
            crate::provenance::e2e_codex_provenance_on_trivial_change,
        ),
        p("codex", Gate, e2e_codex_gate_blocks_protected_change),
        p("codex", Tokens, e2e_codex_tokens_attributed_to_task),
        p("codex", ContextInjection, e2e_codex_context_injected),
    ]
}

/// The certification's spine (runs everywhere — no jj, no agent, no network).
///
/// For every DRIVABLE registered harness, assert that each capability it declares
/// has a live test in [`proofs`]. If a harness declares a capability with no
/// certifying test, a green rig run would NOT mean "fully supported" — so this
/// fails, naming the gap. Adding a new drivable harness (or flipping
/// `supports_blocking`) therefore forces adding the matching live test.
#[test]
fn capability_coverage_is_complete() {
    let proofs = proofs();
    let mut gaps: Vec<String> = Vec::new();

    for def in aiki::harnesses::iter() {
        // The live rig SPAWNS the agent, so only drivable harnesses are
        // certifiable here. Registered-but-not-drivable harnesses (e.g. the
        // Cursor IDE hook agent, the Gemini stub) are certified when they gain a
        // runtime — at which point this guard starts requiring their tests.
        if def.runtime.is_none() {
            continue;
        }
        let id = def.identity.id;
        for cap in declared_capabilities(def) {
            let covered = proofs
                .iter()
                .any(|p| p.harness_id == id && p.capability == cap);
            if !covered {
                gaps.push(format!(
                    "  - harness '{id}' declares {cap:?} but no live test certifies it"
                ));
            }
        }
    }

    assert!(
        gaps.is_empty(),
        "harness certification gaps — a green rig run would NOT prove full support:\n{}\n\
         Add an e2e_<harness>_* test for each gap and register it in proofs().",
        gaps.join("\n")
    );
}

// =============================================================================
// Shared certification helpers
// =============================================================================

/// Native `hooks stdin` shorthand flag for a harness (`--claude` / `--codex`).
fn hooks_flag(agent: &str) -> &'static str {
    match agent {
        "claude-code" | "claude" => "--claude",
        "codex" => "--codex",
        other => panic!("no hooks-stdin shorthand for agent '{other}'"),
    }
}

/// Read a file the agent produced, after its work has been absorbed back into the
/// repo. Checks the working copy first, then falls back to `jj file show` (the
/// file may live only in an absorbed change, not the working copy).
fn file_content_after_run(repo: &Path, filename: &str) -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(repo.join(filename)) {
        return Some(s);
    }
    let out = process::Command::new("jj")
        .args(["file", "show", filename])
        .current_dir(repo)
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

// =============================================================================
// 1. GATE: supports_blocking proven live (aiki denies a real action)
// =============================================================================

/// The protected file the agent is told to create (and must be DENIED). Its
/// basename fragment `PROTECTED_BY_AIKI` is the substring [`GATE_POLICY_YAML`]
/// matches on, so it denies both the write and any command that mentions it.
const GATE_PROTECTED: &str = "PROTECTED_BY_AIKI.txt";
/// A control file the agent creates first — proves the agent actually ran, so a
/// missing protected file means "gate denied it", not "agent never tried".
const GATE_CONTROL: &str = "gate_control.txt";

/// User `.aiki/hooks.yml` that DENIES any mutation/command targeting
/// [`GATE_MARKER`], on BOTH gate channels:
///   - `change.permission_asked` — claude Write/Edit → `event.file_paths`
///   - `shell.permission_asked`  — codex normalizes EVERY `PreToolUse`
///     (`apply_patch` included) to a shell ask → `event.command`
///
/// Validated live via `aiki hooks stdin` for both harnesses: the protected op
/// yields `permissionDecision:"deny"`, anything else is allowed.
const GATE_POLICY_YAML: &str = r#"name: "e2e gate certification policy"
version: "1"
change.permission_asked:
    - if: event.write
      then:
          - if: event.file_paths.contains("PROTECTED_BY_AIKI")
            then:
                - block: "e2e gate: protected change denied"
shell.permission_asked:
    - if: event.command.contains("PROTECTED_BY_AIKI")
      then:
          - block: "e2e gate: protected shell/patch denied"
"#;

fn install_gate_policy(repo: &Path) {
    std::fs::write(repo.join(".aiki/hooks.yml"), GATE_POLICY_YAML).expect("write gate policy");
}

/// A synthetic native pre-tool payload for `agent` that targets either the
/// protected or an allowed path — used to drive the REAL `aiki hooks stdin`
/// consumer path and confirm the policy MATCHER is effective before we trust the
/// live agent.
fn gate_probe_payload(repo: &Path, agent: &str, protected: bool) -> String {
    let name = if protected { GATE_PROTECTED } else { "allowed.txt" };
    let cwd = repo.display();
    match agent {
        "claude-code" | "claude" => format!(
            r#"{{"session_id":"cert-probe","transcript_path":"/tmp/x","cwd":"{cwd}","permission_mode":"bypassPermissions","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":"{cwd}/{name}","content":"x"}},"tool_use_id":"probe"}}"#
        ),
        "codex" => format!(
            r#"{{"session_id":"cert-probe","turn_id":"t","transcript_path":"/tmp/x","cwd":"{cwd}","model":"gpt-5.5","permission_mode":"bypassPermissions","hook_event_name":"PreToolUse","tool_name":"apply_patch","tool_input":{{"command":"*** Begin Patch\n*** Add File: {name}\n+x\n*** End Patch\n"}},"tool_use_id":"probe"}}"#
        ),
        other => panic!("no gate probe payload for agent '{other}'"),
    }
}

/// Send a probe through `aiki hooks stdin` and return combined stdout+stderr.
fn run_gate_probe(repo: &Path, agent: &str, protected: bool) -> String {
    let out = crate::common::e2e_aiki(repo)
        .current_dir(repo)
        .args(["hooks", "stdin", hooks_flag(agent), "PreToolUse"])
        .write_stdin(gate_probe_payload(repo, agent, protected))
        .output()
        .expect("aiki hooks stdin");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Deterministic proof the gate policy is effective: the protected op is DENIED
/// and an allowed op is not. Fails loudly if the matcher silently stops matching
/// (the exact false-green this suite exists to prevent).
fn assert_gate_policy_effective(repo: &Path, agent: &str) {
    let denied = run_gate_probe(repo, agent, true);
    assert!(
        denied.contains(r#""permissionDecision":"deny""#),
        "gate policy did NOT deny a protected {agent} op via hooks stdin — the \
         matcher is broken, so the live block below would be a false positive.\n{denied}"
    );
    let allowed = run_gate_probe(repo, agent, false);
    assert!(
        !allowed.contains(r#""permissionDecision":"deny""#),
        "gate policy wrongly denied an ALLOWED {agent} op: {allowed}"
    );
}

fn run_gate_blocks_protected_change(agent: &str) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);
    install_gate_policy(repo);

    // Step 1 — deterministic: the policy matcher actually denies (no false green).
    assert_gate_policy_effective(repo, agent);

    // Step 2 — behavioural: the REAL agent is denied the protected write.
    let task_id = create_task(repo, "e2e gate: attempt a protected write");
    set_task_instructions(
        repo,
        &task_id,
        &format!(
            "Do these steps in order, exactly once each:\n\
             1. Create a file named {control} with the content 'ok'.\n\
             2. Create a file named {protected} with the content 'should be blocked'.\n\
             3. Close this task with confidence 3 and summary 'attempted'.\n\
             If step 2 is denied or blocked, do NOT retry or work around it; just \
             continue to step 3.",
            control = GATE_CONTROL,
            protected = GATE_PROTECTED,
        ),
    );

    let (_success, stdout, stderr) = aiki_run(repo, &task_id, agent, Duration::from_secs(180));
    eprintln!("gate run stdout: {stdout}\ngate run stderr: {stderr}");
    // The run may exit non-zero if the agent treats the denial as fatal; assert
    // on the filesystem effect, not the exit status.

    // Control present ⇒ the agent did work (so an absent protected file means the
    // gate denied it, not that the agent never tried).
    assert!(
        file_in_jj_history(repo, GATE_CONTROL),
        "control file {GATE_CONTROL} missing — the agent didn't run, so the gate \
         assertion is inconclusive for {agent}"
    );
    // Protected file absent from BOTH working copy and jj history ⇒ aiki's gate
    // actually DENIED the mutation.
    assert!(
        !file_in_jj_history(repo, GATE_PROTECTED),
        "GATE FAILED: {GATE_PROTECTED} exists despite the deny policy — \
         supports_blocking is not enforced for {agent}"
    );
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_gate_blocks_protected_change() {
    if !jj_available() || !agent_available("claude") {
        eprintln!("Skipping: jj/claude not available");
        return;
    }
    run_gate_blocks_protected_change("claude-code");
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_gate_blocks_protected_change() {
    if !jj_available() || !agent_available("codex") {
        eprintln!("Skipping: jj/codex not available");
        return;
    }
    run_gate_blocks_protected_change("codex");
}

// =============================================================================
// 2. TOKENS: per-turn token usage rolls up to the driving task
// =============================================================================

fn run_tokens_attributed_to_task(agent: &str) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_id = create_task(repo, "e2e tokens: trivial work");
    set_task_instructions(
        repo,
        &task_id,
        "Create a file called note.txt with the content 'hi'. Then close this \
         task with confidence 3 and summary 'done'.",
    );

    let (success, stdout, stderr) = aiki_run(repo, &task_id, agent, Duration::from_secs(180));
    eprintln!("tokens run stdout: {stdout}\ntokens run stderr: {stderr}");
    assert!(success, "aiki run failed for {agent}");
    assert!(
        wait_for_task_closed(repo, &task_id, Duration::from_secs(30)),
        "task not closed after aiki run"
    );

    // Consumer-path readout of the denormalized `data["tokens"]` rollup.
    let out = crate::common::e2e_aiki(repo)
        .current_dir(repo)
        .args(["task", "show", &task_id, "-o", "tokens"])
        .output()
        .expect("aiki task show -o tokens");
    let raw = String::from_utf8_lossy(&out.stdout);
    let tokens: u64 = raw.trim().parse().unwrap_or(0);
    assert!(
        tokens > 0,
        "no tokens attributed to the driven task (data[tokens]={raw:?}) — token \
         tracking is not wired end-to-end for {agent}"
    );
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_tokens_attributed_to_task() {
    if !jj_available() || !agent_available("claude") {
        eprintln!("Skipping: jj/claude not available");
        return;
    }
    run_tokens_attributed_to_task("claude-code");
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_tokens_attributed_to_task() {
    if !jj_available() || !agent_available("codex") {
        eprintln!("Skipping: jj/codex not available");
        return;
    }
    run_tokens_attributed_to_task("codex");
}

// =============================================================================
// 3. CONTEXT INJECTION: aiki's injected context reaches + is consumed
// =============================================================================

/// A marker present ONLY in aiki's injected session-start context — never in the
/// task prompt — so a file carrying it proves the injection reached the agent.
const INJECT_MARKER: &str = "AIKI-CERT-INJECT-4F3A9C";
const INJECT_FILE: &str = "injected.txt";

/// User `.aiki/hooks.yml` that injects, at session start, a directive the agent
/// can only have learned from aiki's injected context. Composes with the core
/// session.started injection (validated live via `aiki hooks stdin SessionStart`).
const INJECT_POLICY_YAML: &str = r#"name: "e2e injection certification policy"
version: "1"
session.started:
    - context: "AIKI SETUP (do this before anything else): create a file named injected.txt whose exact content is the token AIKI-CERT-INJECT-4F3A9C, then continue with your task."
"#;

fn install_injection_policy(repo: &Path) {
    std::fs::write(repo.join(".aiki/hooks.yml"), INJECT_POLICY_YAML)
        .expect("write injection policy");
}

fn run_context_injected(agent: &str) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);
    install_injection_policy(repo);

    let task_id = create_task(repo, "e2e injection: follow injected setup");
    // The prompt deliberately does NOT mention the marker or the file; only the
    // injected SessionStart context does. A file carrying the marker therefore
    // proves aiki's injected context reached AND was consumed by the agent.
    set_task_instructions(
        repo,
        &task_id,
        "Carry out any setup steps aiki gave you at session start, then close \
         this task with confidence 3 and summary 'done'.",
    );

    let (_success, stdout, stderr) = aiki_run(repo, &task_id, agent, Duration::from_secs(180));
    eprintln!("injection run stdout: {stdout}\ninjection run stderr: {stderr}");

    let content = file_content_after_run(repo, INJECT_FILE);
    let ok = content
        .as_deref()
        .map(|c| c.contains(INJECT_MARKER))
        .unwrap_or(false);
    assert!(
        ok,
        "injected marker not found in {INJECT_FILE} (content={content:?}) — aiki's \
         SessionStart context injection did not reach / was not consumed by {agent}"
    );
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_context_injected() {
    if !jj_available() || !agent_available("claude") {
        eprintln!("Skipping: jj/claude not available");
        return;
    }
    run_context_injected("claude-code");
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_context_injected() {
    if !jj_available() || !agent_available("codex") {
        eprintln!("Skipping: jj/codex not available");
        return;
    }
    run_context_injected("codex");
}
