//! Application state: chat items, input editor, menus, and pi event handling.
//!
//! Design: `on_key` is synchronous (editing only). Anything that must talk to
//! pi is queued as a `KeyAction` and executed by the async event loop in
//! `flush_action()`.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::pi_client::PiClient;
use crate::protocol::{args_summary, extract_models, parse_event, AmEvent, CmdKind, PiEvent, Rec};

/// One step of the event loop: either a key event or a pi record.
pub enum Step {
    Key(KeyEvent),
    Rec(Option<Rec>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Idle,
    Running,
}

#[derive(Clone, PartialEq)]
pub enum KeyAction {
    None,
    Enter,
    Esc,
    Quit,
    PickModel(String),
}

pub enum Item {
    User(String),
    Asst(AssistMsg),
    Note(String),
    Err(String),
}

pub struct AssistMsg {
    pub text: String,
    pub thinking: String,
    pub thinking_collapsed: bool,
    pub tools: Vec<ToolRow>,
    pub streaming: bool,
}

impl AssistMsg {
    fn new() -> Self {
        Self {
            text: String::new(),
            thinking: String::new(),
            thinking_collapsed: true,
            tools: Vec::new(),
            streaming: true,
        }
    }
}

pub struct ToolRow {
    pub name: String,
    pub summary: String,
    pub running: bool,
    pub is_error: bool,
    pub output: String,
    pub duration_ms: Option<u64>,
}

pub struct Queue {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

pub enum Menu {
    Slash { selected: usize },
    Models { list: Vec<(String, String)>, selected: usize },
    Help,
}

pub const SLASH_COMMANDS: &[&str] = &[
    "/model",
    "/think",
    "/clear",
    "/abort",
    "/compact",
    "/stats",
    "/help",
    "/quit",
];

pub struct App {
    pub client: Option<PiClient>,
    pub cfg: Config,
    pub phase: Phase,
    pub items: Vec<Item>,
    pub input: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub history: Vec<Vec<String>>,
    pub hist_idx: Option<usize>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub session_id: Option<String>,
    pub session_name: Option<String>,
    pub queue: Queue,
    pub last_usage: Option<crate::protocol::Usage>,
    pub total_cost: f64,
    pub total_tokens: u64,
    pub menu: Option<Menu>,
    pub stderr_tail: VecDeque<String>,
    pub exiting: bool,
    pending_action: Option<KeyAction>,
    cur_usage: Option<crate::protocol::Usage>,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        Self {
            client: None,
            cfg,
            phase: Phase::Connecting,
            items: vec![Item::Note("connecting to pi (rpc mode) …".into())],
            input: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            history: Vec::new(),
            hist_idx: None,
            model: None,
            thinking: None,
            session_id: None,
            session_name: None,
            queue: Queue {
                steering: Vec::new(),
                follow_up: Vec::new(),
            },
            last_usage: None,
            total_cost: 0.0,
            total_tokens: 0,
            menu: None,
            stderr_tail: VecDeque::with_capacity(5),
            exiting: false,
            pending_action: None,
            cur_usage: None,
        }
    }

    pub async fn start(&mut self) -> anyhow::Result<()> {
        let client = PiClient::spawn(&self.cfg).await?;
        self.client = Some(client);
        if let Some(c) = self.client.as_mut() {
            c.send(CmdKind::GetState, json!({ "type": "get_state" })).await;
        }
        Ok(())
    }

    /// Block for the next input event (key or pi record). Encapsulating the
    /// select here keeps the `&mut self` borrow to a single site.
    pub async fn next(&mut self, key_rx: &mut mpsc::Receiver<KeyEvent>) -> Step {
        tokio::select! {
            k = key_rx.recv() => Step::Key(k.expect("key channel closed")),
            r = self.client.as_mut().expect("client present").next() => Step::Rec(r),
        }
    }

    // ---------------- key handling (synchronous: editing + queueing) ----------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.menu.is_some() && self.handle_menu_key(key) {
            return;
        }
        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => self.newline(),
            KeyCode::Enter => self.pending_action = Some(KeyAction::Enter),
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => self.newline(),
            KeyCode::Esc => self.pending_action = Some(KeyAction::Esc),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.exiting = true,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.input.join("").is_empty() {
                    self.exiting = true;
                }
            }
            KeyCode::Char('t') => self.toggle_last_thinking(),
            KeyCode::Char('?') => self.menu = Some(Menu::Help),
            KeyCode::Tab => self.complete_slash(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    self.cursor_col = self.input[self.cursor_line].chars().count();
                }
            }
            KeyCode::Right => {
                let len = self.input[self.cursor_line].chars().count();
                if self.cursor_col < len {
                    self.cursor_col += 1;
                } else if self.cursor_line + 1 < self.input.len() {
                    self.cursor_line += 1;
                    self.cursor_col = 0;
                }
            }
            KeyCode::Home => self.cursor_col = 0,
            KeyCode::End => self.cursor_col = self.input[self.cursor_line].chars().count(),
            KeyCode::Char(c) => {
                self.hist_idx = None;
                let line = &mut self.input[self.cursor_line];
                let len = line.chars().count();
                if self.cursor_col > len {
                    self.cursor_col = len;
                }
                let mut chars: Vec<char> = line.chars().collect();
                chars.insert(self.cursor_col, c);
                *line = chars.into_iter().collect();
                self.cursor_col += 1;
            }
            _ => {}
        }
    }

    /// Execute the queued key action (async: may send to pi).
    pub async fn flush_action(&mut self) {
        let action = self.pending_action.take();
        match action {
            Some(KeyAction::Enter) => self.do_enter().await,
            Some(KeyAction::Esc) => self.do_esc().await,
            Some(KeyAction::PickModel(id)) => {
                if let Some(c) = self.client.as_mut() {
                    c.send(CmdKind::SetModel, json!({ "type": "set_model", "model": id })).await;
                }
            }
            Some(KeyAction::Quit) | Some(KeyAction::None) | None => {}
        }
    }

    async fn do_enter(&mut self) {
        let text = self.input.join("\n");
        if text.trim().is_empty() {
            return;
        }
        self.history.push(self.input.clone());
        self.hist_idx = None;
        self.input = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;

        if text.starts_with('/') && Self::is_known_slash(&text) {
            self.dispatch_slash(&text).await;
        } else {
            self.items.push(Item::User(text.clone()));
            if self.phase == Phase::Running {
                self.send_prompt(&text, true).await;
            } else {
                self.phase = Phase::Running;
                self.send_prompt(&text, false).await;
            }
        }
    }

    async fn do_esc(&mut self) {
        if self.phase == Phase::Running {
            // pi docs: clear_queue before abort; restore text into the editor
            if let Some(c) = self.client.as_mut() {
                c.send(CmdKind::ClearQueue, json!({ "type": "clear_queue" })).await;
                c.send(CmdKind::Abort, json!({ "type": "abort" })).await;
            }
            self.note("aborted · queued messages cleared");
        } else if self.input.iter().any(|l| !l.is_empty()) {
            self.input = vec![String::new()];
            self.cursor_line = 0;
            self.cursor_col = 0;
        }
    }

    fn is_known_slash(text: &str) -> bool {
        let head = text.split_whitespace().next().unwrap_or("");
        SLASH_COMMANDS.iter().any(|c| head == *c)
    }

    async fn dispatch_slash(&mut self, text: &str) {
        let mut parts = text.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        let c = match self.client.as_mut() {
            Some(c) => c,
            None => return,
        };
        match cmd {
            "/help" => self.menu = Some(Menu::Help),
            "/quit" | "/exit" => self.exiting = true,
            "/model" => {
                if arg.is_empty() {
                    c.send(CmdKind::GetModels, json!({ "type": "get_available_models" })).await;
                } else {
                    c.send(CmdKind::SetModel, json!({ "type": "set_model", "model": arg }))
                        .await;
                }
            }
            "/think" => {
                if arg.is_empty() {
                    c.send(CmdKind::CycleThinking, json!({ "type": "cycle_thinking_level" })).await;
                } else {
                    c.send(CmdKind::SetThinking, json!({ "type": "set_thinking_level", "level": arg }))
                        .await;
                }
            }
            "/clear" => {
                c.send(CmdKind::NewSession, json!({ "type": "new_session" })).await;
            }
            "/abort" => { let _ = c.send(CmdKind::Abort, json!({ "type": "abort" })).await; }
            "/compact" => { let _ = c.send(CmdKind::Compact, json!({ "type": "compact" })).await; }
            "/stats" => { let _ = c.send(CmdKind::Stats, json!({ "type": "get_session_stats" })).await; }
            _ => {
                // unknown slash command: send to pi (extension/skill/template)
                if self.phase == Phase::Running {
                    self.send_prompt(text, true).await;
                } else {
                    self.phase = Phase::Running;
                    self.send_prompt(text, false).await;
                }
            }
        }
    }

    async fn send_prompt(&mut self, text: &str, steering: bool) {
        let mut msg = json!({ "type": "prompt", "message": text });
        if steering {
            msg["streamingBehavior"] = json!("steer");
        }
        if let Some(c) = self.client.as_mut() {
            if c.send(CmdKind::Prompt, msg).await {
                if steering {
                    self.note(format!("→ steering: {text}"));
                }
            } else {
                self.error("pi is gone; cannot send prompt");
                self.phase = Phase::Idle;
            }
        }
    }

    // ---------------- editor (synchronous) ----------------

    fn newline(&mut self) {
        self.hist_idx = None;
        self.input.insert(self.cursor_line + 1, String::new());
        self.cursor_line += 1;
        self.cursor_col = 0;
    }

    fn backspace(&mut self) {
        let line = &mut self.input[self.cursor_line];
        if self.cursor_col > 0 {
            let mut chars: Vec<char> = line.chars().collect();
            if self.cursor_col <= chars.len() {
                chars.remove(self.cursor_col - 1);
                *line = chars.into_iter().collect();
                self.cursor_col -= 1;
            }
        } else if self.cursor_line > 0 {
            let removed_len = self.input[self.cursor_line - 1].chars().count();
            self.input.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = removed_len.min(self.input[self.cursor_line].chars().count());
        }
    }

    fn delete(&mut self) {
        if self.cursor_line + 1 < self.input.len() && self.cursor_col == self.input[self.cursor_line].chars().count() {
            let next = self.input.remove(self.cursor_line + 1);
            let line = &mut self.input[self.cursor_line];
            *line = format!("{line}{next}");
            return;
        }
        let line = &mut self.input[self.cursor_line];
        let mut chars: Vec<char> = line.chars().collect();
        if self.cursor_col < chars.len() {
            chars.remove(self.cursor_col);
            *line = chars.into_iter().collect();
        }
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.hist_idx {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.hist_idx = Some(idx);
        self.input = self.history[idx].clone();
        self.cursor_line = 0;
        self.cursor_col = self.input[0].chars().count();
    }

    fn history_next(&mut self) {
        if let Some(i) = self.hist_idx {
            if i + 1 >= self.history.len() {
                self.hist_idx = None;
                self.input = vec![String::new()];
                self.cursor_line = 0;
                self.cursor_col = 0;
            } else {
                self.hist_idx = Some(i + 1);
                self.input = self.history[i + 1].clone();
                self.cursor_line = 0;
                self.cursor_col = self.input[0].chars().count();
            }
        }
    }

    fn complete_slash(&mut self) {
        if self.input[0].starts_with('/') {
            self.menu = Some(Menu::Slash { selected: 0 });
        }
    }

    fn toggle_last_thinking(&mut self) {
        for item in self.items.iter_mut().rev() {
            if let Item::Asst(a) = item {
                if !a.thinking.is_empty() {
                    a.thinking_collapsed = !a.thinking_collapsed;
                    return;
                }
            }
        }
    }

    fn handle_menu_key(&mut self, key: KeyEvent) -> bool {
        match &self.menu {
            Some(Menu::Slash { selected }) => match key.code {
                KeyCode::Up => {
                    self.menu = Some(Menu::Slash {
                        selected: selected.saturating_sub(1),
                    });
                    true
                }
                KeyCode::Down => {
                    self.menu = Some(Menu::Slash {
                        selected: (selected + 1).min(SLASH_COMMANDS.len() - 1),
                    });
                    true
                }
                KeyCode::Enter | KeyCode::Tab => {
                    let cmd = SLASH_COMMANDS[*selected].to_string() + " ";
                    self.input = vec![cmd.clone()];
                    self.cursor_line = 0;
                    self.cursor_col = cmd.chars().count();
                    self.menu = None;
                    true
                }
                KeyCode::Esc => {
                    self.menu = None;
                    true
                }
                _ => false,
            },
            Some(Menu::Models { list, selected }) => match key.code {
                KeyCode::Up => {
                    self.menu = Some(Menu::Models {
                        list: list.clone(),
                        selected: selected.saturating_sub(1),
                    });
                    true
                }
                KeyCode::Down => {
                    self.menu = Some(Menu::Models {
                        list: list.clone(),
                        selected: (selected + 1).min(list.len().saturating_sub(1)),
                    });
                    true
                }
                KeyCode::Enter => {
                    if let Some((id, _)) = list.get(*selected).cloned() {
                        self.pending_action = Some(KeyAction::PickModel(id));
                    }
                    self.menu = None;
                    true
                }
                KeyCode::Esc => {
                    self.menu = None;
                    true
                }
                _ => false,
            },
            Some(Menu::Help) => matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')),
            None => false,
        }
    }

    // ---------------- pi events ----------------

    pub fn on_record(&mut self, rec: Rec) {
        match rec {
            Rec::Cmd { kind, resp } => self.on_cmd_response(kind, &resp),
            Rec::Event(v) => {
                if v.get("type").and_then(|t| t.as_str()) == Some("response") {
                    // uncorrelated response (e.g. a parse error): surface failures
                    if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
                        self.error(
                            v.get("error")
                                .and_then(|x| x.as_str())
                                .unwrap_or("command failed")
                                .to_string(),
                        );
                    }
                    return;
                }
                self.on_event(parse_event(&v));
            }
            Rec::Stderr(line) => {
                self.stderr_tail.push_back(line);
                if self.stderr_tail.len() > 5 {
                    self.stderr_tail.pop_front();
                }
            }
        }
    }

    fn on_cmd_response(&mut self, kind: CmdKind, resp: &serde_json::Value) {
        let ok = resp.get("success").and_then(|x| x.as_bool()).unwrap_or(false);
        let data = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let err = || {
            resp.get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("command failed")
                .to_string()
        };
        match kind {
            CmdKind::Prompt => {
                if !ok {
                    self.phase = Phase::Idle;
                    self.error(err());
                } else if data
                    .get("disposition")
                    .and_then(|x| x.as_str())
                    .is_some_and(|d| d == "handled")
                {
                    self.phase = Phase::Idle; // consumed by an extension command
                }
            }
            CmdKind::GetState => {
                if ok {
                    self.model = data
                        .get("model")
                        .and_then(|m| {
                            m.get("name")
                                .or_else(|| m.get("id"))
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                        })
                        .or_else(|| data.get("model").and_then(|x| x.as_str()).map(|s| s.to_string()));
                    self.thinking = data
                        .get("thinkingLevel")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                    self.session_id = data
                        .get("sessionId")
                        .or_else(|| data.get("sessionFile"))
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                    self.session_name = data
                        .get("name")
                        .and_then(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    self.items
                        .retain(|i| !matches!(i, Item::Note(n) if n.starts_with("connecting")));
                    self.note(format!(
                        "connected · {} · model {} · thinking {}",
                        self.session_id
                            .as_deref()
                            .map(|s| {
                                let base = s.rsplit('/').next().unwrap_or(s);
                                base.chars().take(12).collect::<String>()
                            })
                            .unwrap_or_else(|| "in-memory".into()),
                        self.model.as_deref().unwrap_or("?"),
                        self.thinking.as_deref().unwrap_or("?"),
                    ));
                    self.phase = Phase::Idle;
                } else {
                    self.error(err());
                }
            }
            CmdKind::GetModels => {
                if ok {
                    let list = extract_models(&data);
                    if list.is_empty() {
                        self.error("no models available");
                    } else {
                        self.menu = Some(Menu::Models { list, selected: 0 });
                    }
                } else {
                    self.error(err());
                }
            }
            CmdKind::SetModel => {
                if ok {
                    self.note(format!(
                        "model → {}",
                        data.get("modelId")
                            .and_then(|x| x.as_str())
                            .or_else(|| data.get("model").and_then(|x| x.as_str()))
                            .unwrap_or("set")
                    ));
                } else {
                    self.error(err());
                }
            }
            CmdKind::SetThinking => {
                if ok {
                    self.note("thinking level set");
                } else {
                    self.error(err());
                }
            }
            CmdKind::NewSession => {
                if ok {
                    let cancelled = data.get("cancelled").and_then(|x| x.as_bool()).unwrap_or(false);
                    if cancelled {
                        self.note("new session cancelled by extension");
                    } else {
                        self.items.retain(|i| !matches!(i, Item::Asst(_)));
                        self.items.retain(|i| !matches!(i, Item::User(_)));
                        self.items.retain(|i| !matches!(i, Item::Note(n) if n.starts_with("connected")));
                        self.note("new session started");
                    }
                } else {
                    self.error(err());
                }
            }
            CmdKind::Abort => {
                if ok {
                    self.note("abort requested");
                }
            }
            CmdKind::Compact => {
                if ok {
                    self.note("compaction requested");
                } else {
                    self.error(err());
                }
            }
            CmdKind::Stats => {
                if ok {
                    self.note(format!("stats: {}", compact_json(&data)));
                } else {
                    self.error(err());
                }
            }
            CmdKind::ClearQueue => {
                if ok {
                    // restore first queued steering text into the editor
                    if let Some(texts) = data.get("steering").and_then(|x| x.as_array()) {
                        if let Some(first) = texts.first().and_then(|x| x.as_str()) {
                            self.input = first.split('\n').map(str::to_string).collect();
                            self.cursor_line = self.input.len().saturating_sub(1);
                            if let Some(last) = self.input.last() {
                                self.cursor_col = last.chars().count();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub fn on_event(&mut self, e: PiEvent) {
        match e {
            PiEvent::AgentStart => self.phase = Phase::Running,
            PiEvent::AgentSettled { aborted } => {
                self.phase = Phase::Idle;
                if aborted {
                    self.note("agent run aborted");
                }
            }
            PiEvent::TurnEnd => {}
            PiEvent::MessageStart { role } if role == "assistant" => {
                self.items.push(Item::Asst(AssistMsg::new()));
            }
            PiEvent::MessageStart { .. } => {}
            PiEvent::MessageUpdate { usage, ame } => {
                if let Some(u) = usage {
                    self.cur_usage = Some(u);
                }
                match ame {
                    AmEvent::TextDelta { delta, .. } => self.append_to_last_asst(|a| a.text.push_str(&delta)),
                    AmEvent::TextEnd { content, .. } => self.append_to_last_asst(|a| a.text = content),
                    AmEvent::ThinkingDelta { delta, .. } => self.append_to_last_asst(|a| a.thinking.push_str(&delta)),
                    AmEvent::ThinkingEnd { content, .. } => self.append_to_last_asst(|a| a.thinking = content),
                    AmEvent::Error { msg } => self.error(msg),
                    AmEvent::ToolCallStart { .. } | AmEvent::Other => {}
                }
            }
            PiEvent::MessageEnd => {
                if let Some(a) = self.last_asst_mut() {
                    a.streaming = false;
                }
                if let Some(u) = self.cur_usage.take() {
                    self.total_cost += u.cost;
                    self.total_tokens += u.total;
                    self.last_usage = Some(u);
                }
            }
            PiEvent::ToolStart { name, args, .. } => {
                self.append_to_last_asst(|a| {
                    a.tools.push(ToolRow {
                        name: name.clone(),
                        summary: args_summary(&args),
                        running: true,
                        is_error: false,
                        output: String::new(),
                        duration_ms: None,
                    });
                });
            }
            PiEvent::ToolUpdate { partial, .. } => {
                self.append_to_last_asst(|a| {
                    if let Some(row) = a.tools.iter_mut().find(|t| t.running) {
                        row.output = partial;
                    }
                });
            }
            PiEvent::ToolEnd { result, .. } => {
                self.append_to_last_asst(|a| {
                    if let Some(row) = a.tools.iter_mut().find(|t| t.running) {
                        row.running = false;
                        row.is_error = result.is_error;
                        row.duration_ms = result.duration_ms;
                        row.output = result.output;
                    } else if let Some(row) = a.tools.last_mut() {
                        row.running = false;
                        row.is_error = result.is_error;
                        row.duration_ms = result.duration_ms;
                        row.output = result.output;
                    }
                });
            }
            PiEvent::QueueUpdate { steering, follow_up } => {
                self.queue.steering = steering;
                self.queue.follow_up = follow_up;
            }
            PiEvent::SessionNameChanged { name } => self.session_name = name,
            PiEvent::ThinkingLevelChanged { level } => {
                self.thinking = Some(level.clone());
                self.note(format!("thinking level → {level}"));
            }
            PiEvent::CompactionStart { reason } => self.note(format!("compacting ({reason}) …")),
            PiEvent::CompactionEnd { aborted, error, tokens_after } => {
                if aborted {
                    self.note("compaction aborted");
                } else if let Some(err) = error {
                    self.error(format!("compaction failed: {err}"));
                } else {
                    self.note(match tokens_after {
                        Some(t) => format!("compaction done · ~{t} tokens"),
                        None => "compaction done".to_string(),
                    });
                }
            }
            PiEvent::AutoRetryStart { attempt, max_attempts, error } => self.note(format!(
                "retry {attempt}/{max_attempts}{}",
                error.map(|e| format!(" ({e})")).unwrap_or_default()
            )),
            PiEvent::AutoRetryEnd { success, final_error } => {
                if success {
                    self.note("retry succeeded");
                } else if let Some(e) = final_error {
                    self.error(format!("retry failed: {e}"));
                }
            }
            PiEvent::ExtensionError { error } => self.error(error),
            PiEvent::Other(_) => {}
        }
    }

    // ---------------- helpers ----------------

    fn note(&mut self, text: impl Into<String>) {
        self.items.push(Item::Note(text.into()));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.items.push(Item::Err(text.into()));
    }

    fn last_asst_mut(&mut self) -> Option<&mut AssistMsg> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Asst(a) => Some(a),
            _ => None,
        })
    }

    /// Apply `f` to the last assistant message, creating one if absent.
    fn append_to_last_asst(&mut self, f: impl FnOnce(&mut AssistMsg)) {
        if self.last_asst_mut().is_none() {
            self.items.push(Item::Asst(AssistMsg::new()));
        }
        if let Some(a) = self.last_asst_mut() {
            f(a);
        }
    }
}

fn compact_json(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 200 {
        s.chars().take(200).collect::<String>() + "…"
    } else {
        s
    }
}
