mod agent;
mod api;
mod plugins;
mod tools;
mod ui;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use clap::Parser;
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    api_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    system_prompt: Option<String>,
    plugins: Option<String>,
    context_limit: Option<u64>,
    provider: Option<String>,
}

fn load_config() -> Config {
    let path = dirs();
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Config::default(),
    };
    match serde_json::from_str(&contents) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: failed to parse {}: {}", path.display(), e);
            Config::default()
        }
    }
}

fn dirs() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("lean_agent.json")
}

#[derive(Parser)]
#[command(name = "coding-agent", about = "A minimal coding agent")]
struct Args {
    /// OpenAI-compatible API base URL
    #[arg(long)]
    api_url: Option<String>,

    /// Model to use
    #[arg(long, short)]
    model: Option<String>,

    /// API key (optional)
    #[arg(long, short)]
    api_key: Option<String>,

    /// Custom system prompt (optional)
    #[arg(long)]
    system_prompt: Option<String>,

    /// Directory to load Lua plugins from (default: ./plugins, skipped if absent)
    #[arg(long)]
    plugins: Option<String>,

    /// Model context window size in tokens (for context usage display)
    #[arg(long)]
    context_limit: Option<u64>,

    /// Provider name to route through (e.g. "Together")
    #[arg(long)]
    provider: Option<String>,
}

fn resolve(args: Args) -> Resolved {
    let cfg = load_config();

    let api_url = args.api_url
        .or(cfg.api_url)
        .unwrap_or_else(|| "http://0.0.0.0:8080/v1".to_string());
    let model = args.model
        .or(cfg.model)
        .unwrap_or_else(|| "default".to_string());
    let api_key = args.api_key.or(cfg.api_key);
    let system_prompt = args.system_prompt.or(cfg.system_prompt);
    let plugins = args.plugins.or(cfg.plugins);
    let context_limit = args.context_limit.or(cfg.context_limit).unwrap_or(128000);

    let provider = args.provider.or(cfg.provider);

    Resolved {
        api_url,
        model,
        api_key,
        system_prompt,
        plugins,
        context_limit,
        provider,
    }
}

struct Resolved {
    api_url: String,
    model: String,
    api_key: Option<String>,
    system_prompt: Option<String>,
    plugins: Option<String>,
    context_limit: u64,
    provider: Option<String>,
}

fn main() {
    let args = Args::parse();
    let r = resolve(args);

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();

    let client = api::ApiClient::new(r.api_url, r.api_key);
    let model = r.model.clone();
    let system_prompt = r.system_prompt.clone();
    let plugin_dir = r.plugins.clone();
    let provider = r.provider.clone();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let cancel_agent = cancel.clone();
    std::thread::spawn(move || {
        agent::run(client, model, system_prompt, plugin_dir, provider, cancel_agent, cmd_rx, ui_tx);
    });

    let mut app = ui::App::new(r.model, r.context_limit, cancel, cmd_tx, ui_rx);

    if let Err(e) = app.run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
