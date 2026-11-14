//! Rendering for pi-milya: left family pane + right chat pane, menus,
//! the first-run wizard, and the status bar.


use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use pi_milya::app::{App, Item, Phase};

const PANE_W: u16 = 30;

pub fn draw(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    let [body, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    draw_body(f, app, body);
    draw_status(f, app, status);
}

fn draw_body(f: &mut ratatui::Frame, app: &App, area: Rect) {
    // wizard takes over the whole body
    if app.wizard.is_some() {
        draw_wizard(f, app, area);
        return;
    }
    let [left, right] = Layout::horizontal([Constraint::Length(PANE_W), Constraint::Min(0)]).areas(area);
    draw_family_pane(f, app, left);
    draw_chat_pane(f, app, right);
}

// ---------------- left: family ----------------

fn draw_family_pane(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(" family ", Style::default().fg(Color::Cyan)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // per-member blocks: face (up to 3 lines) + name + model line
    const PER: u16 = 5;
    let rows = inner.height.saturating_sub(1) / PER;
    let offset = if app.active >= rows as usize {
        app.active - rows as usize + 1
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, m) in app.members.iter().enumerate().skip(offset).take(rows as usize) {
        let active = i == app.active;
        let (dot, dot_style) = match m.phase {
            Phase::Running => ("●", Color::Yellow),
            Phase::Connecting => ("◌", Color::Magenta),
            Phase::Idle => ("○", if active { Color::Green } else { Color::DarkGray }),
        };
        let name_style = if active {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let face = m.member.face.lines().collect::<Vec<_>>();
        for (li, fl) in face.iter().enumerate().take(3) {
            let mut spans = vec![Span::raw(" ".repeat(4))];
            if li == 0 {
                spans.push(Span::styled(format!("{dot} "), Style::default().fg(dot_style)));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(
                fl.chars().take((PANE_W - 8) as usize).collect::<String>(),
                if active { Style::default().fg(Color::Green) } else { Style::default().fg(Color::DarkGray) },
            ));
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                m.member.name.chars().take((PANE_W - 8) as usize).collect::<String>(),
                name_style,
            ),
        ]));
        let model_short = m.model.as_deref().unwrap_or(&m.member.model);
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{} · {}", model_short, m.member.thinking)
                    .chars()
                    .take((PANE_W - 6) as usize)
                    .collect::<String>(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        if i != app.members.len() - 1 {
            lines.push(Line::raw(""));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------- right: chat ----------------

fn draw_chat_pane(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let active = app.members.get(app.active);
    if active.is_none() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" chat ", Style::default().fg(Color::Cyan)));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let msg = vec![
            Line::raw("the family is empty — type /new and press enter"),
        ];
        f.render_widget(Paragraph::new(msg), inner);
        return;
    }
    let m = active.unwrap();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {} · {} ", m.member.name, m.model.as_deref().unwrap_or("…")),
            Style::default().fg(Color::Cyan),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [scroll, input_area, hint] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(4), Constraint::Length(1)]).areas(inner);

    draw_scrollback(f, app, m, scroll);
    draw_input(f, app, input_area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ctrl+n/p member · /new /edit · tab slash · ? help ", Style::default().fg(Color::DarkGray)),
        ])),
        hint,
    );
}

fn draw_scrollback(f: &mut ratatui::Frame, _app: &App, m: &pi_milya::app::MemberState, area: Rect) {
    let mut content: Vec<Line> = Vec::new();
    let h = area.height as usize;
    for item in m.items.iter() {
        let mut item_lines: Vec<Line> = Vec::new();
        match item {
            Item::User { text: t, at } => {
                for l in t.lines() {
                    let mut spans = vec![
                        Span::styled("you> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        Span::styled(
                            format!("{} ", pi_milya::app::fmt_hm(*at)),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ];
                    if l.is_empty() && spans.len() == 2 {
                        spans.push(Span::raw(String::new()));
                    } else {
                        spans.push(Span::raw(l.to_string()));
                    }
                    item_lines.push(Line::from(spans));
                }
            }
            Item::Asst(a) => {
                for l in a.thinking.lines().take(400) {
                    item_lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                if a.thinking_collapsed && !a.thinking.is_empty() {
                    item_lines.truncate(1);
                    item_lines.push(Line::from(Span::styled(
                        format!("  … {} more chars (t to expand)", a.thinking.chars().count()),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                for t in a.tools.iter() {
                    let status = if t.running {
                        "…".to_string()
                    } else if t.is_error {
                        "✗".to_string()
                    } else {
                        "✓".to_string()
                    };
                    let style = if t.running {
                        Style::default().fg(Color::Yellow)
                    } else if t.is_error {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default().fg(Color::Green)
                    };
                    item_lines.push(Line::from(vec![
                        Span::styled(format!("  {status} "), style),
                        Span::styled(t.name.clone(), style),
                        Span::styled(format!(" {}", t.summary), Style::default().fg(Color::DarkGray)),
                    ]));
                }
                for l in a.text.lines() {
                    item_lines.push(Line::from(Span::raw(format!("pi>  {l}"))));
                }
                if a.streaming && a.text.is_empty() {
                    item_lines.push(Line::from(Span::styled("pi>  …", Style::default().fg(Color::DarkGray))));
                }
            }
            Item::Note(t) => item_lines.push(Line::from(Span::styled(
                t.clone(),
                Style::default().fg(Color::DarkGray),
            ))),
            Item::Err(t) => item_lines.push(Line::from(Span::styled(
                format!("  ! {t}"),
                Style::default().fg(Color::Red),
            ))),
        }
        content.extend(item_lines);
    }
    // keep the last `h` lines
    let trimmed: Vec<Line> = if content.len() > h {
        content[content.len() - h..].to_vec()
    } else {
        content.clone()
    };
    f.render_widget(Paragraph::new(trimmed), area);
}

fn draw_input(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for (i, l) in app.input.iter().enumerate() {
        let prefix = if i == 0 {
            "» "
        } else {
            "  "
        };
        let s = l.clone();
        let mut spans = vec![Span::styled(prefix.to_string(), Style::default().fg(Color::Cyan))];
        if i == app.cursor_line {
            let chars: Vec<char> = s.chars().collect();
            let col = app.cursor_col.min(chars.len());
            let before: String = chars[..col].iter().collect();
            let cursor = chars.get(col).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
            let after: String = chars.iter().skip(col + 1).collect();
            spans.push(Span::raw(before));
            spans.push(Span::styled(
                cursor,
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(after));
        } else {
            spans.push(Span::raw(s));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))),
        area,
    );
}

// ---------------- wizard ----------------

fn draw_wizard(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let Some(w) = &app.wizard else { return };
    let (step_n, title) = match w.step {
        pi_milya::app::WizStep::Name => (1, "name"),
        pi_milya::app::WizStep::Model => (2, "model"),
        pi_milya::app::WizStep::Thinking => (3, "thinking"),
        pi_milya::app::WizStep::Face => (4, "face"),
        pi_milya::app::WizStep::Prompt => (5, "system prompt"),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            if w.is_new {
                format!(" meeting the family · step {step_n}/5 · {title}")
            } else {
                format!(" editing {name} · step {step_n}/5 · {title}", name = w.name)
            },
            Style::default().fg(Color::Cyan),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    match w.step {
        pi_milya::app::WizStep::Name => {
            lines.push(Line::from(Span::styled("give this family member a name", Style::default().fg(Color::DarkGray))));
            lines.push(Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Cyan)),
                Span::raw(w.name.clone()),
                Span::styled("█", Style::default().bg(Color::Cyan).fg(Color::Black)),
            ]));
            lines.push(Line::from(Span::styled("enter to continue · esc to go back", Style::default().fg(Color::DarkGray))));
        }
        pi_milya::app::WizStep::Model => {
            if w.model_list.is_empty() {
                lines.push(Line::from(Span::styled(
                    if w.model_pending { "fetching models from pi …" } else { "no models available — check your llamesa setup" },
                    Style::default().fg(Color::DarkGray),
                )));
            }
            for (i, (id, label)) in w.model_list.iter().enumerate() {
                let sel = i == w.sel;
                let marker = if sel { "▸ " } else { "  " };
                let style = if sel { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::Gray) };
                lines.push(Line::from(Span::styled(
                    format!("{marker}{} — {}", label, id),
                    style,
                )));
            }
            if !w.model_list.is_empty() {
                lines.push(Line::from(Span::styled("up/down · enter to pick · esc to go back", Style::default().fg(Color::DarkGray))));
            }
        }
        pi_milya::app::WizStep::Thinking => {
            for (i, l) in pi_milya::family::THINKING_LEVELS.iter().enumerate() {
                let sel = i == w.sel;
                let marker = if sel { "▸ " } else { "  " };
                let style = if sel { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::Gray) };
                lines.push(Line::from(Span::styled(format!("{marker}{l}"), style)));
            }
            lines.push(Line::from(Span::styled("up/down · enter to pick · esc to go back", Style::default().fg(Color::DarkGray))));
        }
        pi_milya::app::WizStep::Face => {
            lines.push(Line::raw("pick a face for the member:"));
            for (i, (name, art)) in pi_milya::family::FACE_PRESETS.iter().enumerate() {
                let sel = i == w.sel;
                for (li, fl) in art.iter().enumerate() {
                    let style = if sel { Style::default().fg(Color::Green).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::DarkGray) };
                    lines.push(Line::from(Span::styled(format!("  {}{}{} ", if li == 0 { if sel { "▸ " } else { "  " } } else { "    " }, fl, if li == art.len() - 1 { format!("  · {name}") } else { String::new() }), style)));
                }
            }
            lines.push(Line::from(Span::styled("up/down · enter to pick · esc to go back", Style::default().fg(Color::DarkGray))));
        }
        pi_milya::app::WizStep::Prompt => {
            lines.push(Line::from(Span::styled("system prompt (appended to pi's prompt; empty is fine)", Style::default().fg(Color::DarkGray))));
            for (i, l) in w.prompt.iter().enumerate() {
                let prefix = if i == 0 { "> " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(prefix.to_string(), Style::default().fg(Color::Cyan)),
                    Span::raw(l.clone()),
                    if i == 0 { Span::styled("█", Style::default().bg(Color::Cyan).fg(Color::Black)) } else { Span::raw("") },
                ]));
            }
            lines.push(Line::from(Span::styled("shift+enter newline · enter saves · esc to go back", Style::default().fg(Color::DarkGray))));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------- status bar ----------------

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let name = app.members.get(app.active).map(|m| m.member.name.as_str()).unwrap_or("…");
    let model = app.members.get(app.active).and_then(|m| m.model.clone()).unwrap_or_else(|| "…".into());
    let thinking = app.members.get(app.active).and_then(|m| m.thinking.clone()).unwrap_or_else(|| "…".into());
    let phase = app.members.get(app.active).map(|m| m.phase).unwrap_or(Phase::Idle);
    let (ph, ph_style) = match phase {
        Phase::Running => ("running", Color::Yellow),
        Phase::Connecting => ("connecting", Color::Magenta),
        Phase::Idle => ("idle", Color::Green),
    };
    let cost = app.members.get(app.active).map(|m| m.total_cost).unwrap_or(0.0);
    let tokens = app.members.get(app.active).map(|m| m.total_tokens).unwrap_or(0);
    let family = format!("{}/{} members", app.active + 1, app.members.len().max(1));
    let left = format!(" {name} · {model} · {thinking}");
    let right = format!(" {ph} · {family} · {tokens} tok · ${cost:.4}  ");
    let width = area.width as usize;
    let left_t: String = left.chars().take(width.saturating_sub(right.chars().count()).saturating_sub(8)).collect();
    let line = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(left_t, Style::default().fg(Color::Gray)),
        Span::styled(" ", Style::default()),
        Span::styled(right, Style::default().fg(ph_style)),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        area,
    );
}
