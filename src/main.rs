mod agent;
mod edit;
mod tools;
mod slm_harness;

use serde_json::Value;
use std::io::{self, Write};
use std::path::PathBuf;

/// Parse a config file of `KEY=value` lines into the process environment.
/// `#` comments and blank lines are skipped; lines without `=` are ignored.
/// Applied before any other env parsing so later `std::env::var` calls
/// observe these values.
fn load_config_file(path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("config {path}: {e}"))?;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if k.is_empty() {
                continue;
            }
            // SAFETY: single-threaded at startup; there are no concurrent env readers.
            unsafe { std::env::set_var(k, v.trim()) };
        }
    }
    Ok(())
}

fn main() {
    // ---- config file (per-model var list) ---------------------------------
    // A config file is a plain KEY=value list (one var per line; `#` comments
    // and blank lines ignored). Pass it with --config <path> or
    // OPENHARN_CONFIG=<path>, or let openharn auto-load configs/<model>.conf
    // when present. This is the output of tests/tune_model.sh — it removes the
    // need to retype the OPENHARN_* flags for a known-good model. Loaded first
    // so every later std::env::var sees these values.
    let mut config_path: Option<String> =
        std::env::var("OPENHARN_CONFIG").ok().filter(|s| !s.is_empty());

    let args: Vec<String> = std::env::args().collect();
    let mut cwd_arg: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "--config" {
            if i + 1 < args.len() {
                config_path = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("[config] --config requires a path; ignoring");
                i += 1;
            }
        } else if let Some(p) = a.strip_prefix("--config=") {
            config_path = Some(p.to_string());
            i += 1;
        } else {
            if cwd_arg.is_none() {
                cwd_arg = Some(a.clone());
            }
            i += 1;
        }
    }

    if config_path.is_none() {
        let m = std::env::var("OPENHARN_MODEL").unwrap_or_else(|_| "local".into());
        let auto = format!("configs/{}.conf", m);
        if std::path::Path::new(&auto).exists() {
            config_path = Some(auto);
        }
    }

    if let Some(p) = &config_path {
        match load_config_file(p) {
            Ok(()) => println!("[config] loaded {}", p),
            Err(e) => eprintln!("[config] {}", e),
        }
    }

    // Config via env, defaulting to a local llama-server. For a cloud provider,
    // set OPENHARN_BASE_URL + OPENHARN_API_KEY (any OpenAI-compatible endpoint).
    let base_url =
        std::env::var("OPENHARN_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080/v1".into());
    let model = std::env::var("OPENHARN_MODEL").unwrap_or_else(|_| "local".into());
    let api_key = std::env::var("OPENHARN_API_KEY").ok().filter(|s| !s.is_empty());

    // Working directory the agent operates on: first positional CLI arg, else cwd.
    let cwd: PathBuf = cwd_arg
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    let friendly_results = std::env::var_os("OPENHARN_FRIENDLY_RESULTS").is_some();

    let max_tokens: u32 = std::env::var("OPENHARN_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4096);

    let cfg = agent::Config {
        base_url,
        model,
        api_key,
        max_turns: 25,
        max_tokens,
        temperature: 0.2,
        friendly_results,
    };

    println!(
        "openharn · {} · model={} · dir={}",
        cfg.base_url,
        cfg.model,
        cwd.display()
    );
    println!("type a request; /reset clears context, /exit quits.\n");

    let mut history: Vec<Value> = Vec::new();
    let mut session = tools::Session::new(cwd.clone());
    let stdin = io::stdin();
    loop {
        print!("\x1b[1m› \x1b[0m");
        io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF
        }
        let line = line.trim();
        match line {
            "" => continue,
            "/exit" | "/quit" => break,
            "/reset" => {
                history.clear();
                session = tools::Session::new(cwd.clone());
                println!("(context reset)");
            }
            _ => agent::run(&cfg, &mut history, &mut session, line),
        }
    }
}
