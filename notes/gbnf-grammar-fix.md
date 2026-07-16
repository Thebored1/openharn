# Notes: GBNF grammar was broken since inception (fixed 2026-07-13)

## The bug

The `tool_grammar()` function in `src/agent.rs` generated GBNF rule names using
underscores from tool names — e.g. `t-glob_system`, `kv-grep_system`. The GBNF spec
(grammars/README.md) requires rule names to be **dashed-lowercase**: `move`, `check-mate`,
`item-list`. Underscores in rule names cause llama.cpp's grammar parser to reject the
entire grammar with `failed to parse grammar`.

Every `OPENHARN_STRICT_TOOLS=1` run since the grammar was written silently fell back to
unconstrained output. The grammar was never actually constraining anything.

## What was broken

```
# BEFORE (broken) — tool name "glob_system" becomes rule "t-glob_system" (underscore)
obj ::= t-read | t-edit | t-glob_system | t-grep_system | ...
# llama.cpp parser: error at "_system"

# AFTER (fixed) — underscores replaced with dashes
obj ::= t-read | t-edit | t-glob-system | t-grep-system | ...
# parses cleanly
```

Additionally, `root ::= call` forced tool-only output — the model couldn't answer in
plain text. Added `text` escape hatch: `root ::= call | text`.

## Impact

- **Before**: STRICT_TOOLS mode was a no-op. Models got unconstrained output regardless.
- **After**: STRICT_TOOLS actually constrains output. LFM2-8B went from 3/6 → **6/6** on
  behavioral tests with `PROMPT_TOOLS=1 STRICT_TOOLS=1 NO_THINK=1`.

## Fix

```rust
// agent.rs:tool_grammar()
let rn = |s: &str| s.replace('_', "-");  // GBNF rule names must be dashed-lowercase
// ...
obj_alts.push(format!("t-{}", rn(name)));
// root ::= call | text  (was: root ::= call)
```

## Why it wasn't caught

- Unit tests (`active_schemas_filters_and_grammar_constrains`) only checked string
  contents of the grammar, not that it actually parses on llama-server.
- The grammar failure was silent — the server returned 400, the harness retried without
  grammar, and the model produced unconstrained output that looked plausible.
- MiniCPM-V-4.6 passes all tests either way (it emits valid tool calls natively), so the
  broken grammar was never the bottleneck for strong models.

## Lesson

Test generated grammars against the actual parser, not just string assertions. A grammar
that looks correct but doesn't parse is worse than no grammar — it gives false confidence.
