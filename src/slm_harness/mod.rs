pub mod actions;
pub mod executor;
pub mod state;
pub mod verifier;

pub use state::SlmState;
pub use actions::{SlmAction, parse_action};
pub use verifier::{validate_action, verify_step_result, VerifierResult};
pub use executor::{execute_action, fold_result};

use crate::tools::Session;
use serde_json::Value;
use std::path::Path;
use tokio::runtime::Runtime;

pub const SYSTEM_PROMPT: &str = r#"You are a file-search subagent. Each turn you get one JSON observation with the
goal, your past actions, and the currently valid actions. Reply with EXACTLY one
JSON object and nothing else. Actions:
{"action":"SEARCH","pattern":"<regex>","file_glob":"**/*","root":"."}
{"action":"READ","path":"<file>","offset":0,"limit":80}
{"action":"ANSWER","files":["<file>"],"evidence":[{"path":"<file>","line_start":1,"line_end":2}],"answer_text":"","confidence":0.9}
{"action":"ESCALATE","reason":"<why>"}
Only use actions listed in valid_actions: ANSWER becomes valid only after
you have READ a file, so when ANSWER is missing, READ the best candidate
file first. If the observation contains feedback, your previous attempt
failed - do something different. Cite only files you have discovered."#;

/// Run the SLM harness loop (blocking, uses async tools via blocking_on).
pub fn run_slm(
    cfg: &crate::agent::Config,
    _history: &mut Vec<Value>,
    session: &mut Session,
    user: &str,
) {
    // Config from env
    let max_steps: usize = std::env::var("OPENHARN_SLM_MAX_STEPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let max_retries_per_step: usize = std::env::var("OPENHARN_SLM_MAX_RETRIES")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    let observation_char_budget: usize = std::env::var("OPENHARN_SLM_OBS_BUDGET")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(2000);

    let workspace = session.cwd.clone();
    let mut state = SlmState::new(user.to_string(), max_steps, observation_char_budget);

    // Reuse a blocking client
    let client = reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(0)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));

    // Use tokio runtime for async tool calls
    let rt = Runtime::new().unwrap();

    while state.step < max_steps {
        let mut action_taken = false;

        for _retry in 0..=max_retries_per_step {
            let observation = state.build_observation();
            let request = serde_json::json!({
                "model": cfg.model,
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": observation}
                ],
                "temperature": cfg.temperature,
                "stream": false,
            });

            let mut req = client.post(&url).json(&request);
            if let Some(k) = &cfg.api_key {
                req = req.bearer_auth(k);
            }

            let resp = match req.send() {
                Ok(r) => r,
                Err(e) => {
                    state.record_failure(format!("request failed: {e}"));
                    continue;
                }
            };

            let resp_json: serde_json::Value = match resp.json() {
                Ok(v) => v,
                Err(e) => {
                    state.record_failure(format!("bad response: {e}"));
                    continue;
                }
            };

            let text = resp_json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();

            // Parse action
            let (action, parse_err) = match parse_action(&text) {
                Ok(a) => (Some(a), None),
                Err(e) => (None, Some(e.to_string())),
            };

            if action.is_none() {
                state.record_failure(format!("parse error: {}", parse_err.unwrap()));
                continue;
            }
            let action = action.unwrap();

            // Pre-execution validation
            let pre = validate_action(&action, &state, &workspace);
            if !pre.ok {
                state.record_failure(pre.reason.clone());
                if !pre.retryable { break; }
                continue;
            }

            // Terminal actions
            if action.is_terminal() {
                handle_terminal(action);
                state.clear_feedback();
                return;
            }

            // Execute (async via tokio)
            let (output, is_error) = rt.block_on(execute_action(&action, &workspace));
            let post = verify_step_result(&action, &output, is_error);
            if !post.ok {
                state.record_failure(post.reason);
                continue;
            }

            // Fold result into state
            fold_result(&action, output.to_string(), &mut state, &workspace);
            state.clear_feedback();
            action_taken = true;
            break;
        }

        if !action_taken {
            println!("\n[stopped: step failed after retries]");
            return;
        }

        state.step += 1;
    }
    println!("[stopped: hit max steps ({})]", max_steps);
}

fn handle_terminal(action: SlmAction) {
    match action {
        SlmAction::Answer(a) => {
            println!("\n[answer] {}", a.answer_text);
            println!("  files: {}", a.files.join(", "));
        }
        SlmAction::Escalate(e) => {
            println!("\n[escalated] {}", e.reason);
        }
        _ => {}
    }
}