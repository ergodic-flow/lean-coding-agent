use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

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

fn line_tag(line: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

    let mut hash = 0x811c9dc5u32;
    for byte in line.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }

    let value = hash & 0x00ff_ffff;
    let tag = [
        ALPHABET[((value >> 18) & 0x3f) as usize],
        ALPHABET[((value >> 12) & 0x3f) as usize],
        ALPHABET[((value >> 6) & 0x3f) as usize],
        ALPHABET[(value & 0x3f) as usize],
    ];

    String::from_utf8_lossy(&tag).into_owned()
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
                    'line_number:tag|content'. Use those line:tag anchors with edit. \
                    Use offset (1-indexed) and limit to read a range. \
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
                description: "Edit a file using line anchors returned by read. To replace or \
                    delete lines, set old_lines to every 'line:tag' anchor in the replaced range; \
                    those contiguous lines are replaced with new_string. To insert, set \
                    insert_after or insert_before to one 'line:tag' anchor. Tags must match the \
                    current file, so stale edits fail safely."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the file"
                        },
                        "old_lines": {
                            "type": "string",
                            "description": "Contiguous line anchors to replace, one per line, e.g. '11:rA3_\\n12:Kq9z'. Every line being replaced must be included. Use empty new_string to delete."
                        },
                        "insert_after": {
                            "type": "string",
                            "description": "Single line anchor after which to insert new_string, e.g. '13:PX0b'"
                        },
                        "insert_before": {
                            "type": "string",
                            "description": "Single line anchor before which to insert new_string, e.g. '13:PX0b'"
                        },
                        "new_string": {
                            "type": "string",
                            "description": "Replacement or inserted lines. Use an empty string to delete old_lines."
                        }
                    },
                    "required": ["file_path", "new_string"]
                }),
            },
        },
    ]
}

pub fn execute(name: &str, args: serde_json::Value, cancel: &AtomicBool) -> String {
    if cancel.load(Ordering::Relaxed) {
        return "cancelled".into();
    }

    match name {
        "bash" => exec_bash(args, cancel),
        "read" => exec_read(args),
        "write" => exec_write(args),
        "edit" => exec_edit(args),
        _ => format!("Unknown tool: {}", name),
    }
}

fn exec_bash(args: serde_json::Value, cancel: &AtomicBool) -> String {
    let command = match args["command"].as_str() {
        Some(c) => c,
        None => return "Error: command is required".into(),
    };

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    if let Some(dir) = args["workdir"].as_str() {
        cmd.current_dir(dir);
    } else {
        cmd.current_dir(std::env::current_dir().unwrap_or_default());
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return format!("Failed to execute: {}", e),
    };

    loop {
        if cancel.load(Ordering::Relaxed) {
            kill_child(&mut child);
            let _ = child.wait_with_output();
            return "cancelled".into();
        }

        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                kill_child(&mut child);
                return format!("Failed to wait for command: {}", e);
            }
        }
    }

    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(e) => return format!("Failed to read command output: {}", e),
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

fn kill_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pgid = format!("-{}", child.id());
        let _ = Command::new("kill").arg("-TERM").arg(&pgid).status();
        std::thread::sleep(Duration::from_millis(100));
        let _ = Command::new("kill").arg("-KILL").arg(&pgid).status();
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill();
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
        .map(|(i, line)| format!("{}:{}|{}", start + i + 1, line_tag(line), line))
        .collect();

    if selected.is_empty() {
        return "[no lines in range]".into();
    }

    let mut result = selected.join("\n");
    result.push_str(&format!(
        "\n[{} lines total, showing lines {}-{}; edit anchors are line:tag]",
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
    let new_string = match args["new_string"].as_str() {
        Some(s) => s,
        None => return "Error: new_string is required".into(),
    };
    let old_lines = args["old_lines"].as_str().filter(|s| !s.trim().is_empty());
    let insert_after = args["insert_after"]
        .as_str()
        .filter(|s| !s.trim().is_empty());
    let insert_before = args["insert_before"]
        .as_str()
        .filter(|s| !s.trim().is_empty());

    let mode_count =
        old_lines.is_some() as u8 + insert_after.is_some() as u8 + insert_before.is_some() as u8;
    if mode_count != 1 {
        return "Error: set exactly one of old_lines, insert_after, or insert_before".into();
    }

    let resolved = resolve_path(file_path);

    let content = match fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) => return format!("Error reading file: {}", e),
    };

    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let had_trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();

    if let Some(anchor) = insert_after {
        let (line_no, expected_tag) = match parse_anchor(anchor) {
            Ok(anchor) => anchor,
            Err(e) => return e,
        };
        if let Err(e) = verify_anchor(&lines, line_no, &expected_tag) {
            return e;
        }

        let insertion = edit_lines(new_string);
        let inserted = insertion.len();
        if inserted == 0 {
            return "Error: new_string is empty; nothing to insert".into();
        }
        lines.splice(line_no..line_no, insertion);

        return write_edited_lines(&resolved, &lines, line_ending, had_trailing_newline)
            .map_or_else(
                |e| e,
                |_| {
                    format!(
                        "Inserted {} line(s) after line {} in {}",
                        inserted,
                        line_no,
                        display_path(&resolved)
                    )
                },
            );
    }

    if let Some(anchor) = insert_before {
        let (line_no, expected_tag) = match parse_anchor(anchor) {
            Ok(anchor) => anchor,
            Err(e) => return e,
        };
        if let Err(e) = verify_anchor(&lines, line_no, &expected_tag) {
            return e;
        }

        let insertion = edit_lines(new_string);
        let inserted = insertion.len();
        if inserted == 0 {
            return "Error: new_string is empty; nothing to insert".into();
        }
        lines.splice((line_no - 1)..(line_no - 1), insertion);

        return write_edited_lines(&resolved, &lines, line_ending, had_trailing_newline)
            .map_or_else(
                |e| e,
                |_| {
                    format!(
                        "Inserted {} line(s) before line {} in {}",
                        inserted,
                        line_no,
                        display_path(&resolved)
                    )
                },
            );
    }

    let anchors = match parse_anchor_list(old_lines.unwrap_or_default()) {
        Ok(anchors) => anchors,
        Err(e) => return e,
    };

    for window in anchors.windows(2) {
        if window[0].0 + 1 != window[1].0 {
            return "Error: old_lines anchors must be contiguous and in increasing line order"
                .into();
        }
    }

    for (line_no, expected_tag) in &anchors {
        if let Err(e) = verify_anchor(&lines, *line_no, expected_tag) {
            return e;
        }
    }

    let start = anchors.first().map(|anchor| anchor.0).unwrap_or(1);
    let end = anchors.last().map(|anchor| anchor.0).unwrap_or(start);
    let replacement = edit_lines(new_string);
    let inserted = replacement.len();
    let removed = end - start + 1;
    lines.splice((start - 1)..end, replacement);

    write_edited_lines(&resolved, &lines, line_ending, had_trailing_newline).map_or_else(
        |e| e,
        |_| {
            format!(
                "Replaced lines {}-{} ({} -> {} line(s)) in {}",
                start,
                end,
                removed,
                inserted,
                display_path(&resolved)
            )
        },
    )
}

fn parse_anchor_list(value: &str) -> Result<Vec<(usize, String)>, String> {
    let mut anchors = Vec::new();
    for raw in value.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        anchors.push(parse_anchor(trimmed)?);
    }

    if anchors.is_empty() {
        return Err("Error: old_lines must contain at least one line:tag anchor".into());
    }

    Ok(anchors)
}

fn parse_anchor(value: &str) -> Result<(usize, String), String> {
    let prefix = value
        .split_once('|')
        .map_or(value, |(prefix, _)| prefix)
        .trim();
    let (line_no, tag) = prefix
        .split_once(':')
        .ok_or_else(|| format!("Error: invalid anchor '{}'; expected line:tag", value))?;
    let line_no = line_no
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("Error: invalid line number in anchor '{}'", value))?;
    let tag = tag.trim();

    if line_no == 0 {
        return Err("Error: line numbers are 1-indexed".into());
    }
    if tag.is_empty() {
        return Err(format!("Error: missing tag in anchor '{}'", value));
    }

    Ok((line_no, tag.to_string()))
}

fn verify_anchor(lines: &[String], line_no: usize, expected_tag: &str) -> Result<(), String> {
    let line = lines
        .get(line_no - 1)
        .ok_or_else(|| format!("Error: line {} is outside the file", line_no))?;
    let actual_tag = line_tag(line);

    if actual_tag == expected_tag {
        return Ok(());
    }

    let matching_lines: Vec<String> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| (line_tag(line) == expected_tag).then(|| (i + 1).to_string()))
        .take(5)
        .collect();

    if matching_lines.is_empty() {
        Err(format!(
            "Error: tag mismatch at line {}: expected {}, found {}. Re-read the file before editing.",
            line_no, expected_tag, actual_tag
        ))
    } else {
        Err(format!(
            "Error: tag mismatch at line {}: expected {}, found {}. Matching tag is now at line(s): {}. Re-read the file before editing.",
            line_no,
            expected_tag,
            actual_tag,
            matching_lines.join(", ")
        ))
    }
}

fn edit_lines(new_string: &str) -> Vec<String> {
    if new_string.is_empty() {
        return Vec::new();
    }

    let mut text = new_string;
    if let Some(stripped) = text.strip_suffix('\n') {
        text = stripped.strip_suffix('\r').unwrap_or(stripped);
    }

    if text.is_empty() {
        return vec![String::new()];
    }

    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect()
}

fn write_edited_lines(
    path: &Path,
    lines: &[String],
    line_ending: &str,
    had_trailing_newline: bool,
) -> Result<(), String> {
    let mut content = lines.join(line_ending);
    if had_trailing_newline && !content.is_empty() {
        content.push_str(line_ending);
    }

    fs::write(path, content).map_err(|e| format!("Error writing file: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(content: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "coding-agent-tools-test-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.txt");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn read_returns_line_tags() {
        let path = temp_file("alpha\nbeta\ngamma\n");

        let output = exec_read(serde_json::json!({
            "file_path": path.to_string_lossy()
        }));

        assert!(output.contains(&format!("2:{}|beta", line_tag("beta"))));
        assert!(output.contains("edit anchors are line:tag"));
    }

    #[test]
    fn edit_replaces_anchored_line() {
        let path = temp_file("alpha\nbeta\ngamma\n");

        let output = exec_edit(serde_json::json!({
            "file_path": path.to_string_lossy(),
            "old_lines": format!("2:{}|beta", line_tag("beta")),
            "new_string": "BETA"
        }));

        assert!(output.contains("Replaced lines 2-2"));
        assert_eq!(fs::read_to_string(path).unwrap(), "alpha\nBETA\ngamma\n");
    }

    #[test]
    fn edit_rejects_stale_anchor() {
        let path = temp_file("alpha\nchanged\ngamma\n");

        let output = exec_edit(serde_json::json!({
            "file_path": path.to_string_lossy(),
            "old_lines": format!("2:{}", line_tag("beta")),
            "new_string": "BETA"
        }));

        assert!(output.contains("tag mismatch"));
        assert_eq!(fs::read_to_string(path).unwrap(), "alpha\nchanged\ngamma\n");
    }

    #[test]
    fn edit_inserts_after_anchor() {
        let path = temp_file("alpha\ngamma\n");

        let output = exec_edit(serde_json::json!({
            "file_path": path.to_string_lossy(),
            "insert_after": format!("1:{}", line_tag("alpha")),
            "new_string": "beta"
        }));

        assert!(output.contains("Inserted 1 line(s) after line 1"));
        assert_eq!(fs::read_to_string(path).unwrap(), "alpha\nbeta\ngamma\n");
    }
}
