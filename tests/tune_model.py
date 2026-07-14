#!/usr/bin/env python3
"""tune_model.py — find the best openharn config for a GGUF on this machine.

Cross-platform port of ``tests/tune_model.sh`` (no shell required, so it runs
on Linux, macOS, and Windows). It launches ``llama-server``, probes whether the
model does native tool-calling and whether it thinks by default, then runs
``tests/behavior.py`` across candidate configs and picks the highest pass-score
(tie-break: speed).

The candidate set is generated exhaustively from the harness flag matrix
(prompt_tools, strict, narrow, yesno, friendly) subject to the same implication
rules as agent.rs, crossed with thinking on/off — so it generalizes to any
model, not just the ones hand-tested.

Accuracy / runtime tradeoff (pick one):
  --full     every valid flag combo x thinking on/off, full 6-case suite (exhaustive, slow)
  --pruned   probe-decided subset, full 6-case suite                (default, balanced)
  --quick    representative subset, 4 representative cases              (fastest)

Usage:
  python tests/tune_model.py <model.gguf> [--full|--pruned|--quick] [--port N] [--llama PATH] [--ctx N]

Output: prints the winning config + a ranking table, saves a ranking log to
tests/tune_logs/<model>.md, and writes the winning OPENHARN_* flags to
configs/<model>.conf — the per-model config file openharn loads with
``--config`` (or auto-loads as configs/<model>.conf). So the tune run is the
save step: the user reuses the result instead of retyping flags.

llama-server: REQUIRED (preinstalled). Located via --llama, $LLAMA_SERVER,
PATH, or the known myelin cpu build; errors clearly if none found.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "debug" / "openharn"
BEHAVIOR = ROOT / "tests" / "behavior.py"
LOGS = ROOT / "tests" / "tune_logs"
CONFIGS = ROOT / "configs"
KNOWN_LLAMA = Path("/home/paper/.local/share/com.paper.myelin/bin/cpu/llama-server")

QUICK_CASES = "greeting_uses_no_tools,find_file_uses_glob_not_grep,missing_file_is_reported_not_faked,edits_real_file_via_anchor"

# OPENHARN_* keys the tuner manages; cleared before each candidate so a stale
# value from the parent environment can't leak into a run.
MANAGED = (
    "OPENHARN_STRICT_TOOLS",
    "OPENHARN_PROMPT_TOOLS",
    "OPENHARN_NARROW",
    "OPENHARN_YESNO",
    "OPENHARN_FRIENDLY_RESULTS",
    "OPENHARN_NO_THINK",
)


def detect_llama(explicit):
    if explicit:
        return explicit
    if os.environ.get("LLAMA_SERVER"):
        return os.environ["LLAMA_SERVER"]
    from shutil import which

    if which("llama-server"):
        return "llama-server"
    if KNOWN_LLAMA.exists():
        return str(KNOWN_LLAMA)
    sys.exit(
        "ERROR: llama-server binary not found.\n"
        "  Install llama.cpp, pass --llama <path>, or set LLAMA_SERVER."
    )


def post_json(url, payload, timeout=90):
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        url, data=data, headers={"Content-Type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read().decode())
    except Exception:
        return None


def health_ok(port):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=2) as r:
            return b'"status":"ok"' in r.read()
    except Exception:
        return False


def probe_thinks(base):
    out = post_json(
        base + "/chat/completions",
        {
            "model": "local",
            "stream": False,
            "temperature": 0.2,
            "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
        },
    )
    try:
        msg = out["choices"][0]["message"]
        return 1 if (msg.get("reasoning_content") or "").strip() else 0
    except Exception:
        return 0


def probe_native(base):
    tools = [
        {
            "type": "function",
            "function": {
                "name": "grep",
                "description": "regex search",
                "parameters": {
                    "type": "object",
                    "properties": {"pattern": {"type": "string"}},
                    "required": ["pattern"],
                },
            },
        }
    ]
    out = post_json(
        base + "/chat/completions",
        {
            "model": "local",
            "stream": False,
            "temperature": 0.2,
            "messages": [
                {"role": "system", "content": "You are a coding agent."},
                {
                    "role": "user",
                    "content": "Search for the word Config and say which file defines it.",
                },
            ],
            "tools": tools,
            "tool_choice": "auto",
        },
    )
    try:
        tc = (out["choices"][0]["message"].get("tool_calls")) or []
        return 1 if tc else 0
    except Exception:
        return 0


class Server:
    """Starts/stops a llama-server subprocess for a given thinking mode."""

    def __init__(self, llama, model, port, ctx):
        self.llama = llama
        self.model = model
        self.port = port
        self.ctx = ctx
        self.proc = None
        self.think = None

    def start(self, think):
        if self.proc is not None and self.think == think:
            return  # already running in the requested mode
        self.stop()
        kw = (
            []
            if think == "on"
            else ["--chat-template-kwargs", '{"enable_thinking":false}']
        )
        args = [
            self.llama,
            "-m",
            self.model,
            "--jinja",
            "--ctx-size",
            str(self.ctx),
            "-ngl",
            "0",
            "--host",
            "127.0.0.1",
            "--port",
            str(self.port),
            "--no-warmup",
        ] + kw
        self.proc = subprocess.Popen(
            args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        self.think = think
        for _ in range(90):
            if health_ok(self.port):
                return
            time.sleep(1)
        sys.exit(f"ERROR: llama-server on :{self.port} never became ready")

    def stop(self):
        if self.proc is None:
            return
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()
        self.proc = None
        self.think = None


def gen_all():
    """Every valid flag combo x thinking, with the agent.rs implication rules."""
    cands = []
    for pt in (0, 1):
        for st in (0, 1):
            for na in (0, 1):
                for ye in (0, 1):
                    for fr in (0, 1):
                        if st == 1 and pt == 0:
                            continue
                        if na == 1 and st == 0:
                            continue
                        if fr == 1 and pt == 0:
                            continue
                        if na == 1 and ye == 1:
                            continue
                        if na == 1 and fr == 1:
                            continue
                        for think in ("on", "off"):
                            cands.append(
                                {
                                    "pt": pt,
                                    "st": st,
                                    "na": na,
                                    "ye": ye,
                                    "fr": fr,
                                    "think": think,
                                }
                            )
    return cands


def candidate_name(c):
    return f"pt{c['pt']} st{c['st']} na{c['na']} ye{c['ye']} fr{c['fr']}"


def candidate_env(c):
    env = {}
    if c["pt"]:
        env["OPENHARN_PROMPT_TOOLS"] = "1"
    if c["st"]:
        env["OPENHARN_STRICT_TOOLS"] = "1"
    if c["na"]:
        env["OPENHARN_NARROW"] = "1"
    if c["ye"]:
        env["OPENHARN_YESNO"] = "1"
    if c["fr"]:
        env["OPENHARN_FRIENDLY_RESULTS"] = "1"
    return env


def run_case(c, port, stem, case_filter, timeout=580):
    env = dict(os.environ)
    for k in MANAGED:
        env.pop(k, None)
    env["OPENHARN_BASE_URL"] = f"http://127.0.0.1:{port}/v1"
    env["OPENHARN_MODEL"] = stem
    env.update(candidate_env(c))
    if case_filter:
        env["OPENHARN_TUNE_CASES"] = case_filter
    try:
        r = subprocess.run(
            [sys.executable, str(BEHAVIOR), str(port)],
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return (0, 6, "(timeout)")
    out = r.stdout + r.stderr
    m = re.findall(r"(\d+)/(\d+) passed", out)
    if m:
        s, t = map(int, m[-1])
    else:
        s, t = 0, 6
    return (s, t, out)


def matches(c, spec):
    return all(c[k] == v for k, v in spec.items())


def prune(cands, mode, native):
    if mode == "full":
        return cands
    if mode == "pruned":
        if native == 1:
            specs = [{"pt": 0, "st": 0, "na": 0, "ye": 0, "fr": 0},
                     {"pt": 0, "st": 0, "na": 0, "ye": 0, "fr": 1}]
        else:
            specs = [{"pt": 1, "st": 1, "na": 0, "ye": 0, "fr": 0},
                     {"pt": 1, "st": 1, "na": 0, "ye": 1, "fr": 0}]
    else:  # quick
        if native == 1:
            specs = [{"pt": 0, "st": 0, "na": 0, "ye": 0, "fr": 0}]
        else:
            specs = [{"pt": 1, "st": 1, "na": 0, "ye": 0, "fr": 0},
                     {"pt": 1, "st": 1, "na": 0, "ye": 1, "fr": 0},
                     {"pt": 1, "st": 0, "na": 0, "ye": 0, "fr": 0}]
    return [c for c in cands if any(matches(c, s) for s in specs)]


def main():
    ap = argparse.ArgumentParser(
        description="Find the best openharn config for a GGUF (cross-platform)."
    )
    ap.add_argument("model", help="path to the .gguf model")
    grp = ap.add_mutually_exclusive_group()
    grp.add_argument("--full", action="store_const", dest="mode", const="full")
    grp.add_argument("--pruned", action="store_const", dest="mode", const="pruned")
    grp.add_argument("--quick", action="store_const", dest="mode", const="quick")
    ap.set_defaults(mode="pruned")
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--llama", default=None, help="path to llama-server binary")
    ap.add_argument("--ctx", type=int, default=16384)
    args = ap.parse_args()

    model = Path(os.path.expanduser(args.model))
    if not model.exists():
        sys.exit(
            "usage: python tests/tune_model.py <model.gguf> [--full|--pruned|--quick]"
        )
    stem = model.stem
    llama = detect_llama(args.llama)
    print("using llama-server:", llama)

    srv = Server(llama, str(model), args.port, args.ctx)
    srv.start("on")
    base = f"http://127.0.0.1:{args.port}/v1"
    native_t = probe_thinks(base)
    native = probe_native(base)
    print(f"== probe: thinks_by_default={native_t}  native_tool_calls={native} ==")

    cands = gen_all()
    # a non-native model can't use the bare native-default config (no tool calls)
    if native == 0:
        cands = [
            c
            for c in cands
            if not (c["pt"] == 0 and c["st"] == 0 and c["na"] == 0 and c["ye"] == 0 and c["fr"] == 0)
        ]
    cands = prune(cands, args.mode, native)
    if not cands:
        srv.stop()
        sys.exit(f"no candidates selected for mode={args.mode}")

    case_filter = QUICK_CASES if args.mode == "quick" else ""

    # group by thinking so the server restarts at most once per mode
    cands.sort(key=lambda c: c["think"])
    results = []
    print(f"\n== running {len(cands)} candidates ({args.mode}) ==")
    for c in cands:
        srv.start(c["think"])
        t0 = time.time()
        s, t, _out = run_case(c, args.port, stem, case_filter)
        dt = int(time.time() - t0)
        print(f"  {candidate_name(c)} (think={c['think']}) -> {s}/{t}  ({dt}s)")
        results.append((c, s, t, dt))

    # rank: max score, then min time
    best = results[0]
    for r in results[1:]:
        if r[1] > best[1] or (r[1] == best[1] and r[3] < best[3]):
            best = r
    win_c, win_s, win_t, win_dt = best

    think_flag = (
        ""
        if win_c["think"] == "on"
        else '--chat-template-kwargs \'{"enable_thinking":false}\''
    )
    server_cmd = (
        f"llama-server -m {model.name} --jinja --ctx-size {args.ctx} -ngl 0 "
        f"--host 127.0.0.1 --port {args.port} --no-warmup {think_flag}"
    ).strip()

    # ---- write per-model config file (KEY=value list openharn can load) -----
    CONFIGS.mkdir(exist_ok=True)
    conf = CONFIGS / f"{stem}.conf"
    with open(conf, "w") as f:
        f.write(f"# openharn config for {stem}\n")
        f.write(
            f"# generated by tests/tune_model.py - best-scoring config "
            f"(score {win_s}/{win_t}, {win_dt}s)\n"
        )
        f.write(f"# server: {server_cmd}\n")
        f.write(f"# run:    OPENHARN_MODEL={stem} ./{EXE.name} . --config {conf}\n")
        for k, v in candidate_env(win_c).items():
            f.write(f"{k}={v}\n")

    # ---- ranking log (.md) --------------------------------------------------
    LOGS.mkdir(parents=True, exist_ok=True)
    log = LOGS / f"{stem}.md"
    L = []
    L.append(f"# tune: {stem}\n")
    L.append(
        f"probe: thinks_by_default={native_t}  native_tool_calls={native}   "
        f"mode={args.mode}\n"
    )
    L.append(
        f"## winner: {candidate_name(win_c)}  ({win_s}/{win_t}, {win_dt}s)\n"
    )
    L.append("```sh")
    L.append("# server")
    L.append(server_cmd)
    L.append("# openharn")
    for k, v in candidate_env(win_c).items():
        L.append(f"export {k}={v}")
    L.append("./target/debug/openharn .")
    L.append("```\n")
    L.append("## ranking\n")
    L.append("| config | think | score | time s |")
    L.append("|---|---|---|---|")
    for c, s, t, dt in results:
        L.append(f"| {candidate_name(c)} | {c['think']} | {s}/{t} | {dt} |")
    content = "\n".join(L) + "\n"
    with open(log, "w") as f:
        f.write(content)
    print("\n" + content)

    srv.stop()
    print(f"log saved: {log}")
    print(f"config saved: {conf}")
    print(f"run: OPENHARN_MODEL={stem} ./target/debug/openharn . --config {conf}")


if __name__ == "__main__":
    main()
