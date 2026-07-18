"""Per-turn verifier-selection on BFCL multi_turn (the ceiling measurement).

Trajectory-level best-of-N was 0/6 (a 4-turn task right 1-in-3 per turn is 1-in-81 whole).
This measures whether selecting PER TURN recovers it: at each turn, generate N diverse
candidates, execute each against the (correct) prefix state, and an ORACLE selector keeps
the candidate whose resulting state matches the ground-truth state for that turn. Uses BFCL's
own executor + state_checker, so "match" is the exact eval signal.

This is a CEILING (the selector uses labels; a deployed one wouldn't) — it answers only:
is the model's per-turn 1-in-N right rate high enough that per-turn selection assembles a
correct trajectory. If per-turn hit is high and trajectories get recovered, the direction is
alive; if per-turn hit is ~0, the model can't even do single turns and it's dead.

Deviations (hypothesis-checking, not benchmark): GPU (6 instances, ports 8080-8085), temp 0.7
for candidate diversity, cache_prompt=false, 3 entries.

Usage: per_turn_selection.py <bfcl_eval_dir> <N>
"""
import ast, json, os, sys, urllib.request
from concurrent.futures import ThreadPoolExecutor

sys.argv_backup = sys.argv
SP = sys.argv[1]
N = int(sys.argv[2]) if len(sys.argv) > 2 else 6
sys.argv = [sys.argv[0]]  # some bfcl imports read argv

from bfcl_eval.eval_checker.multi_turn_eval.multi_turn_utils import execute_multi_turn_func_call
from bfcl_eval.eval_checker.multi_turn_eval.multi_turn_checker import state_checker
from bfcl_eval.model_handler.utils import convert_to_function_call, convert_to_tool
from bfcl_eval.constants.type_mappings import GORILLA_TO_OPENAPI
from bfcl_eval.constants.enums import ModelStyle
from bfcl_eval.constants.executable_backend_config import MULTI_TURN_FUNC_DOC_FILE_MAPPING

PORTS = [8080, 8081, 8082, 8083, 8084, 8085]
FUNCDOC = None
def find_funcdoc(cls):
    p = os.path.join(SP, "data", "multi_turn_func_doc", MULTI_TURN_FUNC_DOC_FILE_MAPPING[cls])
    txt = open(p, encoding="utf-8").read().strip()
    try:
        d = json.loads(txt)
        return d if isinstance(d, list) else [d]
    except json.JSONDecodeError:
        return [json.loads(l) for l in txt.splitlines() if l.strip()]  # JSONL

def load(f):
    return [json.loads(l) for l in open(f, encoding="utf-8") if l.strip()]

entries = {e["id"]: e for e in load(f"{SP}/data/BFCL_v4_multi_turn_base.json")}
answers = {a["id"]: a for a in load(f"{SP}/data/possible_answer/BFCL_v4_multi_turn_base.json")}

def tools_for(e):
    docs = []
    for cls in e["involved_classes"]:
        docs += find_funcdoc(cls)
    return convert_to_tool(docs, GORILLA_TO_OPENAPI, ModelStyle.OPENAI_COMPLETIONS)

def parse_callstr(s):
    """'mv(source='a', destination='b')' -> ('mv', {'source':'a','destination':'b'})"""
    node = ast.parse(s.strip(), mode="eval").body
    name = node.func.id if isinstance(node.func, ast.Name) else node.func.attr
    args = {kw.arg: ast.literal_eval(kw.value) for kw in node.keywords}
    return name, args

def post(port, body, timeout=120):
    body = {**body, "cache_prompt": False}
    req = urllib.request.Request(f"http://127.0.0.1:{port}/v1/chat/completions",
                                 data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))

def gen_candidate(port, messages, tools):
    body = {"model": "x", "temperature": 0.7, "max_tokens": 512, "messages": messages,
            "tools": tools, "tool_choice": "required"}
    m = post(port, body)["choices"][0]["message"]
    tc = m.get("tool_calls") or []
    # -> BFCL call-strings via the same converter the eval uses
    return convert_to_function_call([{c["function"]["name"]: c["function"]["arguments"]} for c in tc])

def execute(call_strings, eid, cls_list, init_cfg):
    res, inst = execute_multi_turn_func_call(call_strings, init_cfg, cls_list, "sel", eid)
    return inst

def run_entry(eid):
    e = entries[eid]; gt = answers[eid]["ground_truth"]
    cls_list = e["involved_classes"]; init_cfg = e["initial_config"]
    tools = tools_for(e)
    committed = []                 # accumulated GT call-strings (correct history)
    messages = [{"role": "system", "content":
                 "You are a tool-using agent. Call the tools needed to satisfy each user turn."}]
    per_turn_hit, per_turn_bestk = [], []
    nonce = 0
    for t, turn_msgs in enumerate(e["question"]):
        messages += turn_msgs      # the user turn
        # N diverse candidates, one per GPU port
        def one(k):
            try: return gen_candidate(PORTS[k % len(PORTS)], messages, tools)
            except Exception: return None
        with ThreadPoolExecutor(max_workers=N) as ex:
            cands = list(ex.map(one, range(N)))
        # oracle target: state after committed + gt[t]
        nonce += 1
        gt_state = execute(committed + gt[t], f"{eid}_gt_{t}_{nonce}", cls_list, init_cfg)
        hit_k = -1
        for k, cs in enumerate(cands):
            if not cs: continue
            try:
                st = execute(committed + cs, f"{eid}_c_{t}_{k}_{nonce}", cls_list, init_cfg)
                if state_checker(st, gt_state)["valid"]:
                    hit_k = k; break
            except Exception:
                continue
        per_turn_hit.append(hit_k >= 0); per_turn_bestk.append(hit_k)
        # commit GT (correct history) + append proper assistant tool_calls + tool results
        committed += gt[t]
        calls = [parse_callstr(s) for s in gt[t]]
        messages.append({"role": "assistant", "content": None,
                         "tool_calls": [{"id": f"c{t}_{i}", "type": "function",
                                         "function": {"name": n, "arguments": json.dumps(a)}}
                                        for i, (n, a) in enumerate(calls)]})
        for i, (n, a) in enumerate(calls):
            messages.append({"role": "tool", "tool_call_id": f"c{t}_{i}", "content": "ok"})
    return {"id": eid, "turns": len(e["question"]), "per_turn_hit": per_turn_hit,
            "best_k": per_turn_bestk, "trajectory_recovered": all(per_turn_hit)}

ids = [f"multi_turn_base_{i}" for i in range(3)]
results = [run_entry(i) for i in ids]
hits = sum(sum(r["per_turn_hit"]) for r in results)
turns = sum(r["turns"] for r in results)
traj = sum(r["trajectory_recovered"] for r in results)
print(f"N={N}  per-turn hit: {hits}/{turns} turns had >=1 correct candidate ({100*hits/turns:.0f}%)")
print(f"trajectory recovered by per-turn selection: {traj}/{len(ids)}   (trajectory-level pass@6 was 0/3)")
for r in results:
    print(f"  {r['id']}: per_turn_hit={r['per_turn_hit']}  winning_candidate_idx={r['best_k']}")
