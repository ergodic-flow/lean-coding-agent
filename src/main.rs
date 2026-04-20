mod agent;
mod api;
mod plugins;
mod tools;
mod ui;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use clap::Parser;

#[derive(Parser)]
#[command(name = "coding-agent", about = "A minimal coding agent")]
struct Args {
    /// OpenAI-compatible API base URL
    #[arg(long, default_value = "http://0.0.0.0:8080/v1")]
    api_url: String,

    /// Model to use
    #[arg(long, short, default_value = "default")]
    model: String,

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
    #[arg(long, default_value = "128000")]
    context_limit: u64,
}

fn main() {
    let args = Args::parse();

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();

    let client = api::ApiClient::new(args.api_url, args.api_key);
    let model = args.model.clone();
    let system_prompt = args.system_prompt.clone();
    let plugin_dir = args.plugins.clone();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let cancel_agent = cancel.clone();
    std::thread::spawn(move || {
        agent::run(client, model, system_prompt, plugin_dir, cancel_agent, cmd_rx, ui_tx);
    });

    let mut app = ui::App::new(args.model, args.context_limit, cancel, cmd_tx, ui_rx);

    if let Err(e) = app.run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
