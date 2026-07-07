# openharn

A tiny, local-first coding agent for **small** language models.

openharn is ~1,500 lines of Rust: a thin agent loop against any OpenAI-compatible
endpoint, plus a battle-tested edit engine, deliberately designed so that a *small*
local model — think **MiniCPM-0.8B** or similar — can do real coding tasks (read,
search, edit, run) reliably.

The premise: **the harness matters more than the model.** A capable model wrapped
in a sloppy harness looks broken; a small model wrapped in a good one punches far
above its weight. openharn is that harness, kept small enough to read in one sitting.

## Why

Most agent frameworks are built and tuned for frontier models. Point a 0.8B at them
and it invents filenames, spirals, hallucinates success, or reprints whole files and
truncates. openharn closes those gaps *structurally* — the environment grounds the
model (failed reads list what actually exists), edits are anchored (never reprint a
file), searches state their true scope, and the conversation is trimmed to fit the
context — so the small model behaves.

## Features

- **10 tools:** `read`, `write`, `edit`, `multiedit`, `glob`, `grep`, `bash`,
  `webfetch`, `todowrite`, `todoread`.
- **Anchored edits** — an exact-string replacer cascade (ported from opencode) that
  tolerates whitespace/indentation/escaping drift, so the model changes a *span*, not
  the whole file.
- **Read-before-edit grounding** — `edit`/`write` require a prior `read`; failed reads
  list the files that *actually* exist.
- **Project + whole-system search** — `glob`/`grep` default to the project; pass
  `scope:"system"` to search every drive (openharn resolves the roots, so the model
  never has to produce a `C:\` path).
- **Streaming output** with dimmed reasoning for hybrid-thinking models, plus a live
  **tokens/sec** readout.
- **Context management** — tool results are capped and the conversation is trimmed to
  fit the model's window (with a retry on overflow).
- **Circuit breaker** — hard-stops a model that gets stuck repeating the same call.
- **Local or cloud** — any OpenAI-compatible endpoint via `base_url` + optional key.

## Quick start

```sh
cargo build

# in one terminal, serve a model (e.g. with llama.cpp):
llama-server -m your-model.gguf --jinja --ctx-size 16384 -ngl 99 --port 8080

# in another:
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 cargo run -- .
```

Then talk to it at the `›` prompt:

```
› find where the config is loaded
› add error handling to the parse function, then build it
/reset   clear the conversation
/exit    quit
```

### Windows launcher

`openharn.cmd <dir>` starts a local MiniCPM server (if not running) and opens the
REPL. Add `-Think` (via `openharn.ps1`) to enable the model's reasoning mode.

## Configuration

| env var | default | meaning |
|---|---|---|
| `OPENHARN_BASE_URL` | `http://127.0.0.1:8080/v1` | OpenAI-compatible endpoint |
| `OPENHARN_MODEL` | `local` | model name sent in the request |
| `OPENHARN_API_KEY` | *(none)* | bearer token, for cloud providers |

The first CLI argument is the working directory the agent operates on (default: cwd).

## Testing

```sh
cargo test                 # unit tests (edit engine, tools, context-fit)
python tests/behavior.py   # behavioral tests against a live model on :8080
```

The behavioral suite encodes real failure cases (over-eager edits, spirals, faked
"not found", scope honesty) — every misbehavior becomes a regression test.

## Architecture

Four files:

- `src/main.rs` — the REPL
- `src/agent.rs` — the whole harness: streaming loop, tool dispatch, context-fit
- `src/tools.rs` — the 10 tools + a per-session read/todo state
- `src/edit.rs` — the anchored replacer cascade

No agent framework. Dependencies: `reqwest`, `serde`, `walkdir`, `regex`, `glob`.

## Credits & license

openharn is MIT licensed (see `LICENSE`).

The edit engine (`src/edit.rs`) is a Rust port of [opencode](https://github.com/sst/opencode)'s
replacer, and the system prompt is adapted from opencode's default prompt — both used
under opencode's MIT license. Full attribution is in [`NOTICE`](NOTICE) and
[`LICENSES/opencode-MIT.txt`](LICENSES/opencode-MIT.txt).
