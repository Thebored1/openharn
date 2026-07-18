# openharn BFCL v4 AST subset (160 entries)

The exact fixed subset used to measure LFM2-8B-A1B-UD-Q2_K_XL through openharn on the
Berkeley Function Calling Leaderboard v4 AST categories. This is the subset behind the
45% -> ~72% AST result (winning config: OPENHARN_NATIVE_TEMPLATE + PLAN_FIRST + DEDUP_CALLS).
See notes/bfcl-v4.md ("The wall moved") and tests/bfcl/README.md in the openharn repo.

## Contents
- test_case_ids_to_generate.json  - the 160 ids (40 per category), the file BFCL's
                                     `--run-ids` reads. Drop into BFCL_PROJECT_ROOT.
- questions/BFCL_v4_<cat>.json     - the 160 test entries (JSONL), verbatim from bfcl-eval.
- possible_answer/BFCL_v4_<cat>.json - the ground-truth answers (JSONL) for those ids.
- manifest.json                    - counts per category.

## Categories (40 each)
simple_python, multiple, parallel, parallel_multiple   (the AST-scored single-turn set;
irrelevance is excluded because it scores abstention, not AST).

## Provenance
- Source: bfcl-eval 2026.3.23 (pip), official BFCL v4 datasets + AST checker (Patil et al., ICML 2025).
- Selection: first 40 ids of each category (tests/bfcl/subset.py --n 40).
- The questions/answers are copied UNMODIFIED from bfcl-eval; this bundle just filters to the
  160 ids so the exact subset is self-contained and reproducible without re-deriving it.

## Reproduce the run
Install bfcl-eval 2026.3.23, register the openharn models (tests/bfcl/register_models.py),
set BFCL_PROJECT_ROOT to a dir containing test_case_ids_to_generate.json, then use
tests/bfcl/run_arm.sh. Full commands in tests/bfcl/README.md.
