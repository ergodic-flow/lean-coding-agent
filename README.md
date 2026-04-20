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
| `--api-url` | `http://0.0.0.0:8080/v1` | OpenAI-compatible API base URL |
| `-m, --model` | `default` | Model to use |
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

### Keybindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Ctrl+C` | Quit |
| `Esc` (once) | Show cancel warning while agent is busy |
| `Esc` (twice, within 2s) | Cancel the in-flight request |
| `Shift+Up/Down` | Scroll conversation by 3 lines |
| `PageUp/PageDown` | Scroll conversation by 20 lines |

## Lua Plugin System

The agent can load custom tools written in Lua from a plugins directory. Each `.lua` file can define one or more tools using the `tool()` function.

![screenshot of tool usage](/assets/image.jpeg)

See [plugins/arithmetic.lua](/plugins/arithmetic.lua) for a concrete example on how to write one.

NOTE: http calls and json parsing are supported in lua plugins.

## Acknowledgements

We are inspired by the following projects:

- [pi.dev](https://pi.dev/)
- [vim](https://www.vim.org/)
- [opencode](https://opencode.ai/)
