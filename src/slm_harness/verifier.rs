use crate::slm_harness::actions::SlmAction;
use crate::slm_harness::state::SlmState;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifierResult {
    pub ok: bool,
    pub reason: String,
    pub retryable: bool,
}

impl VerifierResult {
    pub fn ok() -> Self { Self { ok: true, reason: String::new(), retryable: false } }
    pub fn fail(reason: String, retryable: bool) -> Self { Self { ok: false, reason, retryable } }
}

pub fn validate_action(action: &SlmAction, state: &SlmState, workspace: &Path) -> VerifierResult {
    let valid = state.valid_actions();
    let action_name = match action {
        SlmAction::Search(_) => "SEARCH",
        SlmAction::Read(_) => "READ",
        SlmAction::Answer(_) => "ANSWER",
        SlmAction::Escalate(_) => "ESCALATE",
    };
    if !valid.contains(&action_name) {
        return VerifierResult::fail(
            format!("action {} not valid now; valid: {:?}", action_name, valid),
            true,
        );
    }
    match action {
        SlmAction::Search(a) => {
            if a.pattern.trim().is_empty() {
                return VerifierResult::fail("search pattern cannot be empty".into(), true);
            }
            VerifierResult::ok()
        }
        SlmAction::Read(a) => {
            if a.path.trim().is_empty() {
                return VerifierResult::fail("read path cannot be empty".into(), true);
            }
            if !state.known_files.contains(&a.path) {
                return VerifierResult::fail(
                    format!("file '{}' not discovered yet; SEARCH first", a.path),
                    true,
                );
            }
            VerifierResult::ok()
        }
        SlmAction::Answer(a) => {
            if a.files.is_empty() {
                return VerifierResult::fail("answer requires at least one file".into(), true);
            }
            for f in &a.files {
                if !state.known_files.contains(f) {
                    return VerifierResult::fail(
                        format!("answer cites unknown file '{}'", f),
                        true,
                    );
                }
            }
            if a.answer_text.trim().is_empty() {
                return VerifierResult::fail("answer_text cannot be empty".into(), true);
            }
            VerifierResult::ok()
        }
        SlmAction::Escalate(a) => {
            if a.reason.trim().is_empty() {
                return VerifierResult::fail("escalate reason cannot be empty".into(), false);
            }
            VerifierResult::ok()
        }
    }
}

pub fn verify_step_result(action: &SlmAction, output: &str, is_error: bool) -> VerifierResult {
    if is_error {
        return VerifierResult::fail(format!("tool error: {}", output), true);
    }
    match action {
        SlmAction::Search(_) => {
            if output.trim() == "(no matches)" || output.trim().is_empty() {
                return VerifierResult::fail("search returned no matches".into(), true);
            }
            VerifierResult::ok()
        }
        SlmAction::Read(_) => {
            if output.trim().is_empty() {
                return VerifierResult::fail("read returned empty content".into(), true);
            }
            VerifierResult::ok()
        }
        SlmAction::Answer(_) | SlmAction::Escalate(_) => VerifierResult::ok(),
    }
}