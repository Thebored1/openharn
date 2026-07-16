//! OpenAI-compatible HTTP server that runs openharn's coding-agent loop per
//! request. Enabled by `OPENHARN_SERVE=1` (or `--serve`). Each
//! `POST /v1/chat/completions` builds a fresh tool `Session` for the request's
//! working directory and runs the *same* agent loop as the REPL, then returns
//! the final assistant message in OpenAI chat-completion shape. This is what
//! lets any OpenAI-compatible client (a web UI, another app, an API) drive
//! openharn's tool-using agent — openharn serves the agent, delegating the
//! actual weights/inference to the `llama-server` at `OPENHARN_BASE_URL`.
//!
//! Run via: `OPENHARN_SERVE=1 ./target/debug/openharn .` (port 8090 by default,
//! or `OPENHARN_SERVE_PORT=xxxx`).

use crate::agent::{self, Config};
use crate::tools;
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// Entry point for `OPENHARN_SERVE=1 ./target/debug/openharn .`
pub fn run_agent_server(cfg: Config, cwd: PathBuf, port: u16) {
    let server = match Server::http(format!("127.0.0.1:{port}")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[serve] failed to bind 127.0.0.1:{port}: {e}");
            return;
        }
    };
    println!("openharn server → http://127.0.0.1:{port}/v1/chat/completions");
    println!("  POST /v1/chat/completions   (runs the coding-agent loop per request)");
    println!("  GET  /v1/models");
    println!("  GET  /health");

    // Spawn a thread per request so a slow (CPU) inference turn doesn't block the
    // accept loop — the client connection stays open until the agent answers.
    for request in server.incoming_requests() {
        let cfg = cfg.clone();
        let cwd = cwd.clone();
        std::thread::spawn(move || handle(request, cfg, cwd));
    }
}

fn handle(mut request: Request, cfg: Config, cwd: PathBuf) {
    let method = request.method().clone();
    let url = request.url().to_string();
    match (method, url.as_str()) {
        (Method::Get, "/health") => {
            let _ = request.respond(json_response(StatusCode(200), json!({"status": "ok"})));
        }
        (Method::Get, "/v1/models") => {
            let body = json!({
                "object": "list",
                "data": [{
                    "id": cfg.model,
                    "object": "model",
                    "created": now_secs(),
                    "owned_by": "openharn"
                }]
            });
            let _ = request.respond(json_response(StatusCode(200), body));
        }
        (Method::Post, "/v1/chat/completions") => handle_chat(request, cfg, cwd),
        _ => {
            let _ = request.respond(
                Response::from_string("Not Found").with_status_code(StatusCode(404)),
            );
        }
    }
}

fn handle_chat(mut request: Request, cfg: Config, cwd: PathBuf) {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        let _ = request.respond(json_response(StatusCode(400), json!({"error": "bad body"})));
        return;
    }
    let req: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            let _ =
                request.respond(json_response(StatusCode(400), json!({"error": "invalid json"})));
            return;
        }
    };

    let messages = req["messages"].as_array().cloned().unwrap_or_default();
    if messages.is_empty() {
        let _ = request.respond(
            json_response(StatusCode(400), json!({"error": "messages required"})),
        );
        return;
    }

    // The last message is the new user turn; everything before it is history.
    // `agent::run` pushes its own system prompt (if history is empty) and the new
    // user message, so we pass the prior turns as history and the last as `user`.
    let last = messages.last().unwrap().clone();
    let user = last["content"].as_str().unwrap_or("").to_string();
    let mut history: Vec<Value> = if messages.len() > 1 {
        messages[..messages.len() - 1].to_vec()
    } else {
        Vec::new()
    };

    // Optional per-request overrides.
    let mut local_cfg = cfg.clone();
    if let Some(t) = req["temperature"].as_f64() {
        local_cfg.temperature = t;
    }
    if let Some(m) = req["max_tokens"].as_u64() {
        local_cfg.max_tokens = m as u32;
    }

    // FC-proxy mode (OPENHARN_FC_PROXY=1): if the request carries tool schemas, do a
    // SINGLE constrained tool-call generation and return the tool_calls directly — no
    // agent loop, no tool execution. This exposes openharn's tool-call reliability
    // layer (prompt-tools + strict grammar + text-call recovery) to an external
    // function-calling client such as the BFCL benchmark, so the harness's effect can
    // be measured in isolation. Schema-agnostic: the grammar derives from the request.
    let tools_val = req["tools"].clone();
    let fc_proxy = std::env::var_os("OPENHARN_FC_PROXY").is_some();
    if fc_proxy && tools_val.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        let (tool_calls, content, pt, ct) =
            agent::fc_proxy_once(&local_cfg, &messages, &tools_val);
        let mut message = json!({ "role": "assistant" });
        if tool_calls.is_empty() {
            message["content"] = json!(content);
        } else {
            message["content"] = Value::Null;
            message["tool_calls"] = json!(tool_calls);
        }
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let out = json!({
            "id": id,
            "object": "chat.completion",
            "created": now_secs(),
            "model": cfg.model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": if tool_calls.is_empty() { "stop" } else { "tool_calls" }
            }],
            "usage": { "prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct }
        });
        let _ = request.respond(json_response(StatusCode(200), out));
        return;
    }

    // Run the agent loop for this request on a fresh session.
    let mut session = tools::Session::new(cwd.clone());
    agent::run(&local_cfg, &mut history, &mut session, &user);

    // The agent pushes the final assistant message onto `history`. Find the last
    // assistant turn that actually carries text (tool-only turns have null content).
    let answer = history
        .iter()
        .rev()
        .find(|m| m["role"].as_str() == Some("assistant"))
        .and_then(|m| {
            m["content"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "[agent produced no response]".to_string());

    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let out = json!({
        "id": id,
        "object": "chat.completion",
        "created": now_secs(),
        "model": cfg.model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": answer},
            "finish_reason": "stop"
        }],
        "usage": null
    });
    let _ = request.respond(json_response(StatusCode(200), out));
}

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], b"application/json").unwrap()
}

fn json_response(status: StatusCode, body: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    // `with_header` wraps the body in Cursor<Vec<u8>> (which implements Read),
    // the type tiny_http's `respond` requires.
    Response::from_string(serde_json::to_string(&body).unwrap_or_default())
        .with_status_code(status)
        .with_header(json_header())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
