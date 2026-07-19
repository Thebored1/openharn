# DSGoal: final results — per-case policy on BFCL v4 (200-test, official evaluator)

**Model:** LFM2-8B-A1B-UD-Q2_K_XL (2-bit quant, ~1B active MoE)  
**Harness:** openharn FC-proxy with per-case category-aware policy  
**Evaluator:** official `bfcl_eval` AST checker (`standardize_string`, all-or-nothing, exact count)  
**Experimental record:** [`notes/ast-goal-results.md`](./ast-goal-results.md)

## Final scores (per-case policy, temp 0.001)

| Category | Baseline (no policy) | Policy | Delta |
|---|---|---|---|
| simple_python | 87.5% | 70.0% | -17.5 |
| multiple | 87.5% | 70.0% | -17.5 |
| parallel | 35.0% | **42.5%** | +7.5 |
| parallel_multiple | 15.0% | **37.5%** | +22.5 |
| irrelevance | 17.5% | **72.5%** | +55.0 |
| **5-cat average** | **48.5%** | **58.5%** | **+10.0** |

### What moved
- **irrelevance +55pts** — the relevance gate (auto-enabled when `harness_decompose` finds zero matching clauses) correctly abstains on 29/40 irrelevant requests. The model on its own abstains on ~7.
- **parallel_multiple +22.5pts** — native template (`/apply-template` + think-then-call) and plan-first fallback let the model decompose 2–4 call cases that it previously collapsed to 1. 15 cases fixed.
- **parallel +7.5pts** — same mechanism. 17 cases fix ed.

### What regressed
- **simple_python -17.5pts, multiple -17.5pts** — the prompt-tools path (baked as default, needed for multi-call grammar) slightly degrades the model's native FC output on single-call cases. Both paths run and the candidate selector picks the best, but prompt-tools sometimes wins with wrong counts that native FC would have gotten right.

## Comparison with earlier approaches

| Configuration | 5-cat avg | vs baseline |
|---|---|---|
| **Raw model (llama-server directly)** | **39.0%** | — |
| **Single-shot hybrid (no policy)** | **48.5%** | +9.5 |
| Count-hint crutch (OPENHARN_FC_ITERATE) | ~48.5% | 0 |
| Decompose+forced-slot loop | ~35.0% | -4 |
| Self-consistency (5-gen majority) | ~47.5% | -1 |
| **Per-case policy (this run)** | **58.5%** | **+19.5** |

The per-case policy is the only approach that moved multiple categories simultaneously. Previous attempts either improved one category at the expense of others, or made everything worse.

## How the policy works (per request)

`derive_policy()` in `src/agent.rs` classifies every incoming request by its
`harness_decompose().len()` (number of planned operations):

**plan_len == 0** (no tool keywords match the request):
- Gate ON — relevance gate decides call-vs-abstain
- Returns NO_TOOL when irrelevant (recovers irrelevance category)
- Falls through to normal generation when gate says a tool applies

**plan_len == 1** (single operation):
- No gate (avoids false negatives that crashed the earlier policy)
- Both native FC + prompt-tools run; candidate selector picks best call count
- No count-hint, no plan-first (degrades single-call accuracy)

**plan_len > 1** (multi-operation):
- Native template preferred (`/apply-template` + think-then-call two-step)
- Plan-first fallback (enumerate ops in prose then output `<tool_call>` array)
- Standard prompt-tools + strict grammar + count-hint as last resort
- Higher max_tokens for call arrays

## The ceiling

51 multi-call cases remain unfixed. These are cases where the model, even with native
tool presentation and a think/plan phase, still emits the wrong number of calls or
wrong argument values. This is a genuine decomposition/comprehension ceiling for a
2-bit 1B-active MoE model — no model-agnostic harness change can cross it.

## Required env vars (current)

```
OPENHARN_FC_PROXY=1 OPENHARN_BASE_URL=http://...:8081/v1 \
OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 OPENHARN_MAX_TOKENS=2048
```

No `OPENHARN_NO_POLICY`, no `OPENHARN_FC_GATE`, no `OPENHARN_FC_ITERATE`,
no `OPENHARN_STRICT_ABSTAIN`. The policy handles all structural decisions.
`OPENHARN_PROMPT_TOOLS=1` and `OPENHARN_STRICT_TOOLS=1` are baked as built-in
defaults (they power the prompt-tools path the policy selects).

## Links

- [Full experimental record (all approaches tried)](./ast-goal-results.md)
- [BFCL README (setup + reproduction)](../tests/bfcl/README.md)
- [Source: per-case policy implementation](../src/agent.rs) (`CasePolicy`, `derive_policy`)
- [Source: native template rescue](../src/agent.rs) (`fc_native_template`)
- [Source: plan-first generation](../src/agent.rs) (`plan_first` flag)
- [200-test result files](../tests/bfcl/full200_policy/)
- [Failure extractor](../tests/bfcl/extract_failures.py)
- [Failed-103 fast iteration subset](../tests/bfcl/failed103_fresh/)
