# Why a small model can't call your tools: a harness-level study of LFM2, LFM2.5, and Gemma-E2B under openharn

*A reproducible investigation into structured tool-calling failures in aggressively
quantized local models, and a same-prompt benchmark of eleven GGUF builds driven
through the [openharn](../README.md) agent loop on commodity hardware.*

---

## Abstract

Local, small-language-model coding agents live or die by one narrow capability:
the model's ability to emit a **structured tool call** that the harness can parse
and execute. When we pointed openharn — a ~1,500-line Rust agent loop over any
OpenAI-compatible endpoint — at `LFM2-8B-A1B-UD-Q3_K_XL`, the agent stalled on
every request. This report documents the full root-cause investigation and a
subsequent controlled benchmark.

We show, with token-level evidence, that the failure is **not** a parser/format
mismatch in the harness and **not** a prompt-construction error, but the model's
refusal to *emit* its own native tool-call delimiters: given a byte-perfect prompt
in which the tool-call tokens are registered as single special-token IDs, the model
still generates a Markdown ```` ```bash ```` fence instead of
`<|tool_call_start|>[...]<|tool_call_end|>`. We then benchmark eleven models (seven
LFM variants, three Gemma-E2B variants, one tool-tuned 1.2B) against an identical
conversation-plus-tool-use scenario, logging wall time, failed requests,
tokens/second, and thinking-token volume. The headline result: **tool-calling
competence tracks the model family and post-training, not the quantization level.**
All three `LFM2-v2-8B-A1B` builds — including the higher-fidelity Q4_K_XL — fail
identically (0/4 tool steps), while every `LFM2.5` build and the tool-tuned
`LFM2-1.2B-Tool` succeed. We also isolate a practical control knob for LFM2.5's
otherwise-unstoppable reasoning: an assistant-turn prefill of a closed
`<think></think>` block drives thinking-token output to zero while preserving tool
calls.

---

## 1. Introduction

openharn's design thesis is that **the harness matters more than the model**: a
capable model in a sloppy harness looks broken, and a small model in a good harness
punches above its weight. The harness grounds the model (failed reads enumerate what
actually exists), anchors edits (the model changes a span, never reprints a file),
states the true scope of searches, and trims context to fit the window.

That thesis has an implicit precondition: the model must produce tool calls the
harness can dispatch. openharn consumes the OpenAI `tool_calls` array that
`llama-server` returns; it does not attempt to scrape free-text. So the question
this report answers is narrow and load-bearing: **given a correct harness, which
small models actually emit dispatchable tool calls, and why do some fail?**

We investigate a concrete failure, establish its cause at the token level, and then
generalize with a controlled benchmark across a model zoo already present on the
test machine.

---

## 2. System under test

### 2.1 openharn

openharn is four Rust files: a REPL (`src/main.rs`), the agent loop and
context-fitting (`src/agent.rs`), ten tools with per-session read/todo state
(`src/tools.rs`), and an anchored edit-replacer cascade (`src/edit.rs`). It exposes
ten tools — `read`, `write`, `edit`, `multiedit`, `glob`, `grep`, `bash`,
`webfetch`, `todowrite`, `todoread` — via OpenAI function-calling schemas, sends the
full conversation on every turn, streams the reply, dispatches any tool calls, and
loops until the model returns text with no tool call.

### 2.2 Runtime

| Component | Value |
|---|---|
| Agent | openharn 0.1.0 (Rust, edition 2024) |
| Inference server | `llama.cpp` / `llama-server` build **9608 (70b54e140)** |
| Server flags | `--jinja --ctx-size 8192 -ngl 0 --no-warmup` (CPU-only) |
| OS | Windows 11 Home 26200 |
| GPU | NVIDIA RTX 2050 (4 GB) + Intel UHD (2 GB) |
| Offload | none (see §2.3) |

### 2.3 Why CPU-only

The first launch used `-ngl 99` (offload all layers) with a 16k KV cache. On the
4 GB RTX 2050 this crashed the Vulkan backend mid-request:

```
ggml_vulkan: Device memory allocation of size 268435456 failed.
ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory
```

A 3–5 GB model plus KV cache plus compute buffers does not fit in 4 GB. Because
these are **A1B mixture-of-experts** models (~1 B *active* parameters per token),
CPU inference remains usable (20–35 tok/s for the LFM models), so all measurements
below use `-ngl 0` for stability and comparability.

---

## 3. The tool-call format under investigation

LFM2 / LFM2.5 do **not** emit OpenAI-style `{"name": ..., "arguments": {...}}` JSON.
They emit a **Pythonic call list** bracketed by special tokens:

```
<|tool_call_start|>[glob(pattern="src/*.rs")]<|tool_call_end|>
```

`llama.cpp` (with `--jinja`) ships a parser for this format. It is **lazy /
trigger-based**: the tool-call grammar activates only *after* the model emits the
`<|tool_call_start|>` trigger. If that trigger never appears, the output is treated
as ordinary assistant text and `tool_calls` comes back `null`.

This is the crux of everything that follows. The harness is correct; the parser
exists; the entire question reduces to **whether the model samples the trigger
token.**

---

## 4. Case study: LFM2-8B-A1B-Q3_K_XL never calls a tool

### 4.1 Symptom

Driven through openharn, the model answers in prose and never dispatches a tool.
Hitting the raw endpoint (`tool_choice: "auto"`) reproduces it:

```
CONTENT:    "```bash\nglob src/*.rs\n```"
TOOL_CALLS: null
```

The model clearly *intends* to search — it just renders the intent as a Markdown
shell fence rather than the native call.

### 4.2 Ruling out the harness and the runtime

A natural first hypothesis (and a common, correct critique of naive harnesses) is a
**shape mismatch**: the model emits the Pythonic call between the delimiters, and a
JSON-only parser fails to recognize it. We tested this directly by inspecting the
raw `content` stream — not the parsed field — across three presentations:

| Attempt | `<|tool_call_start|>` in content? | `tool_calls` | Output |
|---|---|---|---|
| `tool_choice: auto` | **no** | null | ```` ```bash\nglob 'src/*.rs'``` ```` |
| system prompt cueing tool use | **no** | null | Markdown fence |
| `tool_choice: "required"` | **no** | null | Markdown fence (returned in ~5 s — no grammar was enforced) |

The delimiters are **absent from the output entirely**. This falsifies the
shape-mismatch hypothesis for this model: there is nothing between delimiters to
extract because there are no delimiters. A regex-and-`literal_eval` fallback — the
correct fix when a model emits the tokens as text — would have nothing to match.

### 4.3 Ruling out prompt construction

We dumped the exact rendered prompt (`/apply-template`) that `llama-server` builds
from the tools array:

```
<|im_start|>system
List of tools: <|tool_list_start|>[{"type": "function", "function": {"name": "glob",
"description": "Find files by glob pattern.", "parameters": {"type": "object",
"properties": {"pattern": {"type": "string"}}, "required": ["pattern"]}}}]<|tool_list_end|><|im_end|>
<|im_start|>user
List the .rs files under src using the glob tool.<|im_end|>
<|im_start|>assistant
```

This is the canonical LFM2 tool block — identical to what
`tokenizer.apply_chat_template(messages, tools=[...])` produces. Special-token
census is balanced (`<|im_start|>`×3, `<|im_end|>`×2, `<|tool_list_start|>`×1,
`<|tool_list_end|>`×1). Presentation is not the problem.

### 4.4 Ruling out tokenizer / special-token registration

We tokenized the delimiters (`/tokenize` with `with_pieces`) to check they are real
special tokens and not split into characters:

| String | Tokens | ID |
|---|---|---|
| `<|tool_list_start|>` | 1 | 8 |
| `<|tool_call_start|>` | 1 | 10 |
| `<|tool_call_end|>` | 1 | 11 |
| `<|im_start|>` | 1 | 6 |

Embedded in text, the string still resolves to the single special token:
`x<|tool_call_start|>[glob(pattern="src")]<|tool_call_end|>` →
`x` / **10** / `[` / `glob` / `(` / `pattern` / `="` / `src` / `")` / `]` / **11**.

The trigger is a registered special token in the low reserved-ID range. The model is
not seeing it as plain characters.

### 4.5 Proving the capability exists in the weights

Constraining generation with an explicit GBNF grammar (no `tools` field) forced the
exact native call:

```
<|tool_call_start|>[glob(pattern="src/*.rs")]<|tool_call_end|>
```

So the model **can** produce token 10 — the weights support it — but will not sample
it spontaneously under normal decoding. (Note: `llama.cpp` refuses a custom grammar
combined with the `tools` field — *"Cannot use custom grammar constraints with
tools"* — so grammar-forcing is not a usable path for real agent operation.)

### 4.6 Verdict

Every alternative explanation is eliminated:

1. **Prompt** — canonical, verified byte-for-byte. ✓ correct
2. **Special tokens** — single registered IDs, not split. ✓ correct
3. **Capability** — grammar-forcing yields the exact call. ✓ present in weights
4. **Generation** — under normal decoding the model never samples token 10. ✗

The failure is a **decoding-behavior deficit**: at this quantization the model
defaults to a Markdown code fence instead of its own tool-call protocol. It is not a
parser-shape bug and not a presentation bug. §7 shows this deficit is a property of
the `LFM2-v2-8B-A1B` family, reproduced at Q3 *and* Q4.

---

## 5. Controlling reasoning in LFM2.5

LFM2.5 models reason by default, and the flag intended to disable it
(`--reasoning off`, which sets `enable_thinking = 0`) is a **no-op** for the LFM2.5
chat template: the template references `preserve_thinking` / `message.thinking` only
to *replay past* assistant thoughts and contains no gate for the current turn. The
model therefore emits `<think>…</think>` regardless, and `llama.cpp` extracts it into
`reasoning_content`.

We measured four suppression strategies (LFM2.5-8B-A1B-Q4_K_M, `max_tokens=256`):

| Method | Thinking chars | Tool call? |
|---|---|---|
| baseline | 1093 | ✗ (ran out mid-think) |
| system "do not think" | 454 | ✓ |
| user `/no_think` | 1136 | ✗ (ignored) |
| **assistant prefill `<think></think>`** | **0** | **✓** |

**Finding:** priming the assistant turn with a closed `<think></think>` block makes
`llama.cpp` continue from an already-closed think state, driving reasoning output to
zero while preserving a clean native tool call. This is the reliable request-level
lever when the template exposes no thinking switch. (openharn does not currently
inject this prefill; wiring it in behind an opt-in flag would make LFM2.5 usable in
a reasoning-off, low-latency mode.)

---

## 6. Benchmark methodology

### 6.1 Scenario (identical for every model)

The harness (`tests/benchmark.py`) drives an openharn-equivalent agent loop with
openharn's ten tool schemas, over a fresh seeded scratch project (a `src/app.py`
defining `class Config`, a `README.md`, and a `settings.toml`). The same system
prompt (`src/prompt.txt`) and the same five user turns are used for all models:

1. **Conversation** — "In one sentence, what kinds of coding tasks can you help
   with?" (no tool expected)
2. **Search** — grep the project for `Config` → expects `grep`
3. **Create** — write `notes.txt` containing `benchmark run` → expects `write`
4. **Find** — locate any `*.toml` file → expects `glob`
5. **Edit** — change `benchmark run` → `benchmark complete` in `notes.txt` (requires
   openharn's read-before-edit grounding) → expects `edit`

Each user turn runs a bounded tool loop (≤4 iterations); tools execute with
openharn's semantics (project-scoped `glob`/`grep`, read-before-edit guard,
anchored-ish replace).

### 6.2 Metrics

- **Time (s)** — wall time for the scenario, excluding model load.
- **Failed requests** — HTTP non-200, timeout, or transport exception. *(Distinct
  from a tool/task miss, which is a well-formed response that simply didn't call the
  right tool.)*
- **Tok/s** — honest aggregate `1000 · Σ predicted_n / Σ predicted_ms` from
  `llama-server` timings. (An earlier per-request `predicted_per_second` mean was
  discarded after it produced >4000 tok/s artifacts on cached/short generations.)
- **Thinking tokens** — `reasoning_content` plus any inline `<think>…</think>`,
  re-tokenized via `/tokenize`.
- **Tool hits** — of the 4 tool-requiring turns, how many produced the expected
  structured call.
- **Task** — PASS iff `notes.txt` ends the run containing `benchmark complete`.

### 6.3 Controls

Same prompt, same scenario, same seeded project, same server flags, same CPU-only
configuration, one model loaded at a time (server killed and relaunched per model),
temperature 0.2, `max_tokens=1024`.

---

## 7. Results

Eleven models, single clean run, CPU-only. **Zero transport-level failed requests
across all models** — every "fail" below is a task/tool-format miss, not a crash.

| Model | Quant | Size | Load s | Time s | Tok/s | Compl.tok | Think tok | Tool hits | Task |
|---|---|---:|---:|---:|---:|---:|---:|:---:|:---:|
| LFM2-1.2B-Tool | Q4_K_M | 697 MB | 3.0 | 18.5 | 33.5 | 310 | 0 | 3/4 | **PASS** |
| LFM2-8B-A1B | Q3_K_S | 3475 MB | 4.1 | 17.4 | 26.2 | 151 | 0 | 0/4 | fail |
| LFM2-8B-A1B | Q3_K_XL | 3506 MB | 4.1 | 17.6 | 24.4 | 151 | 0 | 0/4 | fail |
| LFM2-8B-A1B | Q4_K_XL | 4524 MB | 5.1 | 15.0 | 27.6 | 95 | 0 | 0/4 | fail |
| LFM2.5-1.2B-Instruct | Q4_K_M | 697 MB | 3.0 | 16.2 | 33.5 | 226 | 0 | 3/4 | **PASS** |
| LFM2.5-8B-APEX-Compact | — | 4017 MB | 5.1 | 77.3 | 22.8 | 1280 | 1089 | 4/4 | **PASS** |
| LFM2.5-8B-APEX-Mini | — | 3467 MB | 5.1 | 176.0 | 21.6 | 3308 | 3084 | 3/4 | **PASS** |
| LFM2.5-8B-A1B | Q4_K_M | 4917 MB | 6.1 | 89.4 | 23.1 | 1518 | 1302 | 4/4 | **PASS** |
| gemma-3n-E2B-it | IQ3_XXS | 2216 MB | 5.1 | 20.3 | 8.3 | 80 | 0 | 0/4 | fail |
| gemma-4-E2B-it | IQ4_XS | 2846 MB | 7.1 | 69.4 | 9.5 | 517 | 389 | 3/4 | **PASS** |
| gemma-4-E2B-it-qat | Q4_K_XL | 2499 MB | 6.1 | 91.0 | 9.2 | 725 | 624 | 0/4 | fail |

---

## 8. Discussion

### 8.1 Tool-calling tracks family and post-training, not quantization

The most important result overturns the intuitive "it's just the aggressive quant"
explanation. The `LFM2-v2-8B-A1B` base fails **identically at Q3_K_S, Q3_K_XL, and
Q4_K_XL** (0/4 in every case). Raising the quantization by a full tier changed
nothing. Meanwhile:

- `LFM2-1.2B-Tool` — a **tool-tuned** v2 model 5× smaller — passes.
- Every `LFM2.5` build passes.

The discriminator is therefore **post-training for tool use** (and the LFM2.5
generation), not bit-width. The 8B-A1B v2 base simply was not disposed to emit its
tool-call protocol under normal decoding, and no quant recovers a behavior the
checkpoint doesn't foreground.

### 8.2 The reasoning tax

Every passing 8B LFM2.5 model reasons heavily: 1,089–3,084 thinking tokens per
scenario, which on CPU inflates wall time to 77–176 s versus ~16–18 s for the
non-reasoning small models. Thinking materially improves tool reliability (the 8B
reasoners reach 4/4) but at a latency cost that is punishing without a GPU. The
`<think></think>` prefill from §5 is the mitigation: it recovers small-model latency
while keeping the native tool calls, at some accuracy risk.

### 8.3 Best picks on 4 GB-class hardware

- **Fastest competent:** `LFM2.5-1.2B-Instruct` — passes, ~33 tok/s, zero thinking
  overhead, ~16 s end-to-end.
- **Most reliable:** `LFM2.5-8B-A1B-Q4_K_M` — 4/4 and correct, but slow due to
  reasoning.
- **Purpose-built and tiny:** `LFM2-1.2B-Tool` — passes, fast, non-reasoning.

### 8.4 Gemma-E2B

Gemma-E2B runs but is CPU-slow (~8–9 tok/s). Only `gemma-4-E2B-it-IQ4_XS` completed
the task; `gemma-3n-E2B` (0/4) and the QAT build (0/4) did not reliably tool-call in
this setup. As with LFM, competence did not correlate with the nominal quant tier.

---

## 9. Threats to validity

- **Sampling variance.** Temperature 0.2 is not greedy; borderline models (notably
  `LFM2-1.2B-Tool`, which scored 4/4 in a warm-up and 3/4 in the recorded run) will
  jitter ±1 tool hit run-to-run. Trends across families are robust; a single
  model's exact tool-hit count is not.
- **Token budget.** `max_tokens=1024` can truncate a heavy reasoner mid-thought,
  scoring it as a miss. This penalizes high-latency reasoning models and is a
  deliberate, disclosed bound rather than a neutral choice.
- **Harness fidelity.** The Python benchmark re-implements openharn's tool semantics
  rather than driving the Rust binary directly; its edit matcher is simpler than
  `src/edit.rs`'s six-rung anchored cascade. Tool-*dispatch* behavior is faithful;
  edit-*forgiveness* is not identical.
- **CPU-only.** Tokens/second and wall-time absolutes are specific to this machine
  and would change substantially with GPU offload. Relative ordering should hold.
- **Single run.** Each model was measured once in the reported run. Numbers are
  indicative, not distributions.

---

## 10. Reproducibility

```sh
# 1. Serve a model (CPU-only; drop -ngl 0 / raise it if you have VRAM headroom)
llama-server -m ~/Downloads/LFM2.5-8B-A1B-Q4_K_M.gguf \
  --jinja --ctx-size 8192 -ngl 0 --host 127.0.0.1 --port 8080 --no-warmup

# 2a. Interactive: drive it through the real openharn REPL
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_MODEL=local \
  cargo run -- .

# 2b. Benchmark all models (spawns/kills llama-server per model itself)
python tests/benchmark.py            # writes tests/bench_logs/results.{md,json}
python tests/benchmark.py --only LFM2.5   # filter to a subset; merges into the report
```

Diagnostic probes used in §4–§5 (raw content vs. `tool_calls`, `/apply-template`,
`/tokenize` with `with_pieces`, GBNF grammar forcing, `<think></think>` prefill) are
plain `curl`/`urllib` calls against the OpenAI-compatible endpoint and are described
inline above so they can be re-run against any GGUF.

The machine-readable results (`results.json`, `results.md`) and the console
transcript (`run.out`) live under `tests/bench_logs/`; per-model `llama-server`
logs are written there too but are git-ignored (`*.log`).

---

## 11. Conclusion

For small local coding agents, **structured tool-calling is the gating capability,
and it is a property of the checkpoint's post-training, not of the harness or the
quantization tier.** A correct harness (canonical tool block, registered special
tokens, a working lazy parser) is necessary but not sufficient: if the model does
not sample its own trigger token under normal decoding, no parser can rescue it, and
raising the quant does not help. The practical guidance is to select a model that is
demonstrably disposed to emit tool calls — a tool-tuned build (`LFM2-1.2B-Tool`) or
the tool-competent generation (`LFM2.5`) — and, where reasoning latency is
unacceptable, to suppress thinking at the request level via an assistant `<think></think>`
prefill rather than fighting a template flag that does nothing.

---

### Appendix A — model manifest

| File | Params (active) | Quant | Size | Family |
|---|---|---|---:|---|
| `LFM2-1.2B-Tool-Q4_K_M.gguf` | 1.2 B | Q4_K_M | 697 MB | LFM2 v2, tool-tuned |
| `LFM2-8B-A1B-Q3_K_S.gguf` | 8 B (≈1 B) | Q3_K_S | 3475 MB | LFM2 v2 MoE |
| `LFM2-8B-A1B-UD-Q3_K_XL.gguf` | 8 B (≈1 B) | Q3_K_XL | 3506 MB | LFM2 v2 MoE |
| `LFM2-8B-A1B-UD-Q4_K_XL.gguf` | 8 B (≈1 B) | Q4_K_XL | 4524 MB | LFM2 v2 MoE |
| `LFM2.5-1.2B-Instruct-Q4_K_M.gguf` | 1.2 B | Q4_K_M | 697 MB | LFM2.5 |
| `LFM2.5-8B-A1B-APEX-I-Compact.gguf` | 8 B (≈1 B) | — | 4017 MB | LFM2.5 MoE (APEX) |
| `LFM2.5-8B-A1B-APEX-I-Mini.gguf` | 8 B (≈1 B) | — | 3467 MB | LFM2.5 MoE (APEX) |
| `LFM2.5-8B-A1B-Q4_K_M.gguf` | 8 B (≈1 B) | Q4_K_M | 4917 MB | LFM2.5 MoE |
| `gemma-3n-E2B-it-UD-IQ3_XXS.gguf` | E2B | IQ3_XXS | 2216 MB | Gemma 3n |
| `gemma-4-E2B-it-IQ4_XS.gguf` | E2B | IQ4_XS | 2846 MB | Gemma 4 |
| `gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf` | E2B | Q4_K_XL (QAT) | 2499 MB | Gemma 4 |

### Appendix B — the LFM2.5 tool-block divergence

LFM2 (v2) renders tools inside `<|tool_list_start|>[…]<|tool_list_end|>`. LFM2.5
drops those delimiters and renders a plain `List of tools: [ … ]` line in the system
turn — yet still emits `<|tool_call_start|>[…]<|tool_call_end|>` for calls, and
`llama.cpp` still parses them. The tool-*list* framing and the tool-*call* framing
are independent; only the latter gates dispatch.
