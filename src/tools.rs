//! opencode's tool set, ported: read, write, edit, glob, grep, bash. The harness
//! keeps a per-session read-set so `edit`/`write` fail on a file that wasn't read
//! first (opencode's grounding guard — you can't act on a file you don't
//! understand). `edit` wraps the anchored replacer engine; navigation is glob +
//! grep, not a weak "list".

use crate::edit;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

const DEFAULT_READ_LIMIT: usize = 2000;
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", ".cargo", "dist", "build", ".venv", "__pycache__",
];
/// EXTRA directories skipped only during a whole-system walk (glob_system / grep_system).
/// These OS / app-internal trees dominate a full walk — `C:\Windows\WinSxS` alone is hundreds
/// of thousands of directories — and virtually never hold the user file someone is looking
/// for. Skipping them is what lets a system search actually REACH user directories within the
/// time budget. A file genuinely inside one of these is still findable via `glob` with an
/// absolute `path`.
const SKIP_DIRS_SYSTEM: &[&str] = &[
    "Windows", "Windows.old", "$Recycle.Bin", "$WinREAgent", "$SysReset",
    "System Volume Information", "PerfLogs", "Recovery", "MSOCache", "AppData",
];
/// Wall-clock budget for a whole-system walk (glob_system / grep_system). A weak model
/// routinely emits a pattern that matches nothing, and without a bound the walk traverses
/// every drive to completion (minutes on Windows). Cut it off and say so.
const SYSTEM_SEARCH_BUDGET: Duration = Duration::from_secs(15);

/// Strip ONE pair of matching surrounding quotes from a pattern. Small models constantly
/// wrap arguments in quotes (`'index.html'`, `"*.rs"`), which then match nothing. Quotes are
/// never meaningful in a glob pattern, so removing a surrounding pair is always safe here.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn skippable(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && SKIP_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
}

/// Stricter skip for whole-system walks: everything `skippable` skips, plus the heavy OS /
/// app-internal trees in `SKIP_DIRS_SYSTEM`.
fn skippable_system(entry: &walkdir::DirEntry) -> bool {
    skippable(entry)
        || (entry.file_type().is_dir()
            && SKIP_DIRS_SYSTEM.contains(&entry.file_name().to_string_lossy().as_ref()))
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

/// Live in-place progress line for a whole-system walk (throttled to ~150ms). Shows how far
/// into the time budget we are and how many directories have been scanned, so a long
/// `glob_system`/`grep_system` doesn't look frozen.
fn search_tick(start: Instant, last: &mut Instant, dirs: u64, label: &str) {
    if last.elapsed().as_millis() >= 150 {
        print!(
            "\r\x1b[2m  {label}… {:.0}s / {:.0}s · {dirs} dirs\x1b[0m\x1b[K",
            start.elapsed().as_secs_f64(),
            SYSTEM_SEARCH_BUDGET.as_secs_f64()
        );
        let _ = std::io::stdout().flush();
        *last = Instant::now();
    }
}
/// Erase the live progress line before the result is printed.
fn search_clear() {
    print!("\r\x1b[K");
    let _ = std::io::stdout().flush();
}

/// Shared walk+glob logic used by both glob_tool and glob_system_tool. `label` is `Some` only
/// for the whole-system walk: it enables the time budget + live progress line. Returns
/// `(matches, timed_out)`.
fn do_glob_search(
    roots: &[PathBuf],
    pat: &glob::Pattern,
    basename_ok: bool,
    label: Option<&str>,
) -> (Vec<String>, bool) {
    let mut out: Vec<String> = vec![];
    let start = Instant::now();
    let mut last_tick = start;
    let mut dirs = 0u64;
    let mut timed_out = false;
    let system = label.is_some();
    'outer: for root in roots {
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| if system { !skippable_system(e) } else { !skippable(e) })
            .filter_map(|e| e.ok())
        {
            if let Some(lbl) = label {
                if entry.file_type().is_dir() {
                    dirs += 1;
                }
                search_tick(start, &mut last_tick, dirs, lbl);
                if start.elapsed() >= SYSTEM_SEARCH_BUDGET {
                    timed_out = true;
                    break 'outer;
                }
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
    if label.is_some() {
        search_clear();
    }
    out.sort();
    (out, timed_out)
}

/// Shared walk+grep logic used by both grep and grep_system. `label` is `Some` only for the
/// whole-system walk: it enables the time budget + live progress line. Returns
/// `(matches, timed_out)`.
fn do_grep_search(
    roots: &[PathBuf],
    re: &regex::Regex,
    include: Option<&glob::Pattern>,
    label: Option<&str>,
) -> (Vec<String>, bool) {
    let mut out: Vec<String> = vec![];
    let start = Instant::now();
    let mut last_tick = start;
    let mut dirs = 0u64;
    let mut timed_out = false;
    let system = label.is_some();
    'outer: for root in roots {
      for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| if system { !skippable_system(e) } else { !skippable(e) })
        .filter_map(|e| e.ok())
    {
        if let Some(lbl) = label {
            if entry.file_type().is_dir() {
                dirs += 1;
            }
            search_tick(start, &mut last_tick, dirs, lbl);
            if start.elapsed() >= SYSTEM_SEARCH_BUDGET {
                timed_out = true;
                break 'outer;
            }
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(inc) = include {
            if !entry.file_name().to_str().map(|n| inc.matches(n)).unwrap_or(false) {
                continue;
            }
        }
        // no cap on files searched
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
    if label.is_some() {
        search_clear();
    }
    (out, timed_out)
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
            "glob_system" => self.glob_system_tool(args),
            "grep" => grep(&self.cwd, args),
            "grep_system" => grep_system(&self.cwd, args),
            "bash" => bash(&self.cwd, args),
            "webfetch" => webfetch(args),
            "todowrite" => self.todowrite(args),
            "todoread" => self.todoread(),
            "python" => python(&self.cwd, args),
            other => format!(
                "'{other}' is not an available tool. The tools are: read, write, edit, multiedit, glob, glob_system, grep, grep_system, bash, webfetch, todowrite, todoread. To find a file by name use `glob`; to search file contents use `grep`; for system-wide search use `glob_system` or `grep_system`."
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
        if args.get("scope").is_some() {
            return "Error: 'scope' is not a valid parameter for 'glob'. To search the entire system, use the 'glob_system' tool instead.".into();
        }
        let Some(raw) = args["pattern"].as_str() else {
            return "Error: glob requires 'pattern'.".into();
        };
        let pattern = unquote(raw);
        let pat = match glob::Pattern::new(pattern) {
            Ok(p) => p,
            Err(e) => return format!("Invalid glob pattern: {e}"),
        };
        let basename_ok = !pattern.contains('/') && !pattern.contains('\\');
        let root = resolve(&self.cwd, args["path"].as_str().unwrap_or("."));
        if !root.exists() {
            return ground_missing_path(&root, &self.cwd);
        }
        let (out, _capped) = do_glob_search(&[root.clone()], &pat, basename_ok, None);
        if !out.is_empty() {
            return out.join("\n");
        }
        format!(
            "No files matching '{pattern}' under {} (the project directory — the ONLY place searched). To search the ENTIRE computer, use the 'glob_system' tool instead.",
            root.display()
        )
    }

    fn glob_system_tool(&self, args: &Value) -> String {
        if args.get("scope").is_some() {
            return "Error: 'scope' is not a valid parameter for 'glob_system'. It always searches the entire system.".into();
        }
        let Some(raw) = args["pattern"].as_str() else {
            return "Error: glob_system requires 'pattern'.".into();
        };
        let pattern = unquote(raw);
        let pat = match glob::Pattern::new(pattern) {
            Ok(p) => p,
            Err(e) => return format!("Invalid glob pattern: {e}"),
        };
        let basename_ok = !pattern.contains('/') && !pattern.contains('\\');
        let roots = system_roots();
        let (out, timed_out) = do_glob_search(&roots, &pat, basename_ok, Some("searching all drives"));
        if !out.is_empty() {
            return out.join("\n");
        }
        let where_ = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ");
        let note = if timed_out {
            format!(" (stopped after the {}s time limit before finishing — narrow the search or give an absolute `path` to `glob`)", SYSTEM_SEARCH_BUDGET.as_secs())
        } else {
            String::new()
        };
        format!("No files matching '{pattern}' anywhere on the system — searched {where_}{note}.")
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
    if args.get("scope").is_some() {
        return "Error: 'scope' is not a valid parameter for 'grep'. To search the entire system, use the 'grep_system' tool instead.".into();
    }
    let Some(pat) = args["pattern"].as_str() else {
        return "Error: grep requires 'pattern'.".into();
    };
    let re = match regex::Regex::new(pat) {
        Ok(r) => r,
        Err(e) => return format!("Invalid regex: {e}"),
    };
    let include = args["include"]
        .as_str()
        .and_then(|p| glob::Pattern::new(unquote(p)).ok());
    let root = resolve(cwd, args["path"].as_str().unwrap_or("."));
    if !root.exists() {
        return ground_missing_path(&root, cwd);
    }
    let (out, _capped) = do_grep_search(&[root.clone()], &re, include.as_ref(), None);
    if !out.is_empty() {
        return out.join("\n");
    }
    format!(
        "No matches for /{pat}/ under {} (the project directory — the ONLY place searched). To search the ENTIRE computer, use the 'grep_system' tool instead.",
        root.display()
    )
}

fn grep_system(_cwd: &Path, args: &Value) -> String {
    if args.get("scope").is_some() {
        return "Error: 'scope' is not a valid parameter for 'grep_system'. It always searches the entire system.".into();
    }
    let Some(pat) = args["pattern"].as_str() else {
        return "Error: grep_system requires 'pattern'.".into();
    };
    let re = match regex::Regex::new(pat) {
        Ok(r) => r,
        Err(e) => return format!("Invalid regex: {e}"),
    };
    let include = args["include"]
        .as_str()
        .and_then(|p| glob::Pattern::new(unquote(p)).ok());
    let roots = system_roots();
    let (out, timed_out) = do_grep_search(&roots, &re, include.as_ref(), Some("grepping all drives"));
    if !out.is_empty() {
        return out.join("\n");
    }
    let where_ = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ");
    let note = if timed_out {
        format!(" (stopped after the {}s time limit before finishing)", SYSTEM_SEARCH_BUDGET.as_secs())
    } else {
        String::new()
    };
    format!("No matches for /{pat}/ anywhere on the system — searched {where_}{note}.")
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

fn python(cwd: &Path, args: &Value) -> String {
    let Some(code) = args["code"].as_str() else {
        return "Error: python requires 'code'.".into();
    };
    let output = if cfg!(windows) {
        std::process::Command::new("python")
            .arg("-c")
            .arg(code)
            .current_dir(cwd)
            .output()
    } else {
        std::process::Command::new("python3")
            .arg("-c")
            .arg(code)
            .current_dir(cwd)
            .output()
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
        Err(e) => format!("Error running python: {e}"),
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
            "description":"Fast file pattern matching in a directory tree. Supports glob patterns like \"**/*.rs\" or \"src/**/*.ts\". Returns matching file paths. Use this to find files by NAME. PREFER glob over grep for finding files. When the user names a SPECIFIC directory to look in (e.g. 'find X under C:\\\\Files\\\\Projects'), use this tool and pass that directory as an absolute `path` — do NOT use glob_system, which ignores the path and scans every drive. Only use glob_system when you have no idea where on the computer a file is.",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string","description":"Glob pattern, e.g. **/*.rs or Cargo.toml. Do NOT wrap it in quotes."},
                "path":{"type":"string","description":"Directory to search under. Accepts an ABSOLUTE path (e.g. C:\\\\Files\\\\Projects\\\\sandbox) to search any folder on disk, or a project-relative subdir. Default: project root."}
            },"required":["pattern"]}
        }},
        {"type":"function","function":{
            "name":"glob_system",
            "description":"Search every drive for files matching a glob pattern. Capped at a time limit, and SKIPS OS / app-internal trees (Windows, AppData, $Recycle.Bin, …) so it reaches user files fast — a file inside those is only findable via `glob` with an absolute `path`. Use ONLY when you don't know which directory a file is in; if the user names a directory, use `glob` with that absolute `path` instead.",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string","description":"Glob pattern, e.g. **/*.rs or *.txt. Do NOT wrap it in quotes."}
            },"required":["pattern"]}
        }},
        {"type":"function","function":{
            "name":"grep",
            "description":"Search file CONTENTS by regex in the PROJECT directory. Returns matching `file:line: text`. Supports full regex. Filter files with `include` (e.g. \"*.rs\", \"*.{ts,tsx}\"). Use grep ONLY to search file CONTENTS — never to find a file by its name (use glob or glob_system for that). To search the ENTIRE computer, use the 'grep_system' tool instead.",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string","description":"Regular expression to search for in file contents."},
                "include":{"type":"string","description":"Only search files whose name matches this glob (optional)."},
                "path":{"type":"string","description":"A subdirectory of the project to search under (optional). Default: project root."}
            },"required":["pattern"]}
        }},
        {"type":"function","function":{
            "name":"grep_system",
            "description":"Search the CONTENTS of ALL files on the ENTIRE computer by regex. Use this when you need to search outside the project directory. Supports full regex; filter with `include`.",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string","description":"Regular expression to search for in file contents."},
                "include":{"type":"string","description":"Only search files whose name matches this glob (optional)."}
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
        }},
        {"type":"function","function":{
            "name":"python","description":"Run Python code in a sandboxed subprocess. Returns stdout/stderr. Use for computation, data processing, or any task expressible as code. Variables persist across calls within the same session. IMPORTANT: Use print() to output results — the last expression is NOT automatically returned.",
            "parameters":{"type":"object","properties":{
                "code":{"type":"string","description":"Python code to execute."}
            },"required":["code"]}
        }}
    ])
}

/// Async wrapper for grep tool (for SLM harness)
pub async fn grep_tool(pattern: &str, file_glob: Option<&str>, path: Option<&str>, limit: usize) -> ToolResult {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let args = json!({
        "pattern": pattern,
        "file_glob": file_glob.unwrap_or("**/*"),
        "path": path.unwrap_or("."),
        "limit": limit,
    });
    let out = tokio::task::spawn_blocking(move || grep(&cwd, &args)).await.unwrap_or_else(|e| format!("grep task failed: {e}"));
    let is_error = out.starts_with("Error:");
    let error = if is_error { Some(out.clone()) } else { None };
    ToolResult { output: out, is_error, error }
}

/// Async wrapper for read tool (for SLM harness)
pub async fn read_tool(path: &str, offset: Option<usize>, limit: Option<usize>) -> ToolResult {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let args = json!({
        "path": path,
        "offset": offset.unwrap_or(1),
        "limit": limit.unwrap_or(2000),
    });
    let out = tokio::task::spawn_blocking(move || {
        let mut session = Session::new(cwd);
        session.read_tool(&args)
    }).await.unwrap_or_else(|e| format!("read task failed: {e}"));
    let is_error = out.starts_with("Error:") || out.starts_with("Error ");
    let error = if is_error { Some(out.clone()) } else { None };
    ToolResult { output: out, is_error, error }
}

/// Result type for async tool calls
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    pub error: Option<String>,
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
    fn unquote_strips_one_matching_pair() {
        assert_eq!(unquote("'index.html'"), "index.html");
        assert_eq!(unquote("\"*.rs\""), "*.rs");
        assert_eq!(unquote("*.rs"), "*.rs"); // no quotes → unchanged
        assert_eq!(unquote("'"), "'"); // single char → unchanged
        assert_eq!(unquote("'a\""), "'a\""); // mismatched quotes → unchanged
    }

    #[test]
    fn glob_unquotes_pattern_and_takes_absolute_path() {
        // the exact shape that was failing: a quoted pattern AND an absolute directory.
        let d = tmp();
        std::fs::write(d.join("index.html"), "<html>").unwrap();
        let mut s = Session::new(std::env::current_dir().unwrap());
        let out = s.execute(
            "glob",
            &json!({ "path": d.to_str().unwrap(), "pattern": "'index.html'" }),
        );
        assert!(
            out.contains("index.html"),
            "quoted pattern + absolute path should still find the file, got: {out}"
        );
    }

    #[test]
    #[ignore] // diagnostic: print what glob patterns match a nested path
    fn probe_glob_matching() {
        for pat in ["**/index.html", "index.html", "*index.html", "**/*index.html"] {
            let p = glob::Pattern::new(pat).unwrap();
            for path in ["Files/Projects/sandbox/index.html", "index.html"] {
                eprintln!("[probe] {pat:14} matches_path({path:36}) = {}", p.matches_path(Path::new(path)));
            }
            eprintln!("[probe] {pat:14} matches(basename 'index.html')          = {}", p.matches("index.html"));
        }
    }

    #[test]
    #[ignore] // walks the real filesystem; proves a user file is now REACHED (OS trees skipped)
    fn glob_system_finds_a_reachable_user_file() {
        let dir = std::env::current_dir().unwrap().join("openharn_sysfind_marker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zzq_openharn_marker.html"), "x").unwrap();
        let mut s = Session::new(std::env::current_dir().unwrap());
        let t0 = Instant::now();
        let out = s.execute("glob_system", &json!({ "pattern": "**/zzq_openharn_marker.html" }));
        let dt = t0.elapsed();
        std::fs::remove_dir_all(&dir).ok();
        eprintln!("[demo] glob_system in {:.1}s -> {out}", dt.as_secs_f64());
        assert!(
            out.contains("zzq_openharn_marker.html"),
            "system search should reach + find the marker now that OS trees are skipped: {out}"
        );
    }

    #[test]
    #[ignore] // walks the real filesystem up to the budget; run explicitly with --ignored --nocapture
    fn glob_system_respects_time_budget() {
        let mut s = Session::new(std::env::current_dir().unwrap());
        let t0 = Instant::now();
        // matches ~nothing → forces a full (but bounded) walk of every drive
        let out = s.execute("glob_system", &json!({ "pattern": "zzz_openharn_none_*.qqq" }));
        let dt = t0.elapsed();
        eprintln!("\n[demo] glob_system returned in {:.1}s\n[demo] result: {out}", dt.as_secs_f64());
        assert!(
            dt <= SYSTEM_SEARCH_BUDGET + Duration::from_secs(4),
            "should stop near the {}s budget; took {:.1}s",
            SYSTEM_SEARCH_BUDGET.as_secs(),
            dt.as_secs_f64()
        );
    }

    #[cfg(windows)]
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
        assert!(out.contains("glob_system"), "must offer the system tool: {out}");
        std::fs::remove_dir_all(&d).ok();
    }

    // system_roots must resolve to real FILESYSTEM roots, never the project dir.
    #[test]
    fn system_roots_resolves_to_drive_roots() {
        let roots = system_roots();
        assert!(!roots.is_empty(), "at least one root: {roots:?}");
        assert!(roots.iter().any(|r| r.exists()), "at least one real root: {roots:?}");
        #[cfg(windows)]
        assert!(
            roots.iter().any(|r| r.to_string_lossy().contains(":\\")),
            "windows roots should be drive roots: {roots:?}"
        );
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
