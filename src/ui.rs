use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};

use crate::agent::{AgentCommand, UiEvent};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_INTERVAL_MS: u64 = 80;

enum ConversationItem {
    UserMessage(String),
    Thinking { text: String, is_running: bool },
    AssistantText(String),
    ResponseMeta { tokens: u64, elapsed_secs: f64, tok_per_sec: f64 },
    ToolBlock {
        name: String,
        args_summary: String,
        output: String,
        is_running: bool,
    },
    Error(String),
}

pub struct App {
    items: Vec<ConversationItem>,
    input: String,
    cursor: usize,
    scroll_offset: usize,
    auto_scroll: bool,
    busy: bool,
    busy_since: Option<Instant>,
    model: String,
    context_limit: u64,
    context_tokens: u64,
    total_output_tokens: u64,
    plugin_count: usize,
    pending_cancel: bool,
    esc_press_time: Option<Instant>,
    cancel: Arc<AtomicBool>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    ui_rx: mpsc::Receiver<UiEvent>,
}

impl App {
    pub fn new(
        model: String,
        context_limit: u64,
        cancel: Arc<AtomicBool>,
        cmd_tx: mpsc::Sender<AgentCommand>,
        ui_rx: mpsc::Receiver<UiEvent>,
    ) -> Self {
        Self {
            items: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll_offset: 0,
            auto_scroll: true,
            busy: false,
            busy_since: None,
            model,
            context_limit,
            context_tokens: 0,
            total_output_tokens: 0,
            plugin_count: 0,
            pending_cancel: false,
            esc_press_time: None,
            cancel,
            cmd_tx,
            ui_rx,
        }
    }

    pub fn run(&mut self) -> Result<(), String> {
        crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)
            .map_err(|e| e.to_string())?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

        let result = self.run_loop(&mut terminal);

        crossterm::terminal::disable_raw_mode().ok();
        crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen).ok();
        terminal.show_cursor().ok();

        result
    }

    fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<(), String> {
        loop {
            terminal.draw(|f| self.draw(f)).map_err(|e| e.to_string())?;

            if self.pending_cancel {
                if let Some(t) = self.esc_press_time {
                    if t.elapsed().as_secs_f64() >= 2.0 {
                        self.pending_cancel = false;
                        self.esc_press_time = None;
                    }
                }
            }

            while let Ok(event) = self.ui_rx.try_recv() {
                self.handle_ui_event(event);
            }

            if !event::poll(std::time::Duration::from_millis(50)).map_err(|e| e.to_string())? {
                continue;
            }

            let ev = event::read().map_err(|e| e.to_string())?;
            if let Event::Key(key) = ev {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                let has_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let no_mods = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);

                match key.code {
                    KeyCode::Char('c') if has_ctrl => return Ok(()),
                    KeyCode::Char('a') if has_ctrl && !self.busy => self.cursor = 0,
                    KeyCode::Char('e') if has_ctrl && !self.busy => {
                        self.cursor = self.input.chars().count();
                    }
                    KeyCode::Char('k') if has_ctrl && !self.busy => {
                        let byte_pos = self.char_to_byte(self.cursor);
                        self.input.truncate(byte_pos);
                    }
                    KeyCode::Char('u') if has_ctrl && !self.busy => {
                        let byte_pos = self.char_to_byte(self.cursor);
                        self.input.drain(..byte_pos);
                        self.cursor = 0;
                    }
                    KeyCode::Char('w') if has_ctrl && !self.busy && self.cursor > 0 => {
                        let byte_cursor = self.char_to_byte(self.cursor);
                        let left = &self.input[..byte_cursor];
                        let trimmed = left.trim_end();
                        let word_start = trimmed.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                        let chars_to_remove = left[word_start..].chars().count();
                        let byte_start = self.char_to_byte(self.cursor - chars_to_remove);
                        
                        self.input.drain(byte_start..byte_cursor);
                        self.cursor -= chars_to_remove;
                    }
                    KeyCode::Enter => {
                        if !self.busy && !self.input.is_empty() {
                            let msg = self.input.clone();
                            self.input.clear();
                            self.cursor = 0;
                            self.items.push(ConversationItem::UserMessage(msg.clone()));
                            self.busy = true;
                            self.busy_since = Some(Instant::now());
                            self.auto_scroll = true;
                            self.cmd_tx.send(AgentCommand::Send(msg)).map_err(|e| e.to_string())?;
                        }
                    }
                    KeyCode::Char(ch) if no_mods => {
                        if !self.busy {
                            self.insert_char(ch);
                        }
                    }
                    KeyCode::Backspace => {
                        if !self.busy {
                            self.delete_before_cursor();
                        }
                    }
                    KeyCode::Delete => {
                        if !self.busy {
                            self.delete_at_cursor();
                        }
                    }
                    KeyCode::Left => {
                        if !self.busy && self.cursor > 0 {
                            self.cursor -= 1;
                        }
                    }
                    KeyCode::Right => {
                        if !self.busy && self.cursor < self.input.chars().count() {
                            self.cursor += 1;
                        }
                    }
                    KeyCode::Home => {
                        if !self.busy {
                            self.cursor = 0;
                        }
                    }
                    KeyCode::End => {
                        if !self.busy {
                            self.cursor = self.input.chars().count();
                        }
                    }
                    KeyCode::Up if has_shift => {
                        self.scroll_offset = self.scroll_offset.saturating_sub(3);
                        self.auto_scroll = false;
                    }
                    KeyCode::Down if has_shift => {
                        self.scroll_offset = self.scroll_offset.saturating_add(3);
                    }
                    KeyCode::PageUp => {
                        self.scroll_offset = self.scroll_offset.saturating_sub(20);
                        self.auto_scroll = false;
                    }
                    KeyCode::PageDown => {
                        self.scroll_offset = self.scroll_offset.saturating_add(20);
                    }
                    KeyCode::Esc => {
                        if self.busy {
                            if self.pending_cancel {
                                self.cancel.store(true, Ordering::Relaxed);
                                self.pending_cancel = false;
                                self.esc_press_time = None;
                            } else {
                                self.pending_cancel = true;
                                self.esc_press_time = Some(Instant::now());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::PluginsLoaded { count } => {
                self.plugin_count = count;
            }
            UiEvent::ThinkingStart => {
                self.items.push(ConversationItem::Thinking {
                    text: String::new(),
                    is_running: true,
                });
            }
            UiEvent::ThinkingDelta(delta) => {
                if let Some(ConversationItem::Thinking { ref mut text, .. }) = self.items.last_mut() {
                    text.push_str(&delta);
                }
            }
            UiEvent::TextStart => {
                self.end_active_thinking();
                self.items.push(ConversationItem::AssistantText(String::new()));
            }
            UiEvent::TextDelta(text) => {
                if let Some(ConversationItem::AssistantText(ref mut s)) = self.items.last_mut() {
                    s.push_str(&text);
                }
            }
            UiEvent::ToolCall { name, args_summary } => {
                self.end_active_thinking();
                self.items.push(ConversationItem::ToolBlock {
                    name,
                    args_summary,
                    output: String::new(),
                    is_running: true,
                });
            }
            UiEvent::ToolResult { output_summary } => {
                if let Some(ConversationItem::ToolBlock {
                    output, is_running, ..
                }) = self.items.last_mut()
                {
                    *output = output_summary;
                    *is_running = false;
                }
            }
            UiEvent::Error(msg) => {
                self.end_active_thinking();
                self.items.push(ConversationItem::Error(msg));
            }
            UiEvent::TokenUsage { context, output } => {
                self.context_tokens = context;
                self.total_output_tokens += output;
            }
            UiEvent::ResponseMeta { tokens, elapsed_secs, tok_per_sec } => {
                self.items.push(ConversationItem::ResponseMeta {
                    tokens,
                    elapsed_secs,
                    tok_per_sec,
                });
            }
            UiEvent::Done => {
                self.end_active_thinking();
                self.busy = false;
                self.busy_since = None;
                self.pending_cancel = false;
                self.esc_press_time = None;
                self.cancel.store(false, Ordering::Relaxed);
            }
        }
    }

    fn end_active_thinking(&mut self) {
        for item in self.items.iter_mut().rev() {
            if let ConversationItem::Thinking { ref mut is_running, .. } = item {
                *is_running = false;
                break;
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(frame.area());

        self.draw_header(frame, chunks[0]);
        self.draw_conversation(frame, chunks[1]);
        self.draw_input(frame, chunks[2]);
    }

    fn spinner(&self) -> &'static str {
        match self.busy_since {
            Some(since) => {
                let elapsed = since.elapsed().as_millis() as u64;
                let idx = (elapsed / SPINNER_INTERVAL_MS) as usize % SPINNER_FRAMES.len();
                SPINNER_FRAMES[idx]
            }
            None => "",
        }
    }

    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    fn insert_char(&mut self, ch: char) {
        let byte_pos = self.char_to_byte(self.cursor);
        self.input.insert(byte_pos, ch);
        self.cursor += 1;
    }

    fn delete_before_cursor(&mut self) {
        if self.cursor > 0 {
            let byte_pos = self.char_to_byte(self.cursor - 1);
            self.input.remove(byte_pos);
            self.cursor -= 1;
        }
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor < self.input.chars().count() {
            let byte_pos = self.char_to_byte(self.cursor);
            self.input.remove(byte_pos);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let cancel_label = if self.pending_cancel {
            "  press Esc again to cancel"
        } else {
            ""
        };

        let plugin_label = if self.plugin_count > 0 {
            format!(" | plugins:{}", self.plugin_count)
        } else {
            String::new()
        };

        let base_style = Style::default().fg(Color::Black).bg(Color::Gray);
        let highlight_style = base_style.add_modifier(Modifier::BOLD);

        let mut spans: Vec<Span<'_>> = Vec::new();

        if self.pending_cancel {
            spans.push(Span::styled(
                format!(" {}{}", self.model, plugin_label),
                Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(cancel_label, Style::default().fg(Color::White).bg(Color::Red)));
        } else {
            spans.push(Span::styled(format!(" {}{}", self.model, plugin_label), highlight_style));
            spans.push(Span::styled(" | ", base_style));
            spans.push(Span::styled("Shift+↑↓ scroll · Ctrl+C quit", Style::default().fg(Color::DarkGray).bg(Color::Gray)));
        }

        let ctx_pct = if self.context_limit > 0 {
            let pct = if self.context_tokens > 0 {
                (self.context_tokens as f64 / self.context_limit as f64 * 100.0).min(100.0)
            } else {
                0.0
            };
            format!(
                "ctx:{}/{} ({:.0}%) · out:{} ",
                format_tokens(self.context_tokens),
                format_tokens(self.context_limit),
                pct,
                format_tokens(self.total_output_tokens),
            )
        } else if self.context_tokens > 0 {
            format!(
                "ctx:{} · out:{} ",
                format_tokens(self.context_tokens),
                format_tokens(self.total_output_tokens),
            )
        } else {
            String::new()
        };

        let tokens_width = ctx_pct.chars().count() as u16;
        let left_width = area.width.saturating_sub(tokens_width);

        let token_style = if self.pending_cancel {
            Style::default().fg(Color::White).bg(Color::Red)
        } else {
            Style::default().fg(Color::Black).bg(Color::Gray)
        };

        let span_chars: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let pad = left_width as usize;
        if span_chars < pad {
            spans.push(Span::styled(" ".repeat(pad - span_chars), base_style));
        }

        spans.push(Span::styled(ctx_pct, token_style));

        let header = Line::from(spans);
        let paragraph = Paragraph::new(header);
        frame.render_widget(paragraph, area);
    }

    fn draw_conversation(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let all_lines = self.build_lines(area.width);
        let total = all_lines.len();
        let visible = area.height as usize;

        let max_scroll = total.saturating_sub(visible);
        if self.auto_scroll {
            self.scroll_offset = max_scroll;
        }
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        let start = self.scroll_offset;

        // Consuming iteration to avoid mass-cloning visual Strings on every render pass.
        let mut visible_lines: Vec<Line<'_>> = all_lines
            .into_iter()
            .skip(start)
            .take(visible)
            .collect();
            
        while visible_lines.len() < visible {
            visible_lines.push(Line::from(""));
        }

        let paragraph = Paragraph::new(visible_lines);
        frame.render_widget(paragraph, area);
    }

    fn draw_input(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.busy {
            let s = self.spinner();
            let cancel_hint = if self.pending_cancel {
                "  press Esc again to cancel"
            } else {
                "  Esc to cancel"
            };
            let line = Line::from(vec![
                Span::styled(
                    format!(" {} ", s),
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                Span::styled("generating...", Style::default().fg(Color::Green)),
                Span::styled(cancel_hint, Style::default().fg(Color::DarkGray)),
            ]);
            let paragraph = Paragraph::new(line);
            frame.render_widget(paragraph, inner);
        } else {
            let prompt_style = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);

            let line = Line::from(vec![
                Span::styled("> ", prompt_style),
                Span::styled(self.input.clone(), Style::default()),
            ]);
            let paragraph = Paragraph::new(line);
            frame.render_widget(paragraph, inner);

            // Guard against the cursor rendering offscreen if the input exceeds terminal width
            let cursor_x = (inner.x + 2 + self.cursor as u16).min(inner.right().saturating_sub(1));
            let cursor_y = inner.y;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn build_lines(&self, width: u16) -> Vec<Line<'static>> {
        let w = width as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        for item in &self.items {
            match item {
                ConversationItem::UserMessage(msg) => {
                    lines.push(Line::from(Span::styled(
                        " You",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )));
                    for wrapped in wrap_with_prefix(msg, "  ", w) {
                        lines.push(Line::from(Span::styled(wrapped, Style::default().fg(Color::Cyan))));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::Thinking { text, is_running } => {
                    let dim = Style::default().fg(Color::DarkGray);
                    let label = if *is_running { "  Thinking..." } else { "  Thought" };
                    lines.push(Line::from(Span::styled(label, dim)));
                    for wrapped in wrap_with_prefix(text, "  ", w) {
                        lines.push(Line::from(Span::styled(wrapped, dim)));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::AssistantText(msg) => {
                    lines.push(Line::from(Span::styled(
                        " Assistant",
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                    )));
                    for wrapped in wrap_with_prefix(msg, "  ", w) {
                        lines.push(Line::from(wrapped));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::ResponseMeta { tokens, elapsed_secs, tok_per_sec } => {
                    let dim = Style::default().fg(Color::DarkGray);
                    let meta = format!("  {} tokens · {:.1} tok/s · {:.1}s", tokens, tok_per_sec, elapsed_secs);
                    for wrapped in wrap_with_prefix(&meta, "", w) {
                        lines.push(Line::from(Span::styled(wrapped, dim)));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::ToolBlock {
                    name,
                    args_summary,
                    output,
                    is_running,
                } => {
                    let status = if *is_running { " ..." } else { "" };
                    let header = format!(" [{}] {}{}", name, args_summary, status);
                    lines.push(Line::from(Span::styled(
                        header,
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )));
                    let dim = Style::default().fg(Color::DarkGray);
                    
                    for wrapped in wrap_with_prefix(output, " │ ", w) {
                        lines.push(Line::from(Span::styled(wrapped, dim)));
                    }

                    if !output.is_empty() {
                        lines.push(Line::from(Span::styled(" └", dim)));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::Error(msg) => {
                    let err = format!(" Error: {}", msg);
                    for wrapped in wrap_with_prefix(&err, "", w) {
                        lines.push(Line::from(Span::styled(wrapped, Style::default().fg(Color::Red))));
                    }
                    lines.push(Line::from(""));
                }
            }
        }
        lines
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Consolidates lines wrapping mechanisms seamlessly keeping spaces.
fn wrap_with_prefix(text: &str, prefix: &str, max_width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let prefix_len = prefix.chars().count();
    let inner = max_width.saturating_sub(prefix_len);

    if inner == 0 || text.is_empty() {
        for _ in text.lines() {
            out.push(prefix.to_string());
        }
        if out.is_empty() {
            out.push(prefix.to_string());
        }
        return out;
    }

    for line in text.lines() {
        if line.is_empty() {
            out.push(prefix.to_string());
            continue;
        }
        
        let mut current = String::new();
        // Uses split_inclusive to ensure original word spacing safely traverses lines.
        for word in line.split_inclusive(' ') {
            let word_len = word.chars().count();
            if current.chars().count() + word_len <= inner {
                current.push_str(word);
            } else {
                if !current.is_empty() {
                    out.push(format!("{}{}", prefix, current));
                    current.clear();
                }
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(format!("{}{}", prefix, current));
        }
    }
    
    if out.is_empty() {
        out.push(prefix.to_string());
    }
    
    out
}
