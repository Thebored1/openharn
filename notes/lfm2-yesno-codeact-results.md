# Notes: LFM2-8B-A1B with YES/NO + Code-as-Action + CodeAct

## Test Results (YES/NO + CodeAct, `OPENHARN_YESNO=1`)

| Test | Result | Notes |
|---|---|---|
| greeting_uses_no_tools | ✅ PASS | |
| no_repeat_spiral | ✅ PASS | |
| missing_file_is_reported_not_faked | ❌ FAIL | No tool calls, just text |
| system_search_uses_scope_flag | ❌ FAIL | Doesn't pick glob_system |
| edits_real_file_via_anchor | ❌ FAIL | "[yesno] no tools selected" |
| grounding_limits_total_calls | ❌ FAIL | 0 calls |

**2/6 PASS**

## Why It Fails

| Test | Failure Mode |
|---|---|
| missing_file | Model answers "file not found" in text instead of using python to check |
| system_search | Model doesn't select `glob_system` in YES/NO pass |
| edit_anchor | YES/NO selects no tools → continues without tools |
| grounding | 0 tool calls |

The model **only works when explicitly prompted "write python code to..."** — then YES/NO selects python, model writes code, CodeAct executes it.

## When LFM2-8B Works

```bash
# Explicit python task → works
OPENHARN_YESNO=1 OPENHARN_NO_THINK=1 ./openharn /workspace
> in python compute 2+2
[yesno] selected: ["python"]
·· python {"code":"2 + 2"}
Result: 4
```

## Comparison

| Mode | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|
| Default (LFM2) | ✅ | ✅ | ❌ | ✅* | ❌ | ❌ | 3/6 |
| YES/NO + CodeAct (LFM2) | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | 2/6 |
| Default (MiniCPM) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 6/6 |
| YES/NO + CodeAct (MiniCPM) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 6/6 |

* = false positive (model mentioned glob_system in text, didn't call it)

## Conclusion

**LFM2-8B is not a general tool-calling model.** The YES/NO + CodeAct combo only helps for explicit python tasks. For a general coding agent on CPU, use MiniCPM-V-4.6 or Qwen 2.5 3B+.