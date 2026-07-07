//! The anchored edit engine — a faithful Rust port of opencode's replacer cascade
//! (packages/opencode/src/tool/edit.ts). This is the heart of the harness: the
//! model proposes `old_string -> new_string`, and instead of demanding a
//! byte-perfect match, we try a ladder of increasingly forgiving matchers so a
//! small/imperfect model still lands the edit:
//!
//!   simple  -> line-trimmed -> block-anchor -> whitespace-normalized
//!           -> indentation-flexible -> escape-normalized
//!
//! Each matcher yields candidate substrings that actually exist in `content`; the
//! driver takes the first UNIQUE one (unless `replace_all`), and refuses a match
//! that balloons far past `old_string` (a fuzzy matcher gone wild).

type Replacer = fn(&str, &str) -> Vec<String>;

const SIMILARITY_THRESHOLD: f64 = 0.65;

/// Classic Levenshtein edit distance over chars (for block-anchor middle-line
/// similarity).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return a.len().max(b.len());
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Reconstruct the exact original substring for `lines[start..=end]`. Because
/// `content` was split on '\n', joining the same slice with '\n' reproduces the
/// byte-exact span (including any '\r'), which the driver then locates.
fn join(lines: &[&str], start: usize, end: usize) -> String {
    lines[start..=end].join("\n")
}

/// 1. Exact — the model got it byte-perfect.
fn simple(_content: &str, find: &str) -> Vec<String> {
    vec![find.to_string()]
}

/// 2. Line-trimmed — same lines, but leading/trailing whitespace differs.
fn line_trimmed(content: &str, find: &str) -> Vec<String> {
    let orig: Vec<&str> = content.split('\n').collect();
    let mut search: Vec<&str> = find.split('\n').collect();
    if search.last() == Some(&"") {
        search.pop();
    }
    if search.is_empty() || orig.len() < search.len() {
        return vec![];
    }
    let mut out = vec![];
    for i in 0..=(orig.len() - search.len()) {
        if (0..search.len()).all(|j| orig[i + j].trim() == search[j].trim()) {
            out.push(join(&orig, i, i + search.len() - 1));
        }
    }
    out
}

/// 3. Block-anchor — match a ≥3-line block by its first and last lines (trimmed),
/// tolerating drift in the middle (validated by average Levenshtein similarity).
/// This is the one that saves small models that fumble the interior of a block.
fn block_anchor(content: &str, find: &str) -> Vec<String> {
    let orig: Vec<&str> = content.split('\n').collect();
    let mut search: Vec<&str> = find.split('\n').collect();
    if search.len() < 3 {
        return vec![];
    }
    if search.last() == Some(&"") {
        search.pop();
    }
    let first = search[0].trim();
    let last = search[search.len() - 1].trim();
    let block_size = search.len();
    let max_delta = std::cmp::max(1, (block_size as f64 * 0.25).floor() as usize);

    let mut candidates: Vec<(usize, usize)> = vec![];
    for i in 0..orig.len() {
        if orig[i].trim() != first {
            continue;
        }
        for j in (i + 2)..orig.len() {
            if orig[j].trim() == last {
                let actual = j - i + 1;
                if (actual as isize - block_size as isize).unsigned_abs() <= max_delta {
                    candidates.push((i, j));
                }
                break; // only the first matching last-line
            }
        }
    }
    if candidates.is_empty() {
        return vec![];
    }

    // Average similarity of the interior lines for one candidate.
    let interior_sim = |start: usize, end: usize| -> f64 {
        let actual = end - start + 1;
        let n = std::cmp::min(block_size.saturating_sub(2), actual.saturating_sub(2));
        if n == 0 {
            return 1.0;
        }
        let mut sim = 0.0;
        let mut j = 1;
        while j < block_size - 1 && j < actual - 1 {
            let o = orig[start + j].trim();
            let s = search[j].trim();
            let maxlen = o.chars().count().max(s.chars().count());
            if maxlen != 0 {
                sim += 1.0 - levenshtein(o, s) as f64 / maxlen as f64;
            }
            j += 1;
        }
        sim / n as f64
    };

    if candidates.len() == 1 {
        let (s, e) = candidates[0];
        if interior_sim(s, e) >= SIMILARITY_THRESHOLD {
            return vec![join(&orig, s, e)];
        }
        return vec![];
    }
    let (best, max_sim) = candidates.iter().fold((None, -1.0), |(best, mx), &(s, e)| {
        let sim = interior_sim(s, e);
        if sim > mx {
            (Some((s, e)), sim)
        } else {
            (best, mx)
        }
    });
    match best {
        Some((s, e)) if max_sim >= SIMILARITY_THRESHOLD => vec![join(&orig, s, e)],
        _ => vec![],
    }
}

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 4. Whitespace-normalized — collapse all runs of whitespace; match whole lines
/// or whole multi-line blocks whose normalized form equals the (normalized) find.
fn whitespace_normalized(content: &str, find: &str) -> Vec<String> {
    let nf = normalize_ws(find);
    let lines: Vec<&str> = content.split('\n').collect();
    let mut out = vec![];
    for line in &lines {
        if normalize_ws(line) == nf {
            out.push((*line).to_string());
        }
    }
    let find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.len() > 1 && lines.len() >= find_lines.len() {
        for i in 0..=(lines.len() - find_lines.len()) {
            let block = join(&lines, i, i + find_lines.len() - 1);
            if normalize_ws(&block) == nf {
                out.push(block);
            }
        }
    }
    out
}

/// 5. Indentation-flexible — strip the common leading indentation from both sides
/// so a block indented at a different level still matches.
fn indentation_flexible(content: &str, find: &str) -> Vec<String> {
    let dedent = |text: &str| -> String {
        let lines: Vec<&str> = text.split('\n').collect();
        let min_indent = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min();
        match min_indent {
            None => text.to_string(),
            Some(m) => lines
                .iter()
                .map(|l| {
                    if l.trim().is_empty() {
                        (*l).to_string()
                    } else {
                        l[m..].to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    };
    let nf = dedent(find);
    let clines: Vec<&str> = content.split('\n').collect();
    let flen = find.split('\n').count();
    let mut out = vec![];
    if clines.len() >= flen {
        for i in 0..=(clines.len() - flen) {
            let block = join(&clines, i, i + flen - 1);
            if dedent(&block) == nf {
                out.push(block);
            }
        }
    }
    out
}

/// Turn literal escape sequences (`\n`, `\t`, …) into their real chars — for the
/// classic small-model bug of emitting `\n` as two characters.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\'') => out.push('\''),
                Some('"') => out.push('"'),
                Some('`') => out.push('`'),
                Some('\\') => out.push('\\'),
                Some('\n') => out.push('\n'),
                Some('$') => out.push('$'),
                _ => {
                    out.push('\\');
                    continue;
                }
            }
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// 6. Escape-normalized — match after un-escaping the find (and blocks of content).
fn escape_normalized(content: &str, find: &str) -> Vec<String> {
    let uf = unescape(find);
    let mut out = vec![];
    if content.contains(&uf) {
        out.push(uf.clone());
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let flen = uf.split('\n').count();
    if lines.len() >= flen {
        for i in 0..=(lines.len() - flen) {
            let block = join(&lines, i, i + flen - 1);
            if unescape(&block) == uf {
                out.push(block);
            }
        }
    }
    out
}

/// Guard: refuse a match whose span is wildly larger than the requested
/// `old_string` (a forgiving matcher latching onto too much).
fn is_disproportionate(search: &str, old: &str) -> bool {
    let old_lines = old.split('\n').count();
    let search_lines = search.split('\n').count();
    if search_lines >= std::cmp::max(old_lines + 3, old_lines * 2) {
        return true;
    }
    if old_lines == 1 {
        return false;
    }
    search.trim().len() > std::cmp::max(old.trim().len() + 500, old.trim().len() * 4)
}

/// Apply `old -> new` in `content`, walking the replacer ladder. Errors mirror
/// opencode's so the model gets actionable feedback.
pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String, String> {
    if old == new {
        return Err("No changes to apply: old_string and new_string are identical.".into());
    }
    if old.is_empty() {
        return Err("old_string cannot be empty. Provide the exact text to replace, or use write to replace the whole file.".into());
    }
    let ladder: &[Replacer] = &[
        simple,
        line_trimmed,
        block_anchor,
        whitespace_normalized,
        indentation_flexible,
        escape_normalized,
    ];
    let mut found_any = false;
    for replacer in ladder {
        for search in replacer(content, old) {
            let Some(index) = content.find(&search) else {
                continue;
            };
            found_any = true;
            if is_disproportionate(&search, old) {
                return Err("Refusing replacement: the matched span is much larger than old_string. Re-read the file and provide the exact text.".into());
            }
            if replace_all {
                return Ok(content.replace(&search, new));
            }
            if index != content.rfind(&search).unwrap() {
                continue; // not unique — try a different candidate/replacer
            }
            return Ok(format!("{}{}{}", &content[..index], new, &content[index + search.len()..]));
        }
    }
    if found_any {
        Err("Found multiple matches for old_string. Add more surrounding context to make it unique, or set replace_all.".into())
    } else {
        Err("Could not find old_string in the file. It must match the existing text (whitespace/indentation are tolerated).".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact() {
        assert_eq!(replace("hello world", "world", "there", false).unwrap(), "hello there");
    }

    #[test]
    fn tolerates_leading_whitespace() {
        let content = "fn main() {\n    let x = 1;\n}";
        // model dropped the indentation on the line it wants to change
        let out = replace(content, "let x = 1;", "let x = 2;", false).unwrap();
        assert_eq!(out, "fn main() {\n    let x = 2;\n}");
    }

    #[test]
    fn tolerates_escaped_newline() {
        // the classic bug: model sends "a\nb" as literal backslash-n
        let content = "line a\nline b\nline c";
        let out = replace(content, "line a\\nline b", "X", false).unwrap();
        assert_eq!(out, "X\nline c");
    }

    #[test]
    fn block_anchor_tolerates_middle_drift() {
        let content = "start\n  middle content here\n  another middle\nend";
        // anchors (start/end) exact, middle slightly wrong
        let find = "start\n  middle contnt here\n  another middl\nend";
        let out = replace(content, find, "REPLACED", false).unwrap();
        assert_eq!(out, "REPLACED");
    }

    #[test]
    fn rejects_not_found() {
        assert!(replace("abc", "xyz", "q", false).is_err());
    }

    #[test]
    fn rejects_ambiguous() {
        // "x" appears twice → not unique, no replace_all
        assert!(replace("x and x", "x", "y", false).is_err());
        // replace_all handles it
        assert_eq!(replace("x and x", "x", "y", true).unwrap(), "y and y");
    }

    #[test]
    fn rejects_identical() {
        assert!(replace("abc", "abc", "abc", false).is_err());
    }
}
