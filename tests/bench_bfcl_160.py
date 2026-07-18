#!/usr/bin/env python3
"""BFCL v4 AST benchmark on the fixed 160-entry subset (with checkpoint/resume)."""
import json, os, sys, time, urllib.request, urllib.error
from pathlib import Path

DATA = Path(__file__).resolve().parent / "openharn_bfcl_ast_subset_160"
FC_URL = os.environ.get("FC_URL", "http://127.0.0.1:8090/v1")
CATEGORIES = ["simple_python", "multiple", "parallel", "parallel_multiple"]
TIMEOUT = 180
MAX_TOKENS = 2048
CHECKPOINT = DATA / "checkpoint.json"  # incremental save

def load_jsonl(path):
    out = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                d = json.loads(line); out[d["id"]] = d
    return out

def load_checkpoint():
    if CHECKPOINT.exists():
        with open(CHECKPOINT) as f:
            return json.load(f)
    return {"done_ids": [], "results": {}, "cat_totals": {}}

def save_checkpoint(cp):
    with open(CHECKPOINT, "w") as f:
        json.dump(cp, f)

def name_escape(n):
    """Sanitize dotted names to underscores (OpenAI FC schema requirement)."""
    return n.replace(".", "_")

def name_unescape(n):
    """Map back underscore to dot for AST matching (BFCL underscore_to_dot)."""
    # Try with dots first; if not found, keep original
    return n

def tool_from_func(func):
    params = func.get("parameters", {})
    props = {}
    for k, v in params.get("properties", {}).items():
        ptype = v.get("type", "string")
        js_type = {"dict": "object", "float": "number", "int": "integer", "str": "string"}.get(ptype, ptype)
        prop = {"type": js_type}
        if "enum" in v: prop["enum"] = v["enum"]
        props[k] = prop
    return {"type": "function", "function": {
        "name": name_escape(func["name"]), "description": func.get("description", ""),
        "parameters": {"type": "object", "properties": props, "required": params.get("required", [])}
    }}

def call_fc(messages, tools, retry=True):
    # temperature 0.0 matches the official BFCL evaluation (deterministic).
    body = json.dumps({"model":"local","messages":messages,"tools":tools,"temperature":0.0,"max_tokens":MAX_TOKENS}).encode()
    req = urllib.request.Request(f"{FC_URL}/chat/completions", data=body, headers={"Content-Type":"application/json"})
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            data = json.loads(r.read())
    except Exception as e:
        if retry:
            import time as _time_
            _time_.sleep(2)
            return call_fc(messages, tools, retry=False)
        return None, str(e)
    tc = data.get("choices",[{}])[0].get("message",{}).get("tool_calls",[])
    content = data.get("choices",[{}])[0].get("message",{}).get("content","")
    for t in tc:
        try: t["function"]["arguments"] = json.loads(t["function"]["arguments"])
        except: t["function"]["arguments"] = {}
    if not tc and retry:
        import time as _time_
        _time_.sleep(2)
        return call_fc(messages, tools, retry=False)
    return tc, content

import re as _re

def _standardize(s):
    """Exact match with BFCL's standardize_string (ast_checker.py:174)."""
    regex = r"[ \,\.\/\-\_\*\^]"
    return _re.sub(regex, "", s).lower().replace("'", '"')

def val_match(vals, actual):
    for ev in vals:
        if ev == "" and actual in (None, "", False): return True
        if isinstance(ev,bool) and isinstance(actual,bool) and ev==actual: return True
        if isinstance(ev,(int,float)) and isinstance(actual,(int,float)):
            if float(ev) == float(actual): return True
            continue
        if isinstance(ev,str) and isinstance(actual,str):
            if _standardize(ev) == _standardize(actual): return True
            continue
        if isinstance(actual,list) and isinstance(ev,list) and len(ev)==1:
            # BFCL wraps expected nested values in a list for possible_answer
            return val_match(ev, actual)
        if isinstance(ev, list):
            # BFCL's list_checker standardizes string elements
            std_ev = [_standardize(x) if isinstance(x,str) else x for x in ev]
            std_actual = [_standardize(x) if isinstance(x,str) else x for x in actual]
            if std_actual == std_ev: return True
            continue
        if ev == actual: return True
    return False

def evaluate(q, ground_truths, cat=None):
    messages = [{"role": m.get("role","user"), "content": m.get("content","")} for m in q["question"][0]]
    tools = [tool_from_func(f) for f in q["function"]]
    tc, content = call_fc(messages, tools)
    if tc is None:
        return {"score": 0.0, "reason": f"error: {content}", "actual_names": []}
    
    actual = [(t.get("function",{}).get("name",""), t.get("function",{}).get("arguments",{})) for t in tc]
    if not actual:
        return {"score": 0.0, "reason": f"no calls; content: {content[:80]}", "actual_names": []}
    if not ground_truths:
        return {"score": 0.0, "reason": "unexpected calls", "actual_names": [n for n,_ in actual]}

    # Build required-param sets per function name (for faithful required/optional checks).
    req_by_name = {}
    for f in q["function"]:
        req_by_name[f["name"].replace(".", "_")] = set(f.get("parameters", {}).get("required", []))

    # BFCL convert_func_name: expected names use dots; model output uses underscores
    # (OpenAI FC requirement). Convert expected names to underscored form.
    def exp_name(n):
        return n.replace(".", "_")
    norm_gt = []
    for gt in ground_truths:
        for gname, gargs in gt.items():
            norm_gt.append((exp_name(gname), gargs))

    # FAITHFUL to official BFCL: all-or-nothing per test case.
    # 1. Exact function count required (parallel_function_checker_no_order:wrong_count).
    if len(actual) != len(norm_gt):
        return {"score": 0.0,
                "reason": f"count {len(actual)} vs {len(norm_gt)}",
                "actual_names": [n for n,_ in actual]}

    def call_valid(gname, gargs, aname, aargs):
        # Function name must match.
        if aname != gname:
            return False
        req = req_by_name.get(gname, set())
        # All required params must be present in the model output.
        for p in req:
            if p not in aargs:
                return False
        # No unexpected params (params not in the possible-answer spec).
        for k in aargs:
            if k not in gargs:
                return False
        # Every provided param must match one of its allowed values.
        for k, avalue in aargs.items():
            if not val_match(gargs[k], avalue):
                return False
        # Any required-by-possible-answer param (no "" option) must be present.
        for k, vals in gargs.items():
            if k not in aargs and "" not in vals:
                return False
        return True

    # Faithful to official BFCL category checkers:
    #  - parallel / parallel_multiple: greedy no-order matching, ALL calls must match.
    #  - simple / multiple: official BFCL validates ONLY model_output[0] (the first call),
    #    after the exact-count gate. Extra calls beyond the first are ignored by the
    #    official checker, so we mirror that here.
    if cat in ("parallel", "parallel_multiple"):
        _used = set()
        matched = 0
        for gname, gargs in norm_gt:
            for ai, (aname, aargs) in enumerate(actual):
                if ai in _used:
                    continue
                if call_valid(gname, gargs, aname, aargs):
                    _used.add(ai)
                    matched += 1
                    break
        ok = (matched == len(norm_gt))
        reason = f"ok {matched}/{len(norm_gt)}" if ok else f"matched {matched}/{len(norm_gt)}"
    else:
        # simple / multiple: count already matched; validate only the first actual call
        # against the (single) expected call.
        gname, gargs = norm_gt[0]
        ok = call_valid(gname, gargs, actual[0][0], actual[0][1])
        reason = "ok 1/1" if ok else f"matched 0/1"
    score = 1.0 if ok else 0.0
    return {"score": score, "reason": reason, "actual_names": [n for n,_ in actual]}

def main():
    global FC_URL
    if "--url" in sys.argv:
        FC_URL = sys.argv[sys.argv.index("--url")+1]
    print(f"FC_URL={FC_URL}", flush=True)

    try:
        with urllib.request.urlopen("http://127.0.0.1:8090/health", timeout=5) as r:
            if json.loads(r.read()).get("status")!="ok": print("server bad"); sys.exit(1)
    except Exception as e:
        print(f"server unreachable: {e}"); sys.exit(1)

    cp = load_checkpoint()
    done = set(cp["done_ids"])
    all_flat = []

    for cat in CATEGORIES:
        qs = load_jsonl(QUESTIONS := DATA/"questions"/f"BFCL_v4_{cat}.json")
        ans = load_jsonl(ANSWERS := DATA/"possible_answer"/f"BFCL_v4_{cat}.json")
        for qid, q in sorted(qs.items()):
            gt = ans.get(qid,{}).get("ground_truth",[]) if qid in ans else []
            all_flat.append((qid, cat, q, gt))

    total = len(all_flat)
    done_count = len([x for x in all_flat if x[0] in done])
    print(f"Total: {total}, Already done: {done_count}, Remaining: {total-done_count}", flush=True)
    t0 = time.time()

    for idx, (qid, cat, q, gt) in enumerate(all_flat):
        if qid in done:
            continue
        try:
            res = evaluate(q, gt, cat)
        except Exception as e:
            res = {"score": 0.0, "reason": f"crash: {e}", "actual_names": []}
        cp["done_ids"].append(qid)
        cp["results"][qid] = res
        cat_t = cp["cat_totals"]
        cat_t[cat] = cat_t.get(cat, {"num":0,"den":0})
        cat_t[cat]["num"] += res["score"]
        cat_t[cat]["den"] += 1
        cp["last_update"] = time.time()
        save_checkpoint(cp)

        elapsed = time.time() - t0
        rate = (idx+1-done_count) / max(elapsed, 1)
        rem = (total - idx - 1) / max(rate, 0.001)
        status = "✓" if res["score"] >= 1.0 else ("~" if res["score"] >= 0.5 else "✗")
        print(f"  [{idx+1-done_count}/{total-done_count}] {status} {qid:35s} score={res['score']:.2f}  [{rem:.0f}s left]", flush=True)

    # Final output
    print(f"\n{'='*60}", flush=True)
    overall_num = overall_den = 0
    for cat in CATEGORIES:
        ct = cp["cat_totals"].get(cat, {"num":0,"den":0})
        pct = (ct["num"]/ct["den"]*100) if ct["den"] else 0
        print(f"  {cat:25s}: {pct:.1f}% ({ct['num']:.0f}/{ct['den']})", flush=True)
        overall_num += ct["num"]; overall_den += ct["den"]
    overall = (overall_num/overall_den*100) if overall_den else 0
    print(f"  {'OVERALL':25s}: {overall:.1f}% ({overall_num:.0f}/{overall_den})", flush=True)

    with open(DATA/"results.json","w") as f:
        json.dump({"overall_pct": overall, "per_category": {c: cp["cat_totals"].get(c) for c in CATEGORIES}}, f, indent=2)
    print(f"\nSaved: {DATA/'results.json'}", flush=True)
    CHECKPOINT.unlink(missing_ok=True)
    return 0 if overall >= 60 else 1

if __name__ == "__main__":
    sys.exit(main())
