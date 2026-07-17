"""Enumeration probe: does asking the model to LIST the required calls (and counting the
lines ourselves) beat asking it to assert a count?

Motivation (notes/bfcl-v4.md): the counting micro-pass was unreliable because it threw the
reasoning away and resampled a bare digit — a number is an assertion with nothing to check.
Enumeration removes the assertion: the model names the calls it can see; the HARNESS counts
`len(lines)`. Counting becomes arithmetic we do, not a guess it makes. This is how
LLMCompiler/TinyAgent actually work — they never ask "how many", they ask for a plan.

Procedure (draft-then-constrain, the pattern that survived every replication):
  1. render the model's own template; let it reason UNCONSTRAINED (stop </think>).
  2. generate the list, one `CALL: <tool>` per line. The tool name is grammar-locked to the
     REAL names (can't hallucinate); the NUMBER of lines is free.
  3. predicted count = number of CALL: lines.

Reference points (NO dumb constants): the model's own implicit count in the winning config
(~80%), and the free-text list precision (~83%). Two instruction wordings, because single
runs have repeatedly lied — if enumeration is real, both agree.

Needs llama-server on :8080 with --jinja. Usage:
    python tests/bfcl/enumerate_probe.py <bfcl_eval_dir>
"""
import json
import re
import sys
import time
import urllib.request

SP = sys.argv[1]
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


def names(e):
    return [f["name"].replace(".", "_") for f in e["function"]]


def tools_txt(e):
    out = []
    for f in e["function"]:
        props = (f.get("parameters") or {}).get("properties") or {}
        out.append(f"- {f['name'].replace('.','_')}({', '.join(props)}): {f.get('description','')[:110]}")
    return "\n".join(out)


def lit(s):
    return '"%s"' % s.replace('\\', '\\\\').replace('"', '\\"')


def list_grammar(e):
    """One `CALL: <known tool>` per line, 1..9 lines. Names locked; line-count free."""
    toolalt = " | ".join(lit(n) for n in names(e))
    return (
        'root ::= line ( nl line )* nl?\n'
        f'line ::= "CALL: " ( {toolalt} )\n'
        'nl ::= "\\n"\n'
    )


BASE = ("List every separate tool/function call required to fully satisfy the user's "
        "request — one per line, in the form `CALL: <tool_name>`. Each independent sub-task "
        "needs its own call; a tool that returns only one property per call needs one call "
        "per that property.\nAvailable tools:\n{t}")
# Two NEUTRAL paraphrases — a robustness check, not a distribution hint. Neither tells the
# model how many calls to expect (that would leak this dataset's mostly-2 shape). If
# enumeration is real, both wordings agree; if it's prompt-pattern-matching like the counting
# pass, they diverge.
WORDINGS = {
    "plain": BASE,
    "paraphrase": ("Break the user's request into the individual tool calls needed to "
                   "carry it out, and write each one on its own line as `CALL: <tool_name>`. "
                   "A tool that returns only one property per call needs one call per "
                   "property.\nAvailable tools:\n{t}"),
}


def run(label, instr, grammar_locked):
    preds = {}
    t0 = time.time()
    for tid in ids:
        e = data[tid]
        msgs = [{"role": "system", "content": instr.format(t=tools_txt(e))},
                {"role": "user", "content": e["question"][0][0]["content"]}]
        p = -1
        try:
            prompt = post("/apply-template", {"messages": msgs})["prompt"]
            m = re.search(r"<([A-Za-z_]+)>\s*$", prompt)
            if m:  # unconstrained reasoning first
                tag = m.group(1)
                r1 = post("/completion", {"prompt": prompt, "stop": [f"</{tag}>"],
                                          "n_predict": 700, "temperature": 0.0})
                prompt += r1.get("content", "") + f"</{tag}>\n"
            body = {"prompt": prompt, "n_predict": 200, "temperature": 0.0}
            if grammar_locked:
                body["grammar"] = list_grammar(e)
            c = post("/completion", body).get("content", "")
            lines = [ln for ln in c.splitlines() if ln.strip().upper().startswith("CALL:")]
            p = len(lines) if lines else -1
        except Exception:
            p = -1
        preds[tid] = p
    ok = sum(1 for t, p in preds.items() if p == len(ans[t]["ground_truth"]))
    hard = [t for t in ids if len(ans[t]["ground_truth"]) != 2]
    hard_ok = sum(1 for t in hard if preds[t] == len(ans[t]["ground_truth"]))
    parsed = sum(1 for p in preds.values() if p != -1)
    prec = 100.0 * ok / parsed if parsed else 0.0
    dist = {}
    for p in preds.values():
        dist[p] = dist.get(p, 0) + 1
    print(f"{label:22s} exact {ok:2d}/40 = {100*ok/40:5.1f}%   needs>=3: {hard_ok}/{len(hard)}   "
          f"parsed {parsed:2d}/40   precision {prec:5.1f}%   {time.time()-t0:4.0f}s  "
          f"dist {dict(sorted(dist.items()))}")


gt = {}
for t in ids:
    n = len(ans[t]["ground_truth"])
    gt[n] = gt.get(n, 0) + 1
print(f"ground-truth call-count distribution: {dict(sorted(gt.items()))}")
print("reference points: model implicit count 80.0% | free-text-list precision ~83%")
print("-" * 112)
for label, instr in WORDINGS.items():
    run(f"enumerate/{label}", instr, grammar_locked=True)
