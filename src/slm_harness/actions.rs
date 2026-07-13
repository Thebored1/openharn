use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "UPPERCASE")]
pub enum SlmAction {
    Search(SearchAction),
    Read(ReadAction),
    Answer(AnswerAction),
    Escalate(EscalateAction),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAction {
    pub pattern: String,
    #[serde(default = "default_file_glob")]
    pub file_glob: String,
    #[serde(default = "default_root")]
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAction {
    pub path: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerAction {
    pub files: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub answer_text: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalateAction {
    pub reason: String,
}

fn default_file_glob() -> String { "**/*".to_string() }
fn default_root() -> String { ".".to_string() }
fn default_limit() -> usize { 80 }
fn default_confidence() -> f32 { 0.9 }

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unknown action: {0}")]
    UnknownAction(String),
    #[error("missing required field: {0}")]
    MissingField(String),
}

impl SlmAction {
    pub fn is_terminal(&self) -> bool {
        matches!(self, SlmAction::Answer(_) | SlmAction::Escalate(_))
    }
}

pub fn parse_action(text: &str) -> Result<SlmAction, ParseError> {
    let value: serde_json::Value = serde_json::from_str(text.trim())?;
    let action = value.get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ParseError::MissingField("action".to_string()))?;
    match action.to_uppercase().as_str() {
        "SEARCH" => {
            let pattern = value.get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ParseError::MissingField("pattern".to_string()))?;
            Ok(SlmAction::Search(SearchAction {
                pattern: pattern.to_string(),
                file_glob: value.get("file_glob").and_then(|v| v.as_str()).unwrap_or("**/*").to_string(),
                root: value.get("root").and_then(|v| v.as_str()).unwrap_or(".").to_string(),
            }))
        }
        "READ" => {
            let path = value.get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ParseError::MissingField("path".to_string()))?;
            Ok(SlmAction::Read(ReadAction {
                path: path.to_string(),
                offset: value.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                limit: value.get("limit").and_then(|v| v.as_u64()).unwrap_or(80) as usize,
            }))
        }
        "ANSWER" => {
            let files = value.get("files")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
                .unwrap_or_default();
            let evidence = value.get("evidence")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| {
                    let path = v.get("path")?.as_str()?;
                    let line_start = v.get("line_start")?.as_u64()? as usize;
                    let line_end = v.get("line_end")?.as_u64()? as usize;
                    Some(Evidence { path: path.to_string(), line_start, line_end })
                }).collect())
                .unwrap_or_default();
            let answer_text = value.get("answer_text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(SlmAction::Answer(AnswerAction {
                files,
                evidence,
                answer_text,
                confidence: value.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.9) as f32,
            }))
        }
        "ESCALATE" => {
            let reason = value.get("reason")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ParseError::MissingField("reason".to_string()))?;
            Ok(SlmAction::Escalate(EscalateAction { reason: reason.to_string() }))
        }
        _ => Err(ParseError::UnknownAction(action.to_string())),
    }
}