"""openharn multi-model benchmark.

Runs an identical conversation + tool-call scenario (search / create / find / edit,
plus plain chat) against every model, driving the SAME openharn-style agent loop and
tool set through a local llama-server. For each model it logs:

  - wall time (scenario only, excludes model load)
  - failed requests (HTTP error / timeout / exception)
  - tokens/sec (token-weighted mean of llama-server's own timing)
  - thinking tokens (reasoning_content + inline <think> blocks, tokenized)
  - plus: completion tokens, tool calls emitted vs expected, task success

The harness spawns/kills llama-server per model, seeds a fresh scratch project so the
tools have real targets, and executes tools with openharn's semantics (read-before-edit
grounding, project-scoped glob/grep). Same system prompt + same user turns for all.

Usage:
  python tests/benchmark.py              # all models, CPU
  python tests/benchmark.py --port 8080
"""
import argparse, json, os, pathlib, shutil, subprocess, sys, time, urllib.request, urllib.error, re

ROOT = pathlib.Path(__file__).resolve().parent.parent
DL = pathlib.Path.home() / "Downloads"
LLAMA = pathlib.Path(os.environ.get("LLAMA_SERVER",
    r"C:\Users\Paper\AppData\Local\Microsoft\WinGet\Packages\ggml.llamacpp_Microsoft.Winget.Source_8wekyb3d8bbwe\llama-server.exe"))
SYSTEM = (ROOT / "src" / "prompt.txt").read_text(encoding="utf-8")

# ---- models: (label, filename, extra llama-server flags) ---------------------
# Excluded per the user's instruction (unreliable / not worth the time):
#   LFM2-1.2B-Tool, LFM2.5-1.2B-Instruct  — the 1.2B tier hallucinates tool
#     *results* instead of emitting real tool calls.
#   gemma-3n-E2B-IQ3_XXS, gemma-4-E2B-IQ4_XS — dropped from the research set.
MODELS = [
    ("LFM2-8B-A1B-Q3_K_S",    "LFM2-8B-A1B-Q3_K_S.gguf",          []),
    ("LFM2-8B-A1B-Q3_K_XL",   "LFM2-8B-A1B-UD-Q3_K_XL.gguf",      []),
    ("LFM2-8B-A1B-Q4_K_XL",   "LFM2-8B-A1B-UD-Q4_K_XL.gguf",      []),
    ("LFM2.5-8B-APEX-Compact","LFM2.5-8B-A1B-APEX-I-Compact.gguf",[]),
    ("LFM2.5-8B-APEX-Mini",   "LFM2.5-8B-A1B-APEX-I-Mini.gguf",   []),
    ("LFM2.5-8B-A1B-Q4_K_M",  "LFM2.5-8B-A1B-Q4_K_M.gguf",        []),
    ("gemma-4-E2B-qat-Q4_KXL","gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf",[]),
]

# ---- the scenario: identical for every model ---------------------------------
# Each entry: (user_message, expected_tool_or_None). expected is used only to score
# whether the model produced the right kind of structured call — never to steer it.
SCENARIO = [
    ("Hi! In one sentence, what kinds of coding tasks can you help me with?", None),
    ("Search the project for the word 'Config' and tell me which file defines it.", "grep"),
    ("Create a new file named notes.txt whose contents are exactly: benchmark run", "write"),
    ("Find any file in the project whose name ends with .toml.", "glob"),
    ("In notes.txt, change the text 'benchmark run' to 'benchmark complete'.", "edit"),
]

PORT = 8080
BASE = None  # set in main
REQ_TIMEOUT = 150      # seconds per HTTP request
MAX_TOOL_ITERS = 4     # tool-loop iterations per user turn
MAX_TOKENS = 1024      # cap so a runaway thinker still terminates a turn
REASONING_OFF = False  # --reasoning-off: inject a closed <think></think> prefill (LFM2.5 no-think)
THINK_PREFILL = "<think></think>"
REPORT_STEM = "results"  # writes bench_logs/<stem>.{json,md}


# ============================ scratch project =================================
def seed_project(d: pathlib.Path):
    (d / "src").mkdir(parents=True, exist_ok=True)
    (d / "src" / "app.py").write_text(
        "class Config:\n    debug = False\n    port = 8080\n\n"
        "def load_config():\n    return Config()\n", encoding="utf-8")
    (d / "README.md").write_text("# Demo project\nA tiny sample project.\n", encoding="utf-8")
    (d / "settings.toml").write_text("[server]\nport = 8080\n", encoding="utf-8")


# ============================ openharn-style tools ============================
SKIP = {".git", "node_modules", "target", "__pycache__"}

def _resolve(cwd, p):
    p = (p or ".").replace("\\", "/").lstrip("/")
    return (cwd / p) if p not in ("", ".") else cwd

class Session:
    def __init__(self, cwd):
        self.cwd = cwd
        self.read = set()

    def execute(self, name, args):
        try:
            return getattr(self, f"t_{name}")(args)
        except AttributeError:
            return f"'{name}' is not an available tool."
        except Exception as e:
            return f"tool error: {e}"

    def _walk(self):
        for r, ds, fs in os.walk(self.cwd):
            ds[:] = [x for x in ds if x not in SKIP]
            for f in fs:
                yield pathlib.Path(r) / f

    def t_read(self, a):
        path = _resolve(self.cwd, a.get("path"))
        try:
            text = path.read_text(encoding="utf-8")
        except Exception as e:
            names = ", ".join(sorted(x.name for x in self.cwd.iterdir())) if self.cwd.is_dir() else ""
            return f"Error reading {path.name}: {e}. Files that exist: {names}."
        self.read.add(str(path))
        lines = text.splitlines()
        return "\n".join(f"{i+1}: {l}" for i, l in enumerate(lines)) or "(empty file)"

    def t_write(self, a):
        path = _resolve(self.cwd, a.get("path"))
        content = a.get("content", "")
        if path.exists() and str(path) not in self.read:
            return f"Error: {path.name} exists and was not read first."
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        self.read.add(str(path))
        return f"Wrote {path.name} ({len(content)} bytes)."

    def t_edit(self, a):
        path = _resolve(self.cwd, a.get("path"))
        if str(path) not in self.read:
            return f"Error: you must `read` {path.name} before editing it."
        old, new = a.get("old_string", ""), a.get("new_string", "")
        try:
            content = path.read_text(encoding="utf-8")
        except Exception as e:
            return f"Error reading {path.name}: {e}"
        if old == new:
            return "No changes: old_string and new_string identical."
        if old and old in content:
            if a.get("replace_all"):
                content = content.replace(old, new)
            elif content.count(old) > 1:
                return "Found multiple matches; add context or set replace_all."
            else:
                content = content.replace(old, new, 1)
            path.write_text(content, encoding="utf-8")
            return f"Edited {path.name} ({len(content)} bytes)."
        # forgiving fallback: whitespace-normalized line match
        norm = lambda s: " ".join(s.split())
        for line in content.splitlines():
            if norm(line) == norm(old):
                content = content.replace(line, new, 1)
                path.write_text(content, encoding="utf-8")
                return f"Edited {path.name} ({len(content)} bytes)."
        return "Could not find old_string in the file."

    def t_multiedit(self, a):
        for e in a.get("edits", []):
            r = self.t_edit({"path": a.get("path"), **e})
            if r.startswith(("Error", "Could not", "Found", "No changes")):
                return f"multiedit aborted: {r}"
        return f"Applied {len(a.get('edits', []))} edits."

    def t_glob(self, a):
        pat = a.get("pattern", "")
        base = pat.split("/")[-1]
        import fnmatch
        hits = []
        for p in self._walk():
            rel = str(p.relative_to(self.cwd)).replace("\\", "/")
            if fnmatch.fnmatch(rel, pat) or fnmatch.fnmatch(p.name, pat) or fnmatch.fnmatch(p.name, base):
                hits.append(rel)
        return "\n".join(sorted(hits)[:100]) or f"No files matching '{pat}' in the project."

    def t_grep(self, a):
        try:
            rx = re.compile(a.get("pattern", ""))
        except re.error as e:
            return f"Invalid regex: {e}"
        inc = a.get("include")
        import fnmatch
        out = []
        for p in self._walk():
            if inc and not fnmatch.fnmatch(p.name, inc):
                continue
            try:
                for i, line in enumerate(p.read_text(encoding="utf-8").splitlines()):
                    if rx.search(line):
                        rel = str(p.relative_to(self.cwd)).replace("\\", "/")
                        out.append(f"{rel}:{i+1}: {line.strip()[:200]}")
            except Exception:
                continue
        return "\n".join(out[:100]) or f"No matches for /{a.get('pattern')}/ in the project."

    def t_bash(self, a):
        try:
            r = subprocess.run(a.get("command", ""), shell=True, cwd=self.cwd,
                               capture_output=True, text=True, timeout=30)
            return (r.stdout + r.stderr)[:4000] or f"(no output, exit {r.returncode})"
        except Exception as e:
            return f"Error running command: {e}"

    def t_webfetch(self, a):
        return "webfetch disabled in benchmark."

    def t_todowrite(self, a):
        return f"Todo list updated ({len(a.get('todos', []))} items)."

    def t_todoread(self, a):
        return "The todo list is empty."


def tool_schemas():
    return json.loads((ROOT / "tests" / "_schemas.json").read_text(encoding="utf-8"))


# ============================ llama-server driver =============================
def start_server(gguf, flags, log_path):
    args = [str(LLAMA), "-m", str(gguf), "--jinja", "--ctx-size", "8192",
            "-ngl", "0", "--host", "127.0.0.1", "--port", str(PORT), "--no-warmup"] + flags
    lf = open(log_path, "w", encoding="utf-8", errors="replace")
    proc = subprocess.Popen(args, stdout=lf, stderr=subprocess.STDOUT)
    return proc, lf

def wait_health(timeout=200):
    start = time.time()
    while time.time() - start < timeout:
        try:
            with urllib.request.urlopen(f"{BASE}/health", timeout=2) as r:
                if json.load(r).get("status") == "ok":
                    return True
        except Exception:
            pass
        time.sleep(1)
    return False

def stop_server(proc, lf):
    try:
        proc.terminate()
        proc.wait(timeout=10)
    except Exception:
        try: proc.kill()
        except Exception: pass
    try: lf.close()
    except Exception: pass
    # belt-and-suspenders on Windows
    subprocess.run("taskkill /F /IM llama-server.exe", shell=True,
                   capture_output=True)

def count_tokens(text):
    if not text:
        return 0
    try:
        body = json.dumps({"content": text, "add_special": False}).encode()
        req = urllib.request.Request(f"{BASE}/tokenize", data=body,
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=30) as r:
            return len(json.load(r).get("tokens", []))
    except Exception:
        return 0

THINK_RX = re.compile(r"<think>(.*?)</think>", re.S)

def chat(history, schemas):
    """One request. Returns (message, metrics, error_or_None)."""
    # Reasoning-off: prime the assistant turn with a closed <think></think> block so
    # llama-server continues from an already-finished think state. Sent only, never
    # stored in history.
    msgs = history + [{"role": "assistant", "content": THINK_PREFILL}] if REASONING_OFF else history
    body = json.dumps({
        "model": "bench", "messages": msgs, "tools": schemas,
        "tool_choice": "auto", "temperature": 0.2, "stream": False,
        "max_tokens": MAX_TOKENS,
    }).encode()
    req = urllib.request.Request(f"{BASE}/v1/chat/completions", data=body,
                                 headers={"Content-Type": "application/json"})
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=REQ_TIMEOUT) as r:
            d = json.load(r)
    except urllib.error.HTTPError as e:
        return None, {"latency": time.time() - t0}, f"HTTP {e.code}"
    except Exception as e:
        return None, {"latency": time.time() - t0}, f"{type(e).__name__}: {e}"
    dt = time.time() - t0
    ch = d["choices"][0]
    msg = ch.get("message", {})
    usage = d.get("usage", {})
    timings = d.get("timings", {})
    # thinking tokens: explicit reasoning_content + any inline <think> blocks
    reasoning = msg.get("reasoning_content") or ""
    inline = "".join(THINK_RX.findall(msg.get("content") or ""))
    think_txt = reasoning + inline
    m = {
        "latency": dt,
        "completion_tokens": usage.get("completion_tokens", 0),
        # robust tok/s comes from aggregating predicted_n / predicted_ms across the
        # run (below); per-request predicted_per_second spikes on cached/short gens.
        "pred_n": timings.get("predicted_n") or 0,
        "pred_ms": timings.get("predicted_ms") or 0.0,
        "think_tokens": count_tokens(think_txt),
        "has_tool_call": bool(msg.get("tool_calls")),
        "finish": ch.get("finish_reason"),
    }
    return msg, m, None


def run_scenario(agg, label):
    scratch = pathlib.Path(os.environ["TEMP"]) / f"ohbench_{label}_{int(time.time())}"
    if scratch.exists():
        shutil.rmtree(scratch, ignore_errors=True)
    scratch.mkdir(parents=True)
    seed_project(scratch)
    sess = Session(scratch)
    schemas = agg["_schemas"]
    history = [{"role": "system", "content": SYSTEM}]

    for turn_i, (user, expected) in enumerate(SCENARIO):
        history.append({"role": "user", "content": user})
        got_expected_tool = False
        for _ in range(MAX_TOOL_ITERS):
            msg, m, err = chat(history, schemas)
            agg["requests"] += 1
            agg["latency"] += m["latency"]
            if err:
                agg["failed"] += 1
                agg["errors"].append(f"turn{turn_i+1}: {err}")
                break
            agg["completion_tokens"] += m["completion_tokens"]
            agg["think_tokens"] += m["think_tokens"]
            agg["pred_n"] += m["pred_n"]
            agg["pred_ms"] += m["pred_ms"]
            # record assistant turn
            a = {"role": "assistant", "content": msg.get("content") or None}
            if msg.get("tool_calls"):
                a["tool_calls"] = msg["tool_calls"]
            history.append(a)
            if not msg.get("tool_calls"):
                break  # final text answer for this user turn
            for tc in msg["tool_calls"]:
                name = tc.get("function", {}).get("name", "")
                if name == expected:
                    got_expected_tool = True
                try:
                    targs = json.loads(tc.get("function", {}).get("arguments") or "{}")
                except Exception:
                    targs = {}
                result = sess.execute(name, targs)
                history.append({"role": "tool", "tool_call_id": tc.get("id", ""),
                                "content": result[:4000]})
        if expected is not None and got_expected_tool:
            agg["tool_hits"] += 1

    # task success: notes.txt exists and contains the edited text
    nf = scratch / "notes.txt"
    agg["task_ok"] = nf.exists() and "benchmark complete" in nf.read_text(encoding="utf-8", errors="replace")
    shutil.rmtree(scratch, ignore_errors=True)


def main():
    global BASE, PORT, REASONING_OFF, REPORT_STEM
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--only", default="", help="substring filter on model label")
    ap.add_argument("--reasoning-off", action="store_true",
                    help="inject a closed <think></think> prefill; writes to results_noreason.*")
    args = ap.parse_args()
    PORT = args.port
    BASE = f"http://127.0.0.1:{PORT}"
    if args.reasoning_off:
        REASONING_OFF = True
        REPORT_STEM = "results_noreason"

    # dump openharn's tool schemas once (from the running binary's source of truth
    # we hand-mirror them here to keep the harness self-contained)
    (ROOT / "tests" / "_schemas.json").write_text(json.dumps(SCHEMAS), encoding="utf-8")

    logdir = ROOT / "tests" / "bench_logs"
    logdir.mkdir(exist_ok=True)
    # merge with any prior results so an incremental `--only` run folds in without
    # clobbering the other models' numbers.
    prior = {}
    pj = logdir / f"{REPORT_STEM}.json"
    if pj.exists():
        try:
            for r in json.loads(pj.read_text(encoding="utf-8")):
                prior[r["label"]] = r
        except Exception:
            pass
    ordered = lambda: [prior[l] for l, _, _ in MODELS if l in prior]
    subprocess.run("taskkill /F /IM llama-server.exe", shell=True, capture_output=True)

    for label, fname, flags in MODELS:
        if args.only and args.only.lower() not in label.lower():
            continue
        gguf = DL / fname
        agg = {"label": label, "file": fname, "requests": 0, "failed": 0,
               "latency": 0.0, "completion_tokens": 0, "think_tokens": 0,
               "pred_n": 0, "pred_ms": 0.0, "tool_hits": 0,
               "expected_tools": sum(1 for _, e in SCENARIO if e), "errors": [],
               "task_ok": False, "load_ok": False, "load_s": None, "_schemas": SCHEMAS}
        if not gguf.exists() or gguf.stat().st_size < 100_000_000:
            agg["errors"].append("file missing or incomplete (still downloading?)")
            agg.pop("_schemas", None)
            prior[label] = agg; _emit(agg); _write_report(ordered(), logdir); continue
        print(f"\n=== {label} : loading ===", flush=True)
        t0 = time.time()
        proc, lf = start_server(gguf, flags, logdir / f"{label}.log")
        if not wait_health():
            agg["load_s"] = round(time.time() - t0, 1)
            agg["errors"].append("server failed to become healthy (load OOM/crash?)")
            agg.pop("_schemas", None)
            stop_server(proc, lf); prior[label] = agg; _emit(agg); _write_report(ordered(), logdir); continue
        agg["load_ok"] = True
        agg["load_s"] = round(time.time() - t0, 1)
        print(f"    loaded in {agg['load_s']}s; running scenario ...", flush=True)
        try:
            run_scenario(agg, label)
        except Exception as e:
            agg["errors"].append(f"scenario crashed: {type(e).__name__}: {e}")
        stop_server(proc, lf)
        agg.pop("_schemas", None)
        prior[label] = agg; _emit(agg)
        _write_report(ordered(), logdir)

    _write_report(ordered(), logdir)
    print(f"\nreport written to {logdir/(REPORT_STEM+'.json')} and {logdir/(REPORT_STEM+'.md')}")


def _tps(agg):
    return round(1000.0 * agg["pred_n"] / agg["pred_ms"], 1) if agg.get("pred_ms") else 0

def _emit(agg):
    tps = _tps(agg)
    print(f"    [{agg['label']}] reqs={agg['requests']} failed={agg['failed']} "
          f"time={round(agg['latency'],1)}s tok/s={tps} "
          f"think_tok={agg['think_tokens']} tool_hits={agg['tool_hits']}/{agg['expected_tools']} "
          f"task_ok={agg['task_ok']}", flush=True)


def _write_report(results, logdir):
    clean = [{k: v for k, v in r.items() if k != "_schemas"} for r in results]
    (logdir / f"{REPORT_STEM}.json").write_text(json.dumps(clean, indent=2), encoding="utf-8")
    rows = ["| Model | Load s | Reqs | Failed | Time s | Tok/s | Compl.tok | Think tok | Tool hits | Task |",
            "|---|---|---|---|---|---|---|---|---|---|"]
    for r in clean:
        tps = _tps(r)
        status = "ok" if r.get("load_ok") else "LOAD FAIL"
        rows.append(f"| {r['label']} | {r.get('load_s')} | {r['requests']} | {r['failed']} | "
                    f"{round(r['latency'],1)} | {tps} | {r['completion_tokens']} | {r['think_tokens']} | "
                    f"{r['tool_hits']}/{r['expected_tools']} | {'PASS' if r.get('task_ok') else status} |")
    (logdir / f"{REPORT_STEM}.md").write_text("\n".join(rows) + "\n", encoding="utf-8")


# openharn's 10 tool schemas (mirrored from src/tools.rs::schemas)
SCHEMAS = [
    {"type":"function","function":{"name":"read","description":"Read a file. Returns its text with 1-based line numbers. You must read a file before you edit or write it.","parameters":{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}}},
    {"type":"function","function":{"name":"edit","description":"Performs exact string replacements in files. You must `read` the file first. It fails if old_string is not found or is found multiple times. Never reprint the whole file.","parameters":{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["path","old_string","new_string"]}}},
    {"type":"function","function":{"name":"write","description":"Write a file, overwriting any existing file. If it exists you MUST read it first.","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}},
    {"type":"function","function":{"name":"glob","description":"Fast file pattern matching, e.g. **/*.rs. Returns matching file paths. Searches the project directory.","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"scope":{"type":"string","enum":["project","system"]}},"required":["pattern"]}}},
    {"type":"function","function":{"name":"grep","description":"Fast content search using regular expressions. Returns matching file:line: text. Filter files with include.","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"include":{"type":"string"},"path":{"type":"string"},"scope":{"type":"string","enum":["project","system"]}},"required":["pattern"]}}},
    {"type":"function","function":{"name":"bash","description":"Run a shell command in the project root; returns stdout+stderr.","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}},
    {"type":"function","function":{"name":"multiedit","description":"Make multiple edits to a single file in one call. You must read the file first. All-or-nothing.","parameters":{"type":"object","properties":{"path":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["old_string","new_string"]}}},"required":["path","edits"]}}},
    {"type":"function","function":{"name":"webfetch","description":"Fetch a URL and return its readable text.","parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},
    {"type":"function","function":{"name":"todowrite","description":"Create/replace the task todo list. Send the full list each time.","parameters":{"type":"object","properties":{"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["content","status"]}}},"required":["todos"]}}},
    {"type":"function","function":{"name":"todoread","description":"Read the current todo list.","parameters":{"type":"object","properties":{}}}},
]

if __name__ == "__main__":
    main()
