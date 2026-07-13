use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRecord {
    pub pattern: String,
    pub file_glob: String,
    pub n_hits: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRecord {
    pub path: String,
    pub offset: usize,
    pub limit: usize,
    pub n_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlmState {
    pub goal: String,
    pub max_steps: usize,
    pub step: usize,
    pub searches: VecDeque<SearchRecord>,
    pub hits: VecDeque<Hit>,
    pub reads: VecDeque<ReadRecord>,
    pub known_files: Vec<String>,
    pub failed_actions: Vec<String>,
    pub feedback: String,
    pub last_result: Value,
    pub observation_char_budget: usize,

    pub max_hits_shown: usize,
    pub max_files_shown: usize,
}

impl SlmState {
    pub fn new(goal: String, max_steps: usize, observation_char_budget: usize) -> Self {
        Self {
            goal,
            max_steps,
            step: 0,
            searches: VecDeque::new(),
            hits: VecDeque::new(),
            reads: VecDeque::new(),
            known_files: Vec::new(),
            failed_actions: Vec::new(),
            feedback: String::new(),
            last_result: json!({}),
            observation_char_budget,
            max_hits_shown: 8,
            max_files_shown: 10,
        }
    }

    pub fn record_search(&mut self, pattern: String, file_glob: String, hits: Vec<Hit>) {
        let n = hits.len();
        self.searches.push_back(SearchRecord { pattern: pattern.clone(), file_glob, n_hits: n });
        if self.searches.len() > 8 { self.searches.pop_front(); }
        for hit in &hits {
            self.hits.push_back(hit.clone());
            if self.hits.len() > 50 { self.hits.pop_front(); }
            if !self.known_files.contains(&hit.path) {
                self.known_files.push(hit.path.clone());
            }
        }
        self.last_result = json!({
            "type": "search",
            "pattern": pattern,
            "n_hits": n,
            "hits": hits.iter().take(self.max_hits_shown).map(|h| json!({"f": h.path, "l": h.line, "t": &h.text[..h.text.len().min(120)]}))
                .collect::<Vec<_>>()
        });
    }

    pub fn record_read(&mut self, path: String, offset: usize, limit: usize, content: String, n_lines: usize) {
        self.reads.push_back(ReadRecord { path: path.clone(), offset, limit, n_lines });
        if self.reads.len() > 8 { self.reads.pop_front(); }
        if !self.known_files.contains(&path) {
            self.known_files.push(path.clone());
        }
        self.last_result = json!({
            "type": "read",
            "f": path,
            "o": offset,
            "n": n_lines,
            "content": content[..content.len().min(1200)].to_string()
        });
    }

    pub fn record_failure(&mut self, description: String) {
        self.failed_actions.push(description.chars().take(200).collect());
        if self.failed_actions.len() > 5 { self.failed_actions.remove(0); }
        self.feedback = description.chars().take(300).collect();
    }

    pub fn clear_feedback(&mut self) {
        self.feedback.clear();
    }

    pub fn valid_actions(&self) -> Vec<&'static str> {
        let mut actions = vec!["SEARCH"];
        if !self.known_files.is_empty() {
            actions.push("READ");
        }
        if !self.reads.is_empty() {
            actions.push("ANSWER");
        }
        actions.push("ESCALATE");
        actions
    }

    pub fn build_observation(&self) -> String {
        let mut obs = json!({
            "goal": self.goal,
            "step": self.step,
            "steps_left": self.max_steps.saturating_sub(self.step),
            "valid_actions": self.valid_actions(),
            "searches": self.searches.iter().rev().take(4).rev().collect::<Vec<_>>(),
            "files": self.known_files.iter().take(self.max_files_shown).collect::<Vec<_>>(),
            "reads": self.reads.iter().rev().take(4).rev().collect::<Vec<_>>(),
            "last_result": self.last_result,
        });
        if !self.feedback.is_empty() {
            obs["feedback"] = json!(self.feedback);
        }
        let text = serde_json::to_string(&obs).unwrap_or_default();
        if text.len() > self.observation_char_budget {
            let mut lr = obs["last_result"].clone();
            if let Some(content) = lr["content"].as_str() {
                let overshoot = text.len() - self.observation_char_budget;
                let new_len = content.len().saturating_sub(overshoot).max(100);
                lr["content"] = json!(content[..new_len].to_string());
                obs["last_result"] = lr;
                return serde_json::to_string(&obs).unwrap_or_default();
            }
        }
        text
    }
}