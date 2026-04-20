use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::api::{ToolDef, ToolFunction};

fn resolve_path(file_path: &str) -> PathBuf {
    let path = Path::new(file_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

fn display_path(path: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    match path.strip_prefix(&cwd) {
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "bash".into(),
                description: "Execute a bash command. Returns stdout and stderr combined. \
                    Appends [exit code: N] on non-zero exit and [no output] when there is no output."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The bash command to execute"
                        },
                        "workdir": {
                            "type": "string",
                            "description": "Working directory for the command (optional)"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "read".into(),
                description: "Read the contents of a file. Returns lines prefixed with \
                    'line_number: content'. Use offset (1-indexed) and limit to read a range. \
                    Output ends with '[N lines total, showing lines X-Y]'."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the file"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Line number to start reading from (1-indexed)"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of lines to read"
                        }
                    },
                    "required": ["file_path"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "write".into(),
                description:
                    "Write content to a file. Creates parent directories if needed. \
                    Overwrites existing files. Returns the number of lines written."
                        .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the file"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to write"
                        }
                    },
                    "required": ["file_path", "content"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "edit".into(),
                description: "Replace an exact substring in a file. The old_string must match \
                    exactly (no regex or fuzzy matching). Fails if old_string is not found. \
                    If old_string appears multiple times, set replace_all=true or it will fail \
                    with the match count. Returns the number of occurrences replaced."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the file"
                        },
                        "old_string": {
                            "type": "string",
                            "description": "Text to find in the file"
                        },
                        "new_string": {
                            "type": "string",
                            "description": "Text to replace it with"
                        },
                        "replace_all": {
                            "type": "boolean",
                            "description": "Replace all occurrences instead of just the first (default: false)"
                        }
                    },
                    "required": ["file_path", "old_string", "new_string"]
                }),
            },
        },
    ]
}

pub fn execute(name: &str, args: serde_json::Value) -> String {
    match name {
        "bash" => exec_bash(args),
        "read" => exec_read(args),
        "write" => exec_write(args),
        "edit" => exec_edit(args),
        _ => format!("Unknown tool: {}", name),
    }
}

fn exec_bash(args: serde_json::Value) -> String {
    let command = match args["command"].as_str() {
        Some(c) => c,
        None => return "Error: command is required".into(),
    };

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);

    if let Some(dir) = args["workdir"].as_str() {
        cmd.current_dir(dir);
    } else {
        cmd.current_dir(std::env::current_dir().unwrap_or_default());
    }

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => return format!("Failed to execute: {}", e),
    };

    let mut result = String::new();
    if !output.stdout.is_empty() {
        result.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if !output.status.success() {
        result.push_str(&format!(
            "\n[exit code: {}]",
            output.status.code().unwrap_or(-1)
        ));
    }

    if result.is_empty() {
        "[no output]".into()
    } else {
        result
    }
}

fn exec_read(args: serde_json::Value) -> String {
    let file_path = match args["file_path"].as_str() {
        Some(p) => p,
        None => return "Error: file_path is required".into(),
    };
    let resolved = resolve_path(file_path);

    let content = match fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) => return format!("Error reading file: {}", e),
    };

    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64();

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if total == 0 {
        return "[empty file]".into();
    }

    let start = if offset > 0 {
        (offset - 1).min(total)
    } else {
        0
    };

    let iter: Box<dyn Iterator<Item = (usize, &&str)>> = if let Some(lim) = limit {
        Box::new(lines[start..].iter().enumerate().take(lim as usize))
    } else {
        Box::new(lines[start..].iter().enumerate())
    };

    let selected: Vec<String> = iter
        .map(|(i, line)| format!("{}: {}", start + i + 1, line))
        .collect();

    if selected.is_empty() {
        return "[no lines in range]".into();
    }

    let mut result = selected.join("\n");
    result.push_str(&format!(
        "\n[{} lines total, showing lines {}-{}]",
        total,
        start + 1,
        start + selected.len()
    ));

    result
}

fn exec_write(args: serde_json::Value) -> String {
    let file_path = match args["file_path"].as_str() {
        Some(p) => p,
        None => return "Error: file_path is required".into(),
    };
    let content = match args["content"].as_str() {
        Some(c) => c,
        None => return "Error: content is required".into(),
    };
    let resolved = resolve_path(file_path);

    if let Some(parent) = resolved.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                return format!("Error creating directory: {}", e);
            }
        }
    }

    match fs::write(&resolved, content) {
        Ok(()) => {
            let lines = content.lines().count();
            format!("Wrote {} lines to {}", lines, display_path(&resolved))
        }
        Err(e) => format!("Error writing file: {}", e),
    }
}

fn exec_edit(args: serde_json::Value) -> String {
    let file_path = match args["file_path"].as_str() {
        Some(p) => p,
        None => return "Error: file_path is required".into(),
    };
    let old_string = match args["old_string"].as_str() {
        Some(s) => s,
        None => return "Error: old_string is required".into(),
    };
    let new_string = match args["new_string"].as_str() {
        Some(s) => s,
        None => return "Error: new_string is required".into(),
    };
    let replace_all = args["replace_all"].as_bool().unwrap_or(false);
    let resolved = resolve_path(file_path);

    let content = match fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) => return format!("Error reading file: {}", e),
    };

    let count = content.matches(old_string).count();
    if count == 0 {
        return "Error: old_string not found in file".into();
    }
    if count > 1 && !replace_all {
        return format!(
            "Error: old_string found {} times. Use replace_all=true to replace all.",
            count
        );
    }

    let new_content = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };

    match fs::write(&resolved, &new_content) {
        Ok(()) => format!(
            "            Replaced {} occurrence(s) in {}",
            if replace_all { count } else { 1 },
            display_path(&resolved)
        ),
        Err(e) => format!("Error writing file: {}", e),
    }
}
