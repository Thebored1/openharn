"""Completeness-classification benchmark (clean, no generation variance).

Fixed input: 40 base plans (from itg_none) + their true label = passes BFCL (COMPLETE) or
fails (INCOMPLETE). A good gate must flag the INCOMPLETE ones so the loop can recover them.

Runs a checker over the SAME fixed plans and reports the confusion matrix. Checkers:
  ownq4     : the Q4 model, grammar-locked YES/NO "is every part covered?"  (GPU pool)
  minicheck : MiniCheck-FT5 entailment: doc=request, claim="calls X and Y and ... were made"

Usage: classify.py <bfcl_eval_dir> <ownq4|minicheck>
"""
import json, glob, sys, urllib.request

SP, CHECKER = sys.argv[1], sys.argv[2]
PORTS = [8080, 8081, 8082, 8083, 8084, 8085]

def load(f):
    return [json.loads(l) for l in open(f, encoding="utf-8") if l.strip()]

data = {e["id"]: e for e in load(f"{SP}/data/BFCL_v4_parallel_multiple.json")}
base = {json.loads(l)["id"]: json.loads(l)["result"] for l in
        open(r"C:\Users\Paper\AppData\Local\Temp\itg_none\result\openharn-minicpm-harness\non_live\BFCL_v4_parallel_multiple_result.json", encoding="utf-8") if l.strip()}
sf = glob.glob(r"C:\Users\Paper\AppData\Local\Temp\itg_none\score\**\*parallel_multiple*_score.json", recursive=True)[0]
failed = {r["id"] for r in load(sf)[1:]}
# true label: True = COMPLETE (passes BFCL)
label = {tid: (tid not in failed) for tid in base}

def render_calls(plan):
    parts = []
    for call in plan:
        for name, args in call.items():
            parts.append(f"{name}({args})")
    return parts

def request_of(tid):
    return data[tid]["question"][0][0]["content"]

def check_ownq4(tid, plan):
    port = PORTS[hash(tid) % len(PORTS)]
    calls = "\n".join(f"{i+1}. {c}" for i, c in enumerate(render_calls(plan))) or "(none)"
    sysm = ("You verify tool-call plans. Given a user request and the tool calls made, decide "
            "whether EVERY part of the request is covered by a call. If any sub-task has no "
            "corresponding call, it is NOT complete. Reply exactly YES or NO.")
    usr = f"Request:\n{request_of(tid)}\n\nCalls made:\n{calls}\n\nIs every part covered?"
    body = {"model": "x", "temperature": 0.0, "max_tokens": 4, "cache_prompt": False,
            "grammar": 'root ::= "YES" | "NO"\n', "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "system", "content": sysm}, {"role": "user", "content": usr}]}
    req = urllib.request.Request(f"http://127.0.0.1:{port}/v1/chat/completions",
                                 data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    c = json.load(urllib.request.urlopen(req, timeout=60))["choices"][0]["message"].get("content") or ""
    return c.strip().upper().startswith("YES")   # True = predicts COMPLETE

_mc = None
def check_minicheck(tid, plan):
    global _mc
    if _mc is None:
        # MiniCheck hardcodes device_map="auto"; force a plain CPU load (torch is CPU-only)
        import transformers
        _orig = transformers.AutoModelForSeq2SeqLM.from_pretrained
        def _patched(ckpt, *a, **kw):
            kw.pop("device_map", None)
            return _orig(ckpt, *a, **kw)
        transformers.AutoModelForSeq2SeqLM.from_pretrained = staticmethod(_patched)
        from minicheck.minicheck import MiniCheck
        _mc = MiniCheck(model_name="flan-t5-large", enable_prefix_caching=False)
    # CORRECT MiniCheck framing: document = evidence (what the calls did),
    # claim = the thing we assert is true (the user's request, i.e. "this was all done").
    # If a sub-task's call is missing from the document, that part of the claim is
    # unsupported -> NO. This is MiniCheck's actual (doc supports claim?) relation.
    calls = render_calls(plan)
    doc = "The following actions were performed:\n" + "\n".join(f"- called {c}" for c in calls)
    claim = request_of(tid)
    pred, _, _, _ = _mc.score(docs=[doc], claims=[claim])
    return bool(pred[0])   # 1 = claim supported by the actions = complete

check = {"ownq4": check_ownq4, "minicheck": check_minicheck}[CHECKER]

tp = tn = fp = fn = 0
for tid, plan in base.items():
    pred_complete = check(tid, plan)
    true_complete = label[tid]
    if pred_complete and true_complete: tp += 1
    elif not pred_complete and not true_complete: tn += 1
    elif pred_complete and not true_complete: fp += 1     # missed an incomplete -> gate fails to recover
    else: fn += 1                                          # false alarm -> over-calls

n_inc = sum(1 for v in label.values() if not v)
n_comp = sum(1 for v in label.values() if v)
recall_inc = tn / n_inc if n_inc else 0                    # caught incompletes
acc = (tp + tn) / len(base)
print(f"checker={CHECKER}   accuracy {acc*100:.1f}%   ({tp+tn}/40)")
print(f"  INCOMPLETE plans (n={n_inc}): correctly flagged {tn}  (recall {recall_inc*100:.0f}%)  <- the gate's job")
print(f"  COMPLETE   plans (n={n_comp}): correctly passed  {tp}  (false-alarms {fn})")
print(f"  confusion: TP(pass&complete)={tp} TN(flag&incomplete)={tn} FP(pass BUT incomplete)={fp} FN(flag BUT complete)={fn}")
