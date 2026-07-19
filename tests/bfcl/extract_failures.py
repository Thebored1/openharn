"""Extract failed / non-perfect cases from the 200-test BFCL run.

Mirrors bfcl_eval's own ast_file_runner exactly: for each case it runs
handler.decode_ast(result, ReturnFormat.PYTHON) then ast_checker(...), so the
pass/fail labels are identical to the leaderboard CSV. Writes failures.json:
every case that did NOT score perfect, with the question, the model's tool
call(s), the ground-truth possible answers, and the checker error.
"""
import json
from pathlib import Path

from bfcl_eval.constants.enums import Language, ReturnFormat
from bfcl_eval.eval_checker.ast_eval import ast_checker as ac
from bfcl_eval.eval_checker.eval_runner import get_handler

ROOT = Path("/home/paper/openharn/tests/bfcl/full200")
RES = ROOT / "result/openharn-lfm2-harness/non_live"
DATA = Path("/home/paper/.local/share/uv/python/cpython-3.11.15-linux-x86_64-gnu/lib/python3.11/site-packages/bfcl_eval/data")
PA = DATA / "possible_answer"

CATS = ["simple_python", "multiple", "parallel", "parallel_multiple", "irrelevance"]


def load_jsonl(p):
    return [json.loads(l) for l in p.read_text(encoding="utf-8").splitlines() if l.strip()]


def main():
    handler = get_handler("openharn-lfm2-harness")
    failures = []
    summary = {}
    for cat in CATS:
        qfile = DATA / f"BFCL_v4_{cat}.json"
        pafile = PA / f"BFCL_v4_{cat}.json"
        rfile = RES / f"BFCL_v4_{cat}_result.json"
        if not (qfile.exists() and rfile.exists()):
            print(f"skip {cat}: missing file")
            continue
        questions = {e["id"]: e for e in load_jsonl(qfile)}
        answers = {e["id"]: e for e in load_jsonl(pafile)} if pafile.exists() else {}
        results = {e["id"]: e for e in load_jsonl(rfile)}

        cat_pass = cat_total = 0
        for cid, res in results.items():
            q = questions.get(cid, {})
            pa = answers.get(cid, {})
            func_desc = q.get("function", [])
            possible = pa.get("possible_answer", pa.get("ground_truth", []))
            model_output = res.get("result", [])
            cat_total += 1

            # Decode the raw model string-output into structured AST (official path).
            try:
                decoded = handler.decode_ast(model_output, ReturnFormat.PYTHON, False)
            except Exception as e:
                failures.append({
                    "id": cid, "category": cat,
                    "question": q.get("question", [[]])[0][0]["content"] if q.get("question") else "",
                    "model_output": model_output, "expected": possible,
                    "error": f"decode_failed: {e}",
                })
                continue

            if cat == "irrelevance":
                success = len(decoded) == 0
                if success:
                    cat_pass += 1
                else:
                    failures.append({
                        "id": cid, "category": cat,
                        "question": q.get("question", [[]])[0][0]["content"] if q.get("question") else "",
                        "model_output": decoded, "expected": "(no tool call expected)",
                        "error": ["model emitted a tool call on an irrelevant request"],
                    })
                continue
            try:
                chk = ac.ast_checker(func_desc, decoded, possible, Language.PYTHON, cat, "openharn-lfm2-harness")
            except Exception as e:
                chk = {"valid": False, "error": [f"CHECKER_ERROR: {e}"]}
            if chk.get("valid"):
                cat_pass += 1
            else:
                failures.append({
                    "id": cid, "category": cat,
                    "question": q.get("question", [[]])[0][0]["content"] if q.get("question") else "",
                    "model_output": decoded, "expected": possible,
                    "error": chk.get("error", []),
                })
        summary[cat] = (cat_pass, cat_total)
        print(f"{cat}: {cat_pass}/{cat_total} passed, {cat_total-cat_pass} failed")

    out = ROOT / "failures.json"
    out.write_text(json.dumps(failures, indent=2, ensure_ascii=False), encoding="utf-8")
    total = sum(t for _, t in summary.values())
    print(f"\nTotal failures: {len(failures)} / {total}")
    print(f"Wrote {out}")
    print("Per-category:", {k: f"{p}/{t}" for k, (p, t) in summary.items()})


if __name__ == "__main__":
    main()
