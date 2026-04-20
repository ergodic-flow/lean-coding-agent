# lean-coding-agent

A lean and mean coding agent that runs in your terminal. Connects to any OpenAI-compatible API (Ollama, OpenAI, LM Studio, etc.) and provides file editing, shell execution, and a Lua plugin system for custom tools.

Written in Rust.

![screenshot of tool](/assets/screenshot.jpeg)

## Quick Start

```bash
# Build
cargo build --release

# Run against your OpenAI compatible server
./target/release/coding-agent --api-url http://0.0.0.0:8080/v1 --api-key YOUR_API_KEY -m default

# With custom plugins directory
./target/release/coding-agent --plugins ./my-tools
```

## CLI Options

| Flag | Default | Description |
|------|---------|-------------|
| `--api-url` | `http://localhost:11434/v1` | OpenAI-compatible API base URL |
| `-m, --model` | `qwen3:30b-a3b` | Model to use |
| `-a, --api-key` | none | API key (optional for local models) |
| `--system-prompt` | built-in | Override the default system prompt |
| `--plugins` | `./plugins` | Directory to load Lua plugins from (skipped if absent) |
| `--context-limit` | `128000` | Model context window size (for usage display) |

## Built-in Tools

Four tools are always available:

- **bash** — Execute shell commands. Returns stdout, stderr, and exit code.
- **read** — Read file contents with line numbers. Supports offset and limit for ranged reads.
- **write** — Write to a file. Creates parent directories automatically. Overwrites existing files.
- **edit** — Replace exact text in a file. Supports single or global replacement. Fails on ambiguous matches unless `replace_all` is set.

All file tools resolve relative paths against the current working directory.

## TUI

The terminal UI is built with ratatui and shows:

- **Header bar** — Model name, status (idle/thinking/canceling), and token usage stats
- **Conversation area** — Scrollable history of user messages, assistant responses, thinking, tool calls with output, and per-response metadata
- **Input bar** — Single-line input with visible cursor

### Keybindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Ctrl+C` | Quit |
| `Esc` (once) | Show cancel warning while agent is busy |
| `Esc` (twice, within 2s) | Cancel the in-flight request |
| `Shift+Up/Down` | Scroll conversation by 3 lines |
| `PageUp/PageDown` | Scroll conversation by 20 lines |

### Token Usage Display

The header shows real-time token stats, right-aligned:

```
ctx:12.3k/128k (10%) · out:1.2k
```

- **ctx** — Current context size from the latest API response, shown as a fraction of the model's context window with percentage
- **out** — Cumulative completion tokens generated across all turns

After each assistant response, a metadata line shows throughput:

```
487 tokens · 32.5 tok/s · 15.0s
```

## Lua Plugin System

The agent can load custom tools written in Lua from a plugins directory. Each `.lua` file can define one or more tools using the `tool()` function.

![screenshot of tool usage](/assets/image.jpeg)

### How It Works

1. On startup, the agent scans the plugins directory for `.lua` files
2. Each file is executed in a Lua 5.4 runtime (vendored, no system dependency)
3. Tools are registered via the `tool()` function
4. Tool definitions are merged with built-in tools and sent to the model
5. When the model calls a plugin tool, the handler function executes in Lua

Bad plugins are skipped with a warning — the agent continues without them.

### Writing a Plugin

```lua
tool {
    name = "my_tool",
    description = "What this tool does",
    parameters = {
        type = "object",
        properties = {
            input = {
                type = "string",
                description = "Some input"
            }
        },
        required = { "input" }
    },
    handler = function(args)
        return "result: " .. args.input
    end
}
```

The `parameters` table follows JSON Schema format — it's passed directly to the API as the tool's parameter schema.

The `handler` function receives a table of arguments (converted from JSON) and must return a string.

### Example: arithmetic.lua

The included plugin provides basic arithmetic:

```lua
tool {
    name = "arithmetic",
    description = "Perform basic arithmetic operations (add, subtract, multiply, divide) on two numbers.",
    parameters = {
        type = "object",
        properties = {
            operation = {
                type = "string",
                description = "The operation: add, subtract, multiply, or divide"
            },
            a = { type = "number", description = "First operand" },
            b = { type = "number", description = "Second operand" }
        },
        required = { "operation", "a", "b" }
    },
    handler = function(args)
        local op = args.operation
        local a = tonumber(args.a)
        local b = tonumber(args.b)
        if op == "add" then return string.format("%.10g", a + b) end
        if op == "subtract" then return string.format("%.10g", a - b) end
        if op == "multiply" then return string.format("%.10g", a * b) end
        if op == "divide" then
            if b == 0 then return "Error: division by zero" end
            return string.format("%.10g", a / b)
        end
        return "Error: unknown operation"
    end
}
```

Drop `.lua` files into `./plugins/` and restart the agent to load them.

## Architecture

```
src/
├── main.rs      CLI parsing, thread spawning
├── api.rs       OpenAI-compatible HTTP client (ureq)
├── agent.rs     Agent loop: send messages, dispatch tools, stream events to UI
├── tools.rs     Built-in tools: bash, read, write, edit
├── plugins.rs   Lua plugin loader and executor (mlua)
└── ui.rs        Terminal UI (ratatui + crossterm)
```

The agent runs on a background thread and communicates with the UI via channels:
- `AgentCommand::Send` — UI sends user input to the agent
- `UiEvent` — Agent sends responses, tool output, token stats, and completion signals to the UI

A shared `AtomicBool` cancel flag lets the UI signal cancellation. The agent checks it between iterations of its tool-calling loop.

## Acknowledgements

We are inspired by the following projects:

- [pi.dev](https://pi.dev/)
- [vim](https://www.vim.org/)
- [opencode](https://opencode.ai/)
