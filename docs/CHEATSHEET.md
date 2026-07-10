# openharn cheat sheet

A one-page quick reference. openharn is a tiny agent loop that drives a **small**
model over any OpenAI-compatible endpoint, tuned to run on **CPU / weak hardware**.

---

## Run it

```sh
cargo build                                   # build ./target/debug/openharn

# 1) serve a model — CPU-only (-ngl 0) is the intended target:
llama-server -m model.gguf --jinja --ctx-size 8192 -ngl 0 --port 8080

# 2) point openharn at it and pick a working directory (the last arg):
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 cargo run -- .
```

Windows (PowerShell), against an already-running server:

```powershell
$env:OPENHARN_BASE_URL="http://127.0.0.1:8080/v1"
.\target\debug\openharn.exe .
```

---

## Environment variables

| Var | Default | Meaning |
|---|---|---|
| `OPENHARN_BASE_URL` | `http://127.0.0.1:8080/v1` | OpenAI-compatible endpoint |
| `OPENHARN_MODEL` | `local` | model name sent in the request |
| `OPENHARN_API_KEY` | *(none)* | bearer token, for cloud providers |
| `OPENHARN_SHOW_THINKING` | *(unset)* | set to `1` to stream the raw chain-of-thought instead of the collapsed live meter |
| `OPENHARN_NO_THINK` | *(unset)* | set to `1` for **reasoning-off** — primes a closed `<think></think>` per turn so a hybrid-thinking model (LFM2.5) skips most reasoning. Much faster on CPU; some quality trade-off. |

First CLI arg = the working directory the agent operates on (default: cwd).

---

## REPL

| Input | Action |
|---|---|
| *(any text)* | send a request to the agent |
| `/reset` | clear the conversation + read/todo state |
| `/exit` / `/quit` | quit |
| `Ctrl-C` / EOF | quit |

---

## The 10 tools

| Tool | What it does |
|---|---|
| `read` | read a file (1-based line numbers); required before `edit`/`write` |
| `write` | write/overwrite a file (must `read` first if it exists) |
| `edit` | anchored exact-string replace (tolerates whitespace/indent/escape drift) |
| `multiedit` | several edits to one file, all-or-nothing |
| `glob` | find files by pattern, e.g. `**/*.rs` |
| `grep` | regex search of file contents, filter with `include` |
| `bash` | run a shell command in the working dir |
| `webfetch` | fetch a URL as readable text |
| `todowrite` / `todoread` | plan + track multi-step work |

**Search scope:** `glob`/`grep` default to the **project** dir. To search the whole
computer, the model passes `scope:"system"` (openharn resolves every drive itself — it
never has to produce a `C:\` path).

---

## Reading the per-turn readout

After each turn openharn prints (dim):

```
  think 345 tok · 14.8s · 23 tok/s     ← reasoning phase: tokens · seconds · tok/s
  reply  20 tok ·  0.9s · 23 tok/s     ← answer phase
  total 15.7s                          ← wall time for the whole turn
```

While generating, a live `thinking… N tok · Ns · X tok/s` meter updates in place, then
is erased when the answer (or a tool call) begins. A tool call is shown as
`· <tool> {args}`.

---

## Launcher scripts

Start a local `llama-server` (if one isn't already up) and open the REPL:

| Platform | Command |
|---|---|
| Linux/macOS | `./openharn.sh <dir>` (add `--think` for reasoning mode) |
| Windows | `openharn.cmd <dir>` (or `openharn.ps1 <dir> -Think`) |

Override via env: `OPENHARN_GGUF`, `LLAMA_SERVER`, `OPENHARN_PORT`.

---

## Picking a model (CPU)

- **Best in testing:** `LFM2.5-8B-A1B-APEX-I-Compact` (4/4 tools; best speed × size ×
  reasoning balance). Add `OPENHARN_NO_THINK=1` for ~3× faster turns.
- **Tool-calling is the gating capability**, and it tracks the model *family /
  post-training*, not the quant tier. Prefer **LFM2.5** builds or a **tool-tuned**
  model.
- The **LFM2-v2 `8B-A1B` base won't emit tool calls at any quant** (Q3 → Q4) — it
  writes a Markdown ```` ```bash ```` fence instead. See
  [`small-model-tool-calling.md`](small-model-tool-calling.md) for the full study and
  the 11-model benchmark.
- Reasoning models are more reliable but pay a **thinking tax** (100s–1000s of tokens
  per turn) that dominates CPU wall-clock.

---

## Built-in guardrails (why the small model behaves)

- **Read-before-edit** — `edit`/`write` refuse a file you didn't `read` this session;
  a failed read lists the files that actually exist.
- **Anchored edits** — a 6-rung replacer cascade matches a *span* even if whitespace/
  indentation/escaping drifted, so the model never reprints a whole file.
- **Circuit breaker** — an exact-repeat tool call isn't re-run; 3 repeats hard-stops a
  stuck model.
- **Tool-call recovery** — a structured tool call the server left as text
  (`<tool_call>[…]` / `<|tool_call|>`, Granite-style; list or object shape) is parsed
  and dispatched instead of stalling. Only fires when the native parse found nothing.
- **Context fit** — tool results are capped and the oldest whole turns are dropped to
  fit the model's window (with a retry that shrinks further on a 400 overflow).

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| `ggml_vulkan: ErrorOutOfDeviceMemory` on load | You offloaded to a GPU that can't fit the model. Use **`-ngl 0`** (CPU) — the intended target. |
| Model answers in prose / never calls tools | The model isn't emitting structured tool calls — use an LFM2.5 or tool-tuned model (see *Picking a model*). openharn already recovers *text-emitted structured* calls (Granite `<tool_call>`); it can't recover a Markdown ```` ```bash ```` fence (LFM2-v2). |
| `[stopped: … kept repeating the same tool call]` | Circuit breaker fired; rephrase the request. |
| Turns feel slow | Most of the wall-clock is *thinking* tokens, not slow inference — a smaller/less-reasoning model helps more than tuning llama.cpp. |
| Stop the server | `taskkill /F /IM llama-server.exe` (Windows) · `pkill llama-server` (Unix) |

---

## Tests

```sh
cargo test                 # unit tests (edit engine, tools, context-fit)
python tests/behavior.py   # behavioral tests against a live model on :8080
python tests/benchmark.py  # multi-model conversation+tool benchmark → tests/bench_logs/
```
