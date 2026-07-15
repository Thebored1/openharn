//! The whole harness, in one place: a blocking loop against any OpenAI-compatible
//! endpoint. It keeps the REAL conversation (system, user, assistant-with-tool_calls,
//! and tool RESULTS) so the model always has coherent context, streams the
//! assistant's text live, dispatches tool calls to `tools`, and stops when the
//! model replies with no tool call.
//!
//! Deliberately not a framework: the entire agent behaviour is visible and ours to
//! control (streaming, tool parsing, context, stop conditions).

use crate::tools;
use crate::slm_harness;
use regex::Regex;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};

/// One OpenAI-compatible endpoint. Local (`llama-server`, no key) or a cloud
/// provider (base_url + key) — same loop either way.
#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub max_turns: usize,
    pub max_tokens: u32,
    pub temperature: f64,
    pub friendly_results: bool,
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

/// Classify user input as needing a tool (TOOL) or plain chat (CHAT).
/// Non-streaming, fast call. Shows result to the user.
fn run_intent_detection(
    cfg: &Config,
    client: &reqwest::blocking::Client,
    url: &str,
    user: &str,
) -> &'static str {
    let prompt = format!(
        "Classify the user's request. Reply with exactly one word: TOOL or CHAT.\n\
         CHAT = greeting, small talk, question about the system, request for explanation.\n\
         TOOL = needs to read/write/find/edit files, search code, run commands, fetch URLs.\n\n\
         Examples:\n\
         User: hello\nClassification: CHAT\n\
         User: what is 2+2?\nClassification: CHAT\n\
         User: explain how to use grep\nClassification: CHAT\n\
         User: what files are in src/\nClassification: TOOL\n\
         User: read the file main.rs\nClassification: TOOL\n\
         User: search for TODO in code\nClassification: TOOL\n\
         User: run cargo build\nClassification: TOOL\n\
         User: what does foo.txt contain?\nClassification: TOOL\n\
         User: what's in foo.txt?\nClassification: TOOL\n\
         User: does foo.txt exist?\nClassification: TOOL\n\
         User: what is the content of foo.txt\nClassification: TOOL\n\n\
         User: {user}\n\n\
         Classification:"
    );
    let request = json!({
        "model": cfg.model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.0,
        "max_tokens": 10,
        "stream": false,
    });
    let mut req = client.post(url).json(&request);
    if let Some(k) = &cfg.api_key {
        req = req.bearer_auth(k);
    }
    let text = req.send()
        .ok()
        .and_then(|r| r.json::<Value>().ok())
        .and_then(|v| v["choices"][0]["message"]["content"].as_str().map(|s| s.to_uppercase()))
        .unwrap_or_default();
    let is_tool = text.contains("TOOL");
    println!("[intent] {}", if is_tool { "TOOL — will call tools" } else { "CHAT — answering directly" });
    if is_tool { "TOOL" } else { "CHAT" }
}

/// Format tool result as a natural-language response. Takes the user's original
/// request + tool result, streams a friendly answer, and adds it to history.
fn run_friendly_response(
    cfg: &Config,
    client: &reqwest::blocking::Client,
    url: &str,
    user: &str,
    result: &str,
    history: &mut Vec<Value>,
) {
    let messages = vec![
        json!({"role": "system", "content": "You are a helpful coding assistant. Given the user's request and the tool result, answer naturally in plain text. CRITICAL: If the tool result contains an error message (like file not found), you MUST tell the user the file could not be found. NEVER guess or fabricate file contents."}),
        json!({"role": "user", "content": format!("User request: {user}\n\nTool result: {result}\n\nAnswer the user's question naturally.")}),
    ];
    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "temperature": cfg.temperature,
        "max_tokens": 1024,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    let resp = match client.post(url).json(&body).send() {
        Ok(r) => r,
        Err(e) => { println!("[friendly] request failed: {e}"); return; }
    };
    let (content, _tool_calls) = stream_response(resp, false, false);
    history.push(json!({"role": "assistant", "content": content}));
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
    let max_calls: usize = std::env::var("OPENHARN_MAX_CALLS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let total_max: usize = std::env::var("OPENHARN_TOTAL_MAX")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let mut repeats = 0usize;
    let mut total_calls = 0usize;
    let mut no_tools = false;
    // Reasoning-off: with OPENHARN_NO_THINK set, prime each request with a closed
    // <think></think> assistant turn so a hybrid-thinking model (LFM2.5) continues from
    // an already-finished think state and skips reasoning — much faster on CPU. The
    // prefill is sent only, never stored in `history`.
    // Reliability / scope knobs — most useful for weak models and weak servers:
    //   OPENHARN_TOOLS=a,b,c         restrict the agent to a subset of tools
    //   OPENHARN_NARROW=1            preset: read-only navigation (read, grep, glob), strict + prompt-tools
    //   OPENHARN_STRICT_TOOLS=1      grammar-constrain the reply to a *schema-valid* tool call or plain
    //                                text — a weak model then cannot invent field names or malform a call
    //   OPENHARN_PROMPT_TOOLS=1      describe tools in the prompt & omit the `tools` field (no-native-tools servers)
    //   OPENHARN_MAX_CALLS=<n>       per-turn circuit-breaker limit (default 1)
    //   OPENHARN_TOTAL_MAX=<n>       total calls across all turns before tools are removed (default 5)
// OPENHARN_SLM=1 enables the compact structured-observation harness (slm-agents style):
    // - Minimal JSON observation per turn (goal, valid_actions, searches, files, reads, last_result, feedback)
    // - Constrained action space (valid_actions only, ANSWER requires prior READ)
    // - Externalized state (all durable state in SlmState, not in conversation history)
    // - Per-step verification (pre-execution + post-execution)
    // - Retry localization (failed step re-prompted with feedback; task never restarts)
    let slm_mode = std::env::var_os("OPENHARN_SLM").is_some();

    if slm_mode {
        return crate::slm_harness::run_slm(cfg, history, session, user);
    }
    // OPENHARN_YESNO=1 enables NLT-style two-pass tool selection:
    // Pass 1: model sees each tool as YES/NO → selects subset
    // Pass 2: only selected tools are advertised; model fills args
    let yesno_mode = std::env::var_os("OPENHARN_YESNO").is_some();
    let narrow = std::env::var_os("OPENHARN_NARROW").is_some();
    let allowed: Option<Vec<String>> = if narrow {
        Some(["read", "grep", "glob"].iter().map(|s| s.to_string()).collect())
    } else {
        std::env::var("OPENHARN_TOOLS").ok().map(|s| {
            s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
        })
    };
let strict = narrow || std::env::var_os("OPENHARN_STRICT_TOOLS").is_some();
    let prompt_tools = strict || std::env::var_os("OPENHARN_PROMPT_TOOLS").is_some();
    // no_think prefills a `</think>` assistant turn — which is itself grammar-invalid,
    // so it can't combine with strict's grammar (and weak models don't reason anyway).
    let no_think = std::env::var_os("OPENHARN_NO_THINK").is_some() && !strict;
    let schemas = active_schemas(&allowed);

    // YES/NO two-pass tool selection (NLT 2025): if enabled, run Pass 1 each turn
    let mut effective_schemas = active_schemas(&allowed);
    if yesno_mode {
        // Re-run Pass 1 each turn since relevant tools may change
        let selected = run_yesno_pass1(cfg, &effective_schemas, &client, &url, &cfg.api_key, user);
        if selected.is_empty() {
            println!("[yesno] no tools selected — continuing without tools");
        } else {
            println!("[yesno] selected: {:?}", selected);
            effective_schemas = active_schemas(&Some(selected));
        }
    }

    // FRIENDLY_RESULTS: classify user intent before the tool loop
    let friendly_mode = cfg.friendly_results && prompt_tools;
    let intent = if friendly_mode {
        run_intent_detection(cfg, &client, &url, user)
    } else {
        "TOOL"
    };

    // CHAT intent: skip tools entirely, just answer directly
    if intent == "CHAT" {
        fit_context(history, budget);
        // For chat, strip tool schemas from the system prompt — don't mention tools
        let mut wire: Vec<Value> = history.iter().enumerate().map(|(i, m)| {
            if i == 0 && m["role"] == "system" {
                json!({"role": "system", "content": "You are a helpful assistant. Answer the user's question directly and concisely."})
            } else {
                m.clone()
            }
        }).collect();
        if no_think {
            wire.push(json!({ "role": "assistant", "content": "<think></think>" }));
        }
        let body = json!({
            "model": cfg.model,
            "messages": wire,
            "temperature": cfg.temperature,
            "max_tokens": cfg.max_tokens,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        let resp = match client.post(&url).json(&body).send() {
            Ok(r) => r,
            Err(e) => {
                println!("[error] chat request failed: {e}");
                return;
            }
        };
        if !resp.status().is_success() {
            println!("[error] HTTP {}", resp.status());
            return;
        }
        let (content, _) = stream_response(resp, no_think, false);
        history.push(json!({"role": "assistant", "content": content}));
        return;
    }

    let mut retried_text = false;

    for _ in 0..cfg.max_turns {
        let mut call_count = 0usize;
        // Keep the conversation within the model's context before every request.
        fit_context(history, budget);
        let mut wire = if prompt_tools {
            flatten_for_prompt_tools(history, &effective_schemas)
        } else {
            history.clone()
        };
        if no_think {
            wire.push(json!({ "role": "assistant", "content": "<think></think>" }));
        }
        let mut body = json!({
            "model": cfg.model,
            "messages": wire,
            "temperature": cfg.temperature,
            "stream": true,
            // final chunk carries usage (and llama-server adds `timings`) → tok/s
            "stream_options": { "include_usage": true },
        });
        if no_tools {
            // no tools or grammar — model can only answer in text
        } else if prompt_tools {
            // grammar-constrain the output to a schema-valid tool call (or plain text)
            if strict {
                body["grammar"] = json!(tool_grammar(&effective_schemas));
            }
        } else {
            body["tools"] = effective_schemas.clone();
            body["tool_choice"] = json!("auto");
        }
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

        let (mut content, mut tool_calls) = stream_response(resp, no_think, false);

        // Fallback tool-call parse: some models (e.g. Granite 3.x) emit a valid
        // structured call as TEXT — `<tool_call>[{"name":…,"arguments":{…}}]` — that the
        // server's parser (expecting a different trigger) leaves in `content`. Recover it
        // so the harness dispatches the tool instead of stalling. Only runs when the
        // native parse produced nothing, so it never overrides a real answer.
        if tool_calls.is_empty() && !no_tools {
            if let Some(parsed) = parse_text_tool_calls(&content) {
                tool_calls = parsed;
                content.clear(); // the content WAS the call, not an answer
            }
        }

        // Per-turn grounding: if the model made more than MAX_CALLS tool calls,
        // truncate BEFORE executing so only the first MAX_CALLS are dispatched.
        // The excess calls are discarded — the model sees it was too eager and must
        // learn to make fewer calls per turn.
        let per_turn_truncated = if !no_tools && tool_calls.len() > max_calls {
            let excess = tool_calls.len() - max_calls;
            tool_calls.truncate(max_calls);
            Some(excess)
        } else {
            None
        };

        // Record the assistant turn so the next turn's context stays coherent (and
        // the KV-cache prefix stable).
        let mut assistant = json!({ "role": "assistant" });
        assistant["content"] = if content.is_empty() { Value::Null } else { json!(content) };
        if !tool_calls.is_empty() {
            assistant["tool_calls"] = json!(tool_calls);
        }
        history.push(assistant);

        if tool_calls.is_empty() {

            if friendly_mode {
                println!("\n[intent] {intent} — model responded with text");
            }
            println!("{}", content);
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
            } else if allowed.as_ref().is_some_and(|a| !a.iter().any(|t| t == name)) {
                // narrow / restricted mode — this tool isn't available
                format!(
                    "'{name}' is not available in this mode. Available tools: {}.",
                    allowed.as_ref().unwrap().join(", ")
                )
            } else {
                session.execute(name, &args)
            };
            history.push(json!({ "role": "tool", "tool_call_id": id, "content": cap_result(result) }));
        }
        call_count += tool_calls.len();
        total_calls += tool_calls.len();
        
        // Friendly results mode: after tool execution, format a natural response and stop
        if friendly_mode {
            let tool_results: Vec<String> = history
                .iter()
                .filter(|m| m["role"].as_str() == Some("tool"))
                .filter_map(|m| m["content"].as_str())
                .map(|s| s.to_string())
                .collect();
            let summary = if tool_results.is_empty() {
                "No results were found.".to_string()
            } else {
                tool_results.iter().rev().take(4).rev().cloned().collect::<Vec<_>>().join("\n---\n")
            };
            println!("\n[{} call(s). Formatting result…]", call_count);
            run_friendly_response(cfg, &client, &url, user, &summary, history);
            return;
        }
        
        if repeats >= 3 {
            println!("\n[stopped: the model kept repeating the same tool call — it's stuck. Try rephrasing.]");
            return;
        }
        if let Some(excess) = per_turn_truncated {
            // Per-turn truncation: the model made too many calls. Feed back what
            // it got and tell it to make fewer calls next time.
            let tool_results: Vec<String> = tool_calls.iter().filter_map(|tc| {
                let id = tc["id"].as_str()?;
                history.iter().rev().find_map(|m| {
                    if m["role"].as_str() == Some("tool") && m["tool_call_id"].as_str() == Some(id) {
                        m["content"].as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
            }).collect();
            let summary = if tool_results.is_empty() {
                "No tool results were returned.".to_string()
            } else {
                tool_results.join("\n---\n")
            };
            println!("\n[per-turn grounding: {} excess calls truncated. {} executed.]", excess, tool_calls.len());
            history.push(json!({"role": "user", "content": format!(
                "You made {} tool calls this turn, but only {} {} allowed per turn. You executed only the first {} call(s). The other {} were discarded.\n\nThe results you got are:\n{}\n\nIn your next turn, make at most {} tool call(s) and wait for the results before making more.",
                tool_calls.len() + excess,
                max_calls,
                if max_calls == 1 { "is" } else { "are" },
                tool_calls.len(),
                excess,
                summary,
                max_calls
            )}));
            continue;
        }
        if call_count >= max_calls || total_calls >= total_max {
            if friendly_mode {
                // Friendly results: format the tool result as a natural response
                let tool_results: Vec<String> = history
                    .iter()
                    .filter(|m| m["role"].as_str() == Some("tool"))
                    .filter_map(|m| m["content"].as_str())
                    .map(|s| s.to_string())
                    .collect();
                let summary = if tool_results.is_empty() {
                    "No results were found.".to_string()
                } else {
                    tool_results.iter().rev().take(4).rev().cloned().collect::<Vec<_>>().join("\n---\n")
                };
                println!("\n[{} call(s). Formatting result…]", call_count);
                run_friendly_response(cfg, &client, &url, user, &summary, history);
                return;
            }
            let tool_results: Vec<String> = history
                .iter()
                .filter(|m| m["role"].as_str() == Some("tool"))
                .filter_map(|m| m["content"].as_str())
                .map(|s| s.to_string())
                .collect();
            let summary = if tool_results.is_empty() {
                "No tool results were returned.".to_string()
            } else {
                tool_results.iter().rev().take(4).rev().cloned().collect::<Vec<_>>().join("\n---\n")
            };
            println!("\n[{} calls ({} total). Feeding grounding back and letting model answer.]", call_count, total_calls);
            if total_calls >= total_max {
                no_tools = true;
            }
            history.push(json!({"role": "user", "content": format!(
                "You have made {} tool calls so far. The results you got are:\n{}\n\nSTOP calling tools and answer the user with what you now know (including if something was not found).",
                total_calls, summary
            )}));
            continue;
        }
    }
    println!("[stopped: hit max turns ({})]", cfg.max_turns);
}

/// Read the SSE stream: print assistant text live (thinking dimmed), accumulate
/// the (possibly chunked) tool-call deltas, and return the full text + assembled
/// tool calls. Prints a tok/s stats line from llama-server's timings (or usage +
/// wall time as a fallback).
fn stream_response(
    resp: reqwest::blocking::Response,
    no_think: bool,
    suppress_output: bool,
) -> (String, Vec<Value>) {
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
                        if !suppress_output {
                        print!("\x1b[2m");
                        }
                        thinking = true;
                    }
                    if !suppress_output {
                    print!("{r}");
                    }
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
                        if !suppress_output {
                        print!("{t}");
                        }
                        io::stdout().flush().ok();
                        printed = true;
                    } else if content.matches("</think>").count() >= 2 {
                        answer_started = true;
                        if live {
                            if !suppress_output {
                            print!("\r\x1b[K");
                            }
                            live = false;
                        }
                        reply_start.get_or_insert_with(std::time::Instant::now);
                        let ans = strip_think(&content); // answer produced so far
                        if !ans.is_empty() {
                            if !suppress_output {
                            print!("{ans}");
                            }
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
                        if !suppress_output {
                        print!("\r\x1b[K"); // erase the live thinking line before the answer
                        }
                        live = false;
                    }
                    if thinking {
                        if !suppress_output {
                        print!("\x1b[0m\n\n"); // close dim thinking before the answer
                        }
                        thinking = false;
                    }
                    reply_start.get_or_insert_with(std::time::Instant::now);
                    reply_tokens += 1;
                    // Suppress a text tool-call (Granite <tool_call>/<|tool_call|>) from the
                    // display — run() parses & dispatches it. Buffer the head until we can tell.
                    match hide_call {
                        Some(true) => {} // a tool call in text form — keep it off-screen
                        Some(false) => {
                            if !suppress_output {
                            print!("{t}");
                            }
                            io::stdout().flush().ok();
                            printed = true;
                        }
                        None => {
                            let tr = content.trim_start();
                            if tr.starts_with("<|tool_call|>") || tr.starts_with("<tool_call>") {
                                hide_call = Some(true);
                            } else if !"<|tool_call|>".starts_with(tr) && !"<tool_call>".starts_with(tr) {
                                hide_call = Some(false);
                                if !suppress_output {
                                print!("{content}"); // flush the buffered head, then stream
                                }
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
        if !suppress_output {
        print!("\r\x1b[K"); // erase the live thinking/working line
        }
    }
    if thinking {
        if !suppress_output {
        print!("\x1b[0m"); // never leave the terminal dimmed
        }
    }
    if no_think {
        let clean = strip_think(&content);
        if answer_started {
            if printed {
                println!(); // answer already streamed live; close the line
            }
        } else if !clean.is_empty() {
            if !suppress_output {
            println!("{clean}"); // reasoning never resolved to a streamed answer — print it
            }
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
            if !suppress_output {
            println!("\x1b[2m  {p}\x1b[0m");
            }
        }
        if !suppress_output {
        println!("\x1b[2m  total {:.1}s\x1b[0m", (end - started).as_secs_f64());
        }
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
    for marker in ["<|tool_call|>", "```"] {
        if let Some(rest) = s.strip_prefix(marker) {
            s = rest;
            break;
        }
    }
    let s = s.trim();
    let mut calls = Vec::new();
    // Try JSON tool-call format first (Granite/llama-server style)
    if let Some(open) = s.find(['[', '{']) {
        let is_arr = s.as_bytes()[open] == b'[';
        let close = if is_arr { s.rfind(']') } else { s.rfind('}') };
        if let Some(close) = close {
            if close > open {
                if let Ok(val) = serde_json::from_str::<Value>(&s[open..=close]) {
                    let items: Vec<Value> = match val {
                        Value::Array(a) => a,
                        obj => vec![obj],
                    };
                    for (i, item) in items.iter().enumerate() {
                        let f = item.get("function").unwrap_or(item);
                        if let Some(name) = f.get("name").and_then(|v| v.as_str()) {
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
                    }
                    if !calls.is_empty() {
                        return Some(calls);
                    }
                }
            }
        }
    }
    // Fallback: parse function-call syntax like `read({"path": "foo.txt"})` or `read(foo.txt)`
    // Also handles backticks: `read({"path": "foo.txt"})`
    // Maps positional args to each tool's first required parameter.
    fn tool_param(name: &str) -> Option<&'static str> {
        Some(match name {
            "read" => "path",
            "edit" => "path",
            "write" => "path",
            "multiedit" => "path",
            "glob" => "pattern",
            "glob_system" => "pattern",
            "grep" => "pattern",
            "grep_system" => "pattern",
            "bash" => "command",
            "webfetch" => "url",
            "todowrite" => "todos",
            "python" => "code",
            _ => return None,
        })
    }
    // Only match at line start or after backtick — avoids matching code like `std::fs::write(...)`
    let pattern = Regex::new(r"(?m)(?:^|\s)`?(\w+)\((\{.*?\}|[^)]*)\)`?").ok()?;
    for cap in pattern.captures_iter(s) {
        let name = cap.get(1).map(|m| m.as_str())?;
        let args_str = cap.get(2).map(|m| m.as_str()).unwrap_or("{}").trim();
        // Skip if args look like code (not a simple tool argument)
        if !args_str.starts_with('{') && args_str.contains('"') {
            continue;
        }
        let args = if args_str.starts_with('{') {
            args_str.to_string()
        } else if let Some(param) = tool_param(name) {
            // Map positional arg to the tool's first required parameter
            json!({param: args_str}).to_string()
        } else {
            continue;
        };
        calls.push(json!({
            "id": format!("call_{}", calls.len()),
            "type": "function",
            "function": { "name": name, "arguments": args }
        }));
    }
    if calls.is_empty() { None } else { Some(calls) }
}

/// YES/NO two-pass tool selection (NLT 2025).
/// Pass 1: present each tool as a YES/NO question, model responds with JSON.
/// Returns list of selected tool names.
fn run_yesno_pass1(
    cfg: &Config,
    schemas: &Value,
    client: &reqwest::blocking::Client,
    url: &str,
    api_key: &Option<String>,
    user_task: &str,
) -> Vec<String> {
    let Some(arr) = schemas.as_array() else { return Vec::new() };
    let mut questions = String::new();
    for (i, tool) in arr.iter().enumerate() {
        let name = tool["function"]["name"].as_str().unwrap_or("");
        let desc = tool["function"]["description"].as_str().unwrap_or("");
        questions.push_str(&format!("{}. {} — {}\n", i + 1, name, desc));
    }
    let prompt = format!(
        "User task: {}\n\nSelect tools needed. Reply with valid JSON only:\n{{
  \"selections\": {{
    \"read\": \"YES\"|\"NO\",
    \"write\": \"YES\"|\"NO\",
    \"edit\": \"YES\"|\"NO\",
    \"glob\": \"YES\"|\"NO\",
    \"grep\": \"YES\"|\"NO\",
    \"glob_system\": \"YES\"|\"NO\",
    \"grep_system\": \"YES\"|\"NO\",
    \"bash\": \"YES\"|\"NO\",
    \"python\": \"YES\"|\"NO\",
    \"webfetch\": \"YES\"|\"NO\",
    \"todowrite\": \"YES\"|\"NO\",
    \"todoread\": \"YES\"|\"NO\"
  }}
}}",
        user_task
    );

    let request = json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": "You are a tool selector. Reply with valid JSON only."},
            {"role": "user", "content": prompt}
        ],
        "temperature": 0.0,
        "stream": false,
    });

    let mut req = client.post(url).json(&request);
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }

    let resp = match req.send() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let resp_json: Value = match resp.json() {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let text = resp_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");

    let mut selected = Vec::new();
    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
        if let Some(sel) = parsed["selections"].as_object() {
            for (tool, choice) in sel {
                if choice.as_str() == Some("YES") {
                    selected.push(tool.clone());
                }
            }
        }
    }
    selected
}

/// The tool schemas advertised for this run, optionally filtered to an allowed subset
/// (`OPENHARN_TOOLS` / `OPENHARN_NARROW`). `None` = all ten tools.
fn active_schemas(allowed: &Option<Vec<String>>) -> Value {
    match allowed {
        None => tools::schemas(),
        Some(names) => Value::Array(
            tools::schemas()
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|t| {
                            t["function"]["name"]
                                .as_str()
                                .is_some_and(|n| names.iter().any(|x| x == n))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        ),
    }
}

/// A generic-JSON tail shared by every generated grammar (for argument values whose type
/// we don't tightly constrain — arrays, nested objects).
const GRAMMAR_TAIL: &str = r#"value ::= object | array | string | number | "true" | "false" | "null"
object ::= "{" ws ( string ws ":" ws value ( ws "," ws string ws ":" ws value )* )? ws "}"
array ::= "[" ws ( value ( ws "," ws value )* )? ws "]"
string ::= "\"" ( [^"\\\n\r] | "\\" ["\\/bfnrt] )* "\""
number ::= "-"? [0-9]+ ( "." [0-9]+ )?
integer ::= "-"? [0-9]+
boolean ::= "true" | "false"
ws ::= [ \t\n\r]*
"#;

/// The GBNF grammar for a single argument value, tightened by JSON-schema type/enum where
/// we can (string / integer / boolean / one-of-enum), falling back to generic `value`.
fn value_rule_for(spec: &Value) -> String {
    let q = |s: &str| format!("\"\\\"{s}\\\"\"");
    if let Some(en) = spec["enum"].as_array() {
        let alts: Vec<String> = en.iter().filter_map(|v| v.as_str()).map(q).collect();
        if !alts.is_empty() {
            return format!("( {} )", alts.join(" | "));
        }
    }
    match spec["type"].as_str().unwrap_or("") {
        "string" => "string".into(),
        "integer" | "number" => "integer".into(),
        "boolean" => "boolean".into(),
        _ => "value".into(),
    }
}

/// Generate a GBNF grammar that constrains the model's reply to EITHER a schema-valid
/// tool call — `<tool_call>[{"name": <known tool>, "arguments": {<only known keys, typed>}}]`
/// — OR plain text (any reply not starting with `<`). Used in strict/narrow modes so a
/// weak model physically cannot invent a field name, misname a tool, or malform a call.
fn tool_grammar(schemas: &Value) -> String {
    let lit = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    // GBNF rule names must be dashed-lowercase (e.g. "check-mate"). Tool names
    // like glob_system contain underscores; replace them with dashes so the
    // grammar parser accepts the rule names.
    let rn = |s: &str| s.replace('_', "-");
    let mut g = String::new();
    // root: either a tool call or plain text (text = anything not starting with <)
    g.push_str("root ::= call | text\n");
    g.push_str("text ::= [^<] | [^<] text\n");
    g.push_str(&format!(
        "call ::= {} ws {} ws obj ( ws {} ws obj )* ws {}\n",
        lit("<tool_call>"), lit("["), lit(","), lit("]")
    ));
    let mut obj_alts: Vec<String> = Vec::new();
    let mut rules = String::new();
    if let Some(arr) = schemas.as_array() {
        for t in arr {
            let name = match t["function"]["name"].as_str() {
                Some(n) => n,
                None => continue,
            };
            let rname = rn(name);
            obj_alts.push(format!("t-{rname}"));
            let props = t["function"]["parameters"]["properties"].as_object();
            let mut kvs: Vec<String> = Vec::new();
            if let Some(props) = props {
                for (k, spec) in props {
                    kvs.push(format!("{} ws {} ws {}", lit(&format!("\"{k}\"")), lit(":"), value_rule_for(spec)));
                }
            }
            if kvs.is_empty() {
                rules.push_str(&format!("a-{rname} ::= {} ws {}\n", lit("{"), lit("}")));
            } else {
                rules.push_str(&format!("kv-{rname} ::= {}\n", kvs.join(" | ")));
                rules.push_str(&format!(
                    "a-{rname} ::= {} ws ( kv-{rname} ( ws {} ws kv-{rname} )* )? ws {}\n",
                    lit("{"), lit(","), lit("}")
                ));
            }
            let name_lit = lit(&format!("\"{name}\""));
            let closing = lit("}");
            rules.push_str(&format!(
                "t-{rname} ::= {open} ws {qname} ws {colon} ws {name_lit} ws {comma} ws {qargs} ws {colon} ws a-{rname} ws {closing}\n",
                open = lit("{"),
                qname = lit("\"name\""),
                colon = lit(":"),
                name_lit = name_lit,
                comma = lit(","),
                qargs = lit("\"arguments\""),
                closing = closing
            ));
        }
    }
    let obj = if obj_alts.is_empty() { "value".to_string() } else { obj_alts.join(" | ") };
    g.push_str(&format!("obj ::= {obj}\n"));
    g.push_str(&rules);
    g.push_str(GRAMMAR_TAIL);
    g
}

/// Prompt-tools mode: render the tool set as a text description + the exact call format
/// the model should emit (what `parse_text_tool_calls` recovers). Used when the server
/// has no native tool-calling.
fn tool_prompt(schemas: &Value) -> String {
    let mut s = String::from(
        "You do NOT have a tool API. To call a tool, reply with ONLY this line and nothing else:\n\
         <tool_call>[{\"name\": \"<tool>\", \"arguments\": { ... }}]\n\
         Otherwise, answer the user normally. Available tools:\n",
    );
    if let Some(arr) = schemas.as_array() {
        for t in arr {
            let f = &t["function"];
            let name = f["name"].as_str().unwrap_or("");
            let desc = f["description"].as_str().unwrap_or("");
            let required: Vec<&str> = f["parameters"]["required"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let params = f["parameters"]["properties"]
                .as_object()
                .map(|o| {
                    o.keys()
                        .map(|k| if required.contains(&k.as_str()) { k.clone() } else { format!("[{k}]") })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let short = desc.split(['.', '\n']).next().unwrap_or(desc);
            s.push_str(&format!("- {name}({params}): {short}\n"));
        }
    }
    s
}

/// Render internal `tool_calls` back into the text form the model is told to emit, so a
/// prior tool-calling assistant turn round-trips as plain text on the wire.
fn render_calls_text(tool_calls: &Value) -> String {
    let items: Vec<Value> = tool_calls
        .as_array()
        .map(|a| {
            a.iter()
                .map(|tc| {
                    let name = tc["function"]["name"].as_str().unwrap_or("");
                    let args_raw = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let args: Value = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));
                    json!({ "name": name, "arguments": args })
                })
                .collect()
        })
        .unwrap_or_default();
    format!("<tool_call>{}", Value::Array(items))
}

/// Flatten openharn's internal history (system + tool_calls + tool-role results) into
/// plain system/user/assistant messages a tool-unaware server accepts: the tools are
/// described in the system prompt, assistant tool_calls become their text form, and tool
/// results become user messages.
fn flatten_for_prompt_tools(history: &[Value], schemas: &Value) -> Vec<Value> {
    history
        .iter()
        .map(|m| match m["role"].as_str().unwrap_or("") {
            "system" => {
                let base = m["content"].as_str().unwrap_or("");
                json!({ "role": "system", "content": format!("{base}\n\n{}", tool_prompt(schemas)) })
            }
            "assistant" if m.get("tool_calls").is_some() => {
                let text = render_calls_text(&m["tool_calls"]);
                let content = m["content"].as_str().filter(|s| !s.is_empty());
                let full = match content {
                    Some(c) => format!("{c}\n{text}"),
                    None => text,
                };
                json!({ "role": "assistant", "content": full })
            }
            "tool" => {
                let c = m["content"].as_str().unwrap_or("");
                json!({ "role": "user", "content": format!("Tool result:\n{c}") })
            }
            _ => m.clone(),
        })
        .collect()
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
    #[test]
    fn active_schemas_filters_and_grammar_constrains() {
        let s = active_schemas(&Some(vec!["glob".into()]));
        assert_eq!(s.as_array().unwrap().len(), 1, "only glob kept");
        let g = tool_grammar(&s);
        // the grammar names glob and its real args, and has a text escape hatch
        assert!(g.contains("root ::= call | text"));
        assert!(g.contains(r#""\"glob\"""#));
        assert!(g.contains(r#""\"pattern\"""#));
        // grep's `include` key must NOT be a valid key for glob (no invented fields)
        assert!(!g.contains("kv-grep"));
        // glob's `path` key IS valid; `scope` was removed from the schema
        assert!(g.contains(r#""\"path\""#));
        assert!(!g.contains("\"scope\""));
    }

    #[test]
    fn prompt_tools_flattens_to_plain_roles() {
        let hist = vec![
            json!({"role":"system","content":"SYS"}),
            json!({"role":"user","content":"find configs"}),
            json!({"role":"assistant","tool_calls":[{"id":"c0","type":"function",
                "function":{"name":"grep","arguments":"{\"pattern\":\"Config\"}"}}]}),
            json!({"role":"tool","tool_call_id":"c0","content":"src/app.py:1: class Config"}),
        ];
        let wire = flatten_for_prompt_tools(&hist, &tools::schemas());
        // system carries the tool descriptions + the call format
        assert!(wire[0]["content"].as_str().unwrap().contains("<tool_call>"));
        assert!(wire[0]["content"].as_str().unwrap().contains("grep"));
        // the assistant tool_call became its text form, no tool_calls field on the wire
        assert!(wire[2].get("tool_calls").is_none());
        assert!(wire[2]["content"].as_str().unwrap().contains("<tool_call>"));
        assert!(wire[2]["content"].as_str().unwrap().contains("Config"));
        // the tool result became a plain user message (no tool role reaches the server)
        assert_eq!(wire[3]["role"], "user");
        assert!(wire[3]["content"].as_str().unwrap().contains("Tool result"));
        assert!(wire.iter().all(|m| m["role"] != "tool"));
    }

    #[test]
    fn prompt_tools_respects_filtered_schemas() {
        // Regression: flatten_for_prompt_tools must use the filtered schemas,
        // not the full tool set. Only "read" and "glob" are advertised.
        let hist = vec![
            json!({"role":"system","content":"SYS"}),
            json!({"role":"user","content":"find files"}),
        ];
        let filtered = active_schemas(&Some(vec!["read".into(), "glob".into()]));
        let wire = flatten_for_prompt_tools(&hist, &filtered);
        let sys = wire[0]["content"].as_str().unwrap();
        // read and glob must be present
        assert!(sys.contains("read"), "read missing from filtered prompt");
        assert!(sys.contains("glob"), "glob missing from filtered prompt");
        // tools NOT in the filtered set must NOT appear
        assert!(!sys.contains("grep"), "grep leaked into filtered prompt");
        assert!(!sys.contains("bash"), "bash leaked into filtered prompt");
        assert!(!sys.contains("edit"), "edit leaked into filtered prompt");
        assert!(!sys.contains("write"), "write leaked into filtered prompt");
        assert!(!sys.contains("glob_system"), "glob_system leaked into filtered prompt");
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
    fn parses_function_call_syntax() {
        // Backtick-wrapped function call
        let calls = parse_text_tool_calls(r#"`grep_system(poems.md)`"#).expect("should parse");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "grep_system");
        let args: Value = serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["pattern"], "poems.md");

        // Bare function call without backticks
        let calls = parse_text_tool_calls("read(src/main.rs)").expect("should parse");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "read");
        let args: Value = serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["path"], "src/main.rs");

        // Multiple args in bash
        let calls = parse_text_tool_calls("bash(ls -la)").expect("should parse");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "bash");
        let args: Value = serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["command"], "ls -la");

        // Unknown tool returns None
        assert!(parse_text_tool_calls("foobar(x)").is_none());
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
