use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::api::{self, ChatRequest, Message, StreamOptions};
use crate::plugins::PluginManager;
use crate::tools;

const SYSTEM_PROMPT: &str = "\
You are a coding agent. You help users with software engineering tasks \
by reading, writing, and editing files, and running shell commands.

## Guidelines
- Always read a file before editing it to understand current contents.
- Prefer edit over write for targeted changes.
- Verify changes by running relevant commands (tests, linters, type checkers).
- Be concise and direct. Do not add unnecessary comments unless asked.
- When running bash, combine related commands with &&.";

const MAX_TOOL_ITERATIONS: usize = 50;
const MAX_TOOL_OUTPUT: usize = 50_000;

pub enum AgentCommand {
    Send(String),
}

pub enum UiEvent {
    PluginsLoaded { count: usize },
    ThinkingStart,
    ThinkingDelta(String),
    TextStart,
    TextDelta(String),
    ToolCall { name: String, args_summary: String },
    ToolResult { output_summary: String },
    TokenUsage { context: u64, output: u64 },
    ResponseMeta { tokens: u64, elapsed_secs: f64, tok_per_sec: f64 },
    Error(String),
    Done,
}

pub fn run(
    client: api::ApiClient,
    model: String,
    system_prompt: Option<String>,
    plugin_dir: Option<String>,
    provider: Option<String>,
    cancel: Arc<AtomicBool>,
    cmd_rx: mpsc::Receiver<AgentCommand>,
    ui_tx: mpsc::Sender<UiEvent>,
) {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let base_prompt = system_prompt.unwrap_or_else(|| SYSTEM_PROMPT.to_string());
    let prompt = format!(
        "{}\n\nCurrent working directory: {}",
        base_prompt, cwd
    );
    let mut messages = vec![Message::System { content: prompt }];
    let mut all_tools = tools::definitions();

    let mut plugins = PluginManager::new();
    if let Some(ref dir) = plugin_dir {
        match plugins.load_dir(Path::new(dir)) {
            Ok(count) if count > 0 => {
                let _ = ui_tx.send(UiEvent::PluginsLoaded { count });
            }
            Err(e) => {
                let _ = ui_tx.send(UiEvent::Error(format!("[plugins] {}", e)));
            }
            _ => {}
        }
    }
    all_tools.extend(plugins.definitions().to_vec());

    loop {
        let cmd = match cmd_rx.recv() {
            Ok(c) => c,
            Err(_) => return,
        };

        match cmd {
            AgentCommand::Send(input) => {
                messages.push(Message::User { content: input });
                if let Err(e) = agent_loop(&client, &model, &provider, &mut messages, &all_tools, &plugins, &cancel, &ui_tx) {
                    let _ = ui_tx.send(UiEvent::Error(e));
                }
                let _ = ui_tx.send(UiEvent::Done);
            }
        }
    }
}

fn agent_loop(
    client: &api::ApiClient,
    model: &str,
    provider: &Option<String>,
    messages: &mut Vec<Message>,
    tool_defs: &[api::ToolDef],
    plugins: &PluginManager,
    cancel: &AtomicBool,
    ui_tx: &mpsc::Sender<UiEvent>,
) -> Result<(), String> {
    for _ in 0..MAX_TOOL_ITERATIONS {
        if cancel.load(Ordering::Relaxed) {
            cancel.store(false, Ordering::Relaxed);
            return Ok(());
        }
        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.clone(),
            stream: true,
            tools: Some(tool_defs.to_vec()),
            stream_options: Some(StreamOptions { include_usage: true }),
            provider: provider.clone(),
        };

        let start = Instant::now();

        let mut has_tool_calls = false;
        let mut tool_call_ids: Vec<String> = Vec::new();
        let mut tool_call_names: Vec<String> = Vec::new();
        let mut thinking_started = false;
        let mut text_started = false;

        client.chat_stream(&request, |event| {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }

            match event {
                api::StreamEvent::ThinkingDelta(text) => {
                    if !thinking_started {
                        thinking_started = true;
                        let _ = ui_tx.send(UiEvent::ThinkingStart);
                    }
                    let _ = ui_tx.send(UiEvent::ThinkingDelta(text));
                    Ok(())
                }
                api::StreamEvent::ContentDelta(text) => {
                    if !text_started {
                        text_started = true;
                        let _ = ui_tx.send(UiEvent::TextStart);
                    }
                    let _ = ui_tx.send(UiEvent::TextDelta(text));
                    Ok(())
                }
                api::StreamEvent::ToolCallBegin { index, id, name } => {
                    has_tool_calls = true;
                    while tool_call_ids.len() <= index {
                        tool_call_ids.push(String::new());
                        tool_call_names.push(String::new());
                    }
                    tool_call_ids[index] = id;
                    tool_call_names[index] = name;
                    Ok(())
                }
                api::StreamEvent::ToolCallDelta { .. } => Ok(()),
                api::StreamEvent::Done { message, usage } => {
                    let elapsed = start.elapsed().as_secs_f64();

                    if let Some(ref u) = usage {
                        let _ = ui_tx.send(UiEvent::TokenUsage {
                            context: u.prompt_tokens,
                            output: u.completion_tokens,
                        });
                        let tok_per_sec = if elapsed > 0.0 {
                            u.completion_tokens as f64 / elapsed
                        } else {
                            0.0
                        };
                        let _ = ui_tx.send(UiEvent::ResponseMeta {
                            tokens: u.completion_tokens,
                            elapsed_secs: elapsed,
                            tok_per_sec,
                        });
                    }

                    let content = if message.content.is_none() && message.tool_calls.is_none() {
                        Some(String::new())
                    } else {
                        message.content.clone()
                    };
                    messages.push(Message::Assistant {
                        content,
                        tool_calls: message.tool_calls.clone(),
                    });

                    if let Some(tcs) = &message.tool_calls {
                        for tc in tcs {
                            let args: serde_json::Value =
                                serde_json::from_str(&tc.function.arguments).unwrap_or_default();

                            let args_summary = summarize_args(&tc.function.name, &args);
                            let _ = ui_tx.send(UiEvent::ToolCall {
                                name: tc.function.name.clone(),
                                args_summary,
                            });

                            let result = if plugins.has_tool(&tc.function.name) {
                                plugins.execute(&tc.function.name, args)
                            } else {
                                tools::execute(&tc.function.name, args)
                            };

                            let display = if result.len() > 500 {
                                let truncated: String = result.chars().take(500).collect();
                                format!("{}...\n[truncated, {} chars total]", truncated, result.len())
                            } else {
                                result.clone()
                            };
                            let _ = ui_tx.send(UiEvent::ToolResult {
                                output_summary: display,
                            });

                            let for_api = if result.len() > MAX_TOOL_OUTPUT {
                                let truncated: String = result.chars().take(MAX_TOOL_OUTPUT).collect();
                                format!(
                                    "{}\n\n[output truncated, {} total characters]",
                                    truncated,
                                    result.len()
                                )
                            } else {
                                result
                            };

                            messages.push(Message::Tool {
                                tool_call_id: tc.id.clone(),
                                content: for_api,
                            });
                        }
                    }

                    Ok(())
                }
            }
        })?;

        if !has_tool_calls {
            return Ok(());
        }
    }

    Err(format!(
        "Agent loop exceeded max iterations ({})",
        MAX_TOOL_ITERATIONS
    ))
}

fn summarize_args(name: &str, args: &serde_json::Value) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    match name {
        "bash" => args["command"]
            .as_str()
            .unwrap_or("?")
            .lines()
            .next()
            .unwrap_or("?")
            .to_string(),
        "read" | "write" | "edit" => args["file_path"]
            .as_str()
            .unwrap_or("?")
            .replace(&cwd, ".")
            ,
        _ => args.to_string(),
    }
}
