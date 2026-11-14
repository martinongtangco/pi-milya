//! ratatui rendering for pi-milya.

use std::io;

use ratatui::backend::Backend;
use ratatui::{
    layout::{Constraint, Layout, Position},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};

use crate::app::{App, Item, Menu, Phase, SLASH_COMMANDS};

pub fn draw<B: Backend>(t: &mut Terminal<B>, app: &App) -> Result<(), B::Error> {
    t.draw(|frame| render(frame, app)).map(|_| ())
}

fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let input_lines = app.input.len().min(4) as u16;
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(input_lines + 1),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(frame, app, chunks[0]);
    render_scrollback(frame, app, chunks[1]);
    render_input(frame, app, chunks[2]);
    render_status(frame, app, chunks[3]);
    if let Some(menu) = &app.menu {
        render_menu(frame, app, menu);
    }
}

// ---------------- header ----------------

fn render_header(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut spans = vec![
        Span::styled(
            format!(" {} ", "pí·milya"),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
    ];
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("model {}", app.model.as_deref().unwrap_or("—")),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("thinking {}", app.thinking.as_deref().unwrap_or("—")),
        Style::default(),
    ));
    if app.total_tokens > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{} tok", fmt_tokens(app.total_tokens)),
            Style::default().fg(Color::DarkGray),
        ));
        if app.total_cost > 0.0 {
            spans.push(Span::styled(
                format!("  ${:.4}", app.total_cost),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

// ---------------- scrollback ----------------

fn render_scrollback(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let lines: Vec<Line> = app
        .items
        .iter()
        .flat_map(|item| render_item(item))
        .collect();
    let block = Block::default().borders(Borders::TOP | Borders::BOTTOM);
    // pin to bottom: slice off the leading lines that don't fit
    let visible = area.height as usize;
    let top = lines.len().saturating_sub(visible);
    let sliced: Vec<ListItem> = lines.iter().skip(top).map(|l| ListItem::new(l.clone())).collect();
    frame.render_widget(List::new(sliced).block(block), area);
}

fn render_item(item: &Item) -> Vec<Line> {
    match item {
        Item::User(text) => text
            .lines()
            .map(|l| {
                Line::from(vec![
                    Span::styled("you> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw(l.to_string()),
                ])
            })
            .collect(),
        Item::Asst(a) => {
            let mut out = Vec::new();
            // thinking block
            if !a.thinking.is_empty() {
                if a.thinking_collapsed {
                    let preview = a
                        .thinking
                        .chars()
                        .take(60)
                        .collect::<String>()
                        .replace('\n', " ");
                    out.push(Line::from(vec![
                        Span::styled("· thinking ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
                        Span::styled(
                            format!("({} chars) ", a.thinking.chars().count()),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("“{preview}”"),
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                        ),
                    ]));
                } else {
                    out.push(Line::from(Span::styled(
                        "· thinking (t to collapse)",
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    )));
                    for l in a.thinking.lines() {
                        out.push(Line::from(Span::styled(
                            format!("   {l}"),
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                        )));
                    }
                }
            }
            // tool rows
            for t in &a.tools {
                let (icon, color) = if t.running {
                    ("…", Color::Yellow)
                } else if t.is_error {
                    ("✗", Color::Red)
                } else {
                    ("✓", Color::Green)
                };
                let dur = t
                    .duration_ms
                    .map(|ms| format!("  {fmt_ms} ", fmt_ms = format_duration(ms)))
                    .unwrap_or_default();
                out.push(Line::from(vec![
                    Span::styled(format!("▸ {icon} "), Style::default().fg(color)),
                    Span::styled(t.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!("  {}", truncate80(&t.summary)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(dur, Style::default().fg(Color::DarkGray)),
                ]));
                if !t.output.is_empty() {
                    let tail: Vec<&str> = t.output.lines().rev().take(2).collect();
                    for l in tail.into_iter().rev() {
                        out.push(Line::from(Span::styled(
                            format!("   {l}"),
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                        )));
                    }
                }
            }
            // text
            let text = if a.text.is_empty() && a.streaming { "…" } else { a.text.as_str() };
            for l in text.lines() {
                out.push(Line::from(Span::raw(l.to_string())));
            }
            if a.streaming && a.text.is_empty() && a.tools.is_empty() {
                out.push(Line::from(Span::styled(
                    "thinking…",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                )));
            }
            out
        }
        Item::Note(text) => vec![Line::from(Span::styled(
            format!("· {text}"),
            Style::default().fg(Color::DarkGray),
        ))],
        Item::Err(text) => vec![Line::from(Span::styled(
            format!("! {text}"),
            Style::default().fg(Color::Red),
        ))],
    }
}

// ---------------- input ----------------

fn render_input(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let lines: Vec<Line> = app
        .input
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let prefix = if i == 0 { "> " } else { "  " };
            Line::from(vec![
                Span::styled(prefix.to_string(), Style::default().fg(Color::Cyan)),
                Span::raw(l.clone()),
            ])
        })
        .collect();
    let block = Block::default().borders(Borders::TOP);
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
    // cursor position (char count; wide glyphs will be approximate)
    let x = area.x + 2 + app.input[app.cursor_line].chars().take(app.cursor_col).count() as u16;
    let y = area.y + 1 + app.cursor_line as u16;
    if y < area.bottom() {
        frame.set_cursor_position(Position::new(x, y));
    }
}

// ---------------- status bar ----------------

fn render_status(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let phase_s = match app.phase {
        Phase::Connecting => "connecting",
        Phase::Idle => "idle",
        Phase::Running => "running",
    };
    let phase_color = match app.phase {
        Phase::Running => Color::Yellow,
        Phase::Idle => Color::Green,
        Phase::Connecting => Color::DarkGray,
    };
    let session = app
        .session_name
        .clone()
        .or_else(|| app.session_id.clone().map(|s| s.chars().take(12).collect()))
        .unwrap_or_else(|| "—".into());
    let mut spans = vec![
        Span::styled(phase_s, Style::default().fg(phase_color).add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "  ·  steer {} · follow {}  ·  {}  ·  enter send/steer · esc abort/clear · tab cmds · ? help",
            app.queue.steering.len(),
            app.queue.follow_up.len(),
            session,
        )),
    ];
    if let Some(last) = app.stderr_tail.back() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("pi: {}", truncate80(last)),
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------------- menus ----------------

fn render_menu(frame: &mut Frame, _app: &App, menu: &Menu) {
    let area = frame.area();
    let (w, h) = match menu {
        Menu::Slash { .. } => (24u16, (SLASH_COMMANDS.len() + 2) as u16),
        Menu::Models { list, .. } => (44u16, list.len().min(12) as u16 + 2),
        Menu::Help => (64u16, 16),
    };
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = ratatui::layout::Rect::new(x, y, w, h);

    let (title, items): (&str, Vec<ListItem>) = match menu {
        Menu::Slash { selected } => (
            " slash commands ",
            SLASH_COMMANDS
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let style = if i == *selected {
                        Style::default().reversed()
                    } else {
                        Style::default()
                    };
                    ListItem::new(Line::from(c.to_string())).style(style)
                })
                .collect(),
        ),
        Menu::Models { list, selected } => (
            " models (enter=select, esc=cancel) ",
            list.iter()
                .enumerate()
                .take(12)
                .map(|(i, (id, label))| {
                    let style = if i == *selected {
                        Style::default().reversed()
                    } else {
                        Style::default()
                    };
                    let text = if id == label {
                        id.clone()
                    } else {
                        format!("{label}  ({id})")
                    };
                    ListItem::new(Line::from(truncate(text, w.saturating_sub(4) as usize))).style(style)
                })
                .collect(),
        ),
        Menu::Help => {
            let help = [
                "enter        send (steers while running)",
                "shift+enter / ctrl+j   newline",
                "esc          abort (running) / clear input",
                "up/down      history",
                "tab          complete slash commands",
                "t            toggle last thinking block",
                "/model       list models (or: /model <id>)",
                "/think       cycle level (or: /think <level>)",
                "/clear       new session",
                "/abort       abort the run",
                "/compact     compact the context",
                "/stats       session stats",
                "ctrl+c       quit",
            ];
            (
                " pi-milya help ",
                help
                    .iter()
                    .map(|l| ListItem::new(Line::from(l.to_string())))
                    .collect(),
            )
        }
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().reversed());
    frame.render_widget(list, popup);
}

// ---------------- utils ----------------

pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn format_duration(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

fn truncate80(s: &str) -> String {
    truncate(s.to_string(), 80)
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}
