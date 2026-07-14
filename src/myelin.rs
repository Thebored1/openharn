//! Myelin notes backend — single-note session + tools (edit_note, write_note, format_note, search_notes, web_search)
//! and an optional OpenAI-compatible HTTP server (OPENHARN_MYELIN=1).
//!
//! Run via: `OPENHARN_MYELIN=1 cargo run` (starts on port 8099 by default,
//! or `OPENHARN_MYELIN_PORT=xxxx`).

use crate::edit;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// Session state: one open note, no filesystem persistence.
#[derive(Clone, Debug, Default)]
pub struct MyelinSession {
    pub note: String,
    todos: Vec<Value>,
}

impl MyelinSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Execute a Myelin tool by name. Returns (new_note, tool_result_message).
    pub fn execute(&mut self, name: &str, args: &Value) -> (String, String) {
        match name {
            "write_note" => self.write_note(args),
            "edit_note" => self.edit_note(args),
            "format_note" => self.format_note(args),
            "search_notes" => self.search_notes(args),
            "web_search" => self.web_search(args),
            "todowrite" => self.todowrite(args),
            "todoread" => self.todoread(),
            other => (self.note.clone(), format!("Unknown tool: {other}")),
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
        if find.is_empty() {
            return (self.note.clone(), "Error: 'find' cannot be empty.".into());
        }
        if !self.note.contains(find) {
            return (self.note.clone(), "Error: 'find' text not found in note.".into());
        }
        match edit::replace(&self.note, find, replace, false) {
            Ok(updated) => {
                self.note = updated;
                (self.note.clone(), "Edit applied.".into())
            }
            Err(e) => (self.note.clone(), format!("Edit failed: {e}")),
        }
    }

    fn format_note(&mut self, args: &Value) -> (String, String) {
        let op = args["operation"].as_str().unwrap_or("");
        let result = match op {
            "remove_headings" => regex::Regex::new(r"(?m)^#{1,6}[ \t]*")
                .unwrap()
                .replace_all(&self.note, "")
                .into_owned(),
            "remove_bold" => self.note.replace("**", ""),
            "lowercase" => self.note.to_lowercase(),
            "strip_markdown" => regex::Regex::new(r"[#*`>_-]")
                .unwrap()
                .replace_all(&self.note, "")
                .into_owned(),
            "bullets_to_numbered" => {
                let mut n = 0;
                let re = regex::Regex::new(r"(?m)^[-*]\s+").unwrap();
                let mut result = String::new();
                let mut last = 0;
                for cap in re.captures_iter(&self.note) {
                    let m = cap.get(0).unwrap();
                    result.push_str(&self.note[last..m.start()]);
                    n += 1;
                    result.push_str(&format!("{n}. "));
                    last = m.end();
                }
                result.push_str(&self.note[last..]);
                result
            }
            "numbered_to_bullets" => {
                regex::Regex::new(r"(?m)^\d+\.\s+")
                    .unwrap()
                    .replace_all(&self.note, "- ")
                    .into_owned()
            }
            _ => return (self.note.clone(), format!("Unknown format operation: {op}")),
        };
        self.note = result;
        (self.note.clone(), format!("Format '{op}' applied."))
    }

    fn search_notes(&self, args: &Value) -> (String, String) {
        let query = args["query"].as_str().unwrap_or("");
        (self.note.clone(), format!("(search_notes stub) query: {query}"))
    }

    fn web_search(&self, args: &Value) -> (String, String) {
        let query = args["query"].as_str().unwrap_or("");
        (self.note.clone(), format!("(web_search stub) query: {query}"))
    }

    fn todowrite(&mut self, args: &Value) -> (String, String) {
        if let Some(todos) = args["todos"].as_array() {
            self.todos = todos.clone();
            (self.note.clone(), format!("Todo list updated ({} items).", self.todos.len()))
        } else {
            (self.note.clone(), "Error: todowrite requires 'todos' array.".into())
        }
    }

    fn todoread(&self) -> (String, String) {
        if self.todos.is_empty() {
            (self.note.clone(), "Todo list is empty.".into())
        } else {
            let rendered = self.todos
                .iter()
                .map(|t| {
                    let mark = match t["status"].as_str().unwrap_or("pending") {
                        "completed" => "[x]",
                        "in_progress" => "[~]",
                        _ => "[ ]",
                    };
                    format!("{mark} {}", t["content"].as_str().unwrap_or(""))
                })
                .collect::<Vec<_>>()
                .join("\n");
            (self.note.clone(), rendered)
        }
    }
}

/// Tool schemas advertised to the model (OpenAI function-calling format).
pub fn myelin_schemas() -> Value {
    json!([
        {"type":"function","function":{
            "name":"edit_note",
            "description":"Replace the exact text `find` with `replace` in the open note (anchored edit). Preferred for small changes.",
            "parameters":{"type":"object","properties":{
                "find":{"type":"string","description":"The exact existing text to replace."},
                "replace":{"type":"string","description":"The replacement text."}
            },"required":["find","replace"]}
        }},
        {"type":"function","function":{
            "name":"write_note",
            "description":"Set the ENTIRE open note body to `content` (empty string clears it). Use for fresh writes / full rewrites.",
            "parameters":{"type":"object","properties":{
                "content":{"type":"string","description":"The full note contents."}
            },"required":["content"]}
        }},
        {"type":"function","function":{
            "name":"format_note",
            "description":"Structural transform of the open note, done in code.",
            "parameters":{"type":"object","properties":{
                "operation":{"type":"string","enum":["remove_headings","remove_bold","lowercase","strip_markdown","bullets_to_numbered","numbered_to_bullets"]}
            },"required":["operation"]}
        }},
        {"type":"function","function":{
            "name":"search_notes",
            "description":"Search the user's OTHER notes (local note store).",
            "parameters":{"type":"object","properties":{
                "query":{"type":"string","description":"Search query."}
            },"required":["query"]}
        }},
        {"type":"function","function":{
            "name":"web_search",
            "description":"Search the web.",
            "parameters":{"type":"object","properties":{
                "query":{"type":"string","description":"Search query."}
            },"required":["query"]}
        }},
        {"type":"function","function":{
            "name":"todowrite",
            "description":"Create/replace the task todo list to plan and track multi-step work. Send the FULL list each time.",
            "parameters":{"type":"object","properties":{
                "todos":{"type":"array","items":{"type":"object","properties":{
                    "content":{"type":"string","description":"What the step is."},
                    "status":{"type":"string","enum":["pending","in_progress","completed"]}
                },"required":["content","status"]}}
            },"required":["todos"]}
        }},
        {"type":"function","function":{
            "name":"todoread",
            "description":"Read the current todo list.",
            "parameters":{"type":"object","properties":{}}
        }}
    ])
}

/// Shared server state: one MyelinSession per client (cookie-based session).
struct MyelinServer {
    sessions: Arc<Mutex<HashMap<String, MyelinSession>>>,
}

impl MyelinServer {
    fn new() -> Self {
        Self { sessions: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn get_or_create(&self, id: &str) -> MyelinSession {
        self.sessions.lock().unwrap()
            .entry(id.to_string())
            .or_insert_with(MyelinSession::new)
            .clone()
    }

    fn save(&self, id: &str, session: MyelinSession) {
        self.sessions.lock().unwrap().insert(id.to_string(), session);
    }

    fn run(&self, port: u16) -> std::io::Result<()> {
        let server = Server::http(format!("127.0.0.1:{port}"))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        println!("Myelin server → http://127.0.0.1:{port}/v1/chat/completions");
        println!("  POST /v1/chat/completions");
        println!("  GET  /health");
        println!("  Cookie: myelin_sid=<id> (auto-issued on first request)");

        for request in server.incoming_requests() {
            self.handle(request);
        }
        Ok(())
    }

    fn handle(&self, mut request: Request) {
        let method = request.method().clone();
        let url = request.url().to_string();

        match (method, url.as_str()) {
            (Method::Get, "/health") => {
                let resp = Response::from_string(r#"{"status":"ok"}"#)
                    .with_header(Header::from_bytes(&b"Content-Type"[..], b"application/json").unwrap());
                request.respond(resp).ok();
            }
            (Method::Post, "/v1/chat/completions") => self.handle_chat(request),
            _ => {
                let resp = Response::from_string("Not Found").with_status_code(StatusCode(404));
                request.respond(resp).ok();
            }
        }
    }

    fn handle_chat(&self, mut request: Request) {
        // Read body
        let mut body = String::new();
        use std::io::Read;
        if request.as_reader().read_to_string(&mut body).is_err() {
            self.respond_json(request, StatusCode(400), json!({"error": "bad body"}));
            return;
        }
        let req: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => {
                self.respond_json(request, StatusCode(400), json!({"error": "invalid json"}));
                return;
            }
        };

        // Session cookie
        let session_id = extract_session_id(&request).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut session = self.get_or_create(&session_id);

        // Upstream model (llama-server)
        let upstream = std::env::var("OPENHARN_MYELIN_UPSTREAM")
            .unwrap_or_else(|_| "http://127.0.0.1:8080/v1".into());
        let model = std::env::var("OPENHARN_MODEL").unwrap_or_else(|_| "myelin".into());
        let api_key = std::env::var("OPENHARN_API_KEY").ok().filter(|s| !s.is_empty());

        let messages = req["messages"].as_array().cloned().unwrap_or_default();
        let temperature = req["temperature"].as_f64().unwrap_or(0.2);
        let tools = myelin_schemas();

        // Inject note frame into the latest user message (Myelin contract)
        let note_frame = if session.note.trim().is_empty() {
            "The open note is titled \"Note\". It is currently empty.".to_string()
        } else {
            format!("The open note is titled \"Note\". Its current content:\n--- NOTE ---\n{}\n--- END ---", session.note)
        };

        let mut out_messages = messages.clone();
        if let Some(last) = out_messages.last_mut() {
            if last["role"] == "user" && last["tool_calls"].is_null() {
                let content = last["content"].as_str().unwrap_or("");
                last["content"] = json!(format!("{note_frame}\n\nUser request: {content}"));
            }
        }

        // Proxy to upstream
        let client = Client::new();
        let body = json!({
            "model": model,
            "messages": out_messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": temperature,
            "stream": false
        });

        let resp = match client.post(&format!("{upstream}/chat/completions")).json(&body).send() {
            Ok(r) => r,
            Err(e) => {
                self.respond_json(request, StatusCode(502), json!({"error": format!("upstream error: {e}")}));
                return;
            }
        };

        let status = resp.status();
        let text = resp.text().unwrap_or_default();

        // Parse and potentially intercept deflection after web_search
        let mut upstream_json: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                self.respond_raw(request, status, text);
                self.save(&session_id, session);
                return;
            }
        };

        // Myelin-specific: intercept deflection ("I will fetch...") after web_search tool result
        if let Some(choices) = upstream_json["choices"].as_array_mut() {
            if let Some(choice) = choices.first_mut() {
                if let Some(msg) = choice["message"].as_object_mut() {
                    let has_web_search_result = messages.iter().any(|m| {
                        m["role"] == "tool" && m["content"].as_str().map_or(false, |c| c.contains("Web results"))
                    });
                    let content = msg["content"].as_str().unwrap_or("").to_lowercase();
                    let deflects = ["would you like", "shall i", "i will now fetch", "i will fetch", "do you want me to", "i can fetch"];
                    if has_web_search_result && deflects.iter().any(|d| content.contains(d)) {
                        msg["tool_calls"] = Value::Null;
                        msg["content"] = json!("Write the information you found into the note using write_note. Do not ask permission.");
                    }
                }
            }
        }

        let final_text = serde_json::to_string(&upstream_json).unwrap_or(text);
        self.respond_raw(request, status, final_text);
        self.save(&session_id, session);
    }

    fn respond_json(&self, request: Request, status: StatusCode, json: Value) {
        let body = serde_json::to_string(&json).unwrap();
        let resp = Response::from_string(body)
            .with_status_code(status)
            .with_header(Header::from_bytes(&b"Content-Type"[..], b"application/json").unwrap());
        request.respond(resp).ok();
    }

    fn respond_raw(&self, request: Request, status: reqwest::StatusCode, body: String) {
        let sc = StatusCode(status.as_u16());
        let resp = Response::from_string(body)
            .with_status_code(sc)
            .with_header(Header::from_bytes(&b"Content-Type"[..], b"application/json").unwrap());
        request.respond(resp).ok();
    }
}

fn extract_session_id(request: &Request) -> Option<String> {
    request.headers().iter().find(|h| h.field.equiv("cookie")).and_then(|h| {
        h.value.as_str().split(';').find_map(|c| {
            let c = c.trim();
            c.strip_prefix("myelin_sid=").map(|v| v.to_string())
        })
    })
}

/// Entry point for `OPENHARN_MYELIN=1 cargo run`
pub fn run_myelin_server(port: u16) {
    let server = MyelinServer::new();
    server.run(port).expect("Myelin server failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_note_anchored() {
        let mut s = MyelinSession::new();
        s.note = "The sky is blue today.".into();
        let (note, _) = s.execute("edit_note", &json!({"find":"blue","replace":"green"}));
        assert_eq!(note, "The sky is green today.");
    }

    #[test]
    fn edit_note_miss_is_error() {
        let mut s = MyelinSession::new();
        s.note = "hello".into();
        let (note, msg) = s.execute("edit_note", &json!({"find":"world","replace":"there"}));
        assert!(msg.starts_with("Error"));
        assert_eq!(note, "hello");
    }

    #[test]
    fn write_note_clears_and_sets() {
        let mut s = MyelinSession::new();
        s.note = "old".into();
        s.execute("write_note", &json!({"content":"new note"}));
        assert_eq!(s.note, "new note");
        s.execute("write_note", &json!({"content":""}));
        assert_eq!(s.note, "");
    }

    #[test]
    fn format_note_remove_headings() {
        let mut s = MyelinSession::new();
        s.note = "# Hello\n## World\ntext".into();
        s.execute("format_note", &json!({"operation":"remove_headings"}));
        assert_eq!(s.note, "Hello\nWorld\ntext");
    }

    #[test]
    fn format_note_bullets_to_numbered() {
        let mut s = MyelinSession::new();
        s.note = "- a\n- b\n- c\n".into();
        s.execute("format_note", &json!({"operation":"bullets_to_numbered"}));
        assert_eq!(s.note, "1. a\n2. b\n3. c\n");
    }
}