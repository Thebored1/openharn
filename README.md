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

- **Dynamic tool schemas:** schemas are never hardcoded; the REPL reads an OpenAI-compatible
  `tools` array from `OPENHARN_TOOLS_SCHEMA`, while serve mode reads `tools` from each request.
- **Built-in handlers:** `read`, `write`, `edit`, `multiedit`, `glob`, `glob_system`, `grep`,
  `grep_system`, `bash`, `webfetch`, `todowrite`, `todoread`, and `python`. A supplied schema
  only works if its function name matches one of these Rust handlers.
- **Anchored edits** — an exact-string replacer cascade (ported from opencode) that
  tolerates whitespace/indentation/escaping drift, so the model changes a *span*, not
  the whole file.
- **Read-before-edit grounding** — existing files must be read in the current session before
  `edit`, `multiedit`, or overwrite; new files may be written directly. Failed reads list the
  files that *actually* exist.
- **Project + whole-system search** — `glob`/`grep` search the project; use the separate
  `glob_system`/`grep_system` handlers to search every drive (openharn resolves the roots,
  so the model never has to produce a `C:\` path).
- **Live meter + per-phase timings** — reasoning is collapsed into a live
  `thinking… N tok · Ns · X tok/s` meter (set `OPENHARN_SHOW_THINKING=1` to see the raw
  chain-of-thought); each turn then prints separate `think` / `reply` / `total` lines.
- **Context management** — tool results are capped at roughly 4,000 characters, searches
  return at most 100 matches, and the conversation is trimmed to fit a roughly 16 KB history
  budget (with a retry on overflow). Narrow searches when results are truncated.
- **Circuit breaker** — hard-stops a model that gets stuck repeating the same call.
- **Tool-call recovery** — if the server leaves a structured tool call as *text* (e.g.
  Granite's `<tool_call>[{…}]` / `<|tool_call|>`, which llama.cpp may not parse),
  openharn recovers and dispatches it instead of stalling.
- **Prompt-tools mode** (`OPENHARN_PROMPT_TOOLS=1`) — for a server with *no* native
  tool-calling at all (e.g. an old llama.cpp fork like bitnet.cpp), openharn describes
  the caller-supplied tools in the system prompt, omits the `tools` field, and flattens
  the history to plain roles — so it can still drive tools on a limited endpoint.
- **Strict & narrow modes** — `OPENHARN_STRICT_TOOLS=1` grammar-forces every reply into a
  *schema-valid* tool call (a weak model can't invent a field or malform a call);
  `OPENHARN_NARROW=1` locks it to read-only navigation for a maximally reliable agent.
  See the [adaptation guide](docs/adapting-openharn.md).
- **Structured SLM mode** (`OPENHARN_SLM=1`) — replaces the full conversation with a small
  JSON observation and one constrained action at a time (`SEARCH`, `READ`, `ANSWER`, or
  `ESCALATE`), with pre/post verification and localized retries. This is useful for models
  that can emit JSON reliably but cannot use native function calling. See
  [dual execution modes](notes/dual-execution-modes.md).
- **Local or cloud** — any OpenAI-compatible endpoint via `base_url` + optional key.
- **Serve mode** (`--serve` / `OPENHARN_SERVE=1`) — openharn itself becomes an
  OpenAI-compatible HTTP server (`POST /v1/chat/completions`, `GET /v1/models`,
  `GET /health`) and runs its coding-agent loop per request. Drive it from any
  OpenAI client, harness, or benchmark.

## Recommended model

Best in testing so far (CPU, from the [benchmark](notes/small-model-tool-calling.md)):

- **`LFM2.5-8B-A1B`** (incl. the `APEX-I-Compact` build) — emits tool calls reliably with prompt-tools + strict; run reasoning-off (`OPENHARN_NO_THINK=1`) for ~3× faster turns on CPU.
- **`LFM2-8B-A1B-UD-Q2_K_XL`** — useful for the coding-agent behavioral suite with
  `OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1`, but do not treat that as a general
  function-calling optimum: the BFCL notes show native FC can outperform that configuration
  on ordinary single-call tasks.

## Quick start

```sh
cargo build

# in one terminal, serve a model (e.g. with llama.cpp). -ngl 0 = CPU-only, the
# intended target; add GPU layers only if you explicitly want them:
llama-server -m your-model.gguf --jinja --ctx-size 16384 -ngl 0 --port 8080

# in another. The REPL is chat-only unless you provide an OpenAI tools-array schema:
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 \
OPENHARN_TOOLS_SCHEMA=notes/sample-tools.json cargo run -- .
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

The launcher defaults to `-ngl 99` (GPU offload); use the manual command above with `-ngl 0`
for the CPU-first path. Override the model/binary via env vars (`OPENHARN_GGUF`,
`LLAMA_SERVER`, `OPENHARN_PORT`).

## Configuration

| env var | default | meaning |
|---|---|---|
| `OPENHARN_BASE_URL` | `http://127.0.0.1:8080/v1` | OpenAI-compatible endpoint |
| `OPENHARN_MODEL` | `local` | model name sent in the request |
| `OPENHARN_API_KEY` | *(none)* | bearer token, for cloud providers |
| `OPENHARN_TOOLS_SCHEMA` | *(none)* | REPL path to an OpenAI-compatible `tools` array; unset means chat-only |
| `OPENHARN_TOOLS` | *(unset)* | comma-separated subset of caller-supplied tools |
| `OPENHARN_NARROW` | *(unset)* | read-only `read,grep,glob` + strict + prompt-tools preset |
| `OPENHARN_PROMPT_TOOLS` | *(unset)* | describe tools in the prompt instead of using native tool calling |
| `OPENHARN_STRICT_TOOLS` | *(unset)* | grammar-constrain replies to valid calls or text |
| `OPENHARN_YESNO` | *(unset)* | select relevant tools in a separate YES/NO pass |
| `OPENHARN_SLM` | *(unset)* | use the compact structured-observation/action harness |
| `OPENHARN_MAX_CALLS` | `1` | per-turn tool-call limit |
| `OPENHARN_TOTAL_MAX` | `5` | total calls before tools are removed |
| `OPENHARN_NO_THINK` | *(unset)* | skip reasoning prefill where supported; faster on CPU |
| `OPENHARN_SHOW_THINKING` | *(unset)* | show raw reasoning instead of the collapsed meter |

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
cargo test                 # local unit tests (no model server required)
python tests/behavior.py   # behavioral tests against a live model on :8080
python tests/benchmark.py  # multi-model conversation/tool benchmark
```

The Python behavioral and benchmark suites require a built binary and a running
OpenAI-compatible model server. Their scores depend on the model, quantization,
`llama-server` version, context size, and configuration; they are not repository-wide
accuracy guarantees.

## Serving (--serve mode)

openharn can act as the server instead of just the client. In normal mode it exposes an
OpenAI-shaped endpoint and runs the full coding-agent loop (tools, retries, context-fit,
and circuit-breaker) on each request. Tool schemas come from the request's `tools` field.
This is useful for an external harness, another model, or a coding benchmark.

`OPENHARN_FC_PROXY=1` is a separate benchmark/proxy mode: when a request includes tools,
it performs exactly one constrained tool-call generation and returns `tool_calls` plus usage.
It does **not** execute tools or run the agent loop.

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

- `POST /v1/chat/completions` — normal mode runs the agent loop and returns final text;
  `tools` schemas are request-supplied and `temperature`/`max_tokens` are passed through.
  With `OPENHARN_FC_PROXY=1`, it returns one generated `tool_calls` result instead and
  does not execute the call.
- `GET /v1/models` — lists the configured `OPENHARN_MODEL`.
- `GET /health` — `{"status":"ok"}`.

Flags / env:

| flag | env | default | meaning |
|---|---|---|---|
| `--serve` | `OPENHARN_SERVE=1` | off | enable serve mode |
| `--serve-port N` | `OPENHARN_SERVE_PORT` | `8090` | listen port |

Each request runs in its own thread on a fresh session rooted at the served
working directory, so concurrent requests are isolated. The server binds to `127.0.0.1`
by default and has no built-in authentication; do not expose it to an untrusted network.
The `bash`, `python`, `webfetch`, and system-search handlers are powerful and should only
be advertised for trusted workloads.

The behavioral suite encodes real failure cases (over-eager edits, spirals, faked
"not found", scope honesty) — every misbehavior becomes a regression test.

## Architecture

Main components:

- `src/main.rs` — CLI, config loading, REPL, and mode selection
- `src/agent.rs` — model transport, streaming loop, tool-call parsing/recovery, context-fit,
  strict grammar, circuit breakers, and alternative selection modes
- `src/tools.rs` — built-in handlers plus per-session filesystem-read and todo state
- `src/edit.rs` — the anchored replacer cascade
- `src/slm_harness/` — structured observations, constrained actions, verification, and retries
- `src/serve.rs` — OpenAI-shaped HTTP server

Tool **schemas** are caller-supplied; tool **handlers and dispatch names** are implemented in
Rust. See [`docs/dynamic-schemas.md`](docs/dynamic-schemas.md).

No agent framework. Dependencies include `reqwest`, `serde`, `walkdir`, `regex`, `glob`,
`tiny_http`, `tokio`, and `uuid`.

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
- [**Adapting openharn**](docs/adapting-openharn.md) — modes and how to modify it for your
  model / server / use case.
- [**Dynamic tool schemas**](docs/dynamic-schemas.md) — how caller-supplied schemas are
  loaded, filtered, and dispatched to built-in handlers.
- [**Execution modes**](notes/dual-execution-modes.md) — default ReAct, structured SLM, and
  grammar-constrained text mode, with their different model requirements.

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
