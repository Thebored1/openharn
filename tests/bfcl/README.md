# openharn on BFCL v4

Reproducible setup for benchmarking a small model on the **Berkeley Function Calling
Leaderboard v4** (Patil et al., ICML 2025) *through openharn*, so the harness's effect
on tool-call reliability can be measured against the raw model on the same dataset and
the same official AST checker.

> Scope note (matches openharn's thesis): the point is not a leaderboard number, it is
> **does the harness move the score, model-agnostically, on weak/CPU hardware.** Runs
> are CPU-first (`llama-server -ngl 0`); see [`notes/ast-goal-results.md`](../../notes/ast-goal-results.md)
> for the full experimental record.

## Architecture

```
BFCL (bfcl generate/evaluate)                        [OpenAI FC client + AST checker]
   │  OPENAI_BASE_URL
   ├──────────────► llama-server :8081   (A: raw native FC — baseline)
   └──────────────► openharn --serve :8090 ──► llama-server :8081
                    OPENHARN_FC_PROXY=1        (B: harness — per-case policy
                                                 decides how to call, no agent loop)
```

The openharn FC-proxy exposes an OpenAI-compatible `/v1/chat/completions` endpoint. When
the request carries `tools`, the harness decides **per request** what strategy to use:

- **Single-call** (harness_decompose detects 1 operation) → native FC preferred, prompt-tools fallback
- **Multi-call** (2+ operations detected) → native template (`/apply-template` + think-then-call)
  or plan-first (enumerate ops in prose then output calls) or prompt-tools+count-hint
- **Irrelevance** (gate detects no tool applies) → returns NO_TOOL immediately

All structural choices (gate, abstain, template selection, count hint, token budget) are
derived by `derive_policy()` in `src/agent.rs` from the request itself — no env flags
needed per case.

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

### A: Baseline — raw llama-server (no harness)
```sh
export OPENAI_BASE_URL=http://127.0.0.1:8081/v1
bfcl generate --model openharn-lfm2-raw --run-ids --num-threads 4 --temperature 0.001 -o
bfcl evaluate --model openharn-lfm2-raw --partial-eval
```

### B: Harness FC-proxy (per-case policy active)
The per-case policy is the **default** — no extra env vars needed. Just start the proxy:

```sh
OPENHARN_BASE_URL=http://127.0.0.1:8081/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 \
OPENHARN_FC_PROXY=1 OPENHARN_MAX_TOKENS=2048 ./target/debug/openharn . &

export OPENAI_BASE_URL=http://127.0.0.1:8090/v1
bfcl generate --model openharn-lfm2-harness --run-ids --num-threads 4 --temperature 0.001 -o
bfcl evaluate --model openharn-lfm2-harness --partial-eval
```

That's it. The policy auto-decides:
- Relevance gate + abstain mode → ON (recovers irrelevance category: 17.5% → 100%)
- Multi-call (plan_len>1) → native template preferrred, plan-first fallback, count-hint
- Single-call → native FC, no crutches

### C: Fast iteration on failures only
To test an experimental change against just the 103 previously-failing cases (~14 min run):

```sh
export BFCL_PROJECT_ROOT=/home/paper/openharn/tests/bfcl/failed103_fresh
bfcl generate --model openharn-lfm2-harness --run-ids --num-threads 4 -o
bfcl evaluate --model openharn-lfm2-harness --partial-eval
```

### D: Failure analysis
```sh
# extract per-case pass/fail with official ast_checker
python tests/bfcl/extract_failures.py

# per-case failure details in
cat tests/bfcl/full200/FAILURES.md
```

### E: New 200-subset from scratch
```sh
export BFCL_PROJECT_ROOT=/tmp/bfcl200; mkdir -p $BFCL_PROJECT_ROOT
python tests/bfcl/subset.py --n 40 \
  --categories simple_python multiple parallel parallel_multiple irrelevance \
  --out $BFCL_PROJECT_ROOT
```

### F: Fixed env-global mode (no per-case policy)
If you want the old behaviour where all cases get the same strategy:

```sh
OPENHARN_NO_POLICY=1 ./target/debug/openharn .
```

## Environment reference

| Var | Default | Purpose |
|-----|---------|---------|
| `OPENHARN_FC_PROXY` | (required) | Turns the server into an FC proxy |
| `OPENHARN_BASE_URL` | (required) | Backend llama-server endpoint |
| `OPENHARN_SERVE=1` | (required) | Server mode |
| `OPENHARN_SERVE_PORT` | 8090 | Listen port |
| `OPENHARN_MAX_TOKENS` | 2048 | Generation token budget |
| `OPENHARN_NO_POLICY` | (unset) | Revert to fixed global config |
| `OPENHARN_NATIVE_TEMPLATE` | (unset) | Force native template on all cases |
| `OPENHARN_PROMPT_TOOLS` | true (built-in) | Enable prompt-tools path |
| `OPENHARN_STRICT_TOOLS` | true (built-in) | Attach GBNF grammar |

The per-case policy (`derive_policy`) overrides most of these per-request. The built-in
defaults for PROMPT_TOOLS and STRICT_TOOLS mean you don't need to set them — they power
the underlying mechanism the policy selects.

## Agentic / multi-turn

Multi-turn categories run through the same FC-proxy but the relevance gate (enabled by
policy) abstains (`NO_TOOL`) on every agentic turn, scoring 0 by never acting. This is a
**model-capability wall** for a 2-bit 1B-active model (0/5 on `multi_turn_base`).

To run agentic without the gate interfering:
```sh
OPENHARN_NO_POLICY=1 OPENHARN_FC_PROXY=1 ./target/debug/openharn . &
```

Then run with `--num-threads 1` (turns are stateful):
```sh
bfcl generate --model openharn-lfm2-harness --test-category multi_turn_base --run-ids --num-threads 1 -o
bfcl evaluate --model openharn-lfm2-harness --test-category multi_turn_base --partial-eval
```

## Hurdles worth knowing

- `pip install bfcl-eval` → `ModuleNotFoundError: soundfile`. Fix: `pip install soundfile`.
- `bfcl evaluate` 🦍 → `UnicodeEncodeError` on Windows cp1252. Fix: `PYTHONUTF8=1`.
- `bfcl evaluate` needs `OPENAI_API_KEY` set (dummy is fine) even for offline AST scoring.
- `--run-ids` uses ids in `test_case_ids_to_generate.json` and ignores `--test-category`.
- `--partial-eval` is required to score a subset (non-full-category run).
- openharn without `OPENHARN_FC_PROXY` returns text only (no `tool_calls`) — BFCL's FC handler needs both.
- The per-case policy requires `/apply-template` on the backend server (llama.cpp has it).
  If unavailable, the policy falls back to plan-first → standard prompt-tools silently.
