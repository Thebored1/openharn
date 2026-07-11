# Notes: reasoning tokens dominate CPU latency

Optimizing openharn on CPU, the instinct is to chase tokens/second. That's the wrong
target — per-turn latency is set mostly by how many *thinking* tokens a reasoning model
emits, not its generation rate or file size. Notes on the measurements and the
reasoning-off knob.

## Where the time goes

A typical turn with a reasoning model, split by phase:

```
think 345 tok · 14.8s · 23 tok/s     ← reasoning
reply  20 tok ·  0.9s · 23 tok/s     ← the answer
total 15.7s
```

Same rate in both phases (~23 tok/s). The 16× time difference is token count: 345 tokens
thinking, 20 answering. On CPU, thinking is the wall-clock. So the lever is fewer tokens,
not faster ones.

## Compact vs Mini: the smaller file is the slower one

Two 8B-A1B reasoning models, same scenario, CPU:

| Model | Size | total tok | thinking tok | tok/s | time | tools |
|---|---:|---:|---:|---:|---:|:--:|
| APEX-Compact | 4017 MB | 1280 | 1089 | 22.8 | 77 s | 4/4 |
| APEX-Mini | 3467 MB | 3308 | 3084 | 21.6 | 176 s | 3/4 |

Mini is the smaller file and 2.3× slower in practice. Not slower per token (21.6 vs 22.8
is noise) — it just thinks 2.8× more (3084 vs 1089 tokens). Turning reasoning off collapses
the gap to ~1.1× (below), confirming the difference was reasoning volume.

Two things to keep:

- **File size ≠ speed for MoE.** Both are 8B-A1B — ~1B *active* params per token. Total
  size only sets how many experts sit in RAM; it barely touches per-token cost. That's why
  the larger Compact even has the higher tok/s. Judge an MoE by active params, not download
  size.
- **"Mini" is a misnomer for latency** — smaller on disk, slowest in use, because of how
  much it reasons.

## Reasoning-off

`OPENHARN_NO_THINK=1` primes each request with a closed `<think></think>` block so the
model continues from a finished think state. Same scenario, CPU:

| Model | reasoning | time | tools | task |
|---|---|---:|:--:|:--:|
| APEX-Compact | on | 77.3 s | 4/4 | PASS |
| APEX-Compact | off | 26.1 s | 4/4 | PASS |
| APEX-Mini | on | 176.0 s | 3/4 | PASS |
| APEX-Mini | off | 29.0 s | 3/4 | PASS |

~3× faster for Compact, ~6× for Mini, same tool accuracy and task success on this
benchmark. Trade-off: reasoning helps on genuinely hard multi-step problems; for chat, a
single tool, or a simple edit it's overhead.

## A correction

The first reasoning-off pass reported `think_tok = 0` and I called it "zero thinking."
That was a measurement artifact — the prefill *shortens* thinking, it doesn't stop it, and
the residual leaks into content wrapped in stray `<think>…</think>` tags the regex didn't
match. The speedup is real (far fewer tokens generated); "much less thinking" is accurate,
"none" isn't. openharn suppresses the leaked reasoning behind the live meter and strips the
stray tags from the answer (`strip_think` in [`src/agent.rs`](../src/agent.rs)).

## Practical

- Budget by thinking tokens, not tok/s. 1000 thinking tokens at 25 tok/s is slower than
  100 at 20.
- Prefer models that reason little for agent work — APEX-Compact reaches 4/4 with the
  fewest thinking tokens of the reliable set.
- `OPENHARN_NO_THINK=1` when latency matters more than the hardest reasoning; on these
  tasks it cost nothing in reliability.
- Read MoE specs by active params (8B-A1B ≈ 1B active).

Notably, the compiler-flag levers did nothing here — flash-attention, thread tuning, and
KV-quant moved tok/s by ~0. The win was getting the model to do less unnecessary work.

Data: [`tests/bench_logs/`](../tests/bench_logs/); harness:
[`tests/benchmark.py`](../tests/benchmark.py) (`--reasoning-off` reproduces the no-think
numbers).
