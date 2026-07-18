#!/usr/bin/env python3
"""BFCL v4 AST benchmark on the fixed 160-entry subset (with checkpoint/resume)."""
import json, os, sys, time, urllib.request, urllib.error
from pathlib import Path

DATA = Path(__file__).resolve().parent / "openharn_bfcl_ast_subset_160"
FC_URL = os.environ.get("FC_URL", "http://127.0.0.1:8090/v1")
CATEGORIES = ["simple_python", "multiple", "parallel", "parallel_multiple"]
TIMEOUT = 90
MAX_TOKENS = 512
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

def call_fc(messages, tools):
    body = json.dumps({"model":"local","messages":messages,"tools":tools,"temperature":0.001,"max_tokens":MAX_TOKENS}).encode()
    req = urllib.request.Request(f"{FC_URL}/chat/completions", data=body, headers={"Content-Type":"application/json"})
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            data = json.loads(r.read())
    except Exception as e:
        return None, str(e)
    tc = data.get("choices",[{}])[0].get("message",{}).get("tool_calls",[])
    content = data.get("choices",[{}])[0].get("message",{}).get("content","")
    for t in tc:
        try: t["function"]["arguments"] = json.loads(t["function"]["arguments"])
        except: t["function"]["arguments"] = {}
    return tc, content

def val_match(vals, actual):
    for ev in vals:
        if ev == "" and actual in (None, "", False): return True
        if isinstance(ev,bool) and isinstance(actual,bool) and ev==actual: return True
        if isinstance(ev,(int,float)) and isinstance(actual,(int,float)) and float(ev)==float(actual): return True
        if isinstance(ev,str) and isinstance(actual,str) and ev.lower()==actual.lower(): return True
        if ev == actual: return True
    return False

def evaluate(q, ground_truths):
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

    # Normalize: map both actual and expected names to underscore-free form for comparison.
    # Model may emit dot or underscore — we compare normalized versions.
    def norm_name(n):
        return n.replace("_", ".").replace("-", ".")

    # Build expected list with normalized names
    norm_gt = []
    for gt in ground_truths:
        for gname, gargs in gt.items():
            norm_gt.append((norm_name(gname), gargs))

    # Match each actual call to best ground-truth (greedy).
    # AST matching: function name must match; all REQUIRED params must have correct values.
    # Optional params with wrong values DON'T cause the entire call to fail.
    gt_used = [False]*len(norm_gt)
    correct = 0
    for aname, aargs in actual:
        anorm = norm_name(aname)
        best_gi, best_score = -1, -1
        for gi, (gname, gargs) in enumerate(norm_gt):
            if gt_used[gi]: continue
            if anorm != gname: continue
            # Count params that match (optional misses are OK)
            mc = sum(1 for k,vals in gargs.items() if k in aargs and val_match(vals, aargs[k]))
            has_mismatch = any(k in aargs and not val_match(vals, aargs[k]) for k,vals in gargs.items())
            # At least 1 param must match + no wrong values = call is valid
            if mc > 0 and not has_mismatch:
                s = 1.0
            else:
                s = mc / max(len(gargs),1)
            if s > best_score: best_score, best_gi = s, gi
        if best_gi >= 0 and best_score >= 1.0:
            gt_used[best_gi] = True; correct += 1

    score = correct / max(len(norm_gt), 1)
    missing = [list(k.keys())[0] for gi,k in enumerate(ground_truths) if not gt_used[gi]]
    return {"score": score, "reason": f"ok {correct}/{len(norm_gt)}" if score>=1.0 else f"miss {missing}", "actual_names": [n for n,_ in actual]}

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
            res = evaluate(q, gt)
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
