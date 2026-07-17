"""Close the loop: does feeding a decomposer's predicted count into the generation FIX
dropped sub-tasks, or just yield filler calls?

MEASURED ANSWER: it hurts. control 57.5% -> +count 37.5% (-20 pts). A confidently-wrong N
forces the model to fabricate filler calls, and the counting pass is too prompt-brittle to
trust (57.5-85% on cosmetic wording). See notes/bfcl-v4.md.

Arm A (control): native tools + tool_choice=required + no-think
Arm B (loop)   : same, plus a draft-then-constrain counting pass whose N is injected as
                 "emit exactly N calls".

Caveat: this re-implements the tool conversion more crudely than BFCL's convert_to_tool, so
the control reads 57.5% rather than the 72.5% headline. Both arms share the pipeline, so the
DELTA is valid; the absolute is not.

Usage: python tests/bfcl/decompose_closeloop.py <bfcl_eval_dir> <out_dir> <control|loop>
then:  BFCL_PROJECT_ROOT=<out_dir> bfcl evaluate --model openharn-minicpm-harness            --test-category parallel_multiple --partial-eval
"""
import json, os, re, sys, time, urllib.request

SP = sys.argv[1]
OUT = sys.argv[2]          # BFCL_PROJECT_ROOT-style dir to write results into
ARM = sys.argv[3]          # "control" | "loop"
ROOT = "http://127.0.0.1:8080"

def load(f):
    return [json.loads(l) for l in open(f, encoding="utf-8") if l.strip()]

entries = load(f"{SP}/data/BFCL_v4_parallel_multiple.json")[:40]

TYPE_MAP = {"float": "number", "dict": "object", "tuple": "array", "any": "string"}
def sanitize(s):
    if isinstance(s, dict):
        return {k: (TYPE_MAP.get(v, v) if k == "type" and isinstance(v, str) else sanitize(v))
                for k, v in s.items()}
    if isinstance(s, list):
        return [sanitize(x) for x in s]
    return s

def post(path, body, timeout=300):
    req = urllib.request.Request(ROOT + path, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))

def tools_txt(e):
    out = []
    for f in e["function"]:
        props = (f.get("parameters") or {}).get("properties") or {}
        out.append(f"- {f['name'].replace('.','_')}({', '.join(props)}): {f.get('description','')[:110]}")
    return "\n".join(out)

COUNT_INSTR = ("Count how many separate tool/function calls are required to fully satisfy "
               "the user's request. Each independent sub-task needs its own call; a tool "
               "that returns only one property per call needs one call per property.\n"
               "Available tools:\n{t}\nThink it through, then end with: COUNT=<digit>")

def decompose_count(e):
    """draft-then-constrain counting micro-pass -> int or -1"""
    msgs = [{"role": "system", "content": COUNT_INSTR.format(t=tools_txt(e))},
            {"role": "user", "content": e["question"][0][0]["content"]}]
    try:
        prompt = post("/apply-template", {"messages": msgs})["prompt"]
        m = re.search(r"<([A-Za-z_]+)>\s*$", prompt)
        if m:
            tag = m.group(1)
            r1 = post("/completion", {"prompt": prompt, "stop": [f"</{tag}>"],
                                      "n_predict": 700, "temperature": 0.0})
            prompt += r1.get("content", "") + f"</{tag}>\n"
        r2 = post("/completion", {"prompt": prompt + "COUNT=", "grammar": "root ::= [1-9]\n",
                                  "n_predict": 2, "temperature": 0.0})
        c = (r2.get("content") or "").strip()
        return int(c[0]) if c[:1].isdigit() else -1
    except Exception:
        return -1

def generate(e, n):
    """winning config; if n>0 inject the count nudge"""
    tools = [{"type": "function", "function": sanitize({**f, "name": f["name"].replace(".", "_")})}
             for f in e["function"]]
    msgs = []
    if n > 0:
        msgs.append({"role": "system", "content":
                     f"This request requires exactly {n} separate tool call(s). "
                     f"Emit all {n} — one per sub-task. Do not stop after the first."})
    msgs.append({"role": "user", "content": e["question"][0][0]["content"]})
    body = {"model": "x", "temperature": 0.001, "max_tokens": 1024, "messages": msgs,
            "tools": tools, "tool_choice": "required",
            "chat_template_kwargs": {"enable_thinking": False}}
    try:
        m = post("/v1/chat/completions", body)["choices"][0]["message"]
        return [{c["function"]["name"]: c["function"]["arguments"]}
                for c in (m.get("tool_calls") or [])]
    except Exception:
        return ""

d = os.path.join(OUT, "result", "openharn-minicpm-harness", "non_live")
os.makedirs(d, exist_ok=True)
t0 = time.time()
counts = {}
with open(os.path.join(d, "BFCL_v4_parallel_multiple_result.json"), "w", encoding="utf-8") as fh:
    for e in entries:
        n = decompose_count(e) if ARM == "loop" else 0
        counts[e["id"]] = n
        res = generate(e, n)
        fh.write(json.dumps({"id": e["id"], "result": res}) + "\n")
print(f"{ARM}: wrote 40 results in {time.time()-t0:.0f}s")
if ARM == "loop":
    print("injected counts:", dict(sorted({k: v for k, v in counts.items()}.items()))
          if len(counts) < 5 else
          "dist " + str({v: list(counts.values()).count(v) for v in sorted(set(counts.values()))}))
