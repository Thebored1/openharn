use crate::slm_harness::{actions::SlmAction, state::SlmState};
use crate::tools::{grep_tool, read_tool, ToolResult};
use std::path::Path;

pub async fn execute_action(action: &SlmAction, workspace: &Path) -> (String, bool) {
    match action {
        SlmAction::Search(a) => {
            let result = grep_tool(&a.pattern, Some(&a.file_glob), Some(&a.root), 50).await;
            (if result.is_error { result.error.unwrap_or_default() } else { result.output }, result.is_error)
        }
        SlmAction::Read(a) => {
            let result = read_tool(&a.path, Some(a.offset), Some(a.limit)).await;
            (if result.is_error { result.error.unwrap_or_default() } else { result.output }, result.is_error)
        }
        _ => (String::new(), false),
    }
}

pub fn fold_result(action: &SlmAction, output: String, state: &mut SlmState, workspace: &Path) {
    match action {
        SlmAction::Search(a) => {
            let mut hits = Vec::new();
            if output.trim() != "(no matches)" && !output.trim().is_empty() {
                for line in output.lines() {
                    if let Some((path, rest)) = line.split_once(':') {
                        if let Some((line_str, text)) = rest.split_once(':') {
                            if let Ok(line) = line_str.parse::<usize>() {
                                let p = Path::new(path);
                                let rel = if p.is_absolute() {
                                    p.strip_prefix(workspace).unwrap_or(p).to_string_lossy().to_string()
                                } else {
                                    p.to_string_lossy().to_string()
                                };
                                hits.push(crate::slm_harness::state::Hit {
                                    path: rel.trim_start_matches("./").to_string(),
                                    line,
                                    text: text.chars().take(120).collect(),
                                });
                            }
                        }
                    }
                }
            }
            state.record_search(a.pattern.clone(), a.file_glob.clone(), hits);
        }
        SlmAction::Read(a) => {
            let n_lines = output.lines().filter(|l| !l.trim().is_empty()).count();
            state.record_read(a.path.clone(), a.offset, a.limit, output, n_lines);
        }
        _ => {}
    }
}