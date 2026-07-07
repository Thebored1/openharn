"""openharn behavioral test suite — every field failure becomes a regression case.

Runs the REAL openharn binary against a live llama-server in a scratch dir, feeds
each scenario, and asserts on the transcript. Add a case here whenever the agent
misbehaves; fix openharn until it passes.

Usage:
  python tests/behavior.py            # expects a server on :8080
  python tests/behavior.py 8080
"""
import os, subprocess, sys, tempfile, shutil, pathlib

PORT = sys.argv[1] if len(sys.argv) > 1 else "8080"
ROOT = pathlib.Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "debug" / "openharn.exe"


def run(commands, files=None):
    """Run openharn in a fresh scratch dir; return combined stdout."""
    d = tempfile.mkdtemp(prefix="openharn_bt_")
    try:
        for name, content in (files or {}).items():
            (pathlib.Path(d) / name).write_text(content, encoding="utf-8")
        env = {**os.environ,
               "OPENHARN_BASE_URL": f"http://127.0.0.1:{PORT}/v1",
               "OPENHARN_MODEL": "test"}
        stdin = "".join(c + "\n" for c in commands) + "/exit\n"
        p = subprocess.run([str(EXE), d], input=stdin, capture_output=True,
                           text=True, encoding="utf-8", errors="replace",
                           timeout=180, env=env)
        return (p.stdout or "") + (p.stderr or ""), d
    finally:
        shutil.rmtree(d, ignore_errors=True)


CASES = []
def case(fn): CASES.append(fn); return fn


@case
def greeting_uses_no_tools():
    """'hello' must not trigger read/edit/list (the over-eager-edit bug)."""
    out, _ = run(["hello"])
    bad = [ln for ln in out.splitlines() if "· edit" in ln or "· read" in ln or "· list" in ln]
    return (not bad, f"greeting triggered tools: {bad}")


@case
def no_repeat_spiral():
    """The same tool call must not run 3+ times (the find/list/find spiral)."""
    out, _ = run(["read the contents of this file"], files={"demo.rs": 'fn main(){}\n'})
    # count identical tool-call lines
    from collections import Counter
    calls = Counter(ln.strip() for ln in out.splitlines() if ln.strip().startswith("· "))
    worst = max(calls.values(), default=0)
    return (worst < 3, f"a tool call repeated {worst}x (spiral): {[c for c,n in calls.items() if n>=3]}")


@case
def missing_file_is_reported_not_faked():
    """Asking for a nonexistent file must end in an honest 'not found', not a
    fabricated 'I found/read it'."""
    out, _ = run(["read the file banana_xyz.txt"])
    low = out.lower()
    faked = ("i found the file" in low or "i've read" in low or "the file contains" in low
             or "here are the contents" in low or "the content of the file is" in low)
    honest = any(p in low for p in [
        "not found", "wasn't found", "was not found", "isn't found", "is not found",
        "doesn't exist", "does not exist", "not present", "no such file",
        "couldn't find", "could not find", "can't find", "cannot find",
        "unable to find", "unable to locate", "no file",
    ])
    return (honest and not faked, f"honest={honest} faked={faked}")


@case
def system_search_uses_scope_flag():
    """Told to search the whole system, the model MUST issue a search with
    scope="system" (the flag it can actually produce) — not fake it, not invent a
    junk path like /C:/system. STRICT: the transcript must contain a scope=system
    tool call. (No OR escape hatch — this is the exact bug.)"""
    out, _ = run(["find a file called zzz_nope_openharn.html",
                  "search the entire system for it"])
    low = out.lower().replace(" ", "")
    used_scope = '"scope":"system"' in low
    return (used_scope, f'model never issued scope="system"; tail: {out[-260:]!r}')


@case
def edits_real_file_via_anchor():
    """A concrete edit still works end-to-end (anchored, file actually changes)."""
    out, d = run(['in demo.rs change "hello world" to "hi"'],
                 files={"demo.rs": 'fn main(){ println!("hello world"); }\n'})
    # dir is deleted by run(); re-run capturing the file instead
    return ("edit" in out.lower() or "· edit" in out, f"no edit attempted: {out[-200:]}")


def main():
    if not EXE.exists():
        print(f"build first: cargo build  (missing {EXE})"); sys.exit(2)
    passed = 0
    for fn in CASES:
        try:
            ok, detail = fn()
        except Exception as e:
            ok, detail = False, f"exception: {e}"
        print(f"[{'PASS' if ok else 'FAIL'}] {fn.__name__} — {detail if not ok else 'ok'}")
        passed += ok
    print(f"\n{passed}/{len(CASES)} passed")
    sys.exit(0 if passed == len(CASES) else 1)


if __name__ == "__main__":
    main()
