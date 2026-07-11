//! opencode's tool set, ported: read, write, edit, glob, grep, bash. The harness
//! keeps a per-session read-set so `edit`/`write` fail on a file that wasn't read
//! first (opencode's grounding guard — you can't act on a file you don't
//! understand). `edit` wraps the anchored replacer engine; navigation is glob +
//! grep, not a weak "list".

use crate::edit;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const DEFAULT_READ_LIMIT: usize = 2000;
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", ".cargo", "dist", "build", ".venv", "__pycache__",
];

fn skippable(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && SKIP_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
}

/// True when the model asked for a whole-system search. A tiny model can't be
/// trusted to produce a valid `C:\` path, so it flips this flag instead and WE
/// resolve the real filesystem roots.
fn is_system_scope(args: &Value) -> bool {
    let s = args["scope"].as_str().unwrap_or("");
    s.eq_ignore_ascii_case("system") || s.eq_ignore_ascii_case("global")
}

/// The actual filesystem roots to search: every existing drive on Windows, `/` on
/// Unix. The model never has to name these.
fn system_roots() -> Vec<PathBuf> {
    if cfg!(windows) {
        let roots: Vec<PathBuf> = ('A'..='Z')
            .map(|c| PathBuf::from(format!("{c}:\\")))
            .filter(|p| p.exists())
            .collect();
        if roots.is_empty() { vec![PathBuf::from("C:\\")] } else { roots }
    } else {
        vec![PathBuf::from("/")]
    }
}

/// The search roots for a call: the whole system when scope=system, else the one
/// (project-relative) path.
fn search_roots(cwd: &Path, args: &Value) -> Vec<PathBuf> {
    if is_system_scope(args) {
        system_roots()
    } else {
        vec![resolve(cwd, args["path"].as_str().unwrap_or("."))]
    }
}

/// Told the model how to *actually* widen — via the flag it can produce, not a
/// path it can't.
const WIDEN_HINT: &str =
    "To search the whole computer, call again with scope=\"system\" (do NOT pass a path for this).";

/// Cap on entries walked in one search so a system scan terminates.
const WALK_CAP: usize = 800_000;

/// Resolve a path against the project root, guarding the Windows `"/."` trap where
/// PathBuf::join with a rooted-but-driveless path escapes to the drive root.
fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let pb = Path::new(p);
    if pb.is_absolute() {
        return pb.to_path_buf();
    }
    let trimmed = p.trim_start_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed == "." {
        return cwd.to_path_buf();
    }
    cwd.join(trimmed)
}

/// Per-session state: the project root and which files have been read (so edit /
/// write can require a prior read, opencode-style).
pub struct Session {
    pub cwd: PathBuf,
    read: HashSet<PathBuf>,
    todos: Vec<Value>,
}

impl Session {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd, read: HashSet::new(), todos: Vec::new() }
    }

    pub fn execute(&mut self, name: &str, args: &Value) -> String {
        match name {
            "read" => self.read_tool(args),
            "write" => self.write_tool(args),
            "edit" => self.edit_tool(args),
            "multiedit" => self.multiedit_tool(args),
            "glob" => self.glob_tool(args),
            "grep" => grep(&self.cwd, args),
            "bash" => bash(&self.cwd, args),
            "webfetch" => webfetch(args),
            "todowrite" => self.todowrite(args),
            "todoread" => self.todoread(),
            other => format!(
                "'{other}' is not an available tool. The tools are: read, write, edit, multiedit, glob, grep, bash, webfetch, todowrite, todoread. To find a file by name use `glob`; to search file contents use `grep`."
            ),
        }
    }

    fn read_tool(&mut self, args: &Value) -> String {
        let Some(p) = args["path"].as_str() else {
            return "Error: read requires 'path'.".into();
        };
        let path = resolve(&self.cwd, p);
        let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = args["limit"].as_u64().unwrap_or(DEFAULT_READ_LIMIT as u64) as usize;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.read.insert(path.clone());
                let lines: Vec<&str> = text.lines().collect();
                let start = (offset - 1).min(lines.len());
                let end = (start + limit).min(lines.len());
                let numbered: String = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, l)| format!("{}: {}", start + i + 1, l))
                    .collect::<Vec<_>>()
                    .join("\n");
                if numbered.is_empty() {
                    return "(empty file)".into();
                }
                if lines.len() > end {
                    format!(
                        "{numbered}\n(Showing lines {}-{} of {}. Use 'offset' to read further.)",
                        offset, end, lines.len()
                    )
                } else {
                    numbered
                }
            }
            Err(e) => ground_missing(&path, self.cwd.as_path(), &e.to_string()),
        }
    }

    fn write_tool(&mut self, args: &Value) -> String {
        let Some(p) = args["path"].as_str() else {
            return "Error: write requires 'path'.".into();
        };
        let content = args["content"].as_str().unwrap_or("");
        let path = resolve(&self.cwd, p);
        if path.exists() && !self.read.contains(&path) {
            return format!(
                "Error: {} already exists and was not read this session. Use `read` first before overwriting it.",
                path.display()
            );
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, content) {
            Ok(_) => {
                self.read.insert(path.clone());
                format!("Wrote {} ({} bytes).", path.display(), content.len())
            }
            Err(e) => format!("Error writing {}: {e}", path.display()),
        }
    }

    fn edit_tool(&mut self, args: &Value) -> String {
        let Some(p) = args["path"].as_str() else {
            return "Error: edit requires 'path'.".into();
        };
        let path = resolve(&self.cwd, p);
        if !self.read.contains(&path) {
            return format!(
                "Error: you must `read` {} before editing it, so you have its exact current contents.",
                path.display()
            );
        }
        let old = args["old_string"].as_str().unwrap_or("");
        let new = args["new_string"].as_str().unwrap_or("");
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return format!("Error reading {}: {e}", path.display()),
        };
        match edit::replace(&content, old, new, replace_all) {
            Ok(updated) => match std::fs::write(&path, &updated) {
                Ok(_) => format!("Edited {} ({} bytes).", path.display(), updated.len()),
                Err(e) => format!("Error writing {}: {e}", path.display()),
            },
            Err(e) => format!("Edit failed: {e}"),
        }
    }

    /// Apply several edits to ONE file in a single call, in order. All-or-nothing:
    /// if any edit fails to match, nothing is written (so the file can't be left
    /// half-edited).
    fn multiedit_tool(&mut self, args: &Value) -> String {
        let Some(p) = args["path"].as_str() else {
            return "Error: multiedit requires 'path'.".into();
        };
        let path = resolve(&self.cwd, p);
        if !self.read.contains(&path) {
            return format!("Error: you must `read` {} before editing it.", path.display());
        }
        let Some(edits) = args["edits"].as_array() else {
            return "Error: multiedit requires 'edits' — an array of {old_string, new_string}.".into();
        };
        let mut content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return format!("Error reading {}: {e}", path.display()),
        };
        for (i, e) in edits.iter().enumerate() {
            let old = e["old_string"].as_str().unwrap_or("");
            let new = e["new_string"].as_str().unwrap_or("");
            let replace_all = e["replace_all"].as_bool().unwrap_or(false);
            match edit::replace(&content, old, new, replace_all) {
                Ok(updated) => content = updated,
                Err(err) => {
                    return format!(
                        "multiedit aborted at edit #{} ({err}). NO changes were written — fix that edit and resend all of them.",
                        i + 1
                    );
                }
            }
        }
        match std::fs::write(&path, &content) {
            Ok(_) => format!("Applied {} edits to {} ({} bytes).", edits.len(), path.display(), content.len()),
            Err(e) => format!("Error writing {}: {e}", path.display()),
        }
    }

    /// Replace the whole todo list with the model's new one, and echo it back so
    /// the model sees the current plan.
    fn todowrite(&mut self, args: &Value) -> String {
        let Some(todos) = args["todos"].as_array() else {
            return "Error: todowrite requires 'todos' — an array of {content, status}.".into();
        };
        self.todos = todos.clone();
        format!("Todo list updated ({} items):\n{}", self.todos.len(), render_todos(&self.todos))
    }

    fn todoread(&self) -> String {
        if self.todos.is_empty() {
            "The todo list is empty.".into()
        } else {
            render_todos(&self.todos)
        }
    }

    fn glob_tool(&self, args: &Value) -> String {
        let Some(pattern) = args["pattern"].as_str() else {
            return "Error: glob requires 'pattern'.".into();
        };
        let pat = match glob::Pattern::new(pattern) {
            Ok(p) => p,
            Err(e) => return format!("Invalid glob pattern: {e}"),
        };
        // A pattern with no path separator is matched against the basename too, so
        // `Cargo.toml` or `*.rs` finds matches anywhere (forgiving for the model).
        let basename_ok = !pattern.contains('/') && !pattern.contains('\\');
        let roots = search_roots(&self.cwd, args);
        let system = is_system_scope(args);
        // forced grounding: a project-scoped search at a path that doesn't exist gets a
        // listing of what does, instead of a bare "no matches" it can loop on.
        if !system {
            if let Some(root) = roots.first() {
                if !root.exists() {
                    return ground_missing_path(root, &self.cwd);
                }
            }
        }
        let mut out: Vec<String> = vec![];
        let mut walked = 0usize;
        let mut capped = false;
        'outer: for root in &roots {
            for entry in WalkDir::new(root)
                .into_iter()
                .filter_entry(|e| !skippable(e))
                .filter_map(|e| e.ok())
            {
                walked += 1;
                if walked > WALK_CAP {
                    capped = true;
                    break 'outer;
                }
                let path = entry.path();
                let rel = path.strip_prefix(root).unwrap_or(path);
                let hit = pat.matches_path(rel)
                    || (basename_ok
                        && entry.file_name().to_str().map(|n| pat.matches(n)).unwrap_or(false));
                if hit {
                    let suffix = if entry.file_type().is_dir() { "/" } else { "" };
                    out.push(format!("{}{}", path.display(), suffix));
                    if out.len() >= 100 {
                        out.push("…[showing first 100 matches]".into());
                        break 'outer;
                    }
                }
            }
        }
        out.sort();
        if !out.is_empty() {
            return out.join("\n");
        }
        if system {
            let where_ = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ");
            let note = if capped { " (search hit its limit before finishing)" } else { "" };
            format!("No files matching '{pattern}' anywhere on the system — searched {where_}{note}.")
        } else {
            format!(
                "No files matching '{pattern}' under {} (the project directory — the ONLY place searched). {WIDEN_HINT}",
                roots.first().map(|r| r.display().to_string()).unwrap_or_default()
            )
        }
    }
}

/// Grounding for a search (glob/grep) pointed at a project path that doesn't exist:
/// name what actually exists so the model corrects instead of re-guessing or looping.
fn ground_missing_path(path: &Path, cwd: &Path) -> String {
    let dir = path.parent().filter(|p| p.is_dir()).unwrap_or(cwd);
    let listing = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .take(30)
                .map(|en| en.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    format!(
        "The path '{}' does not exist under the project. {} contains: {}. Use a path that exists, or omit 'path' to search the whole project.",
        path.display(),
        dir.display(),
        listing
    )
}

/// A failed read grounds the model with what actually exists in the directory, so
/// it corrects in one step instead of guessing more filenames.
fn ground_missing(path: &Path, cwd: &Path, err: &str) -> String {
    // List the immediate parent — but if it doesn't exist (e.g. the model
    // prepended a bogus directory like the project name), fall back to the project
    // root so the model always sees what's actually there and can correct.
    let dir = path
        .parent()
        .filter(|p| p.is_dir())
        .unwrap_or(cwd);
    let listing = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .take(25)
                .map(|en| en.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if listing.is_empty() {
        format!("Error reading {}: {err}. The project directory is {}.", path.display(), cwd.display())
    } else {
        format!(
            "Error reading {}: {err}. Files that exist in {}: {}. Paths are relative to the project root — pick one of these, don't add extra directories.",
            path.display(),
            dir.display(),
            listing
        )
    }
}

fn grep(cwd: &Path, args: &Value) -> String {
    let Some(pat) = args["pattern"].as_str() else {
        return "Error: grep requires 'pattern'.".into();
    };
    let re = match regex::Regex::new(pat) {
        Ok(r) => r,
        Err(e) => return format!("Invalid regex: {e}"),
    };
    let include = args["include"]
        .as_str()
        .and_then(|p| glob::Pattern::new(p).ok());
    let roots = search_roots(cwd, args);
    let system = is_system_scope(args);
    if !system {
        if let Some(root) = roots.first() {
            if !root.exists() {
                return ground_missing_path(root, cwd);
            }
        }
    }
    let mut out: Vec<String> = vec![];
    let mut files = 0usize;
    'outer: for root in &roots {
      for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !skippable(e))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(inc) = &include {
            if !entry.file_name().to_str().map(|n| inc.matches(n)).unwrap_or(false) {
                continue;
            }
        }
        files += 1;
        if files > WALK_CAP {
            out.push("…[search stopped: too many files]".into());
            break 'outer;
        }
        if entry.metadata().map(|m| m.len() > 2_000_000).unwrap_or(true) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                let l = line.trim();
                let l: String = if l.chars().count() > 200 {
                    format!("{}…", l.chars().take(200).collect::<String>())
                } else {
                    l.to_string()
                };
                out.push(format!("{}:{}: {}", entry.path().display(), i + 1, l));
                if out.len() >= 100 {
                    out.push("…[showing first 100 matches]".into());
                    break 'outer;
                }
            }
        }
      }
    }
    if !out.is_empty() {
        return out.join("\n");
    }
    if system {
        let where_ = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ");
        format!("No matches for /{pat}/ anywhere on the system — searched {where_}.")
    } else {
        format!(
            "No matches for /{pat}/ under {} (the project directory — the ONLY place searched). {WIDEN_HINT}",
            roots.first().map(|r| r.display().to_string()).unwrap_or_default()
        )
    }
}

fn bash(cwd: &Path, args: &Value) -> String {
    let Some(cmd) = args["command"].as_str() else {
        return "Error: bash requires 'command'.".into();
    };
    let output = if cfg!(windows) {
        std::process::Command::new("cmd").arg("/C").arg(cmd).current_dir(cwd).output()
    } else {
        std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(cwd).output()
    };
    match output {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            if s.trim().is_empty() {
                s = format!("(no output, exit {})", o.status.code().unwrap_or(-1));
            }
            if s.len() > 8000 {
                s.truncate(8000);
                s.push_str("\n…[truncated]");
            }
            s
        }
        Err(e) => format!("Error running command: {e}"),
    }
}

fn render_todos(todos: &[Value]) -> String {
    todos
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
        .join("\n")
}

/// Fetch a URL and return its readable text (HTML stripped). Capped so a big page
/// can't blow the context.
fn webfetch(args: &Value) -> String {
    let Some(raw) = args["url"].as_str() else {
        return "Error: webfetch requires 'url'.".into();
    };
    let url = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => return format!("webfetch error: {e}"),
    };
    let resp = match client
        .get(&url)
        .header(reqwest::header::USER_AGENT, "openharn/0.1 (+local coding agent)")
        .send()
    {
        Ok(r) => r,
        Err(e) => return format!("Failed to fetch {url}: {e}"),
    };
    if !resp.status().is_success() {
        return format!("Failed to fetch {url}: HTTP {}", resp.status());
    }
    let body = resp.text().unwrap_or_default();
    let text: String = html_to_text(&body).chars().take(12_000).collect();
    if text.trim().is_empty() {
        format!("Fetched {url} but found no readable text.")
    } else {
        format!("{url}:\n\n{text}")
    }
}

/// Strip HTML to readable text: drop script/style, remove tags, decode a few
/// entities, collapse whitespace.
fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for tag in ["script", "style", "noscript", "head"] {
        if let Ok(re) = regex::Regex::new(&format!(r"(?is)<{tag}[^>]*>.*?</{tag}>")) {
            s = re.replace_all(&s, " ").into_owned();
        }
    }
    if let Ok(re) = regex::Regex::new(r"(?s)<[^>]+>") {
        s = re.replace_all(&s, " ").into_owned();
    }
    s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Tool schemas advertised to the model (OpenAI function-calling format). Wording
/// follows opencode's tool descriptions.
pub fn schemas() -> Value {
    json!([
        {"type":"function","function":{
            "name":"read",
            "description":"Read a file. Returns its text with 1-based line numbers. Use `offset`/`limit` to page through large files. You must read a file before you edit or write it.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string","description":"File path, relative to the project root or absolute."},
                "offset":{"type":"integer","description":"1-based line to start from (optional)."},
                "limit":{"type":"integer","description":"Max lines to read (optional)."}
            },"required":["path"]}
        }},
        {"type":"function","function":{
            "name":"edit",
            "description":"Performs exact string replacements in files. You must `read` the file first. `old_string` must match the existing text (whitespace, indentation, and escaped newlines are tolerated). It FAILS if `old_string` is not found, or is found multiple times — then add surrounding lines to make it unique, or set replace_all. Never reprint the whole file; make a targeted edit.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "old_string":{"type":"string","description":"The exact existing text to replace."},
                "new_string":{"type":"string","description":"The replacement text."},
                "replace_all":{"type":"boolean","description":"Replace every occurrence (default false)."}
            },"required":["path","old_string","new_string"]}
        }},
        {"type":"function","function":{
            "name":"write",
            "description":"Write a file to the filesystem, overwriting any existing file. If the file exists you MUST `read` it first. ALWAYS prefer editing existing files; only create new files when needed. Never proactively create documentation/README files unless asked.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "content":{"type":"string","description":"The full file contents to write."}
            },"required":["path","content"]}
        }},
        {"type":"function","function":{
            "name":"glob",
            "description":"Fast file pattern matching. Supports glob patterns like \"**/*.rs\" or \"src/**/*.ts\". Returns matching file paths. Use this to find files by name. By default it searches ONLY the project directory. To search the WHOLE computer/system, set scope=\"system\" — do NOT try to pass a filesystem path for that; the tool finds every drive itself.",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string","description":"Glob pattern, e.g. **/*.rs or Cargo.toml."},
                "path":{"type":"string","description":"A subdirectory of the project to search under (optional). Default: project root. Ignored when scope is \"system\"."},
                "scope":{"type":"string","enum":["project","system"],"description":"\"project\" (default) searches the project dir; \"system\" searches the entire computer (all drives). Use \"system\" when the user asks to search everywhere."}
            },"required":["pattern"]}
        }},
        {"type":"function","function":{
            "name":"grep",
            "description":"Fast content search using regular expressions. Returns matching `file:line: text`. Supports full regex. Filter files with `include` (e.g. \"*.rs\", \"*.{ts,tsx}\"). By default searches the project directory. Set scope=\"system\" to search the whole computer (all drives) — do NOT pass a path for that.",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string","description":"Regular expression to search for in file contents."},
                "include":{"type":"string","description":"Only search files whose name matches this glob (optional)."},
                "path":{"type":"string","description":"A subdirectory of the project to search under (optional). Ignored when scope is \"system\"."},
                "scope":{"type":"string","enum":["project","system"],"description":"\"project\" (default) or \"system\" (the entire computer)."}
            },"required":["pattern"]}
        }},
        {"type":"function","function":{
            "name":"bash",
            "description":"Run a shell command in the project root; returns stdout+stderr. Use for building, running tests, git, etc.",
            "parameters":{"type":"object","properties":{
                "command":{"type":"string","description":"The shell command to run."}
            },"required":["command"]}
        }},
        {"type":"function","function":{
            "name":"multiedit","description":"Make MULTIPLE edits to a SINGLE file in one call. You must `read` the file first. Edits are applied in order; each `old_string` must match. All-or-nothing: if any edit fails, nothing is written. Prefer this over many `edit` calls on the same file.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "edits":{"type":"array","items":{"type":"object","properties":{
                    "old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}
                },"required":["old_string","new_string"]}}
            },"required":["path","edits"]}
        }},
        {"type":"function","function":{
            "name":"webfetch","description":"Fetch a URL and return its readable text content (HTML stripped). Use for docs, references, or a page the user gives you. Only fetch URLs the user provided or that are clearly for programming help.",
            "parameters":{"type":"object","properties":{
                "url":{"type":"string","description":"The URL to fetch."}
            },"required":["url"]}
        }},
        {"type":"function","function":{
            "name":"todowrite","description":"Create/replace the task todo list to plan and track multi-step work. Send the FULL list each time. Mark items in_progress when you start and completed when done, so the user sees progress.",
            "parameters":{"type":"object","properties":{
                "todos":{"type":"array","items":{"type":"object","properties":{
                    "content":{"type":"string","description":"What the step is."},
                    "status":{"type":"string","enum":["pending","in_progress","completed"]}
                },"required":["content","status"]}}
            },"required":["todos"]}
        }},
        {"type":"function","function":{
            "name":"todoread","description":"Read the current todo list.",
            "parameters":{"type":"object","properties":{}}
        }}
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "openharn_t_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rooted_driveless_stays_in_project() {
        let cwd = Path::new("C:\\proj");
        assert_eq!(resolve(cwd, "/."), PathBuf::from("C:\\proj"));
        assert_eq!(resolve(cwd, "/src"), PathBuf::from("C:\\proj").join("src"));
        assert_eq!(resolve(cwd, "src/main.rs"), PathBuf::from("C:\\proj").join("src/main.rs"));
    }

    #[test]
    fn edit_requires_prior_read() {
        let d = tmp();
        let f = d.join("a.txt");
        std::fs::write(&f, "hello world").unwrap();
        let mut s = Session::new(d.clone());
        // edit without reading -> refused
        let out = s.execute("edit", &json!({"path":"a.txt","old_string":"world","new_string":"there"}));
        assert!(out.contains("must `read`"), "{out}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello world");
        // after reading -> allowed
        s.execute("read", &json!({"path":"a.txt"}));
        let out = s.execute("edit", &json!({"path":"a.txt","old_string":"world","new_string":"there"}));
        assert!(out.contains("Edited"), "{out}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello there");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn read_missing_grounds_with_real_files() {
        let d = tmp();
        std::fs::write(d.join("real.rs"), "x").unwrap();
        let mut s = Session::new(d.clone());
        let out = s.execute("read", &json!({"path":"invented.txt"}));
        assert!(out.contains("real.rs"), "{out}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn empty_glob_states_scope_and_offers_system() {
        let d = tmp();
        let mut s = Session::new(d.clone());
        let out = s.execute("glob", &json!({"pattern": "index.html"}));
        assert!(out.contains("project directory"), "must name the scope: {out}");
        assert!(out.contains("scope=\"system\""), "must offer the system flag: {out}");
        std::fs::remove_dir_all(&d).ok();
    }

    // scope="system" must resolve to real FILESYSTEM roots, never the project dir
    // — this is the structural fix (the model flips a flag; we supply the roots).
    #[test]
    fn system_scope_resolves_to_drive_roots_not_project() {
        let proj = PathBuf::from("C:\\some_project");
        let roots = search_roots(&proj, &json!({"scope": "system"}));
        assert!(!roots.contains(&proj), "system scope must not be the project: {roots:?}");
        assert!(roots.iter().any(|r| r.exists()), "at least one real root: {roots:?}");
        #[cfg(windows)]
        assert!(
            roots.iter().any(|r| r.to_string_lossy().contains(":\\")),
            "windows roots should be drive roots: {roots:?}"
        );
        // and project scope still resolves to the project
        let proj_roots = search_roots(&proj, &json!({}));
        assert_eq!(proj_roots, vec![proj]);
    }

    #[test]
    fn glob_finds_by_pattern() {
        let d = tmp();
        std::fs::write(d.join("Cargo.toml"), "x").unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src").join("main.rs"), "x").unwrap();
        let mut s = Session::new(d.clone());
        let out = s.execute("glob", &json!({"pattern":"**/*.rs"}));
        assert!(out.contains("main.rs"), "{out}");
        let out = s.execute("glob", &json!({"pattern":"Cargo.toml"}));
        assert!(out.contains("Cargo.toml"), "{out}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn multiedit_applies_all_edits() {
        let d = tmp();
        let f = d.join("a.rs");
        std::fs::write(&f, "let a = 1;\nlet b = 2;\n").unwrap();
        let mut s = Session::new(d.clone());
        s.execute("read", &json!({"path": "a.rs"}));
        let out = s.execute("multiedit", &json!({"path":"a.rs","edits":[
            {"old_string":"a = 1","new_string":"a = 10"},
            {"old_string":"b = 2","new_string":"b = 20"}
        ]}));
        assert!(out.contains("Applied 2 edits"), "{out}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "let a = 10;\nlet b = 20;\n");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn multiedit_is_all_or_nothing() {
        let d = tmp();
        let f = d.join("a.rs");
        std::fs::write(&f, "let a = 1;\n").unwrap();
        let mut s = Session::new(d.clone());
        s.execute("read", &json!({"path": "a.rs"}));
        // 2nd edit can't match → NOTHING should be written
        let out = s.execute("multiedit", &json!({"path":"a.rs","edits":[
            {"old_string":"a = 1","new_string":"a = 10"},
            {"old_string":"NONEXISTENT","new_string":"x"}
        ]}));
        assert!(out.contains("aborted"), "{out}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "let a = 1;\n", "file must be untouched");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn multiedit_requires_prior_read() {
        let d = tmp();
        std::fs::write(d.join("a.rs"), "x").unwrap();
        let mut s = Session::new(d.clone());
        let out = s.execute("multiedit", &json!({"path":"a.rs","edits":[{"old_string":"x","new_string":"y"}]}));
        assert!(out.contains("must `read`"), "{out}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn todo_write_then_read_roundtrips() {
        let d = tmp();
        let mut s = Session::new(d.clone());
        assert!(s.execute("todoread", &json!({})).contains("empty"));
        s.execute("todowrite", &json!({"todos":[
            {"content":"build","status":"completed"},
            {"content":"test","status":"in_progress"},
            {"content":"ship","status":"pending"}
        ]}));
        let out = s.execute("todoread", &json!({}));
        assert!(out.contains("[x] build"), "{out}");
        assert!(out.contains("[~] test"), "{out}");
        assert!(out.contains("[ ] ship"), "{out}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn html_to_text_strips_tags_and_scripts() {
        let html = "<html><head><style>x{}</style></head><body><script>bad()</script><p>Hello &amp; welcome</p></body></html>";
        let out = html_to_text(html);
        assert_eq!(out, "Hello & welcome", "{out}");
    }

    #[test]
    fn read_missing_falls_back_to_project_root() {
        let d = tmp();
        std::fs::write(d.join("real.rs"), "x").unwrap();
        let mut s = Session::new(d.clone());
        // model prepended a bogus dir; its parent doesn't exist → still grounded
        let out = s.execute("read", &json!({"path": "bogusdir/real.rs"}));
        assert!(out.contains("real.rs"), "must list real project files: {out}");
        assert!(out.contains("don't add extra directories"), "{out}");
        std::fs::remove_dir_all(&d).ok();
    }
}
