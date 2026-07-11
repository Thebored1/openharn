# Notes: running BitNet on CPU

BitNet b1.58 looks ideal for a CPU agent — ternary weights (−1/0/1, ~1.58 bits) turn
matmuls into adds/subtracts, and a 2B model fits in ~1.1 GB with no GPU. These are notes
on getting BitNet-b1.58-2B-4T running and finding out whether it can drive openharn's
tools. Short version: it runs, it's respectably fast on a decent AVX2 CPU, and it can't
reliably use tools.

## The GGUF won't load in mainline llama.cpp

The official BitNet GGUFs (`microsoft/BitNet-b1.58-2B-4T-gguf`, `tiiuae/Falcon3-*-1.58bit`)
don't load in mainline llama.cpp — any version. b9608 and the latest b9947 both fail:

```
gguf_init_from_reader: failed to read tensor info
```

The file is a valid GGUF v3 (correct header, 332 tensors) but the tensors use bitnet.cpp's
private `i2_s` type. Mainline added its own ternary support (`TQ1_0`/`TQ2_0`) — a different
on-disk format. So you have to build bitnet.cpp.

## Building bitnet.cpp (Windows, the annoying bits)

A fresh Windows box was a chain of small fights; recording them so the next person skips
them:

1. Install `cmake` + `clang` (+ Ninja) via winget.
2. bitnet.cpp's script uses the VS **ClangCL** toolset, which wasn't installed. Switched to
   Ninja + `clang-cl` through a `vcvars64` env (plain `clang++` chokes on cmake's
   `/std:c++17` flags).
3. The build references `include/bitnet-lut-kernels.h`, generated per model by
   `utils/codegen_tl2.py` (stdlib-only, so it sidesteps the broken `gguf-py`/`sentencepiece`
   install on Python 3.13/3.14). For 2B-4T upstream reuses the 3B kernel params.
4. Two clang-22 source patches: a `const int8_t *` fix in `src/ggml-bitnet-mad.cpp`, and a
   missing `#include <chrono>` in `common/log.cpp`.
5. `cmake --build build --target llama-server` — building only the server skips the example
   binaries, which don't compile under clang-22 and aren't needed.

The same recipe built cleanly on Fedora (Make generator, system clang).

## Speed

BitNet-b1.58-2B-4T, `i2_s`, CPU, 200-token generation:

| Machine | CPU | SIMD | tok/s |
|---|---|---|---:|
| Windows laptop | Intel, AVX2 | no AVX-512/VNNI | 13.0 |
| myelinbox | Ryzen 5 5500U (Zen 2), AVX2 | no AVX-512/VNNI | 22.7 |

~1.75× spread on the same kernel — the laptop number was as much about the laptop as about
BitNet. On a decent AVX2 chip it's ~23 tok/s, next to the LFM2.5-8B MoE (~26). Neither CPU
has AVX-512/VNNI, which BitNet's fast kernels want, and this is the portable `i2_s` kernel,
not the AVX2-optimized `TL2` (by bitnet.cpp's published ratios TL2 would add ~1.5–2×, ≈35–45
tok/s — estimated, not built). So on the hardware openharn targets — older CPUs, no
AVX-512 — the ternary speed advantage is real but modest.

## No tool API, and the workaround

bitnet.cpp's server is an old llama.cpp fork; it returns `500: Unsupported param: tools`.
`OPENHARN_PROMPT_TOOLS=1` gets around that (describe tools in the prompt, recover the text
call — see [`adaptive-tool-calling.md`](adaptive-tool-calling.md)), which let us test the
actual question.

## Can it use tools? No

Through prompt-tools mode BitNet-2B dispatches calls, but its judgment is unusable:

- *"what files are in src?"* → `glob {"pattern":"*","scope":"system"}` — searches the whole
  computer instead of the project.
- *"find where Config is defined"* → `grep {"include":"Config","path":"src/config"}`, three
  times identically — search term in the wrong field (`include`, not `pattern`), invented
  path, repetition loop the circuit breaker stopped.

Even with a grammar forcing valid calls (`OPENHARN_STRICT_TOOLS=1`), it produced
`glob {"pattern":"*.rust"}` — right format, wrong value. Same class as
`granite-3.1-1b-a400m`: the mechanism works, the model doesn't. BitNet-2B-4T is a general
instruct model, not tool-trained.

Because it can't use tools, we didn't build TL2 — speed on a model that can't drive a tool
isn't worth the conversion cost. Measuring `i2_s` first (cheap) was the point: it gated the
expensive TL2 work behind a signal that never came.

## Verdict

Shelved. It runs and it's fast enough, but this model class (general, non-tool-trained, 2B
ternary) and this runtime (old fork, no tool API) aren't ready to be an agent. Two things
would change that: a tool-trained BitNet (the format/parsing side is solved), or a CPU with
AVX-512/VNNI (then TL2 is worth building).

## Reproduce

```sh
# build bitnet.cpp (Linux; Windows needs Ninja+clang-cl via vcvars — see above)
git clone --recursive https://github.com/microsoft/BitNet.git && cd BitNet
sed -i 's/int8_t \* y_col/const int8_t * y_col/' src/ggml-bitnet-mad.cpp
sed -i 's|#include "log.h"|#include "log.h"\n#include <chrono>|' 3rdparty/llama.cpp/common/log.cpp
python3 utils/codegen_tl2.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 96,96,96 --bm 32,32,32
cmake -B build -DBITNET_X86_TL2=OFF -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++ -DCMAKE_BUILD_TYPE=Release
cmake --build build --target llama-server -j

hf download microsoft/bitnet-b1.58-2B-4T-gguf ggml-model-i2_s.gguf --local-dir .
./build/bin/llama-server -m ggml-model-i2_s.gguf --ctx-size 4096 -ngl 0 --port 8080
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_PROMPT_TOOLS=1 cargo run -- .
```

Hardware: Ryzen 5 5500U (Zen 2, 6c/12t, AVX2, no AVX-512), Fedora 44; an Intel AVX2 laptop,
Windows 11. Both CPU-only.
