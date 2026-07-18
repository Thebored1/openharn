"""Decomposed prompting (plan -> parse -> per-item generation) vs implicit, same GPU backend.

Arm IMPLICIT : one native+required+no-think generation (the baseline).
Arm DECOMPOSE: (1) unconstrained think-on generation asking for a NUMBERED LIST of the
               sub-tasks (no grammar, no tools); (2) regex-parse the list; (3) one focused
               native+required generation per item, given the FULL request as context plus
               "focus on this sub-task" (mitigates cross-item dependencies like pm_27's
               principal=5000); (4) if the list won't parse, fall back to the implicit call.
The harness counts the plan; the model never asserts a number.

Both arms run on the SAME 6 GPU instances -> the delta is within-backend and valid; GPU
Q4_0 depresses absolutes vs CPU, so don't compare these to the CPU 72.5%/80% figures.

Usage: plan_execute.py <bfcl_eval_dir> <out_dir_implicit> <out_dir_decompose>
"""
import json, os, re, sys, threading, time, urllib.request
from concurrent.futures import ThreadPoolExecutor

SP, OUT_I, OUT_D = sys.argv[1], sys.argv[2], sys.argv[3]
PORTS = [8080, 8081, 8082, 8083, 8084, 8085]

def load(f):
    return [json.loads(l) for l in open(f, encoding="utf-8") if l.strip()]

entries = load(f"{SP}/data/BFCL_v4_parallel_multiple.json")[:40]
ans = {a["id"]: a for a in load(f"{SP}/data/possible_answer/BFCL_v4_parallel_multiple.json")}

TYPE_MAP = {"float": "number", "dict": "object", "tuple": "array", "any": "string"}
def sanitize(s):
    if isinstance(s, dict):
        return {k: (TYPE_MAP.get(v, v) if k == "type" and isinstance(v, str) else sanitize(v)) for k, v in s.items()}
    if isinstance(s, list):
        return [sanitize(x) for x in s]
    return s

def post(port, path, body, timeout=120):
    body = {**body, "cache_prompt": False}
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))

def tools_of(e):
    return [{"type": "function", "function": sanitize({**f, "name": f["name"].replace(".", "_")})}
            for f in e["function"]]

def gen_calls(port, msgs, tools):
    body = {"model": "x", "temperature": 0.001, "max_tokens": 512, "messages": msgs,
            "tools": tools, "tool_choice": "required", "chat_template_kwargs": {"enable_thinking": False}}
    m = post(port, "/v1/chat/completions", body)["choices"][0]["message"]
    return [{"name": c["function"]["name"], "args": c["function"]["arguments"]}
            for c in (m.get("tool_calls") or [])]

PLAN = ("Break the user's request into a numbered list of the separate tool calls needed to "
        "fully satisfy it. Do NOT write the calls themselves — just list, one per line as "
        "'1. ...', what each call should accomplish. One line per independent sub-task.")

def plan_items(port, request):
    body = {"model": "x", "temperature": 0.001, "max_tokens": 1200,
            "messages": [{"role": "system", "content": PLAN}, {"role": "user", "content": request}]}
    m = post(port, "/v1/chat/completions", body)["choices"][0]["message"]
    text = m.get("content") or ""
    items = re.findall(r"(?m)^\s*\d+[.)]\s+(.+?)\s*$", text)
    return [it.strip() for it in items if it.strip()]

def per_item(port, request, item, tools):
    sysm = ("You are given a user request and ONE sub-task from it. Emit exactly the single "
            "tool call that performs this sub-task. Use the full request for any shared "
            "values (numbers, names) the sub-task needs.")
    usr = f"Full request:\n{request}\n\nSub-task to perform now:\n{item}"
    got = gen_calls(port, [{"role": "system", "content": sysm}, {"role": "user", "content": usr}], tools)
    return got[0] if got else None

def process(e, port):
    request = e["question"][0][0]["content"]
    tools = tools_of(e)
    implicit = gen_calls(port, [{"role": "user", "content": request}], tools)
    items = plan_items(port, request)
    parsed = len(items) > 0
    if parsed:
        calls = []
        seen = set()
        for it in items:
            c = per_item(port, request, it, tools)
            if c and (c["name"], c["args"]) not in seen:
                seen.add((c["name"], c["args"]))
                calls.append(c)
        if not calls:
            calls = implicit
            parsed = False
    else:
        calls = implicit
    return {"id": e["id"], "implicit": implicit, "decompose": calls,
            "n_items": len(items), "parsed": parsed, "gt": len(ans[e["id"]]["ground_truth"])}

results = [None] * len(entries)
def worker(idx):
    try:
        results[idx] = process(entries[idx], PORTS[idx % len(PORTS)])
    except Exception as ex:
        results[idx] = {"id": entries[idx]["id"], "implicit": "", "decompose": "",
                        "n_items": -1, "parsed": False, "gt": len(ans[entries[idx]["id"]]["ground_truth"]),
                        "err": str(ex)}

t0 = time.time()
with ThreadPoolExecutor(max_workers=len(PORTS)) as ex:
    list(ex.map(worker, range(len(entries))))
dt = time.time() - t0

for arm, out in [("implicit", OUT_I), ("decompose", OUT_D)]:
    d = os.path.join(out, "result", "openharn-minicpm-harness", "non_live")
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "BFCL_v4_parallel_multiple_result.json"), "w", encoding="utf-8") as fh:
        for r in results:
            calls = r[arm]
            res = [{c["name"]: c["args"]} for c in calls] if isinstance(calls, list) else ""
            fh.write(json.dumps({"id": r["id"], "result": res}) + "\n")

def count_acc(arm):
    return sum(1 for r in results if isinstance(r[arm], list) and len(r[arm]) == r["gt"])
parsed_n = sum(1 for r in results if r["parsed"])
print(f"{dt:.0f}s  |  planning parsed: {parsed_n}/40")
print(f"count-accuracy  implicit {count_acc('implicit')}/40   decompose {count_acc('decompose')}/40")
dist_i = {}; dist_d = {}
for r in results:
    for arm, dd in [("implicit", dist_i), ("decompose", dist_d)]:
        n = len(r[arm]) if isinstance(r[arm], list) else -1
        dd[n] = dd.get(n, 0) + 1
print(f"  implicit  dist {dict(sorted(dist_i.items()))}")
print(f"  decompose dist {dict(sorted(dist_d.items()))}   (gt {{2:33,3:3,4:4}})")
json.dump(results, open(os.path.join(OUT_D, "trace.json"), "w"), indent=2, default=str)
