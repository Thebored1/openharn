"""Register openharn's BFCL experiment models into an installed `bfcl-eval`.

BFCL scores a model by name; to compare the raw llama-server against the openharn
harness on the *same* dataset + AST checker, we register two OpenAI-compatible FC
models. Both use `OpenAICompletionsHandler`; the endpoint is chosen at run time via
`OPENAI_BASE_URL`, so the same handler hits either the raw llama-server (baseline) or
the openharn FC-proxy (harness). `underscore_to_dot=True` is REQUIRED: BFCL sanitizes
dotted function names (e.g. `math.factorial`) to `math_factorial` for the OpenAI FC
schema, so the checker must map the model's underscored output back to dots — without
it, correct calls are scored as `wrong_func_name`.

Idempotent. Run once against the venv that has bfcl-eval:
    python tests/bfcl/register_models.py
"""
import re
import sys
from pathlib import Path

import bfcl_eval
from bfcl_eval.constants import model_config as mc

CFG = Path(mc.__file__)

ENTRIES = '''api_inference_model_map = {
    # ---- openharn BFCL experiment (added locally by tests/bfcl/register_models.py) --
    # Same OpenAI-compatible FC handler for both; the endpoint is chosen at run time
    # via OPENAI_BASE_URL. "raw" -> llama-server directly; "harness" -> openharn's
    # FC-proxy (OPENHARN_FC_PROXY=1). model_name is the wire "model" field.
    "openharn-lfm2-raw": ModelConfig(
        model_name="local",
        display_name="LFM2-8B-A1B Q2_K_XL - raw native-FC (llama.cpp)",
        url="https://huggingface.co/LiquidAI/LFM2-8B-A1B",
        org="LiquidAI",
        license="LFM-Open",
        model_handler=OpenAICompletionsHandler,
        input_price=None,
        output_price=None,
        is_fc_model=True,
        underscore_to_dot=True,
    ),
    "openharn-lfm2-harness": ModelConfig(
        model_name="local",
        display_name="LFM2-8B-A1B Q2_K_XL - openharn harness FC-proxy",
        url="https://github.com/openharn/openharn",
        org="openharn",
        license="MIT",
        model_handler=OpenAICompletionsHandler,
        input_price=None,
        output_price=None,
        is_fc_model=True,
        underscore_to_dot=True,
    ),
'''


def main():
    text = CFG.read_text(encoding="utf-8")
    if "openharn-lfm2-raw" in text:
        print("already registered:", CFG)
        return
    marker = "api_inference_model_map = {"
    if marker not in text:
        sys.exit(f"could not find '{marker}' in {CFG}")
    text = text.replace(marker, ENTRIES, 1)
    CFG.write_text(text, encoding="utf-8")
    # sanity: re-import in a fresh process is the real check, but confirm the keys parse
    print("registered openharn-lfm2-raw / openharn-lfm2-harness into", CFG)


if __name__ == "__main__":
    main()
