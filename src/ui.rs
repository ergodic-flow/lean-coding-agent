use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
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
    UserMessage { text: String, images: Vec<String> },
    Thinking { text: String, is_running: bool },
    AssistantText(String),
    ResponseMeta { tokens: u64, elapsed_secs: f64, tok_per_sec: f64 },
    ToolBlock {
        index: usize,
        name: String,
        args_summary: Option<String>,
        output: String,
        is_running: bool,
    },
    Error(String),
}

struct ImageAttachment {
    filename: String,
    base64_data: String,
    media_type: String,
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
    pending_images: Vec<ImageAttachment>,
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
            pending_images: Vec::new(),
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
        crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste)
            .map_err(|e| e.to_string())?;
        crossterm::execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .map_err(|e| e.to_string())?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

        let result = self.run_loop(&mut terminal);

        crossterm::terminal::disable_raw_mode().ok();
        crossterm::execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags).ok();
        crossterm::execute!(terminal.backend_mut(), crossterm::event::DisableBracketedPaste).ok();
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
            match ev {
                Event::Key(key) => {
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
                        KeyCode::Char('d') if has_ctrl && !self.busy => {
                            self.pending_images.clear();
                        }
                        KeyCode::Enter if has_shift => {
                            if !self.busy {
                                self.insert_char('\n');
                            }
                        }
                        KeyCode::Enter => {
                            if !self.busy && (!self.input.is_empty() || !self.pending_images.is_empty()) {
                                let msg = self.input.clone();
                                let images = std::mem::take(&mut self.pending_images);
                                let image_names: Vec<String> = images.iter().map(|i| i.filename.clone()).collect();
                                self.input.clear();
                                self.cursor = 0;
                                self.items.push(ConversationItem::UserMessage {
                                    text: msg.clone(),
                                    images: image_names,
                                });
                                self.busy = true;
                                self.busy_since = Some(Instant::now());
                                self.auto_scroll = true;
                                self.cmd_tx
                                    .send(AgentCommand::Send {
                                        text: msg,
                                        images: images
                                            .into_iter()
                                            .map(|a| crate::agent::ImageAttachment {
                                                filename: a.filename,
                                                base64_data: a.base64_data,
                                                media_type: a.media_type,
                                            })
                                            .collect(),
                                    })
                                    .map_err(|e| e.to_string())?;
                            }
                        }
                        KeyCode::Char(ch) if no_mods => {
                            if !self.busy {
                                self.insert_char(ch);
                            }
                        }
                        KeyCode::Backspace => {
                            if !self.busy {
                                if self.input.is_empty() && !self.pending_images.is_empty() {
                                    self.pending_images.pop();
                                } else {
                                    self.delete_before_cursor();
                                }
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
                Event::Paste(text) => {
                    if !self.busy {
                        let trimmed = text.trim();
                        let path = trimmed
                            .trim_start_matches("file://")
                            .trim_matches('"')
                            .trim_matches('\'');

                        if is_image_file(path) && std::path::Path::new(path).is_file() {
                            match std::fs::read(path) {
                                Ok(data) => {
                                    if data.len() > 20 * 1024 * 1024 {
                                        self.items.push(ConversationItem::Error(
                                            "Image too large (max 20MB)".to_string(),
                                        ));
                                    } else {
                                        let filename = std::path::Path::new(path)
                                            .file_name()
                                            .map(|f| f.to_string_lossy().to_string())
                                            .unwrap_or_else(|| path.to_string());
                                        let media_type = media_type_for_path(path).to_string();
                                        let base64_data = base64_encode(&data);
                                        self.pending_images.push(ImageAttachment {
                                            filename,
                                            base64_data,
                                            media_type,
                                        });
                                    }
                                }
                                Err(e) => {
                                    self.items.push(ConversationItem::Error(
                                        format!("Failed to read image: {}", e),
                                    ));
                                }
                            }
                        } else {
                            for ch in text.chars() {
                                self.insert_char(ch);
                            }
                        }
                    }
                }
                _ => {}
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
            UiEvent::ToolCallBegin { index, name } => {
                self.end_active_thinking();
                for item in &mut self.items {
                    if let ConversationItem::ToolBlock { is_running, .. } = item {
                        *is_running = false;
                    }
                }
                self.items.push(ConversationItem::ToolBlock {
                    index,
                    name,
                    args_summary: None,
                    output: String::new(),
                    is_running: true,
                });
            }
            UiEvent::ToolCallArgs { index, args_summary } => {
                let target = self.items.iter().rposition(|item| {
                    matches!(item, ConversationItem::ToolBlock { index: i, .. } if *i == index)
                });

                for item in &mut self.items {
                    if let ConversationItem::ToolBlock { is_running, .. } = item {
                        *is_running = false;
                    }
                }

                if let Some(target) = target {
                    if let ConversationItem::ToolBlock {
                        args_summary: ref mut summary,
                        is_running,
                        ..
                    } = &mut self.items[target]
                    {
                        *summary = Some(args_summary);
                        *is_running = true;
                    }
                }
            }
            UiEvent::ToolResult { index, output_summary } => {
                if let Some(target) = self.items.iter().rposition(|item| {
                    matches!(item, ConversationItem::ToolBlock { index: i, .. } if *i == index)
                }) {
                    if let ConversationItem::ToolBlock {
                        output, is_running, ..
                    } = &mut self.items[target]
                    {
                        *output = output_summary;
                        *is_running = false;
                    }
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

    fn input_height(&self, terminal_width: u16) -> u16 {
        let base = if self.input.is_empty() || self.busy {
            3u16
        } else {
            let prompt_width: usize = 2;
            let inner_width = (terminal_width as usize).saturating_sub(2).saturating_sub(prompt_width);
            if inner_width == 0 {
                3u16
            } else {
                let (wrapped_lines, _, _) = self.wrap_input_lines(inner_width);
                let lines = wrapped_lines.len();
                (lines as u16).saturating_add(2)
            }
        };
        let attachment_lines = if self.pending_images.is_empty() { 0u16 } else { 1 };
        base.saturating_add(attachment_lines).min(12)
    }

    fn draw(&mut self, frame: &mut Frame) {
        let input_h = self.input_height(frame.area().width);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(input_h),
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

    fn wrap_input_lines(&self, text_width: usize) -> (Vec<String>, u16, usize) {
        let text_width = text_width.max(1);
        let chars: Vec<char> = self.input.chars().collect();
        let mut wrapped_lines: Vec<String> = Vec::new();
        let mut current_line = String::new();
        let mut cursor_row: u16 = 0;
        let mut cursor_col: usize = 0;

        for (i, ch) in chars.iter().enumerate() {
            if *ch != '\n' && current_line.chars().count() >= text_width {
                wrapped_lines.push(std::mem::take(&mut current_line));
            }

            if i == self.cursor {
                cursor_row = wrapped_lines.len() as u16;
                cursor_col = current_line.chars().count();
            }

            if *ch == '\n' {
                wrapped_lines.push(std::mem::take(&mut current_line));
            } else {
                current_line.push(*ch);
            }
        }

        if self.cursor == chars.len() {
            if current_line.chars().count() >= text_width {
                wrapped_lines.push(std::mem::take(&mut current_line));
                cursor_row = wrapped_lines.len() as u16;
                cursor_col = 0;
            } else {
                cursor_row = wrapped_lines.len() as u16;
                cursor_col = current_line.chars().count();
            }
        }

        if !current_line.is_empty() || chars.is_empty() || self.input.ends_with('\n') {
            wrapped_lines.push(current_line);
        }

        (wrapped_lines, cursor_row, cursor_col)
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
            spans.push(Span::styled(
                "Shift+Enter newline · Shift+↑↓ scroll · Ctrl+C quit",
                Style::default().fg(Color::DarkGray).bg(Color::Gray),
            ));
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

        let has_attachments = !self.pending_images.is_empty() && !self.busy;

        let (input_area, attachment_area) = if has_attachments {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(inner);
            (chunks[1], Some(chunks[0]))
        } else {
            (inner, None)
        };

        if let Some(a_area) = attachment_area {
            let names: Vec<&str> = self.pending_images.iter().map(|a| a.filename.as_str()).collect();
            let label = format!(" [attached: {}]  Ctrl+D clear | Backspace remove last", names.join(", "));
            let paragraph = Paragraph::new(Line::from(Span::styled(
                label,
                Style::default().fg(Color::Magenta),
            )));
            frame.render_widget(paragraph, a_area);
        }

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
            frame.render_widget(paragraph, input_area);
        } else {
            let prompt_style = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);

            let prompt_width: usize = 2;
            let inner_width = input_area.width as usize;
            let text_width = inner_width.saturating_sub(prompt_width).max(1);

            let (wrapped_lines, cursor_row, cursor_col) = self.wrap_input_lines(text_width);

            let mut lines: Vec<Line> = Vec::new();
            for (i, line) in wrapped_lines.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled("> ", prompt_style),
                        Span::styled(line.clone(), Style::default()),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("  ", prompt_style),
                        Span::styled(line.clone(), Style::default()),
                    ]));
                }
            }
            let paragraph = Paragraph::new(lines);
            frame.render_widget(paragraph, input_area);

            let cursor_x = input_area.x + (prompt_width + cursor_col) as u16;
            let cursor_y = input_area.y + cursor_row;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn build_lines(&self, width: u16) -> Vec<Line<'static>> {
        let w = width as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        for item in &self.items {
            match item {
                ConversationItem::UserMessage { text, images } => {
                    lines.push(Line::from(Span::styled(
                        " You",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )));
                    for img in images {
                        lines.push(Line::from(Span::styled(
                            format!("  [img: {}]", img),
                            Style::default().fg(Color::Magenta),
                        )));
                    }
                    for wrapped in wrap_with_prefix(text, "  ", w) {
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
                    index: _,
                    name,
                    args_summary,
                    output,
                    is_running,
                } => {
                    let spinner = if *is_running {
                        format!(" {} {}", self.spinner(), "running...")
                    } else {
                        String::new()
                    };
                    let header = format!(" [{}]{}", name, spinner);
                    lines.push(Line::from(Span::styled(
                        header,
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )));
                    let dim = Style::default().fg(Color::DarkGray);

                    if let Some(args) = args_summary {
                        if !args.is_empty() {
                            let args_line = format!("args: {}", args);
                            for wrapped in wrap_with_prefix(&args_line, " │ ", w) {
                                lines.push(Line::from(Span::styled(wrapped, dim)));
                            }
                        }
                    }

                    for wrapped in wrap_with_prefix(output, " │ ", w) {
                        lines.push(Line::from(Span::styled(wrapped, dim)));
                    }

                    if args_summary.as_ref().is_some_and(|args| !args.is_empty())
                        || !output.is_empty()
                    {
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
        for word in line.split_inclusive(' ') {
            let word_len = word.chars().count();
            if current.chars().count() + word_len <= inner {
                current.push_str(word);
            } else {
                if !current.is_empty() {
                    out.push(format!("{}{}", prefix, current));
                    current.clear();
                }
                if word_len <= inner {
                    current.push_str(word);
                } else {
                    let mut remaining = word.chars().collect::<Vec<char>>();
                    while !remaining.is_empty() {
                        let space = inner - current.chars().count();
                        let take = space.min(remaining.len());
                        for ch in remaining.drain(..take) {
                            current.push(ch);
                        }
                        if !remaining.is_empty() {
                            out.push(format!("{}{}", prefix, current));
                            current.clear();
                        }
                    }
                }
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

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let n = match chunk.len() {
            1 => (chunk[0] as u32) << 16,
            2 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8,
            _ => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32,
        };
        result.push(BASE64_CHARS[((n >> 18) & 0x3F) as usize] as char);
        result.push(BASE64_CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn is_image_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
}

fn media_type_for_path(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/bmp"
    }
}
