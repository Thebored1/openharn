//! The whole harness, in one place: a blocking loop against any OpenAI-compatible
//! endpoint. It keeps the REAL conversation (system, user, assistant-with-tool_calls,
//! and tool RESULTS) so the model always has coherent context, streams the
//! assistant's text live, dispatches tool calls to `tools`, and stops when the
//! model replies with no tool call.
//!
//! Deliberately not a framework: the entire agent behaviour is visible and ours to
//! control (streaming, tool parsing, context, stop conditions).

use crate::tools;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};

/// One OpenAI-compatible endpoint. Local (`llama-server`, no key) or a cloud
/// provider (base_url + key) — same loop either way.
pub struct Config {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub max_turns: usize,
    pub temperature: f64,
}

const SYSTEM: &str = include_str!("prompt.txt");

/// Rough char budget for the running conversation. ~4 chars/token, so ~16000
/// chars ≈ 4000 tokens — leaves ample room for the system prompt, the tool
/// schemas, and the reply inside an 8k context. Shrunk further if the server
/// still rejects (see the 400 handler).
const HISTORY_BUDGET: usize = 16_000;

/// A single tool result can be huge (a whole-system glob returns hundreds of
/// paths). Cap it so one result can't blow the context on its own.
const TOOL_RESULT_CAP: usize = 4_000;

fn cap_result(mut s: String) -> String {
    if s.chars().count() > TOOL_RESULT_CAP {
        s = s.chars().take(TOOL_RESULT_CAP).collect();
        s.push_str("\n…[result truncated — narrow your search (a more specific pattern) if you need more]");
    }
    s
}

/// Trim the conversation to fit `max_chars`, always keeping the system message and
/// dropping OLDEST whole turns first (a user message plus the assistant/tool
/// messages that follow it), so a tool result is never orphaned from its call.
fn fit_context(history: &mut Vec<Value>, max_chars: usize) {
    let total = |h: &[Value]| -> usize { h.iter().map(|m| m.to_string().len()).sum() };
    while total(history) > max_chars && history.len() > 3 {
        // history[0] is system; drop the turn starting at index 1.
        let mut end = 2;
        while end < history.len() && history[end]["role"] != "user" {
            end += 1;
        }
        history.drain(1..end);
    }
}

/// Run one user request to completion, mutating `history` (the live conversation)
/// and `session` (read-tracking) in place so the next request keeps full context.
pub fn run(cfg: &Config, history: &mut Vec<Value>, session: &mut tools::Session, user: &str) {
    if history.is_empty() {
        history.push(json!({ "role": "system", "content": SYSTEM }));
    }
    history.push(json!({ "role": "user", "content": user }));

    // pool_max_idle_per_host(0): never reuse a keep-alive connection — after a
    // streamed (SSE) response the pooled socket can be left half-consumed, and the
    // NEXT request on it fails at send ("error sending request"). A fresh
    // connection each turn avoids that entirely.
    let client = reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(0)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    // Spiral guard: a small model that repeats the exact same call is stuck, not
    // thorough. Track calls made this run and short-circuit exact repeats.
    let mut seen_calls = std::collections::HashSet::<String>::new();
    let mut budget = HISTORY_BUDGET;
    // Circuit breaker: if the model keeps re-issuing calls it already made, it's
    // stuck. The soft "you already did this" nudge isn't always enough for a tiny
    // model, so hard-stop after a few repeats instead of burning every turn.
    let mut repeats = 0usize;
    // Reasoning-off: with OPENHARN_NO_THINK set, prime each request with a closed
    // <think></think> assistant turn so a hybrid-thinking model (LFM2.5) continues from
    // an already-finished think state and skips reasoning — much faster on CPU. The
    // prefill is sent only, never stored in `history`.
    let no_think = std::env::var_os("OPENHARN_NO_THINK").is_some();

    for _ in 0..cfg.max_turns {
        // Keep the conversation within the model's context before every request.
        fit_context(history, budget);
        let mut messages = Value::Array(history.clone());
        if no_think {
            if let Some(arr) = messages.as_array_mut() {
                arr.push(json!({ "role": "assistant", "content": "<think></think>" }));
            }
        }
        let body = json!({
            "model": cfg.model,
            "messages": messages,
            "tools": tools::schemas(),
            "tool_choice": "auto",
            "temperature": cfg.temperature,
            "stream": true,
            // final chunk carries usage (and llama-server adds `timings`) → tok/s
            "stream_options": { "include_usage": true },
        });
        // Send with one retry — a transient connection blip (server briefly busy,
        // a reset socket) resolves on a fresh connection.
        let resp = {
            let mut attempt = 0;
            loop {
                let mut req = client.post(&url).json(&body);
                if let Some(k) = &cfg.api_key {
                    req = req.bearer_auth(k);
                }
                match req.send() {
                    Ok(r) => break r,
                    Err(_) if attempt == 0 => {
                        attempt += 1;
                        std::thread::sleep(std::time::Duration::from_millis(400));
                    }
                    Err(e) => {
                        println!("[error] request failed after retry: {e}");
                        return;
                    }
                }
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().unwrap_or_default();
            // Context overflow: shrink the budget and retry this turn instead of dying.
            if status.as_u16() == 400 && txt.contains("context") && budget > 4_000 {
                budget /= 2;
                continue;
            }
            println!("[error] HTTP {status}: {txt}");
            return;
        }

        let (mut content, mut tool_calls) = stream_response(resp, no_think);

        // Fallback tool-call parse: some models (e.g. Granite 3.x) emit a valid
        // structured call as TEXT — `<tool_call>[{"name":…,"arguments":{…}}]` — that the
        // server's parser (expecting a different trigger) leaves in `content`. Recover it
        // so the harness dispatches the tool instead of stalling. Only runs when the
        // native parse produced nothing, so it never overrides a real answer.
        if tool_calls.is_empty() {
            if let Some(parsed) = parse_text_tool_calls(&content) {
                tool_calls = parsed;
                content.clear(); // the content WAS the call, not an answer
            }
        }

        // Record the assistant turn so the next turn's context stays coherent (and
        // the KV-cache prefix stable).
        let mut assistant = json!({ "role": "assistant" });
        assistant["content"] = if content.is_empty() { Value::Null } else { json!(content) };
        if !tool_calls.is_empty() {
            assistant["tool_calls"] = json!(tool_calls);
        }
        history.push(assistant);

        if tool_calls.is_empty() {
            return; // text was already streamed live
        }

        for tc in &tool_calls {
            let id = tc["id"].as_str().unwrap_or("");
            let name = tc["function"]["name"].as_str().unwrap_or("");
            let args_raw = tc["function"]["arguments"].as_str().unwrap_or("{}");
            let args: Value = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));
            println!("  \x1b[2m· {name} {}\x1b[0m", compact(&args));
            let result = if !seen_calls.insert(format!("{name}:{}", args)) {
                repeats += 1;
                // exact repeat — don't re-execute; tell the model to change course
                "You already made this exact tool call and saw its result. Repeating it will not change anything. Take a DIFFERENT action, or answer the user with what you know (including telling them something was not found)."
                    .to_string()
            } else {
                session.execute(name, &args)
            };
            history.push(json!({ "role": "tool", "tool_call_id": id, "content": cap_result(result) }));
        }
        if repeats >= 3 {
            println!("\n[stopped: the model kept repeating the same tool call — it's stuck. Try rephrasing.]");
            return;
        }
    }
    println!("[stopped: hit max turns ({})]", cfg.max_turns);
}

/// Read the SSE stream: print assistant text live (thinking dimmed), accumulate
/// the (possibly chunked) tool-call deltas, and return the full text + assembled
/// tool calls. Prints a tok/s stats line from llama-server's timings (or usage +
/// wall time as a fallback).
fn stream_response(resp: reqwest::blocking::Response, no_think: bool) -> (String, Vec<Value>) {
    let mut content = String::new();
    let mut tool_calls: Vec<Value> = vec![];
    let mut printed = false;
    let mut thinking = false; // currently inside dim reasoning output (show-thinking mode)
    // By default we DON'T dump the model's raw chain-of-thought — for a small model it's a
    // verbose wall of text. Instead we collapse it into a single live in-place meter and
    // show only the answer. Set OPENHARN_SHOW_THINKING=1 to see the raw reasoning.
    let show_thinking = std::env::var_os("OPENHARN_SHOW_THINKING").is_some();
    let mut live = false; // a live one-line thinking meter is currently on screen
    let mut answer_started = false; // no-think: leaked reasoning closed, now streaming the answer
    let mut hide_call: Option<bool> = None; // Some(true)=content is a text tool-call → don't display
    let started = std::time::Instant::now();
    let mut completion_tokens: Option<u64> = None;
    let mut server_tps: Option<f64> = None;
    // Per-phase accounting: thinking (reasoning_content) vs reply (content). We count
    // one token per non-empty stream chunk (llama-server emits a chunk per token) and
    // stamp when each phase starts, so we can report separate tok/s + seconds for each.
    let mut think_tokens = 0usize;
    let mut reply_tokens = 0usize;
    let mut think_start: Option<std::time::Instant> = None;
    let mut reply_start: Option<std::time::Instant> = None;
    let mut last_meter = started;
    let reader = BufReader::new(resp);
    for line in reader.lines().map_while(Result::ok) {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        // usage/timings ride the final chunk (choices empty) when include_usage is set
        if let Some(u) = chunk["usage"]["completion_tokens"].as_u64() {
            completion_tokens = Some(u);
        }
        if let Some(t) = chunk["timings"]["predicted_per_second"].as_f64() {
            server_tps = Some(t);
        }
        let delta = &chunk["choices"][0]["delta"];
        // hybrid-thinking models stream reasoning separately
        if let Some(r) = delta["reasoning_content"].as_str() {
            if !r.is_empty() {
                think_start.get_or_insert_with(std::time::Instant::now);
                think_tokens += 1;
                if show_thinking {
                    if !thinking {
                        print!("\x1b[2m");
                        thinking = true;
                    }
                    print!("{r}");
                    io::stdout().flush().ok();
                    printed = true;
                } else if last_meter.elapsed().as_millis() >= 120 {
                    // collapse the chain-of-thought into ONE live, in-place line
                    let secs = think_start.unwrap().elapsed().as_secs_f64().max(0.001);
                    print!(
                        "\r\x1b[2m  thinking… {think_tokens} tok · {secs:.1}s · {:.0} tok/s\x1b[0m\x1b[K",
                        think_tokens as f64 / secs
                    );
                    io::stdout().flush().ok();
                    live = true;
                    last_meter = std::time::Instant::now();
                }
            }
        }
        if let Some(t) = delta["content"].as_str() {
            if !t.is_empty() {
                content.push_str(t);
                if no_think {
                    // Reasoning-off: the model still leaks a (shortened) chain-of-thought
                    // into the content with stray <think> tags (an echoed <think></think>
                    // then reasoning then a real </think>). Suppress it behind the meter, and
                    // once the reasoning closes (2nd </think>) start streaming the answer live.
                    if answer_started {
                        reply_tokens += 1;
                        print!("{t}");
                        io::stdout().flush().ok();
                        printed = true;
                    } else if content.matches("</think>").count() >= 2 {
                        answer_started = true;
                        if live {
                            print!("\r\x1b[K");
                            live = false;
                        }
                        reply_start.get_or_insert_with(std::time::Instant::now);
                        let ans = strip_think(&content); // answer produced so far
                        if !ans.is_empty() {
                            print!("{ans}");
                            io::stdout().flush().ok();
                            printed = true;
                        }
                    } else {
                        think_start.get_or_insert_with(std::time::Instant::now);
                        think_tokens += 1;
                        if last_meter.elapsed().as_millis() >= 120 {
                            let secs = think_start.unwrap().elapsed().as_secs_f64().max(0.001);
                            print!(
                                "\r\x1b[2m  thinking… {think_tokens} tok · {secs:.1}s · {:.0} tok/s\x1b[0m\x1b[K",
                                think_tokens as f64 / secs
                            );
                            io::stdout().flush().ok();
                            live = true;
                            last_meter = std::time::Instant::now();
                        }
                    }
                } else {
                    if live {
                        print!("\r\x1b[K"); // erase the live thinking line before the answer
                        live = false;
                    }
                    if thinking {
                        print!("\x1b[0m\n\n"); // close dim thinking before the answer
                        thinking = false;
                    }
                    reply_start.get_or_insert_with(std::time::Instant::now);
                    reply_tokens += 1;
                    // Suppress a text tool-call (Granite <tool_call>/<|tool_call|>) from the
                    // display — run() parses & dispatches it. Buffer the head until we can tell.
                    match hide_call {
                        Some(true) => {} // a tool call in text form — keep it off-screen
                        Some(false) => {
                            print!("{t}");
                            io::stdout().flush().ok();
                            printed = true;
                        }
                        None => {
                            let tr = content.trim_start();
                            if tr.starts_with("<|tool_call|>") || tr.starts_with("<tool_call>") {
                                hide_call = Some(true);
                            } else if !"<|tool_call|>".starts_with(tr) && !"<tool_call>".starts_with(tr) {
                                hide_call = Some(false);
                                print!("{content}"); // flush the buffered head, then stream
                                io::stdout().flush().ok();
                                printed = true;
                            }
                            // else: still an ambiguous prefix — keep buffering, print nothing
                        }
                    }
                }
            }
        }
        if let Some(tcs) = delta["tool_calls"].as_array() {
            for tc in tcs {
                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                while tool_calls.len() <= idx {
                    tool_calls.push(json!({"id":"","type":"function","function":{"name":"","arguments":""}}));
                }
                let slot = &mut tool_calls[idx];
                if let Some(id) = tc["id"].as_str() {
                    if !id.is_empty() {
                        slot["id"] = json!(id);
                    }
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    if !n.is_empty() {
                        slot["function"]["name"] = json!(n);
                    }
                }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    let prev = slot["function"]["arguments"].as_str().unwrap_or("").to_string();
                    slot["function"]["arguments"] = json!(prev + a);
                }
            }
        }
    }
    // drop empty accumulator slots the model never filled
    tool_calls.retain(|t| !t["function"]["name"].as_str().unwrap_or("").is_empty());
    if live {
        print!("\r\x1b[K"); // erase the live thinking/working line
    }
    if thinking {
        print!("\x1b[0m"); // never leave the terminal dimmed
    }
    if no_think {
        let clean = strip_think(&content);
        if answer_started {
            if printed {
                println!(); // answer already streamed live; close the line
            }
        } else if !clean.is_empty() {
            println!("{clean}"); // reasoning never resolved to a streamed answer — print it
        }
        content = clean; // store only the clean answer in history
    } else if printed {
        println!();
    }
    let end = std::time::Instant::now();

    // Per-phase readout: thinking and reply each get their own token count, seconds,
    // and tok/s. Thinking spans first→last reasoning token (i.e. until the reply
    // starts, or the turn ends for a tool-only turn); reply spans first content
    // token→end.
    let mut parts: Vec<String> = Vec::new();
    if think_tokens > 0 {
        let ts = think_start.unwrap_or(started);
        let think_end = reply_start.unwrap_or(end);
        let secs = (think_end - ts).as_secs_f64().max(0.001);
        parts.push(format!(
            "think {think_tokens} tok · {secs:.1}s · {:.0} tok/s",
            think_tokens as f64 / secs
        ));
    }
    if reply_tokens > 0 {
        let rs = reply_start.unwrap_or(started);
        let secs = (end - rs).as_secs_f64().max(0.001);
        parts.push(format!(
            "reply {reply_tokens} tok · {secs:.1}s · {:.0} tok/s",
            reply_tokens as f64 / secs
        ));
    }
    // Tool-only turn (no streamed text): fall back to server usage/timings.
    if parts.is_empty() {
        if let Some(n) = completion_tokens {
            let tps = server_tps.unwrap_or(n as f64 / started.elapsed().as_secs_f64().max(0.001));
            parts.push(format!("{n} tok · {tps:.1} tok/s"));
        }
    }
    if !parts.is_empty() {
        for p in &parts {
            println!("\x1b[2m  {p}\x1b[0m");
        }
        println!("\x1b[2m  total {:.1}s\x1b[0m", (end - started).as_secs_f64());
    }
    (content, tool_calls)
}

/// Recover a structured tool call the server left as plain text. Handles the Granite
/// family shape — an optional `<tool_call>` / `<|tool_call|>` marker followed by a JSON
/// list `[{"name":…, "arguments":{…}}]` — and returns OpenAI-format tool_calls (with
/// `arguments` as a JSON string, as the dispatcher expects). Returns None if `content`
/// isn't a recognizable tool call, so a normal answer is never misread as one.
fn parse_text_tool_calls(content: &str) -> Option<Vec<Value>> {
    let mut s = content.trim();
    for marker in ["<|tool_call|>", "<tool_call>"] {
        if let Some(rest) = s.strip_prefix(marker) {
            s = rest;
            break;
        }
    }
    let s = s.trim();
    // isolate a JSON array `[…]` or a single object `{…}` (models emit both shapes)
    let open = s.find(['[', '{'])?;
    let is_arr = s.as_bytes()[open] == b'[';
    let close = if is_arr { s.rfind(']')? } else { s.rfind('}')? };
    if close < open {
        return None;
    }
    let val: Value = serde_json::from_str(&s[open..=close]).ok()?;
    let items: Vec<Value> = match val {
        Value::Array(a) => a,
        obj => vec![obj],
    };
    let mut calls = Vec::new();
    for (i, item) in items.iter().enumerate() {
        // tolerate an optional {"function": {…}} wrapper, and name/arguments vs
        // name/parameters (Granite echoes the schema key `parameters`).
        let f = item.get("function").unwrap_or(item);
        let name = f.get("name").and_then(|v| v.as_str())?;
        let args = f.get("arguments").or_else(|| f.get("parameters"));
        let args_str = match args {
            Some(a) if a.is_string() => a.as_str().unwrap_or("{}").to_string(),
            Some(a) if !a.is_null() => a.to_string(),
            _ => "{}".to_string(),
        };
        calls.push(json!({
            "id": format!("call_{i}"),
            "type": "function",
            "function": { "name": name, "arguments": args_str }
        }));
    }
    if calls.is_empty() { None } else { Some(calls) }
}

/// In reasoning-off mode a hybrid-thinking model still leaks a (shortened) chain of
/// thought into the content wrapped in stray `<think>…</think>` tags. Keep only the
/// real answer: everything after the last `</think>`, with any tags removed.
fn strip_think(s: &str) -> String {
    let tail = match s.rfind("</think>") {
        Some(i) => &s[i + "</think>".len()..],
        None => s,
    };
    tail.replace("<think>", "").replace("</think>", "").trim().to_string()
}

fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 100 {
        format!("{}…", s.chars().take(100).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_granite_text_tool_call() {
        // Granite emits a valid call as text that the server leaves in `content`.
        let c = r#"<tool_call>[{"arguments": {"pattern": "src/**/*.rs"}, "name": "glob"}]"#;
        let calls = parse_text_tool_calls(c).expect("should parse");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "glob");
        // arguments must be a JSON *string* (what the dispatcher re-parses)
        let args = calls[0]["function"]["arguments"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(args).unwrap()["pattern"], "src/**/*.rs");
    }

    #[test]
    fn parses_piped_marker_and_ignores_prose() {
        assert!(parse_text_tool_calls(r#"<|tool_call|>[{"name":"read","arguments":{"path":"a"}}]"#).is_some());
        // a normal answer must NOT be misread as a tool call
        assert!(parse_text_tool_calls("The src directory contains agent.rs and main.rs.").is_none());
        assert!(parse_text_tool_calls("").is_none());
    }

    #[test]
    fn parses_object_echo_with_parameters() {
        // Granite's other shape: a single object with a `function` wrapper and
        // schema-style `parameters` (filled with real values) instead of `arguments`.
        let c = r#"<tool_call>
{"type":"function","function":{"name":"glob","parameters":{"pattern":"src/**/*.rs","scope":"project"}}}
</tool_call>"#;
        let calls = parse_text_tool_calls(c).expect("should parse object echo");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "glob");
        let args: Value = serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["pattern"], "src/**/*.rs");
    }

    #[test]
    fn cap_result_truncates_and_notes() {
        let out = cap_result("z".repeat(10_000));
        assert!(out.chars().count() < TOOL_RESULT_CAP + 200, "len {}", out.chars().count());
        assert!(out.contains("truncated"));
        assert_eq!(cap_result("short".into()), "short");
    }

    #[test]
    fn fit_context_keeps_system_and_fits_budget() {
        let big = "x".repeat(5_000);
        let mut h = vec![
            json!({"role":"system","content":"sys"}),
            json!({"role":"user","content":big}),
            json!({"role":"assistant","content":"a1"}),
            json!({"role":"user","content":"q2"}),
            json!({"role":"assistant","content":"recent"}),
        ];
        fit_context(&mut h, 3_000);
        assert_eq!(h[0]["role"], "system", "system must survive");
        let total: usize = h.iter().map(|m| m.to_string().len()).sum();
        assert!(total <= 3_200, "must fit budget, got {total}");
        assert_eq!(h.last().unwrap()["content"], "recent", "recent turn kept");
    }

    // Dropping happens by whole turn, so a tool result can never be left without
    // its preceding assistant tool_call (which the server would reject).
    #[test]
    fn fit_context_never_orphans_a_tool_result() {
        let big = "y".repeat(6_000);
        let mut h = vec![
            json!({"role":"system","content":"sys"}),
            json!({"role":"user","content":"q1"}),
            json!({"role":"assistant","tool_calls":[{"id":"c1","function":{"name":"glob","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"c1","content":big}),
            json!({"role":"user","content":"q2"}),
            json!({"role":"assistant","content":"a2"}),
        ];
        fit_context(&mut h, 2_000);
        let orphan = h
            .iter()
            .enumerate()
            .any(|(i, m)| m["role"] == "tool" && (i == 0 || h[i - 1]["role"] != "assistant"));
        assert!(!orphan, "tool result orphaned: {h:?}");
    }
}
