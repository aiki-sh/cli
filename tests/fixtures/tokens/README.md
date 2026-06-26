# Token-usage golden transcripts

Per-harness raw transcript fixtures with realistic token usage, committed so the
token-extraction parsers can be driven end to end against real provider output.

These are **raw** provider transcripts, not expected results. The exact
post-fix `TokenUsage` assertions land with the Phase 2+ extractor fixes
(see `ops/now/token-tracking-fixes.md`). Until then the parser tests assert only
structural facts that survive the fix (tokens present, model/response extracted).

Loaded two ways, both off these same files:

- **Parser unit tests** in `cli/src/editors/{claude_code,codex}/events.rs` pull a
  fixture in with `include_str!` and run the real `parse_transcript_lines`.
- **Consumer-path integration tests** point a scripted fake agent (see
  `tokens_fixture_path` / the `AIKI_FAKE_TRANSCRIPT_*` env vars in
  `cli/tests/common/mod.rs`) at a fixture so it lands at the path the harness
  reads, then drive the extraction.

## Fixtures

### `claude_streaming_and_tool_use.jsonl` (Claude Code)

One turn (single trailing `user` line, then the assistant entries). It carries:

- A **streaming snapshot + finalized pair for the same API call** (`msg_01A`):
  the snapshot has `stop_reason: null` and `content: []`; the finalized entry has
  `stop_reason: "tool_use"`. Both report **identical** `input_tokens`,
  `cache_read_input_tokens`, and `cache_creation_input_tokens`, differing only in
  `output_tokens` (12 → 178). Summing both double-counts the call (defect A1); the
  shared `message.id` is what lets the fix dedup them.
- A **genuine multi-tool-use turn**: distinct calls `msg_02B` and `msg_03C` whose
  `input_tokens` grow across calls (4 → 12000 → 12500). These are legitimately
  additive and must keep summing.

### `codex_multi_turn_resume.jsonl` (Codex)

A resumed rollout (`session_meta` then mid-conversation events). It carries:

- A **multi-turn cumulative `total_token_usage` sequence** (two `token_count`
  events, cumulative 52000 → 81000 input) that **includes
  `reasoning_output_tokens`** (820, 1450). Phase 2 folds these into `output`
  (defect A4, fixed).
- A **resume boundary**: the first `turn_context` has a large cumulative total
  following it (52000) and **no preceding `token_count`**, the shape that makes a
  resumed turn's delta inflate against a zero baseline (defect A5, fixed). The
  Phase 4 parser (`compute_turn_usage`) resolves the first-turn baseline from
  persisted session state — or, lacking that, the carried-over total the first
  `token_count` itself implies (`total_token_usage - last_token_usage`) — so the
  resumed turn reports only its own usage. Note: real Codex restarts the
  cumulative counter per rollout file (every observed first `token_count` has
  `total == last`), so the carryover shape is a defensive guard, not the observed
  behavior; the persisted baseline is only applied when it is consistent with the
  file's running total (see the open-question-4 note in
  `ops/now/token-tracking-fixes.md`).
- `cached_input_tokens` that are a subset of `input_tokens` (OpenAI convention).
  Phase 2 normalizes `input` down to the disjoint bucket invariant: `input`
  excludes cached, `cache_read` holds the cached count (defect A3, fixed).

### `acp_prompt_response_meta.json` (ACP / Zed)

A `session/prompt` `PromptResponse` carrying per-turn usage in its `_meta`
extension — the channel the proxy reads `stopReason` off of, the sibling `_meta`
it used to discard (defect B2, fixed in Phase 3).

**Shape.** Anthropic-style (`claude-code-acp`): a `model` string plus a `usage`
object whose four buckets are already disjoint (`input_tokens`, `output_tokens`,
`cache_read_input_tokens`, `cache_creation_input_tokens`). The Phase 3 parser
`handlers::parse_acp_meta` consumes exactly this shape (parser unit tests in
`cli/src/editors/acp/handlers.rs`). The `_meta` JSON is still agent-specific: an
agent that reports cache *inside* `input` (OpenAI convention) would need a
per-agent shim in `parse_acp_meta` to normalize before populating the disjoint
buckets; none is needed for the ACP agents shipping today.

### Cursor — no fixture (documented gap, defect B1)

There is **no Cursor fixture** because Cursor exposes no token usage on the
surface aiki consumes:

- Its vendor hooks (`stop`, `sessionEnd`, `afterAgentResponse`, …) carry no usage
  fields — only model / status / duration.
- Its transcript file (`transcript_path`) records tool inputs, not token counts.
- The Cursor CLI's `--output-format stream-json` *does* now emit a final usage
  figure, but aiki has no Cursor execution runtime (`harnesses/cursor` is
  `runtime: None`, "task execution not yet supported") and never drives that
  CLI, so it is unreachable.

So the Cursor path keeps `tokens: None` on both `turn.completed` and
`session.ended` (`cli/src/editors/cursor/events.rs`). `None` is the explicit
"usage unavailable" state: it flows through `aggregate_session_tokens` (which
returns `None` when no turn carried usage) to the display, which renders "usage
unavailable" rather than a silent `0` (defect C3). The unit tests in
`cli/src/editors/cursor/events.rs` assert that both events report `None`. If a
future Cursor surface starts reporting usage, add a fixture here and parse it
into a disjoint `TokenUsage` like the other harnesses.
