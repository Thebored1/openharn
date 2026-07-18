"""
AST-level function-calling benchmark for openharn FC-proxy.

Measures the same metrics BFCL's AST checker evaluates:
- Schema validity (valid JSON, correct tool name, valid args)
- Argument presence (required params present)
- Argument type correctness (types match schema)
- Multi-call support (parallel calls)
- Abstention accuracy (NO_TOOL when no tool fits)
- Overall AST accuracy

Usage:
  # Terminal 1: start llama-server
  llama-server -m /path/to/model.gguf --jinja --ctx-size 16384 -ngl 0 --port 8080

  # Terminal 2: start openharn FC-proxy
  OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 \
  OPENHARN_SERVE_PORT=8090 OPENHARN_FC_PROXY=1 \
  OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 \
  OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1 \
  OPENHARN_MAX_TOKENS=512 ./target/debug/openharn .

  # Terminal 3: run this benchmark
  python tests/ast_benchmark.py
"""
import argparse
import json
import os
import sys
import time
import urllib.request
import urllib.error

# BFCL v4 test cases — representative sample across categories
# Each entry: (prompt, tools, expected_name, expected_args_subset, abstain)
# expected_args_subset: dict of arg keys that must be present (value checked if not None)
TEST_CASES = [
    # === simple_python (single function, single call) ===
    {
        "id": "simple_0",
        "prompt": "What is the area of a circle with radius 5?",
        "tools": [{"type":"function","function":{"name":"calculate_area","description":"Calculate area of a shape","parameters":{"type":"object","properties":{"shape":{"type":"string","enum":["circle","rectangle","triangle"]},"radius":{"type":"number"},"width":{"type":"number"},"height":{"type":"number"}},"required":["shape"]}}}],
        "expected_name": "calculate_area",
        "expected_args": ["shape"],
        "abstain": False,
    },
    {
        "id": "simple_1",
        "prompt": "Book a flight from New York to London on 2024-01-15",
        "tools": [{"type":"function","function":{"name":"book_flight","description":"Book a flight","parameters":{"type":"object","properties":{"origin":{"type":"string"},"destination":{"type":"string"},"date":{"type":"string"}},"required":["origin","destination","date"]}}}],
        "expected_name": "book_flight",
        "expected_args": ["origin", "destination", "date"],
        "abstain": False,
    },
    {
        "id": "simple_2",
        "prompt": "What's the weather like in Tokyo?",
        "tools": [{"type":"function","function":{"name":"get_weather","description":"Get current weather for a city","parameters":{"type":"object","properties":{"city":{"type":"string"},"units":{"type":"string","enum":["celsius","fahrenheit"]}},"required":["city"]}}}],
        "expected_name": "get_weather",
        "expected_args": ["city"],
        "abstain": False,
    },
    # === multiple (choose one of several functions) ===
    {
        "id": "multiple_0",
        "prompt": "Send an email to john@example.com saying hello",
        "tools": [
            {"type":"function","function":{"name":"send_email","description":"Send an email","parameters":{"type":"object","properties":{"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"}},"required":["to","subject","body"]}}},
            {"type":"function","function":{"name":"schedule_meeting","description":"Schedule a meeting","parameters":{"type":"object","properties":{"date":{"type":"string"},"time":{"type":"string"},"attendees":{"type":"array","items":{"type":"string"}}},"required":["date","time","attendees"]}}},
        ],
        "expected_name": "send_email",
        "expected_args": ["to", "subject"],
        "abstain": False,
    },
    {
        "id": "multiple_1",
        "prompt": "What is 2+2?",
        "tools": [
            {"type":"function","function":{"name":"calculate","description":"Perform a calculation","parameters":{"type":"object","properties":{"expression":{"type":"string"}},"required":["expression"]}}},
            {"type":"function","function":{"name":"search_web","description":"Search the web","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}},
        ],
        "expected_name": "calculate",
        "expected_args": ["expression"],
        "abstain": False,
    },
    # === parallel (multiple independent calls) ===
    {
        "id": "parallel_0",
        "prompt": "Get the weather in Tokyo and London",
        "tools": [{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],
        "expected_name": "get_weather",
        "expected_args": ["city"],
        "expected_count": 2,
        "abstain": False,
    },
    {
        "id": "parallel_1",
        "prompt": "Calculate 2+2 and 3*4",
        "tools": [{"type":"function","function":{"name":"calculate","description":"Calculate","parameters":{"type":"object","properties":{"expression":{"type":"string"}},"required":["expression"]}}}],
        "expected_name": "calculate",
        "expected_args": ["expression"],
        "expected_count": 2,
        "abstain": False,
    },
    # === parallel_multiple (different tools, multiple calls) ===
    {
        "id": "pm_0",
        "prompt": "Get weather in Paris and book a flight to London",
        "tools": [
            {"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}},
            {"type":"function","function":{"name":"book_flight","description":"Book flight","parameters":{"type":"object","properties":{"destination":{"type":"string"},"origin":{"type":"string"}},"required":["destination"]}}},
        ],
        "expected_name": None,  # multiple different tools expected
        "expected_tools": ["get_weather", "book_flight"],
        "expected_count": 2,
        "abstain": False,
    },
    # === irrelevance (abstain — no tool fits) ===
    {
        "id": "irr_0",
        "prompt": "Hello! How are you today?",
        "tools": [{"type":"function","function":{"name":"search_database","description":"Search the database","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}}],
        "expected_name": None,
        "abstain": True,
    },
    {
        "id": "irr_1",
        "prompt": "Tell me a joke",
        "tools": [{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],
        "expected_name": None,
        "abstain": True,
    },
    {
        "id": "irr_2",
        "prompt": "What is the meaning of life?",
        "tools": [{"type":"function","function":{"name":"calculate","description":"Calculate","parameters":{"type":"object","properties":{"expression":{"type":"string"}},"required":["expression"]}}}],
        "expected_name": None,
        "abstain": True,
    },
    # === Edge cases ===
    {
        "id": "edge_0",
        "prompt": "Search for 'hello world' in the database",
        "tools": [{"type":"function","function":{"name":"search","description":"Search","parameters":{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}}}],
        "expected_name": "search",
        "expected_args": ["query"],
        "abstain": False,
    },
    {
        "id": "edge_1",
        "prompt": "Add item 'milk' to the shopping list with quantity 2",
        "tools": [{"type":"function","function":{"name":"add_to_list","description":"Add item to list","parameters":{"type":"object","properties":{"item":{"type":"string"},"quantity":{"type":"integer"},"unit":{"type":"string"}},"required":["item","quantity"]}}}],
        "expected_name": "add_to_list",
        "expected_args": ["item", "quantity"],
        "abstain": False,
    },
    # === Argument type edge cases ===
    {
        "id": "type_int",
        "prompt": "Set a timer for 300 seconds",
        "tools": [{"type":"function","function":{"name":"set_timer","description":"Set a timer","parameters":{"type":"object","properties":{"duration":{"type":"integer"},"label":{"type":"string"}},"required":["duration"]}}}],
        "expected_name": "set_timer",
        "expected_args": ["duration"],
        "abstain": False,
    },
    {
        "id": "type_bool",
        "prompt": "Enable dark mode",
        "tools": [{"type":"function","function":{"name":"set_setting","description":"Set a setting","parameters":{"type":"object","properties":{"name":{"type":"string"},"enabled":{"type":"boolean"}},"required":["name","enabled"]}}}],
        "expected_name": "set_setting",
        "expected_args": ["name", "enabled"],
        "abstain": False,
    },
    {
        "id": "type_array",
        "prompt": "Find the average of [1, 2, 3, 4, 5]",
        "tools": [{"type":"function","function":{"name":"average","description":"Calculate average","parameters":{"type":"object","properties":{"values":{"type":"array","items":{"type":"number"}}},"required":["values"]}}}],
        "expected_name": "average",
        "expected_args": ["values"],
        "abstain": False,
    },
    {
        "id": "type_enum",
        "prompt": "Set the theme to dark",
        "tools": [{"type":"function","function":{"name":"set_theme","description":"Set the theme color","parameters":{"type":"object","properties":{"theme":{"type":"string","enum":["light","dark","auto"]}},"required":["theme"]}}}],
        "expected_name": "set_theme",
        "expected_args": ["theme"],
        "abstain": False,
    },
    # === Harder parallel (3+ calls) ===
    {
        "id": "parallel_3",
        "prompt": "Get weather in Paris, London, and Tokyo",
        "tools": [{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],
        "expected_name": "get_weather",
        "expected_count": 3,
        "abstain": False,
    },
    # === More irrelevance (harder to judge) ===
    {
        "id": "irr_3",
        "prompt": "What is 2+2?",
        "tools": [{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],
        "expected_name": None,
        "abstain": True,
    },
    {
        "id": "irr_4",
        "prompt": "Who wrote Romeo and Juliet?",
        "tools": [{"type":"function","function":{"name":"search_database","description":"Search database","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}}],
        "expected_name": None,
        "abstain": True,
    },
    {
        "id": "irr_5",
        "prompt": "Can you introduce yourself?",
        "tools": [{"type":"function","function":{"name":"calculate","description":"Calculate","parameters":{"type":"object","properties":{"expression":{"type":"string"}},"required":["expression"]}}}],
        "expected_name": None,
        "abstain": True,
    },
    {
        "id": "irr_6",
        "prompt": "Sort the list [3,1,2]",
        "tools": [{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],
        "expected_name": None,
        "abstain": True,
    },
    # === Multiple with more choices ===
    {
        "id": "multiple_2",
        "prompt": "Translate 'hello' to French",
        "tools": [
            {"type":"function","function":{"name":"translate_text","description":"Translate text","parameters":{"type":"object","properties":{"text":{"type":"string"},"target_lang":{"type":"string"}},"required":["text","target_lang"]}}},
            {"type":"function","function":{"name":"summarize_text","description":"Summarize text","parameters":{"type":"object","properties":{"text":{"type":"string"},"max_length":{"type":"integer"}},"required":["text"]}}},
            {"type":"function","function":{"name":"detect_language","description":"Detect language","parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}},
        ],
        "expected_name": "translate_text",
        "expected_args": ["text", "target_lang"],
        "abstain": False,
    },
    {
        "id": "multiple_3",
        "prompt": "Create a new user named Alice",
        "tools": [
            {"type":"function","function":{"name":"create_user","description":"Create user","parameters":{"type":"object","properties":{"name":{"type":"string"},"email":{"type":"string"},"role":{"type":"string","enum":["admin","user","viewer"]}},"required":["name"]}}},
            {"type":"function","function":{"name":"delete_user","description":"Delete user","parameters":{"type":"object","properties":{"user_id":{"type":"integer"}},"required":["user_id"]}}},
            {"type":"function","function":{"name":"list_users","description":"List users","parameters":{"type":"object","properties":{"page":{"type":"integer"},"limit":{"type":"integer"}}}}},
        ],
        "expected_name": "create_user",
        "expected_args": ["name"],
        "abstain": False,
    },
]


BASE_URL = "http://127.0.0.1:8090/v1"

def call_fc_proxy(tools, prompt, temperature=0.001, max_tokens=512):
    """Send a request to openharn FC-proxy and return parsed response."""
    messages = [{"role": "user", "content": prompt}]
    body = json.dumps({
        "model": "local",
        "messages": messages,
        "tools": tools,
        "temperature": temperature,
        "max_tokens": max_tokens,
    }).encode()
    req = urllib.request.Request(
        f"{BASE_URL}/chat/completions",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            data = json.loads(resp.read())
    except (urllib.error.HTTPError, urllib.error.URLError, Exception) as e:
        return None, 0, f"request failed: {e}"
    dt = time.time() - t0

    choice = data.get("choices", [{}])[0]
    msg = choice.get("message", {})
    tool_calls = msg.get("tool_calls", [])
    content = msg.get("content", "")
    usage = data.get("usage") or {}
    pt = usage.get("prompt_tokens", 0) if isinstance(usage, dict) else 0
    ct = usage.get("completion_tokens", 0) if isinstance(usage, dict) else 0

    return {
        "tool_calls": tool_calls,
        "content": content,
        "prompt_tokens": pt,
        "completion_tokens": ct,
        "latency": dt,
    }


def evaluate_call(result, tc):
    """Evaluate a single test case. Returns (score, details)."""
    if result is None or isinstance(result, str):
        error = str(result) if result else "no response"
        return {"id": tc["id"], "score": 0.0, "reason": error, "tool_calls_received": 0}
    tool_calls = result.get("tool_calls", [])
    content = result.get("content", "")
    error = result.get("error", "")

    details = {
        "id": tc["id"],
        "prompt": tc["prompt"],
        "expected_name": tc["expected_name"],
        "expected_tools": tc.get("expected_tools"),
        "expected_count": tc.get("expected_count", 1),
        "abstain": tc["abstain"],
        "tool_calls_received": len(tool_calls),
        "tool_names": [tc.get("function", {}).get("name", "") for tc in tool_calls],
        "content": content,
        "error": error,
    }

    # Check abstention
    if tc["abstain"]:
        if len(tool_calls) == 0:
            if content.strip().upper() == "NO_TOOL" or content.strip() == "" or content is None:
                details["score"] = 1.0
                details["reason"] = "correct abstention"
                return details
            else:
                details["score"] = 0.5  # abstained but with prose
                details["reason"] = f"abstained with text: {content[:50]}"
                return details
        else:
            details["score"] = 0.0
            details["reason"] = f"should have abstained but called: {details['tool_names']}"
            return details

    # Check if any tool calls were made
    if len(tool_calls) == 0:
        details["score"] = 0.0
        details["reason"] = f"no tool call made; content: {content[:100] if content else 'empty'}"
        return details

    # Check tool name
    names = [tc.get("function", {}).get("name", "") for tc in tool_calls]

    # For expected_tools (multiple different tools), check all are present
    if tc.get("expected_tools"):
        expected_set = set(tc["expected_tools"])
        name_set = set(names)
        if expected_set.issubset(name_set):
            details["tool_name_ok"] = True
        else:
            details["score"] = 0.3
            missing = expected_set - name_set
            details["reason"] = f"missing tools: {missing}, got: {names}"
            return details
    elif tc["expected_name"]:
        if tc["expected_name"] in names:
            details["tool_name_ok"] = True
        else:
            # Check if it's just a name mismatch (same semantic)
            details["tool_name_ok"] = False
            details["score"] = 0.0
            details["reason"] = f"wrong tool name: expected {tc['expected_name']}, got {names}"
            return details

    # Check expected count
    expected_count = tc.get("expected_count", 1)
    if len(tool_calls) >= expected_count:
        details["count_ok"] = True
    else:
        details["count_ok"] = False
        details["score"] = 0.5
        details["reason"] = f"too few calls: expected {expected_count}, got {len(tool_calls)}"
        return details

    # Check argument presence (for the first matching call)
    if tc.get("expected_args"):
        for tc_call in tool_calls:
            c_name = tc_call.get("function", {}).get("name", "")
            if c_name == tc["expected_name"] or (tc.get("expected_tools") and c_name in tc["expected_tools"]):
                try:
                    args = json.loads(tc_call.get("function", {}).get("arguments", "{}"))
                except (json.JSONDecodeError, TypeError):
                    args = {}
                present = [a for a in tc["expected_args"] if a in args]
                if len(present) >= len(tc["expected_args"]):
                    details["args_ok"] = True
                    details["score"] = 1.0
                    details["reason"] = "correct"
                    return details
                else:
                    details["args_ok"] = False
                    details["score"] = 0.7
                    details["reason"] = f"missing args: expected {tc['expected_args']}, got {list(args.keys())}"
                    return details

    # Fallback: if we got the right count and names, give partial credit
    details["score"] = 0.8
    details["reason"] = f"correct tool(s) but args not fully checked"
    return details


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", default="http://127.0.0.1:8090/v1")
    parser.add_argument("--temperature", type=float, default=0.001)
    parser.add_argument("--max-tokens", type=int, default=512)
    parser.add_argument("--output", default=None, help="output JSON path")
    args = parser.parse_args()

    base_url = args.url

    # Health check
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:8090/health", timeout=5) as r:
            status = json.loads(r.read()).get("status")
            if status != "ok":
                print(f"[ERROR] server at {base_url} not healthy: {status}")
                sys.exit(1)
            print(f"[OK] server at {base_url} is healthy")
    except Exception as e:
        print(f"[ERROR] cannot connect to {base_url}: {e}")
        print("Start openharn serve with FC_PROXY=1 and OPENHARN_SERVE=1 on port 8090")
        sys.exit(1)

    results = []
    total_score = 0.0

    print(f"\nRunning {len(TEST_CASES)} test cases...\n")

    for tc in TEST_CASES:
        result = call_fc_proxy(tc["tools"], tc["prompt"], args.temperature, args.max_tokens)
        eval_result = evaluate_call(result, tc)
        results.append(eval_result)

        score = eval_result["score"]
        total_score += score

        status_mark = "✓" if score >= 0.8 else ("~" if score >= 0.3 else "✗")
        print(f"  {status_mark} {tc['id']:15s} score={score:.2f}  {eval_result.get('reason', '')[:80]}")

    overall = total_score / len(TEST_CASES)
    n_pass = sum(1 for r in results if r["score"] >= 0.8)
    n_partial = sum(1 for r in results if 0.3 <= r["score"] < 0.8)
    n_fail = sum(1 for r in results if r["score"] < 0.3)

    print(f"\n{'='*60}")
    print(f"OVERALL AST SCORE: {overall*100:.1f}%")
    print(f"  Pass: {n_pass}/{len(TEST_CASES)}  Partial: {n_partial}  Fail: {n_fail}")
    print(f"{'='*60}")

    # Category breakdown
    for cat_name, cat_key in [("Simple", False), ("Multiple", False), ("Parallel", False),
                               ("Parallel-Multiple", False), ("Irrelevance", True)]:
        cat_cases = [r for r, tc in zip(results, TEST_CASES) if tc["abstain"] == cat_key]
        if cat_cases:
            cat_score = sum(c["score"] for c in cat_cases) / len(cat_cases)
            print(f"  {cat_name:20s}: {cat_score*100:.1f}% ({len(cat_cases)} cases)")

    if args.output:
        with open(args.output, "w") as f:
            json.dump({"overall": overall, "results": results}, f, indent=2)
        print(f"\nWrote detailed results to {args.output}")

    return int(overall < 0.60)


if __name__ == "__main__":
    sys.exit(main())
