# Can a 1.58-bit model run an agent on a CPU? A BitNet field report

*Building bitnet.cpp from source, running BitNet-b1.58-2B-4T on two AArch64… no,
two x86 CPUs, and finding out whether a ternary model can actually drive
[openharn](../README.md)'s tools — or just generate fast.*

---

## Abstract

BitNet b1.58 is, on paper, the perfect model for a CPU-first agent: ternary weights
(−1/0/1, ~1.58 bits) turn matrix multiplies into adds and subtracts, promising high
throughput and a tiny memory footprint with no GPU. We set out to test whether
BitNet-b1.58-2B-4T could serve as openharn's model. Three findings: (1) BitNet GGUFs
are **not** loadable by mainline llama.cpp — they use bitnet.cpp's private `i2_s`
tensor format, so you must build bitnet.cpp; (2) BitNet's speed is **strongly
hardware-dependent** and its headline advantage is muted on CPUs without AVX‑512/VNNI
— we measured 13 tok/s on one AVX2 laptop and 22.7 tok/s on an AVX2 Ryzen, both with
the same kernel; and (3) bitnet.cpp's server has **no tool-calling API** at all. We
built a general workaround in openharn (prompt-tools mode) and used it to answer the
only question that mattered: **BitNet-2B can emit tool calls but cannot use them
reliably** — wrong argument fields, invented paths, repetition loops. It is not
tool-trained, and 2B ternary lacks the reliability. BitNet is **shelved**; the reusable
win was the harness feature we built to test it.

---

## 1. Why BitNet, and why on a CPU

openharn's thesis is that a *small* model on *weak, CPU-only* hardware can do real
agentic work if the harness is good. BitNet is the most literal expression of that
premise: ternary weights are cheap to compute on a CPU, and a 2B BitNet fits in ~1.1 GB.
If any model class should shine on a potato, it's this one. So the question was concrete:
**can BitNet-b1.58-2B-4T replace our current best (LFM2.5-8B-APEX) as openharn's
model — faster, smaller, still able to drive tools?**

---

## 2. The GGUF wall

The first surprise: the official BitNet GGUFs (`microsoft/BitNet-b1.58-2B-4T-gguf`,
`tiiuae/Falcon3-*-1.58bit-GGUF`) **do not load in mainline llama.cpp**, at any version.
Both a stock build (b9608) and the latest (b9947, which we upgraded to mid-test) fail
identically:

```
gguf_init_from_reader: failed to read tensor info
```

The file is a valid GGUF v3 (correct magic, 332 tensors, readable header) — but the
*tensors* use bitnet.cpp's private `i2_s` ternary type, a dialect only bitnet.cpp
understands. Mainline llama.cpp added its own ternary support (`TQ1_0`/`TQ2_0`), but
that is a **different** on-disk format. So: to run these weights, you must build
bitnet.cpp.

**Takeaway:** "BitNet is just a GGUF" is a trap. The container is standard; the
contents are not.

---

## 3. Building bitnet.cpp (the part nobody screenshots)

bitnet.cpp is a fork of llama.cpp with hand-written ternary CPU kernels. Building it on
a fresh Windows box was a chain of small fights, all recorded here so the next person
skips them:

1. **Toolchain.** Needs `cmake` + `clang`; installed CMake, LLVM (clang 22), Ninja via
   winget.
2. **Generator.** bitnet.cpp's script uses the Visual Studio **ClangCL** toolset, which
   wasn't installed in VS. Switched to **Ninja + `clang-cl`** driven through a
   `vcvars64` environment (clang-cl is the MSVC-compatible driver; plain `clang++`
   chokes on cmake's `/std:c++17`-style flags).
3. **Generated kernel header.** The build references `include/bitnet-lut-kernels.h`,
   which is *generated per model* by `utils/codegen_tl2.py`. For 2B-4T upstream reuses
   the 3B kernel parameters. The codegen is stdlib-only, so it sidesteps the broken
   `gguf-py`/`sentencepiece` install (those have no wheels for Python 3.13/3.14).
4. **clang-22 strictness.** Two source patches: a `const int8_t *` fix in
   `src/ggml-bitnet-mad.cpp`, and a missing `#include <chrono>` in `common/log.cpp`
   (the fork is an old llama.cpp; a modern STL requires the explicit include).
5. **Build the server target only.** `cmake --build build --target llama-server` skips
   the example binaries (imatrix, etc.), which don't compile under clang-22 and aren't
   needed.

The identical recipe built cleanly on Fedora 44 (Ryzen box) — Linux made steps 2 and 5
easier (Make generator, system clang).

---

## 4. Speed: hardware-sensitive, and the headline is muted

BitNet-b1.58-2B-4T, `i2_s` kernel, CPU-only, wall-clock over a 200-token generation:

| Machine | CPU | SIMD | tok/s |
|---|---|---|---:|
| Windows laptop | Intel, AVX2 | no AVX-512/VNNI | **13.0** |
| `myelinbox` | Ryzen 5 5500U (Zen 2), AVX2 | no AVX-512/VNNI | **22.7** |

Two things stand out. First, the **~1.75× spread on the same kernel** shows the laptop
number was as much about the laptop as about BitNet — a fairer read of BitNet-i2_s on a
decent AVX2 chip is ~23 tok/s, right next to our LFM2.5-8B MoE (~26). Second, **neither
CPU has AVX‑512 or VNNI** — the instructions BitNet's fast kernels lean on. We built the
portable `i2_s` kernel, not the AVX2-optimized `TL2`; by bitnet.cpp's published ratios
TL2 would add ~1.5–2× (≈35–45 tok/s estimated — *not measured*, see §6).

**Takeaway, and it's on-thesis:** BitNet's speed advantage is CPU-feature-dependent.
On exactly the kind of older/cheaper hardware openharn targets — AVX2, no AVX‑512 —
that advantage is real but modest, not the order-of-magnitude the headlines imply.

---

## 5. The tool-calling wall, and the workaround

bitnet.cpp's server is an old llama.cpp fork. It answers chat fine, but the moment
openharn sends its tool list it returns:

```
500: Unsupported param: tools
```

No native tool-calling at all. Rather than give up, we built a general harness feature —
**prompt-tools mode** (`OPENHARN_PROMPT_TOOLS=1`): describe the tools in the system
prompt, omit the `tools` field, flatten the internal tool-call/tool-result history into
plain roles on the wire, and recover the model's text tool-call with openharn's parser.
(Full writeup: [`adaptive-tool-calling.md`](adaptive-tool-calling.md).) This let us ask
the real question.

---

## 6. Can BitNet agent? No — and that's why we stopped

Through prompt-tools mode, BitNet-2B **does** emit and dispatch tool calls. But its
*judgment* is unusable. Two representative turns from a live run against our self-built
server:

- *"what files are in src?"* → `glob {"pattern":"*","scope":"system"}` — searches the
  **entire computer** instead of the project.
- *"find where Config is defined"* → `grep {"include":"Config","path":"src/config"}`,
  three times identically — the search term in the **wrong field** (`include`, not
  `pattern`), an **invented path**, and a **repetition loop** the circuit breaker had to
  stop.

This is the same failure class we saw in `granite-3.1-1b-a400m`: the mechanism works,
the model doesn't. BitNet-2B-4T is a general instruct model, not tool-trained, and 2B
ternary doesn't have the reliability to select the right tool with sane arguments.

Because it can't agent, we **deliberately did not build TL2**. Speed on a model that
can't drive a tool is polishing the wrong thing — the whole point of measuring `i2_s`
first (cheap) was to gate the expensive TL2 work behind a signal that never came.

---

## 7. Verdict

**BitNet is shelved:** it runs, it's respectably fast on a decent AVX2 CPU, but this
model class (general, non-tool-trained, 2B ternary) and this runtime (an old fork with
no tool API) aren't ready to be an agent. Two honest caveats keep the door open: (1) a
**tool-trained** BitNet, if one appears, could change the reliability verdict entirely —
the format/parsing side is solved; (2) on a CPU **with** AVX‑512/VNNI, the speed story
would be much stronger, and TL2 would be worth building.

The durable outcome wasn't BitNet — it was the reusable capability we built to evaluate
it, and the discipline of gating expensive work behind cheap signal.

---

## Appendix — reproduction

```sh
# build bitnet.cpp (Linux shown; Windows needs Ninja+clang-cl via vcvars — see §3)
git clone --recursive https://github.com/microsoft/BitNet.git && cd BitNet
sed -i 's/int8_t \* y_col/const int8_t * y_col/' src/ggml-bitnet-mad.cpp
sed -i 's|#include "log.h"|#include "log.h"\n#include <chrono>|' 3rdparty/llama.cpp/common/log.cpp
python3 utils/codegen_tl2.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 96,96,96 --bm 32,32,32
cmake -B build -DBITNET_X86_TL2=OFF -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++ -DCMAKE_BUILD_TYPE=Release
cmake --build build --target llama-server -j

# get the weights (bitnet.cpp format; will NOT load in mainline llama.cpp)
hf download microsoft/bitnet-b1.58-2B-4T-gguf ggml-model-i2_s.gguf --local-dir .

# serve, then drive it from openharn with prompt-tools mode
./build/bin/llama-server -m ggml-model-i2_s.gguf --ctx-size 4096 -ngl 0 --port 8080
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_PROMPT_TOOLS=1 cargo run -- .
```

Hardware: Ryzen 5 5500U (Zen 2, 6c/12t, AVX2, no AVX‑512), Fedora 44; and an Intel AVX2
laptop, Windows 11. Both CPU-only (`-ngl 0`).
