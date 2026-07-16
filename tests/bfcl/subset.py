"""Write a BFCL `test_case_ids_to_generate.json` selecting the first N entries of each
category, so a run covers a fixed, reproducible subset (full BFCL v4 is too slow on a
CPU-only box). Writes to $BFCL_PROJECT_ROOT.

    python tests/bfcl/subset.py --n 40 \
        --categories simple_python multiple parallel parallel_multiple irrelevance
"""
import argparse
import json
import os
from pathlib import Path

import bfcl_eval

DATA = Path(bfcl_eval.__file__).parent / "data"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=40)
    ap.add_argument("--categories", nargs="+", required=True)
    ap.add_argument("--out", default=os.environ.get("BFCL_PROJECT_ROOT", "."))
    args = ap.parse_args()

    ids = {}
    for c in args.categories:
        f = DATA / f"BFCL_v4_{c}.json"
        entries = [json.loads(l) for l in f.read_text(encoding="utf-8").splitlines() if l.strip()]
        ids[c] = [e["id"] for e in entries[: args.n]]
        print(f"{c}: {len(entries)} total -> {len(ids[c])} selected")

    out = Path(args.out) / "test_case_ids_to_generate.json"
    out.write_text(json.dumps(ids), encoding="utf-8")
    print("wrote", out, "-", sum(len(v) for v in ids.values()), "entries")


if __name__ == "__main__":
    main()
