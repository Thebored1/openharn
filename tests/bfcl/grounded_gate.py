"""Iterative grounded gate on BFCL parallel_multiple.

HYPOTHESIS: the residual failure is dropped sub-tasks (wrong_count). Instead of asking the
model to ASSERT a count (which was unstable), start from its implicit multi-call, then run a
grounded completeness GATE (MiniCheck-style: document=request, claim="these calls satisfy
it", binary). If NO, ask for the one missing call and loop. The gate can only RECOVER a
dropped sub-task, never disrupt a working case. Count emerges from grounded binary checks.

Checker is pluggable:  ownq4  = same Q4 model, grammar-locked YES/NO
                       minicheck = MiniCheck-FT5 entailment (separate script feeds it)

DEVIATIONS from the openharn thesis, on purpose, because this is hypothesis-checking not a
benchmark: runs on GPU (6 full-offload MiniCPM-Q4 instances, ports 8080-8085), one model,
cache_prompt=false (no KV cache reuse). Noted in notes/bfcl-v4.md.

Usage: iterative_gate.py <bfcl_eval_dir> <out_dir> <ownq4|none>
  'none' = no gate (implicit-only control, should reproduce ~72.5% BFCL / 80% count)
"""
import json, os, re, sys, threading, time, urllib.request
from concurrent.futures import ThreadPoolExecutor

SP, OUT, CHECKER = sys.argv[1], sys.argv[2], sys.argv[3]
PORTS = [8080, 8081, 8082, 8083, 8084, 8085]
MAX_EXTRA = 3

def load(f):
    return [json.loads(l) for l in open(f, encoding="utf-8") if l.strip()]

entries = load(f"{SP}/data/BFCL_v4_parallel_multiple.json")[:40]
ans = {a["id"]: a for a in load(f"{SP}/data/possible_answer/BFCL_v4_parallel_multiple.json")}

TYPE_MAP = {"float": "number", "dict": "object", "tuple": "array", "any": "string"}
def sanitize(s):
    if isinstance(s, dict):
        return {k: (TYPE_MAP.get(v, v) if k == "type" and isinstance(v, str) else sanitize(v))
                for k, v in s.items()}
    if isinstance(s, list):
        return [sanitize(x) for x in s]
    return s

def post(port, path, body, timeout=120):
    body = {**body, "cache_prompt": False}       # NO cache, per instruction
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))

def tools_of(e):
    return [{"type": "function", "function": sanitize({**f, "name": f["name"].replace(".", "_")})}
            for f in e["function"]]

def render(calls):
    return "\n".join(f"{i+1}. {c['name']}({c['args']})" for i, c in enumerate(calls)) or "(none yet)"

def chat(port, msgs, tools, choice="required", nothink=True, maxtok=512):
    body = {"model": "x", "temperature": 0.001, "max_tokens": maxtok, "messages": msgs,
            "tools": tools, "tool_choice": choice}
    if nothink:
        body["chat_template_kwargs"] = {"enable_thinking": False}
    m = post(port, "/v1/chat/completions", body)["choices"][0]["message"]
    return [{"name": c["function"]["name"], "args": c["function"]["arguments"]}
            for c in (m.get("tool_calls") or [])]

def gate_ownq4(port, request, calls):
    """grounded completeness check -> True if request fully covered. YES/NO, no-think."""
    sysm = ("You verify tool-call plans. Given a user request and the list of tool calls made "
            "so far, decide whether EVERY part of the request is now covered by a call. If any "
            "sub-task in the request has no corresponding call yet, it is NOT complete.\n"
            "Reply with exactly YES (fully covered) or NO (something is still missing).")
    usr = f"Request:\n{request}\n\nCalls made so far:\n{render(calls)}\n\nIs every part of the request covered?"
    body = {"model": "x", "temperature": 0.0, "max_tokens": 4,
            "grammar": 'root ::= "YES" | "NO"\n', "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "system", "content": sysm}, {"role": "user", "content": usr}]}
    c = post(port, "/v1/chat/completions", body)["choices"][0]["message"].get("content") or ""
    return c.strip().upper().startswith("YES")

def gen_missing(port, request, tools, calls):
    """ask for the ONE tool call still needed that isn't already listed."""
    sysm = ("The user request and the tool calls already made are listed. Emit the ONE "
            "additional tool call required for a part of the request that is NOT yet covered "
            "by an existing call. Do not repeat a call already made.")
    usr = f"Request:\n{request}\n\nAlready made:\n{render(calls)}"
    got = chat(port, [{"role": "system", "content": sysm}, {"role": "user", "content": usr}],
               tools, choice="required")
    made = {(c["name"], c["args"]) for c in calls}
    for c in got:
        if (c["name"], c["args"]) not in made:
            return c
    return None

def process(e, port):
    request = e["question"][0][0]["content"]
    tools = tools_of(e)
    # 1. implicit multi-call (the strong baseline)
    calls = chat(port, [{"role": "user", "content": request}], tools, choice="required")
    gate_log = []
    if CHECKER != "none":
        for _ in range(MAX_EXTRA):
            done = gate_ownq4(port, request, calls)
            gate_log.append(done)
            if done:
                break
            nxt = gen_missing(port, request, tools, calls)
            if nxt is None:
                break
            calls.append(nxt)
    return {"id": e["id"],
            "result": [{c["name"]: c["args"]} for c in calls],
            "n": len(calls), "gt": len(ans[e["id"]]["ground_truth"]), "gate": gate_log}

# distribute 40 entries across the 6 GPU instances
results = [None] * len(entries)
lock = threading.Lock()
def worker(idx):
    port = PORTS[idx % len(PORTS)]
    try:
        results[idx] = process(entries[idx], port)
    except Exception as ex:
        results[idx] = {"id": entries[idx]["id"], "result": "", "n": -1,
                        "gt": len(ans[entries[idx]["id"]]["ground_truth"]), "gate": [f"ERR {ex}"]}

t0 = time.time()
with ThreadPoolExecutor(max_workers=len(PORTS)) as ex:
    list(ex.map(worker, range(len(entries))))
dt = time.time() - t0

# write BFCL result file
d = os.path.join(OUT, "result", "openharn-minicpm-harness", "non_live")
os.makedirs(d, exist_ok=True)
with open(os.path.join(d, "BFCL_v4_parallel_multiple_result.json"), "w", encoding="utf-8") as fh:
    for r in results:
        fh.write(json.dumps({"id": r["id"], "result": r["result"]}) + "\n")

count_ok = sum(1 for r in results if r["n"] == r["gt"])
print(f"checker={CHECKER}  {dt:.0f}s  count-accuracy {count_ok}/40 = {100*count_ok/40:.1f}%")
dist = {}
for r in results:
    dist[r["n"]] = dist.get(r["n"], 0) + 1
print(f"  predicted-count dist {dict(sorted(dist.items()))}   (ground truth {{2:33,3:3,4:4}})")
json.dump([{"id": r["id"], "n": r["n"], "gt": r["gt"], "gate": r["gate"]} for r in results],
          open(os.path.join(OUT, "trace.json"), "w"), indent=2)
