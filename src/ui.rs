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
    Thinking(String),
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
        crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)
            .ok();
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
                match key.code {
                    KeyCode::Char('c')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        if !self.busy && !self.input.is_empty() {
                            let msg = self.input.clone();
                            self.input.clear();
                            self.items.push(ConversationItem::UserMessage(msg.clone()));
                            self.busy = true;
                            self.busy_since = Some(Instant::now());
                            self.auto_scroll = true;
                            self.cmd_tx
                                .send(AgentCommand::Send(msg))
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    KeyCode::Char(ch) => {
                        if !self.busy {
                            self.input.push(ch);
                        }
                    }
                    KeyCode::Backspace => {
                        if !self.busy {
                            self.input.pop();
                        }
                    }
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        self.scroll_offset = self.scroll_offset.saturating_sub(3);
                        self.auto_scroll = false;
                    }
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
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
            UiEvent::Thinking(text) => {
                self.items.push(ConversationItem::Thinking(text));
            }
            UiEvent::AssistantText(text) => {
                self.items.push(ConversationItem::AssistantText(text));
            }
            UiEvent::ToolCall { name, args_summary } => {
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
                self.busy = false;
                self.busy_since = None;
                self.pending_cancel = false;
                self.esc_press_time = None;
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(2),
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

    fn draw_header(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let busy_label = if self.pending_cancel {
            " press Esc again to cancel..."
        } else if self.busy {
            " thinking..."
        } else {
            ""
        };

        let plugin_label = if self.plugin_count > 0 {
            format!(" | plugins:{}", self.plugin_count)
        } else {
            String::new()
        };

        let left_text = if self.busy {
            let s = self.spinner();
            format!(" {} coding-agent | {}{} |{}", s, self.model, plugin_label, busy_label)
        } else {
            format!(
                "   coding-agent | {}{} | Shift+↑↓ scroll · Ctrl+C quit",
                self.model, plugin_label
            )
        };

        let ctx_pct = if self.context_limit > 0 && self.context_tokens > 0 {
            let pct = (self.context_tokens as f64 / self.context_limit as f64 * 100.0).min(100.0);
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

        let style = if self.pending_cancel {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD)
        } else if self.busy {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        };
        let token_fg = if self.pending_cancel { Color::Red } else { Color::Yellow };
        let token_bg = if self.pending_cancel { Color::Red } else if self.busy { Color::Yellow } else { Color::DarkGray };

        let header = Line::from(vec![
            Span::styled(format!("{:<width$}", left_text, width = left_width as usize), style),
            Span::styled(ctx_pct, Style::default().fg(token_fg).bg(token_bg)),
        ]);
        let paragraph = Paragraph::new(header);
        frame.render_widget(paragraph, area);
    }

    fn draw_conversation(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let all_lines = self.build_lines();
        let total = all_lines.len();
        let visible = area.height as usize;

        let max_scroll = total.saturating_sub(visible);
        if self.auto_scroll {
            self.scroll_offset = max_scroll;
        }
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        let start = self.scroll_offset;
        let end = (start + visible).min(total);

        let mut visible_lines: Vec<Line<'_>> = all_lines[start..end].to_vec();
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

        let prompt_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let input_style = if self.busy {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };

        let line = Line::from(vec![
            Span::styled("> ", prompt_style),
            Span::styled(self.input.clone(), input_style),
        ]);
        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, inner);

        if !self.busy {
            let cursor_x = inner.x + 2 + self.input.chars().count() as u16;
            let cursor_y = inner.y;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn build_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        for item in &self.items {
            match item {
                ConversationItem::UserMessage(msg) => {
                    lines.push(Line::from(Span::styled(
                        " You",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )));
                    for line in msg.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            Style::default().fg(Color::Cyan),
                        )));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::Thinking(msg) => {
                    let dim = Style::default().fg(Color::DarkGray);
                    lines.push(Line::from(Span::styled("  Thinking...", dim)));
                    for line in msg.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            dim,
                        )));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::AssistantText(msg) => {
                    lines.push(Line::from(Span::styled(
                        " Assistant",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    )));
                    for line in msg.lines() {
                        lines.push(Line::from(format!("  {}", line)));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::ResponseMeta { tokens, elapsed_secs, tok_per_sec } => {
                    let dim = Style::default().fg(Color::DarkGray);
                    lines.push(Line::from(Span::styled(
                        format!("  {} tokens · {:.1} tok/s · {:.1}s", tokens, tok_per_sec, elapsed_secs),
                        dim,
                    )));
                    lines.push(Line::from(""));
                }
                ConversationItem::ToolBlock {
                    name,
                    args_summary,
                    output,
                    is_running,
                } => {
                    let status = if *is_running {
                        " ...".to_string()
                    } else {
                        String::new()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" [{}] ", name),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{}{}", args_summary, status),
                            Style::default().fg(Color::Yellow),
                        ),
                    ]));
                    let dim = Style::default().fg(Color::DarkGray);
                    for line in output.lines() {
                        lines.push(Line::from(Span::styled(
                            format!(" │ {}", line),
                            dim,
                        )));
                    }
                    if !output.is_empty() {
                        lines.push(Line::from(Span::styled(" └", dim)));
                    }
                    lines.push(Line::from(""));
                }
                ConversationItem::Error(msg) => {
                    lines.push(Line::from(Span::styled(
                        format!(" Error: {}", msg),
                        Style::default().fg(Color::Red),
                    )));
                    lines.push(Line::from(""));
                }
            }
        }

        if self.busy {
            let s = self.spinner();
            lines.push(Line::from(Span::styled(
                format!(" {} Thinking...", s),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
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
