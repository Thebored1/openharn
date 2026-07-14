mod agent;
mod edit;
mod tools;
mod slm_harness;
mod myelin;

use serde_json::Value;
use std::io::{self, Write};
use std::path::PathBuf;

fn main() {
    // Myelin HTTP server mode: OPENHARN_MYELIN=1 serves /v1/chat/completions on OPENHARN_MYELIN_PORT (default 8090)
    if std::env::var_os("OPENHARN_MYELIN").is_some() {
        let port = std::env::var("OPENHARN_MYELIN_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8090);
        return myelin::run_myelin_server(port);
    }

    // Config via env, defaulting to a local llama-server. For a cloud provider,
    // set OPENHARN_BASE_URL + OPENHARN_API_KEY (any OpenAI-compatible endpoint).
    let base_url =
        std::env::var("OPENHARN_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080/v1".into());
    let model = std::env::var("OPENHARN_MODEL").unwrap_or_else(|_| "local".into());
    let api_key = std::env::var("OPENHARN_API_KEY").ok().filter(|s| !s.is_empty());

    // Working directory the agent operates on: first CLI arg, else current dir.
    let cwd: PathBuf = std::env::args()
        .nth(1)
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