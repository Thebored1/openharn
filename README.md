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

**Built for weak hardware.** The whole point is to run a *small* model as well as it
can on a modest, **CPU-only** machine — no serious GPU required. openharn will happily
talk to a GPU-backed or cloud endpoint, but that isn't the target: the defaults, the
launcher scripts, and the [benchmarks](notes/small-model-tool-calling.md) all assume
CPU inference (`llama-server -ngl 0`). If a change only helps on a big GPU, it's out
of scope.

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
- **Live meter + per-phase timings** — reasoning is collapsed into a live
  `thinking… N tok · Ns · X tok/s` meter (set `OPENHARN_SHOW_THINKING=1` to see the raw
  chain-of-thought); each turn then prints separate `think` / `reply` / `total` lines.
- **Context management** — tool results are capped and the conversation is trimmed to
  fit the model's window (with a retry on overflow).
- **Circuit breaker** — hard-stops a model that gets stuck repeating the same call.
- **Tool-call recovery** — if the server leaves a structured tool call as *text* (e.g.
  Granite's `<tool_call>[{…}]` / `<|tool_call|>`, which llama.cpp may not parse),
  openharn recovers and dispatches it instead of stalling.
- **Prompt-tools mode** (`OPENHARN_PROMPT_TOOLS=1`) — for a server with *no* native
  tool-calling at all (e.g. an old llama.cpp fork like bitnet.cpp), openharn describes
  the tools in the system prompt, omits the `tools` field, and flattens the history to
  plain roles — so it can still drive tools on a limited endpoint.
- **Strict & narrow modes** — `OPENHARN_STRICT_TOOLS=1` grammar-forces every reply into a
  *schema-valid* tool call (a weak model can't invent a field or malform a call);
  `OPENHARN_NARROW=1` locks it to read-only navigation for a maximally reliable agent.
  See the [adaptation guide](docs/adapting-openharn.md).
- **Local or cloud** — any OpenAI-compatible endpoint via `base_url` + optional key.

## Recommended model

Best in testing so far (CPU, from the [benchmark](notes/small-model-tool-calling.md)):
**`LFM2.5-8B-A1B-APEX-I-Compact`** — reliably emits tool calls (4/4 on the scenario),
the best balance of speed × size × reasoning of the tested set. Run it reasoning-off
(`OPENHARN_NO_THINK=1`) for ~3× faster turns on CPU. Avoid the `LFM2-v2 8B-A1B` base —
it won't emit tool calls at any quant.

## Quick start

```sh
cargo build

# in one terminal, serve a model (e.g. with llama.cpp). -ngl 0 = CPU-only, the
# intended target; add GPU layers only if you explicitly want them:
llama-server -m your-model.gguf --jinja --ctx-size 16384 -ngl 0 --port 8080

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

One-page reference: [`docs/CHEATSHEET.md`](docs/CHEATSHEET.md).

### Launcher scripts

Convenience wrappers that start a local llama-server (if one isn't already running)
and open the REPL:

- **Linux / macOS:** `./openharn.sh <dir>` (add `--think` for reasoning mode)
- **Windows:** `openharn.cmd <dir>` (or `openharn.ps1 <dir> -Think`)

Override the model/binary via env vars (`OPENHARN_GGUF`, `LLAMA_SERVER`, `OPENHARN_PORT`).

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

## Notes

Notes from building and stress-testing openharn against real small models on CPU:

- [**Which small models can call tools on CPU**](notes/small-model-tool-calling.md) — a
  same-prompt benchmark of a dozen LFM / LFM2.5 / Gemma-E2B / Granite GGUFs, plus a
  token-level look at one that can't. Tool-calling tracks model family and post-training,
  not the quant tier.
- [**Making uncooperative models call tools**](notes/adaptive-tool-calling.md) — the three
  places tool-calling breaks (model / runtime / server) and the workarounds, including
  prompt-tools mode for servers with no tool API.
- [**Reasoning tokens dominate CPU latency**](notes/reasoning-tax.md) — thinking, not
  tokens/sec, sets per-turn time; the 3–6× win from reasoning-off; MoE size ≠ speed.
- [**Running BitNet on CPU**](notes/bitnet-on-cpu.md) — building bitnet.cpp, BitNet's
  hardware-sensitive speed, and finding out it can't reliably use tools.
- [**Adapting openharn**](docs/adapting-openharn.md) — modes and how to modify it for your
  model / server / use case.

Benchmark harness: [`tests/benchmark.py`](tests/benchmark.py); raw results in
`tests/bench_logs/`.

## Credits & license

openharn is MIT licensed (see `LICENSE`).

The edit engine (`src/edit.rs`) is a Rust port of [opencode](https://github.com/sst/opencode)'s
replacer, and the system prompt is adapted from opencode's default prompt — both used
under opencode's MIT license. Full attribution is in [`NOTICE`](NOTICE) and
[`LICENSES/opencode-MIT.txt`](LICENSES/opencode-MIT.txt).

## Myelin notes backend (example adaptation)

The **`myelin-tools` branch** demonstrates building a completely different agent on top of
openharn's harness: a local notes app with tools `edit_note`, `write_note`, `format_note`,
`search_notes`, `web_search` — no filesystem, just one open note.

```sh
# Run the Myelin HTTP server (proxies to upstream llama-server)
OPENHARN_MYELIN=1 OPENHARN_MYELIN_UPSTREAM=http://127.0.0.1:8080/v1 cargo run
# → serves OpenAI-compatible /v1/chat/completions on :8090

# Run Myelin's benchmark (points at the proxy)
python myelin_bench.py --url http://127.0.0.1:8090/v1 --model myelin
```

See [`docs/adapting-openharn-myeelin.md`](docs/adapting-openharn-myeelin.md) for the full
adaptation recipe — the same pattern applies to any domain (SQL explorer, browser
automator, K8s operator, etc.).
