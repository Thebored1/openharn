# Dynamic tool schemas

Tool schemas are **never hardcoded**. openharn advertises whatever tool descriptions the
caller provides. The 13-tool literal that shipped with earlier builds has been removed;
the only source of tools is the caller — per-request in serve mode, or a JSON file in the
REPL.

## Serve mode (`--serve` / `OPENHARN_SERVE=1`)

Tools come from each request's `tools` field (the standard OpenAI `tools` array). The
agent loop passes `&req["tools"]` straight to the harness, so a
`POST /v1/chat/completions` with:

```json
{
  "messages": [{"role": "user", "content": "find .rs files"}],
  "tools": [
    {"type": "function", "function": {
      "name": "glob",
      "description": "Find files by pattern",
      "parameters": {
        "type": "object",
        "properties": {"pattern": {"type": "string"}},
        "required": ["pattern"]
      }
    }}
  ]
}
```

will advertise exactly `glob` to the model. No `tools` field → chat-only (no tool
advertising).

FC-proxy mode (`OPENHARN_FC_PROXY=1`) already worked this way; the agent loop now does
too.

## REPL mode (default)

The REPL reads schemas from `OPENHARN_TOOLS_SCHEMA=<path.json>`. The file must contain a
valid OpenAI `tools` array:

```json
[
  {"type": "function", "function": {
    "name": "read",
    "description": "Read a file",
    "parameters": { "type": "object", "properties": {
      "path": {"type": "string"}
    }, "required": ["path"]}
  }},
  {"type": "function", "function": {
    "name": "bash",
    "description": "Run a shell command",
    "parameters": { "type": "object", "properties": {
      "command": {"type": "string"}
    }, "required": ["command"]}
  }}
]
```

If the env var is unset the REPL runs without tools (chat only).

A sample schema file for the 13 opencode-compatible tools is available at
`../notes/sample-tools.json`.

## Tool name → dispatch

The tool *advertisement* (schemas) is separate from tool *implementation*. openharn's
`Session::execute` dispatches by name to built-in handlers (`read`, `edit`, `bash`,
`glob`, `grep`, etc.). A schema whose `name` matches a handler works directly; an
unknown name returns an error result.

## Filtering still applies

`OPENHARN_TOOLS` / `OPENHARN_NARROW` still restrict which of the **caller-supplied**
schemas the model actually sees. They never invent schemas — they only subset what was
provided.

## Why

- **Model-agnostic**: the harness doesn't prescribe which tools exist. Any OpenAI-style
  tool description works.
- **Delegation**: an upstream client decides the agent's tool surface per request.
- **BFCL interop**: benchmarks send their own schemas; openharn returns reliable
  tool calls without owning the tool list.
- **Tight surface**: a caller that needs only `read`+`grep` sends only those two,
  and a weak model cannot hallucinate others.

## Architecture summary

```
Serve request  ──→ req["tools"] ──→ agent::run(schemas=…) ──→ body["tools"] = effective_schemas
                                                                         ↓
                                                              model calls by name
                                                                         ↓
                                                              Session::execute(name, args)
                                                                         ↓
                                                              returns result to model

REPL           ──→ OPENHARN_TOOLS_SCHEMA ──→ agent::run(schemas=…) ──→ (same path)
```
