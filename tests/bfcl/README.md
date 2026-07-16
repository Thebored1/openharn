# openharn on BFCL v4

Reproducible setup for benchmarking a small model on the **Berkeley Function Calling
Leaderboard v4** (Patil et al., ICML 2025) *through openharn*, so the harness's effect
on tool-call reliability can be measured against the raw model on the same dataset and
the same official AST checker.

> Scope note (matches openharn's thesis): the point is not a leaderboard number, it is
> **does the harness move the score, model-agnostically, on weak/CPU hardware.** Runs
> are CPU-first (`llama-server -ngl 0`); see [`notes/bfcl-v4.md`](../../notes/bfcl-v4.md)
> for results and the GPU-vs-CPU measurement on this box.

## What connects to what

```
BFCL (bfcl generate/evaluate)                        [OpenAI FC client + AST checker]
   │  OPENAI_BASE_URL
   ├──────────────► llama-server :8080   (A: raw native FC — baseline)
   └──────────────► openharn --serve :8090 ──► llama-server :8080
                    OPENHARN_FC_PROXY=1        (B: harness — one constrained
                                                tool-call generation, no agent loop)
```

`openharn --serve` gains an **FC-proxy mode** (`OPENHARN_FC_PROXY=1`): when a request
carries `tools`, it runs exactly ONE constrained tool-call generation (openharn's
prompt-tools + strict grammar, or native tools) and returns the `tool_calls` directly —
no agent loop, no execution. That exposes only openharn's tool-call *reliability* layer,
which is what BFCL single-turn categories score. See `src/serve.rs` / `agent::fc_proxy_once`.

Harness sub-modes (env on the `--serve` process):
- **B1 prompt-tools + strict** — `OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1`
- **B2 native + recovery** — `OPENHARN_FC_PROXY=1` only (native `tools`, plus text-call recovery)

## Prerequisites

```sh
python -m venv .venv && . .venv/Scripts/activate     # or your venv
pip install bfcl-eval soundfile                       # soundfile: BFCL optional-dep fix
python tests/bfcl/register_models.py                  # add the two FC models (idempotent)

export BFCL_PROJECT_ROOT=/path/to/scratch             # result/, score/, id file live here
export PYTHONUTF8=1 PYTHONIOENCODING=utf-8             # BFCL prints emoji; Windows cp1252 crashes without this
export OPENAI_API_KEY=dummy                            # handler requires a key even for local
```

## Run

```sh
# fixed subset (full v4 is too slow on CPU)
python tests/bfcl/subset.py --n 40 \
  --categories simple_python multiple parallel parallel_multiple irrelevance

# A: baseline — point BFCL at llama-server directly
export OPENAI_BASE_URL=http://127.0.0.1:8080/v1
bfcl generate --model openharn-lfm2-raw --run-ids --num-threads 4 --temperature 0.001 -o
bfcl evaluate --model openharn-lfm2-raw --partial-eval

# B: harness — start openharn FC-proxy, point BFCL at it
#   (B2 shown; drop the two env vars' 0->1 for B1)
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 \
OPENHARN_FC_PROXY=1 OPENHARN_MAX_TOKENS=512 ./target/debug/openharn . &
export OPENAI_BASE_URL=http://127.0.0.1:8090/v1
bfcl generate --model openharn-lfm2-harness --run-ids --num-threads 4 --temperature 0.001 -o
bfcl evaluate --model openharn-lfm2-harness --partial-eval

# failure breakdown for either model
python tests/bfcl/analyze.py openharn-lfm2-raw
```

`--partial-eval` scores only the subset present in the result files (not the official
full-category number). `underscore_to_dot=True` in the registration is required so
BFCL's dotted function names (sanitized to underscores for the OpenAI FC schema) are
mapped back during checking.
