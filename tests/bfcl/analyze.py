"""Summarize BFCL score files for a model: per-category accuracy + failure-type
breakdown + sample failures. Reads $BFCL_PROJECT_ROOT/score/<model>/.

    python tests/bfcl/analyze.py <model> [nsamples]
"""
import collections
import glob
import json
import os
import sys

root = os.environ.get("BFCL_PROJECT_ROOT", ".")
model = sys.argv[1]
nsamp = int(sys.argv[2]) if len(sys.argv) > 2 else 4

files = sorted(glob.glob(os.path.join(root, "score", model, "**", "*_score.json"), recursive=True))
if not files:
    print("no score files for", model); sys.exit(0)

overall_c = overall_t = 0
for f in files:
    cat = os.path.basename(f).replace("BFCL_v4_", "").replace("_score.json", "")
    rows = [json.loads(l) for l in open(f, encoding="utf-8") if l.strip()]
    if not rows:
        continue
    summ = rows[0]
    c, t = summ.get("correct_count", 0), summ.get("total_count", 0)
    overall_c += c; overall_t += t
    fails = rows[1:]
    types = collections.Counter(r.get("error_type", "?") for r in fails)
    print(f"\n== {cat}: {c}/{t} = {100*c/max(t,1):.1f}%  ({len(fails)} fails)")
    for et, n in types.most_common():
        print(f"    {n:3d}  {et}")
    for r in fails[:nsamp]:
        mr = r.get("model_result_decoded") or r.get("model_result")
        print(f"    - {r.get('id')}: {json.dumps(mr)[:120]}")
        print(f"        err: {json.dumps(r.get('error'))[:180]}")
print(f"\n=== OVERALL {model}: {overall_c}/{overall_t} = {100*overall_c/max(overall_t,1):.1f}% ===")
