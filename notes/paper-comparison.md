# openharn vs. the Literature: A Comparative Note

This note situates openharn against the prompt-only SLM agent literature. openharn is not an implementation of any single paper — it is a **meta-harness** that adapts to the model's actual capability (native tools, text calls, JSON, grammar-constrained, tool-pruned) rather than forcing one approach. Several openharn findings are absent from the literature.

---

## Papers Cited

| Short Name | Full Citation |
|------------|---------------|
| **ReAct** | Yao, S., Zhao, J., Yu, D., Du, N., Shafran, I., Narasimhan, K., & Cao, Y. (2023). ReAct: Synergizing Reasoning and Acting in Language Models. *ICLR 2023*. arXiv:2210.03629. |
| **Toolformer** | Schick, T., Dwivedi-Yu, J., Dessì, R., Raileanu, R., Lomeli, M., Zettlemoyer, L., Cancedda, N., & Scialom, T. (2023). Toolformer: Language Models Can Teach Themselves to Use Tools. *NeurIPS 2023*. arXiv:2302.04761. |
| **TALM** | Parisi, A., Zhao, Y., & Fiedel, N. (2022). TALM: Tool Augmented Language Models. arXiv:2205.12255. |
| **CodeAct** | Wang, X., Chen, Y., Yuan, L., Zhang, Y., Li, Y., Peng, H., & Ji, H. (2024). Executable Code Actions Elicit Better LLM Agents. *ICML 2024*. arXiv:2402.01030. |
| **NLT** | Johnson, R. T., Pain, M. D., & West, J. D. (2025). Natural Language Tools: A Natural Language Approach to Tool Calling In Large Language Agents. arXiv:2510.14453. |
| **EASYTOOL** | Hsieh, C.-Y., Chen, S., Li, C., Fujii, Y., Ratner, A., Lee, C.-Y., et al. (2023). Tool Documentation Enables Zero-Shot Tool-Usage with Large Language Models. arXiv:2308.00675. (EASYTOOL is a follow-up; see also *EASYTOOL: Enhancing LLM-based Agents with Concise Tool Instruction*, NAACL 2025.) |
| **slm-agents** | Lin, Y., et al. (2024). Specialized Small-Model Subagents: Co-designing Fine-tuned SLMs and Task-Specific Harnesses. arXiv:2604.25850. |
| **Probe&Prefill / When2Tool** | Sun, C.-E., Liu, L., Yan, G., Wang, Z., & Weng, T.-W. (2026). LLM Agents Already Know When to Call Tools — Even Without Reasoning. arXiv:2605.09252. |
| **ASA** | Wang, Y., Zhou, R., Fu, R., Cao, S., Zeng, H., Lu, J., et al. (2026). ASA: Training-Free Representation Engineering for Tool-Calling Agents. arXiv:2602.04935. |
| **SLM Position Paper** | Belcak, P., Heinrich, G., Diao, S., Fu, Y., Dong, X., Muralidharan, S., Lin, Y., & Molchanov, P. (2025). Small Language Models are the Future of Agentic AI. arXiv:2506.02153v2. |

---

## Harness-by-Harness Comparison

### 1. Default ReAct Loop (`OPENHARN_SLM=0`)

| Paper | Core Idea | openharn Implementation | openharn Extensions |
|-------|-----------|-------------------------|---------------------|
| **ReAct** | Interleave Thought → Action → Observation in conversation history. | Full conversation history sent each turn; model sees system + user + all prior assistant/tool turns. | **Circuit breaker**: per-turn (`MAX_CALLS=1`) + total (`TOTAL_MAX=5`) limits; exact-repeat detection (3× hard stop); grounding messages on truncation; context fit with retry on 400 overflow; tool-result cap (4K chars). |
| **Toolformer** | Self-supervised training: insert API calls into CCNet, filter by loss reduction. | Not applicable — no training. | N/A. |
| **TALM** | Text-to-text interface + iterative self-play bootstrapping from few demos. | Not applicable — no training. | N/A. |

**When it works**: Model family has tool post-training (LFM2.5, Granite 4, Qwen 2.5 3B+, Llama 3.1 8B+). With `NO_THINK=1` on CPU: ~15–20 tok/s, passes behavioral suite (MiniCPM-V-4.6: 6/6).

**When it fails**: Model doesn't emit calls at all (LFM2-v2 8B-A1B → 0/4) or emits malformed calls parser can't recover.

---

### 2. SLM Harness (`OPENHARN_SLM=1`)

Direct port of the **five harness requirements** from **slm-agents** (Lin et al., 2024, §3).

| slm-agents Requirement | Paper Implementation (C5) | openharn Implementation |
|------------------------|---------------------------|--------------------------|
| **1. Constrained action space** | `valid_actions()` — only legal next actions (ANSWER requires prior READ) | `valid_actions`: SEARCH always; READ if files discovered; ANSWER if READ executed; ESCALATE always. |
| **2. Minimal structured observation** | Compact JSON: goal, valid_actions, searches, reads, last_result, feedback | Exact schema in `dual-execution-modes.md` lines 47–58. |
| **3. Externalized state** | All durable state in `CustomHarnessState` (not model context) | `SlmState` struct — goal, searches, files, reads, feedback. |
| **4. Cheap deterministic verification** | Pre-execution action validation + post-execution result checks + terminal scoring | `validate_action()` (pre) + `verify_step_result()` (post) in `verifier.rs`. |
| **5. Retry localization** | Re-prompt only failed step with feedback; task never restarts | Inner `retry` loop in `run_slm()` with `feedback` in next observation. |

**Key difference**: slm-agents pairs this harness with a **fine-tuned specialist model** (1.5B/3B LoRA distilled from 72B teacher). openharn uses **base models only** (no fine-tuning — thesis constraint).

| Model | SLM Harness Score | Why |
|-------|-------------------|-----|
| MiniCPM-V-4.6 | 6/6 | Emits valid JSON natively; constrained actions help reliability. |
| LFM2-8B-A1B | 2/6 | Cannot emit valid JSON; harness requires it. |

**Paper's factorial result (C1–C6)**: Custom harness alone (C3) and fine-tuning alone (C4) both **hurt** vs. generic 3B. Only the co-designed pair (C5) works (94.5% success, matches 72B). openharn's SLM harness = their custom harness; base model = C3 equivalent.

---

### 3. Grammar-Constrained Text Mode (`OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1`)

| Paper | Core Idea | openharn Implementation |
|-------|-----------|--------------------------|
| **NLT** (Johnson et al., 2025) | Replace JSON with YES/NO per tool in natural language; parser executes. | `PROMPT_TOOLS=1`: tools described in system prompt text (no native `tools` API). `STRICT_TOOLS=1`: GBNF grammar (`tool_grammar()`) forces every reply to valid `` `` ` `` or plain text. Grammar generated automatically from `schemas()`. |
| **ReAct** | Native tool calls via `tools`/`tool_choice` API. | Native API used by default; grammar-constrained text is a **variant** for models that ignore native API. |

**Why it works for LFM2-8B**:
- LFM2-8B ignores native `tools` API — outputs descriptive text instead of `<tool_call>`.
- GBNF grammar forces valid `<tool_call>` output the model wouldn't emit otherwise.
- Model complies when constrained; grammar acts as structural guide.

| Model | Default | PROMPT_TOOLS+STRICT |
|-------|---------|---------------------|
| LFM2-8B-A1B | 3/6 | **6/6** |
| LFM2-1.2B-Tool | 4/6 | 5/6 |
| MiniCPM-V-4.6 | 6/6 | Works |

**This is a prompt-only approximation of the slm-agents co-designed pair (C5)**:
- slm-agents C5: Specialist model **trained** to emit harness's exact JSON schema (distillation: 428 trajectories, $0.18, H100).
- openharn: Base model **forced** to emit harness's text format via GBNF grammar — **zero training**.

---

### Quick Decision Guide: Which Mode?

| Problem | Reach For | Why |
|---------|-----------|-----|
| Server rejects `tools` field / no native tool API (bitnet.cpp, old forks) | `OPENHARN_PROMPT_TOOLS=1` | Moves tool descriptions into prompt text; omits `tools` field entirely. |
| Model emits valid call but server drops it (Granite `<tool_call>[...]` vs `<|tool_call|>`) | **Automatic** | `parse_text_tool_calls()` recovers text-format calls when native parse returns nothing. |
| Model ignores native `tools` API / won't emit structured calls (LFM2-v2) | `OPENHARN_STRICT_TOOLS=1` (+ `PROMPT_TOOLS=1`) | GBNF grammar forces valid `<tool_call>[...]` output; model complies when constrained. |
| Small model hallucinates with full tool set (LFM2-1.2B) | `OPENHARN_YESNO=1` (+ `STRICT_TOOLS=1`) | Two-pass YES/NO prunes tool list per turn → less confusion → no hallucination. |
| Need max reliability for read-only nav on weak model | `OPENHARN_NARROW=1` | Preset: `read,grep,glob` + strict + prompt-tools; minimal surface, grammar-locked. |

---

### 4. YES/NO Two-Pass Tool Selection (`OPENHARN_YESNO=1`)
|-------|-----------|--------------------------|
| **NLT** | Two-pass: Pass 1 — model selects tools (YES/NO); Pass 2 — model fills args for selected tools only. | Implemented in `agent.rs` lines 213–223. `run_yesno_pass1()` runs each turn; selected tools filtered into `effective_schemas`; grammar + prompt text only advertise selected tools. |

| Model | Default | YESNO+STRICT |
|-------|---------|--------------|
| LFM2-1.2B-Tool | 4/6 | **6/6** (hallucination reduction on complex queries) |
| LFM2-8B-A1B | 3/6 | N/A (Pass 2 uses native API which LFM2 ignores) |

**NLT paper result**: +26.1 pp for open-weight models vs. JSON. openharn confirms: YES/NO narrows tool list → less confusion → no hallucination.

---

### 5. CodeAct (Python Tool)

| Paper | Core Idea | openharn Implementation |
|-------|-----------|--------------------------|
| **CodeAct** (Wang et al., 2024) | Executable Python snippets as unified action space; integrates with Python interpreter. | `python` tool in `tools.rs`. Model emits Python code; harness `exec()`s in restricted sandbox; returns stdout/stderr/return code. Pre-imports `read`, `write`, `edit`, `glob`, `grep`, `bash`, `json`. |

| Model | YES/NO + CodeAct | PROMPT_TOOLS+STRICT |
|-------|------------------|---------------------|
| LFM2-8B-A1B | 2/6 (python-only tasks) | **6/6** (general) |

**CodeAct paper result**: 17 LLMs tested; code actions beat JSON/text by up to 20% success. openharn confirms: works for python-expressible tasks, loses read/search/edit/glob behavioral cases.

---

### 6. Probe&Prefill / When2Tool (Sun et al., 2026)

| Paper | Core Idea | openharn Status |
|-------|-----------|-----------------|
| **Probe&Prefill** | Linear probe on prefill hidden state → prefill "I need a tool" / "I can answer directly". Reduces tool calls 48% at 1.7% accuracy loss. | **Not implemented** — requires white-box hidden-state access (not available via OpenAI-compatible API). |

**Relevance to openharn**: Addresses "over-calling tools" — a real small-model failure mode. openharn mitigates via circuit breakers (`MAX_CALLS`, `TOTAL_MAX`, repeat detection) and grounding messages, but cannot read the model's internal tool-necessity signal.

---

### 7. ASA (Wang et al., 2026)

| Paper | Core Idea | openharn Status |
|-------|-----------|-----------------|
| **ASA** | Gated shared+local activation steering: shared boundary direction calibrates tool-mode entry; domain-local residual steers schema compliance. Improves Qwen3-8B first-call accuracy 24.5% → 41.9%. | **Not implemented** — requires white-box forward hooks. |

**Relevance**: ASA solves **schema compliance** (valid JSON, correct tool names, args). openharn's `STRICT_TOOLS` GBNF grammar achieves similar format enforcement at inference time without model internals.

---

### 8. EASYTOOL (Hsieh et al., 2023 / NAACL 2025)

| Paper | Core Idea | openharn Approach |
|-------|-----------|-------------------|
| **EASYTOOL** | Compress verbose tool docs into concise structured instructions (~30 tokens/tool vs. ~200). Reduces token cost 70%, improves small-model performance. | **Not needed** — token savings in openharn come from **pruning tool set** (`OPENHARN_TOOLS`, `OPENHARN_NARROW`, YES/NO filtering) + **dropping full schemas in prompt-tools mode** (tools described in ~1-line natural language). For CPU 0.8B models, pruning + simplification beats compression. |

---

## openharn Findings Absent from Literature

### 1. `<example>` Blocks Break Weak Models
**Finding**: The 5 `<example>` blocks in `src/prompt.txt` primed LFM2-8B to hallucinate file contents ("contains entries like apple: red, banana: yellow…") instead of reporting "not found" on missing files. Removing all examples fixed LFM2 5/6 → 6/6 without regressing Qwen (stays 6/6).

**No paper mentions this**. Standard practice in ReAct/CodeAct papers is to include few-shot examples; for weak models on CPU, examples are actively harmful.

### 2. Reasoning Tokens = CPU Wall-Clock
**Finding**: On CPU, per-turn latency is dominated by thinking token count, not generation rate. 
- APEX-Compact: 1089 thinking tokens → 77s; with `NO_THINK=1`: 26s (3× speedup, same tool accuracy).
- APEX-Mini: 3084 thinking tokens → 176s; with `NO_THINK=1`: 29s (6× speedup).
- File size ≠ speed for MoE (both 8B-A1B ≈ 1B active params); "Mini" is slower due to more reasoning.

**SLM Position Paper** (Belcak et al., 2025) identifies reasoning overhead as operational cost; openharn quantifies it on CPU and provides `NO_THINK=1` prefill knob.

### 3. Model-Specific Config Discovery
**Finding**: Optimal config is model-dependent and non-obvious:
- Qwen 3.5 0.8B: **Native tools, thinking ON** (6/6). Thinking OFF → falls back to `bash find` (5/6).
- LFM2-8B-A1B: **PROMPT_TOOLS+STRICT, NO_THINK** (6/6). Native tools fail (0/4); examples break it.
- LFM2-1.2B-Tool: **YESNO+STRICT, NO_THINK** (6/6).
- MiniCPM-V-4.6: Any mode (6/6).

No paper tests this matrix; they typically evaluate one config per model family.

### 4. Text Call Recovery (Granite `<tool_call>[...]`)
**Finding**: Granite 3.1 emits valid `<tool_call>[{"arguments":..., "name":...}]` but llama.cpp's template expects ``<|tool_call|>``, so server drops call to `content` with `tool_calls: null`. openharn's `parse_text_tool_calls()` recovers it via regex.

**Papers assume native API works**; this harness/runtime mismatch is a real deployment gap.

### 5. Grammar as "Runtime Distillation"
**Insight**: slm-agents C5 trains a specialist model to emit the harness's JSON schema (distillation). openharn's `STRICT_TOOLS` GBNF grammar **forces the same format compliance at inference** on a base model — zero training, same structural guarantee.

---

## Summary Matrix

| Capability | ReAct | Toolformer | TALM | CodeAct | NLT | EASYTOOL | slm-agents | Probe&Prefill | ASA | openharn |
|------------|-------|------------|------|---------|-----|----------|------------|---------------|-----|----------|
| Native tool calls | ✅ | ✅ (trained) | ✅ | ✅ | ❌ | ✅ | ✅ (specialist) | ✅ | ✅ | ✅ (default) |
| Text-format calls | ❌ | ❌ | ✅ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ (recovery) |
| Prompt-tools mode | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ (`PROMPT_TOOLS`) |
| Grammar constraint | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (`STRICT_TOOLS`) |
| Tool selection (YES/NO) | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ (`YESNO`) |
| Tool pruning | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (`TOOLS`, `NARROW`) |
| Code-as-action | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (`python` tool) |
| Externalized state | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ (SLM harness) |
| Per-step retry + feedback | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ (SLM harness) |
| Circuit breaker / grounding | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (default) |
| CPU benchmarking | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (full suite) |
| Reasoning tax measured | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (`NO_THINK`) |
| Example contamination found | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| Model-specific configs | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| Zero fine-tuning thesis | ✅ | ❌ | ❌ | ❌ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ |

---

## Conclusion

openharn implements **every prompt-only technique** from the literature (ReAct, CodeAct, NLT YES/NO, slm-agents harness) **in one coherent system** with CPU-first benchmarking. It adds:

1. **Grammar-constrained text mode** — a prompt-only approximation of slm-agents co-designed pair (C5) that works on base models (LFM2-8B 6/6).
2. **Multi-harness adaptation** — switches between ReAct, SLM, Grammar, YES/NO, CodeAct per model capability.
3. **Empirical findings absent from papers**: example contamination, reasoning tax quantification, model-specific config matrix, text-call recovery, runtime-distillation via grammar.

The remaining gap is **judgment** (which tool, what args, when to stop) — which no prompt-only method crosses without fine-tuning. openharn solves **form completely**; judgment remains the model's job, as the thesis requires.