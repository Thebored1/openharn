"""Can a grammar-locked micro-pass count sub-tasks better than the model already does?

This probes the biggest residual failure class on BFCL `parallel_multiple` (dropped
sub-tasks) WITHOUT touching openharn: it asks the model only "how many separate tool calls
does this need?" and scores against `len(ground_truth)`.

Four variants, and the comparison between them is the point (see notes/bfcl-v4.md):

  grammar@0   grammar [1-9] from token 0        -> collapses to the PRIOR (answers "1" x40)
  free        no grammar, reason freely         -> ~95% precise but ~half unparseable
  list        no grammar, list the calls        -> same form problem
  two-pass    reason freely, THEN constrain     -> 40/40 parsed, best accuracy

Two baselines it must clear, and the second is brutal:
  - the model's own implicit count in the winning config (~80%)
  - a constant "2" (~82.5%: the category is 33 twos / 3 threes / 4 fours, no ones)

Needs a llama-server on :8080 with --jinja. Usage:
    python tests/bfcl/decompose_probe.py <bfcl_eval_dir> [variant]
"""
import json
import re
import sys
import time
import urllib.request

SP = sys.argv[1]
ONLY = sys.argv[2] if len(sys.argv) > 2 else None
ROOT = "http://127.0.0.1:8080"


def load(f):
    return [json.loads(l) for l in open(f, encoding="utf-8") if l.strip()]


data = {e["id"]: e for e in load(f"{SP}/data/BFCL_v4_parallel_multiple.json")}
ans = {a["id"]: a for a in load(f"{SP}/data/possible_answer/BFCL_v4_parallel_multiple.json")}
ids = [e["id"] for e in load(f"{SP}/data/BFCL_v4_parallel_multiple.json")][:40]


def post(path, body, timeout=300):
    req = urllib.request.Request(ROOT + path, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))


def tools_txt(e):
    out = []
    for f in e["function"]:
        props = (f.get("parameters") or {}).get("properties") or {}
        out.append(f"- {f['name'].replace('.','_')}({', '.join(props)}): "
                   f"{f.get('description','')[:110]}")
    return "\n".join(out)


BASE = ("Count how many separate tool/function calls are required to fully satisfy the "
        "user's request. Each independent sub-task needs its own call; a tool that returns "
        "only one property per call needs one call per property.\n"
        "Available tools:\n{t}\n")
INSTR_DIGIT = BASE + "Reply with a single digit and nothing else."
INSTR_COUNT = BASE + "Think it through, then end with the final count as: COUNT=<digit>"
INSTR_LIST = BASE + "List every required call, one per line, as: CALL: <tool_name>"


def score(name, preds, secs):
    ok = sum(1 for t, p in preds.items() if p == len(ans[t]["ground_truth"]))
    hard = [t for t in ids if len(ans[t]["ground_truth"]) != 2]
    hard_ok = sum(1 for t in hard if preds[t] == len(ans[t]["ground_truth"]))
    parsed = sum(1 for p in preds.values() if p != -1)
    prec = 100.0 * ok / parsed if parsed else 0.0
    dist = {}
    for p in preds.values():
        dist[p] = dist.get(p, 0) + 1
    print(f"{name:12s} exact {ok:2d}/40 = {100*ok/40:5.1f}%   needs>=3: {hard_ok}/{len(hard)}   "
          f"parsed {parsed:2d}/40   precision {prec:5.1f}%   {secs:4.0f}s  dist {dict(sorted(dist.items()))}")


def variant(name):
    preds = {}
    t0 = time.time()
    for tid in ids:
        e = data[tid]
        q = e["question"][0][0]["content"]
        p = -1
        try:
            if name == "grammar@0":
                r = post("/v1/chat/completions", {
                    "model": "x", "temperature": 0.0, "max_tokens": 4, "grammar": "root ::= [1-9]\n",
                    "chat_template_kwargs": {"enable_thinking": False},
                    "messages": [{"role": "system", "content": INSTR_DIGIT.format(t=tools_txt(e))},
                                 {"role": "user", "content": q}]})
                c = (r["choices"][0]["message"].get("content") or "").strip()
                p = int(c[0]) if c[:1].isdigit() else -1
            elif name in ("free", "list"):
                instr = INSTR_COUNT if name == "free" else INSTR_LIST
                r = post("/v1/chat/completions", {
                    "model": "x", "temperature": 0.0, "max_tokens": 1200 if name == "free" else 300,
                    "messages": [{"role": "system", "content": instr.format(t=tools_txt(e))},
                                 {"role": "user", "content": q}]})
                c = r["choices"][0]["message"].get("content") or ""
                if name == "free":
                    m = re.findall(r"COUNT\s*=\s*(\d+)", c)
                    p = int(m[-1]) if m else -1
                else:
                    p = len(re.findall(r"CALL\s*:", c)) or -1
            elif name == "two-pass":
                # draft-then-constrain: free reasoning, then grammar-force ONLY the digit
                msgs = [{"role": "system", "content": INSTR_COUNT.format(t=tools_txt(e))},
                        {"role": "user", "content": q}]
                prompt = post("/apply-template", {"messages": msgs})["prompt"]
                m = re.search(r"<([A-Za-z_]+)>\s*$", prompt)
                if m:
                    tag = m.group(1)
                    r1 = post("/completion", {"prompt": prompt, "stop": [f"</{tag}>"],
                                              "n_predict": 700, "temperature": 0.0})
                    prompt += r1.get("content", "") + f"</{tag}>\n"
                r2 = post("/completion", {"prompt": prompt + "COUNT=",
                                          "grammar": "root ::= [1-9]\n",
                                          "n_predict": 2, "temperature": 0.0})
                c = (r2.get("content") or "").strip()
                p = int(c[0]) if c[:1].isdigit() else -1
        except Exception:
            p = -1
        preds[tid] = p
    score(name, preds, time.time() - t0)


gt_dist = {}
for t in ids:
    n = len(ans[t]["ground_truth"])
    gt_dist[n] = gt_dist.get(n, 0) + 1
const = max(gt_dist.values())
print(f"ground-truth call-count distribution: {dict(sorted(gt_dist.items()))}")
print(f"{'always-2':12s} exact {const}/40 = {100*const/40:5.1f}%   needs>=3: 0/{40-const}   (constant baseline)")
print("-" * 108)
for v in (["grammar@0", "free", "list", "two-pass"] if not ONLY else [ONLY]):
    variant(v)
