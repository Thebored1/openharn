# The reasoning tax: why *thinking*, not tokens/sec, decides CPU agent latency

*On a CPU, a reasoning model's wall-clock is dominated by how much it thinks — not by
how fast it runs, and not by how big it is. Measurements, a counter-intuitive
model comparison, and a reasoning-off mode for [openharn](../README.md).*

---

## Abstract

Optimizing a local coding agent, the instinct is to chase tokens/second. On CPU, that's
the wrong target. Across the LFM2.5-8B reasoning models we found that **per-turn latency
is set almost entirely by the number of thinking tokens the model emits**, not by its
generation rate or its file size. Two consequences: (1) the "faster" model can be the
one that *thinks less*, even if it's larger and slightly slower per token — we show
APEX-Compact beating APEX-Mini by 2.3× in wall-clock purely on thinking volume; and (2)
suppressing reasoning is the single biggest CPU speedup available — a `<think></think>`
prefill cuts turn time 3–6× with no loss of tool accuracy on our benchmark. We also
correct a measurement artifact from our first pass and describe how openharn cleans up
the reasoning a hybrid model leaks even when told not to think.

---

## 1. Where the time actually goes

A representative openharn turn with a reasoning model, broken into phases:

```
think 345 tok · 14.8s · 23 tok/s     ← the reasoning phase
reply  20 tok ·  0.9s · 23 tok/s     ← the actual answer
total 15.7s
```

The generation rate is the same in both phases (~23 tok/s). The 16× time difference is
**purely token count**: the model spent 345 tokens thinking and 20 answering. On CPU,
where you can't hide latency behind a GPU, thinking *is* the wall-clock. This reframes
the optimization target: don't make tokens faster, make the model emit fewer of them.

---

## 2. The counter-intuitive comparison: Compact beats Mini

From the benchmark, two 8B-A1B reasoning models, same scenario, CPU:

| Model | Size | total tok | thinking tok | tok/s | **time** | tools |
|---|---:|---:|---:|---:|---:|:--:|
| APEX-Compact | 4017 MB | 1280 | 1089 | 22.8 | **77 s** | 4/4 |
| APEX-Mini | 3467 MB | 3308 | 3084 | 21.6 | **176 s** | 3/4 |

"Mini" is the *smaller file* and the slower one in practice — by 2.3×. It isn't slower
per token (21.6 vs 22.8 is noise); it just **thinks 2.8× more** (3084 vs 1089 tokens).
The proof is what happens when you remove thinking (§3): the gap collapses from 2.3× to
~1.1×. So the entire wall-clock difference was reasoning volume.

Two lessons fall out:

- **File size ≠ speed for MoE.** Both are 8B-A1B — ~1 B *active* parameters per token.
  Total size only sets how many experts sit in RAM; it barely touches per-token CPU
  cost. That's why the *larger* Compact can even have the higher tok/s. Judge an MoE by
  its active params, not its download size.
- **"Mini" is a misnomer for latency.** Smaller on disk, slowest in use — because of how
  it was tuned to reason, which the filename tells you nothing about.

---

## 3. Reasoning-off: the biggest CPU lever

If thinking is the cost, cutting it is the win. LFM2.5's template exposes no thinking
switch (the `--reasoning off` server flag is a no-op for it), so openharn's
`OPENHARN_NO_THINK=1` primes each request with a **closed `<think></think>` block**,
which makes the model continue from an already-finished think state. Same scenario,
same parameters, CPU:

| Model | reasoning | time | tools | task |
|---|---|---:|:--:|:--:|
| APEX-Compact | on | 77.3 s | 4/4 | PASS |
| APEX-Compact | **off** | **26.1 s** | 4/4 | PASS |
| APEX-Mini | on | 176.0 s | 3/4 | PASS |
| APEX-Mini | **off** | **29.0 s** | 3/4 | PASS |

**~3× faster for Compact, ~6× for Mini — with identical tool accuracy and task
success on this benchmark.** For interactive use on CPU this is the difference between
"unusable" and "snappy." The trade-off is real but bounded: reasoning helps on genuinely
hard multi-step problems; for the common case (chat, a single tool, a simple edit) it's
pure overhead.

---

## 4. An honest correction, and a cleanup

Our first reasoning-off pass reported `think_tok = 0` and we called it "zero thinking."
That was a **measurement artifact**. The `<think></think>` prefill doesn't fully stop a
hybrid model from reasoning — it *shortens* it, and the residual reasoning leaks into the
content stream wrapped in stray `<think>…</think>` tags that our regex didn't match. The
**speedup is real** (the model genuinely emits far fewer tokens); "much less thinking" is
the accurate claim, not "none."

That leak also has to be handled in the REPL, or the user sees a wall of tag soup. So in
reasoning-off mode openharn suppresses the leaked reasoning behind a live meter, starts
streaming the answer only once the reasoning closes (its `</think>`), and strips any
stray tags from the stored/displayed answer (`strip_think` in
[`src/agent.rs`](../src/agent.rs)). The result: you get the speed of near-no-thinking and
a clean answer, not the model's half-suppressed monologue.

---

## 5. Practical guidance

- **On CPU, budget by thinking tokens, not tok/s.** A model that thinks 1000 tokens per
  turn at 25 tok/s is slower than one that thinks 100 at 20.
- **Prefer models that reason *little* for agent work.** APEX-Compact is our default
  because it reaches 4/4 with the *fewest* thinking tokens of the reliable set.
- **Use `OPENHARN_NO_THINK=1` when latency matters more than the hardest reasoning.** It
  is the largest single speedup on CPU and, on our tasks, cost nothing in reliability.
- **Read MoE specs by active params.** 8B-A1B ≈ 1 B active; that's what sets speed, not
  the 3–5 GB on disk.

The meta-point: the biggest CPU performance win we found wasn't a compiler flag or a
kernel — flash-attention, thread tuning, and KV-quant moved our tok/s by *nothing*. It
was getting the model to do **less unnecessary work**. On weak hardware, that's almost
always where the latency is.

---

Data: [`tests/bench_logs/`](../tests/bench_logs/); harness:
[`tests/benchmark.py`](../tests/benchmark.py) (`--reasoning-off` reproduces the no-think
numbers).
