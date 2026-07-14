# Adapting openharn for Myelin (and your own system)

This document shows how to take the **openharn** coding-agent harness and retarget it to a completely different domain: **Myelin**, a local notes app. The same pattern applies to any tool-using agent you want to build.

---

## The core idea

**openharn is not a framework.** It's a ~1,500-line blocking loop that:
1. Sends conversation history to an OpenAI-compatible endpoint
2. Streams the assistant's response (text + tool calls)
3. Executes tool calls via a `Session` trait object
4. Feeds results back, loops until the model stops calling tools

Everything domain-specific lives in **two places**:
- **Tools** — `tools.rs` (coding tools) or `myelin.rs` (notes tools)
- **System prompt** — `prompt.txt` (coding) or `MYELIN_SYSTEM` (notes)

The REPL (`main.rs`) and the HTTP server (`myelin.rs::server`) are just *delivery mechanisms* for the same loop.

---

## What Myelin changes

| Layer | openharn (coding) | Myelin (notes) |
|-------|-------------------|----------------|
| **State** | `Session { cwd, read_set, todos }` | `MyelinSession { note: String, todos }` |
| **Tools** | `read, write, edit, glob, grep, bash, webfetch, ...` | `write_note, edit_note, format_note, search_notes, web_search, todowrite, todoread` |
| **Prompt** | `prompt.txt` (opencode-style, coding agent) | `MYELIN_SYSTEM` (lean, identity + tool contract) |
| **Execution** | Filesystem ops (read-before-write guard) | In-memory string ops (anchored edit via `edit.rs`) |
| **Server** | REPL only | Optional HTTP server (`OPENHARN_MYELIN=1`) |

---

## Step-by-step adaptation

### 1. Define your session state

```rust
// src/myelin.rs
#[derive(Clone, Debug, Default)]
pub struct MyelinSession {
    pub note: String,          // the ONE open note
    todos: Vec<Value>,         // optional, mirrors openharn
}
```

No filesystem, no project root, no `read` set. The note **is** the state.

### 2. Implement your tools

Each tool is a pure function: `(session, args) -> (new_note, result_message)`.

```rust
impl MyelinSession {
    pub fn execute(&mut self, name: &str, args: &Value) -> (String, String) {
        match name {
            "write_note" => self.write_note(args),
            "edit_note"  => self.edit_note(args),
            "format_note" => self.format_note(args),
            "search_notes" => self.search_notes(args),
            "web_search" => self.web_search(args),
            "todowrite" => self.todowrite(args),
            "todoread" => self.todoread(),
            _ => (self.note.clone(), format!("Unknown tool: {name}")),
        }
    }

    fn write_note(&mut self, args: &Value) -> (String, String) {
        let content = args["content"].as_str().unwrap_or("");
        self.note = content.to_string();
        (self.note.clone(), format!("Note written ({} chars).", content.len()))
    }

    fn edit_note(&mut self, args: &Value) -> (String, String) {
        let find = args["find"].as_str().unwrap_or("");
        let replace = args["replace"].as_str().unwrap_or("");
        if find.is_empty() { return (self.note.clone(), "Error: 'find' cannot be empty.".into()); }
        if !self.note.contains(find) { return (self.note.clone(), "Error: 'find' not found.".into()); }
        match edit::replace(&self.note, find, replace, false) {
            Ok(updated) => { self.note = updated; (self.note.clone(), "Edit applied.".into()) }
            Err(e) => (self.note.clone(), format!("Edit failed: {e}")),
        }
    }
    // ... format_note, search_notes, web_search, todowrite, todoread
}
```

**Key point:** `edit_note` reuses openharn's anchored replacer (`edit::replace`) — the *same engine* that powers the coding agent's `edit` tool. You get whitespace/indentation tolerance for free.

### 3. Advertise schemas

```rust
pub fn myelin_schemas() -> Value {
    json!([
        {"type":"function","function":{
            "name":"edit_note",
            "description":"Replace the exact text `find` with `replace` in the open note (anchored edit). Preferred for small changes.",
            "parameters":{"type":"object","properties":{
                "find":{"type":"string"}, "replace":{"type":"string"}
            },"required":["find","replace"]}
        }},
        {"type":"function","function":{
            "name":"write_note",
            "description":"Set the ENTIRE open note body to `content` (empty string clears it). Use for fresh writes / full rewrites.",
            "parameters":{"type":"object","properties":{
                "content":{"type":"string"}
            },"required":["content"]}
        }},
        // ... format_note, search_notes, web_search, todowrite, todoread
    ])
}
```

Descriptions mirror the **bench's expectations** (see `myelin_bench.py`).

### 4. Write the lean system prompt

```rust
pub const MYELIN_SYSTEM: &str = r#"You are the assistant inside Myelin, a local notes app running on the user's own machine.
If asked who or what you are, say you are Myelin's built-in assistant — not ChatGPT, OpenAI, or IBM.
The currently OPEN note is given in the user message. Your tools only ever act on THAT open note;
you cannot create separate new notes.
- Prefer edit_note (replace an exact snippet) for small changes — never reprint the whole note.
- Use write_note only to set the whole body (a fresh note or a full rewrite).
- format_note for structural cleanups (remove headings/bold, change case, list conversions).
- For greetings, thanks, or general chat, reply in plain text and call NO tool.
- When asked to write what you found/researched, put the actual information into the note;
  never write a question or an "I will fetch..." promise as the note body."#;
```

**No few-shot examples.** The bench (and openharn's premise) shows that small models *over-generalize* from examples. Structure > prose.

### 5. (Optional) HTTP server for OpenAI-compatible clients

The bench (`myelin_bench.py`) talks to any `/v1/chat/completions` endpoint. openharn's `myelin.rs::server` provides one:

```bash
# Terminal 1: start Myelin server (proxies to upstream llama-server)
OPENHARN_MYELIN=1 OPENHARN_MYELIN_PORT=8090 OPENHARN_MYELIN_UPSTREAM=http://127.0.0.1:8080/v1 cargo run

# Terminal 2: run the bench against it
python myelin_bench.py --url http://127.0.0.1:8090/v1 --model myelin
```

The server:
- Issues a cookie (`myelin_sid`) per client
- Injects the current note into the **last user message** before proxying upstream
- Intercepts **deflection** after `web_search` ("I will now fetch...") and forces `write_note`

---

## Running the Myelin bench

```bash
# 1. Start upstream model (llama-server)
llama-server -m your-model.gguf --jinja --ctx-size 8192 -ngl 0 --port 8080

# 2. Start Myelin proxy (in another terminal)
OPENHARN_MYELIN=1 OPENHARN_MYELIN_UPSTREAM=http://127.0.0.1:8080/v1 cargo run

# 3. Run bench (points at Myelin proxy)
python myelin_bench.py --url http://127.0.0.1:8090/v1 --model myelin
```

The bench scores **outcomes** (the resulting note text), not which tool was used — so `edit_note` (anchored) and `write_note` (full rewrite) are judged fairly.

---

## Generalizing: how to build YOUR system on openharn

| Your domain | Your `Session` | Your tools | Your prompt |
|-------------|----------------|------------|-------------|
| **Coding agent** (openharn default) | `cwd`, `read_set`, `todos` | `read, write, edit, glob, grep, bash, ...` | `prompt.txt` |
| **Notes app** (Myelin) | `note: String`, `todos` | `write_note, edit_note, format_note, ...` | `MYELIN_SYSTEM` |
| **SQL explorer** | `conn: Pool`, `schema_cache` | `query, describe, explain, export` | "You are a read-only SQL analyst..." |
| **Browser automator** | `browser: Arc<Browser>`, `tabs` | `goto, click, type, extract, screenshot` | "You control a headless browser..." |
| **K8s operator** | `client: Client`, `namespace` | `get, apply, delete, logs, exec` | "You manage cluster resources..." |

**The checklist:**

1. **Define `YourSession`** — what persistent state does the agent need?
2. **Implement `execute(name, args)`** — each tool mutates `YourSession`, returns `(state, message)`.
3. **Write `your_schemas()`** — OpenAI function-calling JSON, descriptions matter (the model reads them).
4. **Write `YOUR_SYSTEM`** — identity + tool contract + "no tool for chit-chat" rule.
5. **Wire it**:
   - For REPL: swap `tools::Session` → `YourSession` in `main.rs`, pass `your_schemas()` to the loop.
   - For HTTP: copy `myelin.rs::server`, change the session type and `execute` call.
6. **Add bench cases** — encode your real failure modes as regression tests (like `myelin_bench.py` does).

---

## What you get for free

| Feature | Where it lives | Works for any domain |
|---------|----------------|---------------------|
| Streaming + tok/s meter | `agent.rs::stream_response` | ✅ |
| Context trimming (keep system, drop oldest turns) | `agent.rs::fit_context` | ✅ |
| Tool-call recovery (Granite `<tool_call>[...]`, `<|tool_call|>`) | `agent.rs::parse_text_tool_calls` | ✅ |
| Strict grammar (`OPENHARN_STRICT_TOOLS=1`) | `agent.rs::tool_grammar` | ✅ |
| Prompt-tools mode (no native tool API) | `agent.rs::flatten_for_prompt_tools` | ✅ |
| Narrow/read-only mode | `agent.rs` env vars | ✅ |
| Circuit breaker (repeat-call detection) | `agent.rs` `seen_calls` | ✅ |
| Anchored edit engine | `edit.rs` | ✅ (reuse for any string replacement) |
| Per-turn / total call limits | `agent.rs` `OPENHARN_MAX_CALLS`, `OPENHARN_TOTAL_MAX` | ✅ |

---

## Files to look at

| File | Purpose |
|------|---------|
| `src/myelin.rs` | Complete Myelin implementation (session, tools, schemas, prompt, server, tests) |
| `src/edit.rs` | Anchored replacer cascade — reusable for any exact-string-replace tool |
| `src/agent.rs` | The harness loop (streaming, context, recovery, grammar, limits) |
| `src/tools.rs` | Coding-agent tools (reference implementation) |
| `src/main.rs` | REPL entry point (also launches Myelin server via `OPENHARN_MYELIN=1`) |
| `tests/behavior.py` | Behavioral tests against a live model (pattern for your own bench) |

---

## License

The adaptation pattern is MIT (same as openharn). The edit engine (`edit.rs`) is a port of opencode's replacer under their MIT license — see `NOTICE` and `LICENSES/opencode-MIT.txt`.