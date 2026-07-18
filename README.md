# openharn

A tiny, local-first coding agent for **small** language models.

openharn is ~3,400 lines of Rust: a thin agent loop against any OpenAI-compatible
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

- **13 tools:** `read`, `write`, `edit`, `multiedit`, `glob`, `grep`, `bash`,
  `python`, `webfetch`, `todowrite`, `todoread`, plus system-scope `glob_system` /
  `grep_system`. Restrict the set with `OPENHARN_TOOLS` for a smaller, safer surface.
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
- **Native-template + plan-first tool calling** — for models whose native format degrades
  under quantization, render the model's *own* tool presentation (`OPENHARN_NATIVE_TEMPLATE=1`)
  instead of flattening it, reason before the grammar clamps (`OPENHARN_PLAN_FIRST` /
  `OPENHARN_PLAN_ALWAYS`), and drop duplicate calls (`OPENHARN_DEDUP_CALLS`). On BFCL v4 this
  took a 2-bit LFM2 from 45% to ~72% AST — see the [BFCL write-up](notes/bfcl-v4.md).
- **Local or cloud** — any OpenAI-compatible endpoint via `base_url` + optional key.
- **Serve mode** (`--serve` / `OPENHARN_SERVE=1`) — openharn itself becomes an
  OpenAI-compatible HTTP server (`POST /v1/chat/completions`, `GET /v1/models`,
  `GET /health`) and runs its coding-agent loop per request. Drive it from any
  OpenAI client, harness, or benchmark.

## Recommended model

Best in testing so far (CPU, from the [benchmark](notes/small-model-tool-calling.md)):

- **`LFM2.5-8B-A1B`** (incl. the `APEX-I-Compact` build) — emits tool calls reliably with prompt-tools + strict; run reasoning-off (`OPENHARN_NO_THINK=1`) for ~3× faster turns on CPU.
- **`LFM2-8B-A1B-UD-Q2_K_XL`** — 2-bit quant, the CPU default. For the coding agent, prompt-tools + strict is the reliable baseline. For *multi-call* tool use (the model's weak spot), the native-template + plan-first path is far stronger — on BFCL v4 it lifts this exact model from 45% to ~72% AST (`OPENHARN_NATIVE_TEMPLATE=1 OPENHARN_PLAN_FIRST=1 OPENHARN_DEDUP_CALLS=1`). Earlier notes said native tool-calling "does not work" at this quant; that was the *prompt-tools flattening* suppressing it — restore the native presentation and it does. See [`notes/bfcl-v4.md`](notes/bfcl-v4.md).

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

### Per-model config files

`tests/tune_model.py` (or `tests/tune_model.sh` on Unix) finds the best flag
combo for a model and writes it to `configs/<model>.conf` — a plain `KEY=value`
list (one var per line; `#` comments and blank lines ignored). openharn loads
this automatically, so you never retype the tuned `OPENHARN_*` flags:

- **Explicit:** `./target/debug/openharn . --config configs/<model>.conf`
- **Auto-load:** if `configs/<OPENHARN_MODEL>.conf` exists, openharn loads it with
  no argument. You can also set `OPENHARN_CONFIG=<path>`.

So the tune run *is* the save step — the winning config is recorded and reused.

### Tuning (find the best config for a model)

The tuner launches `llama-server`, probes whether the model does native
tool-calling and whether it thinks by default, then runs `tests/behavior.py`
across candidate flag combos and picks the highest pass-score (tie-break:
speed). It writes the winner to `configs/<model>.conf` and a ranking log to
`tests/tune_logs/<model>.md`.

```sh
# cross-platform (recommended): Linux, macOS, Windows
python tests/tune_model.py ~/Downloads/LFM2.5-8B-A1B-APEX-I-Compact.gguf

# Unix/Linux/macOS shell only:
./tests/tune_model.sh ~/Downloads/LFM2.5-8B-A1B-APEX-I-Compact.gguf
```

Accuracy / runtime tradeoff: `--pruned` (default, probe-decided subset),
`--full` (exhaustive, slow), `--quick` (4 representative cases). Extra flags:
`--port N`, `--llama <path>`, `--ctx N`. See the script header for details.

After tuning, run openharn with the generated config:

```sh
OPENHARN_MODEL=LFM2.5-8B-A1B-APEX-I-Compact ./target/debug/openharn . \
  --config configs/LFM2.5-8B-A1B-APEX-I-Compact.conf
```

## Testing

```sh
cargo test                 # unit tests (edit engine, tools, context-fit)
python tests/behavior.py   # behavioral tests against a live model on :8080
```

## Serving (--serve mode)

openharn can act as the server instead of just the client: it exposes an
OpenAI-compatible endpoint and runs the full coding-agent loop (tools, retries,
context-fit, circuit-breaker) on each request. This is how you drive openharn
from an external harness, another model, or a benchmark like SWE-bench.

```sh
# start a model somewhere (CPU target):
llama-server -m your-model.gguf --jinja --ctx-size 16384 -ngl 0 --port 8080

# serve openharn on :8090 (tune or pass a config for best results):
OPENHARN_MODEL=lfm2.5 ./target/debug/openharn . \
  --serve --serve-port 8090 --config configs/<model>.conf

# any OpenAI client now talks to openharn:
curl -s http://127.0.0.1:8090/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"lfm2.5","messages":[{"role":"user","content":"What is 2+2?"}]}'
```

Endpoints:

- `POST /v1/chat/completions` — runs the agent loop; `--config`/env flags tune the
  agent, `temperature`/`max_tokens` from the request are passed through.
- `GET /v1/models` — lists the configured `OPENHARN_MODEL`.
- `GET /health` — `{"status":"ok"}`.

Flags / env:

| flag | env | default | meaning |
|---|---|---|---|
| `--serve` | `OPENHARN_SERVE=1` | off | enable serve mode |
| `--serve-port N` | `OPENHARN_SERVE_PORT` | `8090` | listen port |

Each request runs in its own thread on a fresh session rooted at the served
working directory, so concurrent requests are isolated.

The behavioral suite encodes real failure cases (over-eager edits, spirals, faked
"not found", scope honesty) — every misbehavior becomes a regression test.

## Architecture

Five files:

- `src/main.rs` — the REPL
- `src/agent.rs` — the whole harness: streaming loop, tool dispatch, context-fit,
  the FC-proxy + native-template / plan-first tool-calling paths
- `src/tools.rs` — the 13 tools + a per-session read/todo state
- `src/edit.rs` — the anchored replacer cascade
- `src/serve.rs` — the OpenAI-compatible `--serve` HTTP layer

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
- [**How to implement openharn**](notes/how-to-implement-openharn.md) — the design/architecture
  intent behind the code: the loop, the tool surface, grounding, and the reliability ladder.
- [**openharn on BFCL v4**](notes/bfcl-v4.md) — driving a small model through the Berkeley
  Function Calling Leaderboard: the whitespace-bug hunt, the constraint tax, and how native
  presentation + a plan step + dedup take a 2-bit LFM2 from 45% to ~72% AST (and how the same
  levers transfer to MiniCPM-Q4 on GPU).
- [**The composition wall I called immovable**](notes/composition-wall-moved.md) — the narrative
  version: why "the model can't compose two calls" turned out to be the harness suppressing it.
- [**Adapting openharn**](docs/adapting-openharn.md) — modes and how to modify it for your
  model / server / use case.

Benchmark harness: [`tests/benchmark.py`](tests/benchmark.py); raw results in
`tests/bench_logs/`.

## Credits & license

openharn is MIT licensed (see `LICENSE`).

## GBNF string rule fix (important for LFM2 at low quants)

The strict-mode grammar (`OPENHARN_STRICT_TOOLS=1`) generates a GBNF rule for JSON
strings. The original rule allowed literal newlines/carriage returns inside strings:

```bnf
string ::= "\"" ( [^"\\] | "\\" ["\\/bfnrt] )* "\""
```

At low quants (e.g. LFM2-8B Q2_K_XL), the model sometimes emits literal `\n` bytes
inside a JSON string value instead of the escaped `\n` sequence. This produces
invalid JSON that `serde_json` rejects in `parse_text_tool_calls`, silently
dropping the tool call.

**Fix** (`src/agent.rs:973`): exclude newline/carriage-return from the unescaped
character class:

```bnf
string ::= "\"" ( [^"\\\n\r] | "\\" ["\\/bfnrt] )* "\""
```

Now the grammar forces the model to escape newlines (`\n` → `\\n`), producing
valid JSON that parses correctly.
