//! Application state: the family of agent members, per-member chat state,
//! the first-run setup wizard, and pi event routing.
//!
//! Design: `on_key` is synchronous (editing only). Anything that must talk to
//! pi is queued as a `KeyAction` and executed by the async event loop in
//! `flush_action()`. Each member with a live pi process has a forwarder task
//! that tags its records with the member index; the app demultiplexes.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::family::{Family, Member, FACE_PRESETS, THINKING_LEVELS};
use crate::pi_client::PiClient;
use crate::protocol::{
    args_summary, extract_models, parse_event, AmEvent, CmdKind, PiEvent, Rec,
};

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
    NextMember,
    PrevMember,
    NewMember,
    EditMember,
}

pub enum Item {
    User { text: String, at: i64 },
    Asst(AssistMsg),
    Note(String),
    Err(String),
}

/// Unix seconds, now (cheap, no allocation).
pub fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `16:42` local time from unix seconds.
pub fn fmt_hm(ts: i64) -> String {
    let s = ts.rem_euclid(86400) as u32;
    format!("{:02}:{:02}", s / 3600, (s % 3600) / 60)
}

pub struct AssistMsg {
    pub text: String,
    pub thinking: String,
    pub thinking_collapsed: bool,
    /// GUI: user-expanded thinking block (independent of the TUI's `t` toggle).
    pub thinking_open: bool,
    pub tools: Vec<ToolRow>,
    pub streaming: bool,
    pub at: i64,
}

impl AssistMsg {
    fn new() -> Self {
        Self {
            text: String::new(),
            thinking: String::new(),
            thinking_collapsed: true,
            thinking_open: false,
            tools: Vec::new(),
            streaming: true,
            at: now_ts(),
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

/// Per-member runtime state: config + live client + chat.
pub struct MemberState {
    pub member: Member,
    pub client: Option<PiClient>,
    pub phase: Phase,
    pub items: Vec<Item>,
    pub queue: Queue,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub session_id: Option<String>,
    pub last_usage: Option<crate::protocol::Usage>,
    pub total_cost: f64,
    pub total_tokens: u64,
    cur_usage: Option<crate::protocol::Usage>,
}

impl MemberState {
    fn new(member: Member) -> Self {
        let session_id = member.session_id.clone();
        Self {
            member,
            client: None,
            phase: Phase::Connecting,
            items: vec![],
            queue: Queue {
                steering: Vec::new(),
                follow_up: Vec::new(),
            },
            model: None,
            thinking: None,
            session_id,
            last_usage: None,
            total_cost: 0.0,
            total_tokens: 0,
            cur_usage: None,
        }
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.items.push(Item::Note(text.into()));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.items.push(Item::Err(text.into()));
    }

    pub fn on_record(&mut self, rec: Rec) {
        match rec {
            Rec::Cmd { kind, resp } => self.on_cmd_response(kind, &resp),
            Rec::Event(v) => {
                if v.get("type").and_then(|t| t.as_str()) == Some("response") {
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
            Rec::Stderr(_) => {}
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
                    self.phase = Phase::Idle;
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
                        .or_else(|| {
                            data.get("model")
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                        });
                    self.thinking = data
                        .get("thinkingLevel")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                    if let Some(sid) = data
                        .get("sessionId")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                    {
                        if Some(sid.clone()) != self.session_id {
                            self.session_id = Some(sid.clone());
                            self.member.session_id = Some(sid);
                        }
                    }
                    self.items
                        .retain(|i| !matches!(i, Item::Note(n) if n.starts_with("…")));
                    self.phase = Phase::Idle;
                } else {
                    self.error(err());
                    self.phase = Phase::Idle;
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
                    let cancelled = data
                        .get("cancelled")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    if cancelled {
                        self.note("new session cancelled by extension");
                    } else {
                        self.items.retain(|i| !matches!(i, Item::Asst(_)));
                        self.items.retain(|i| !matches!(i, Item::User { .. }));
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
                    AmEvent::TextDelta { delta, .. } => self.apply_to_last_asst(|a| a.text.push_str(&delta)),
                    AmEvent::TextEnd { content, .. } => self.apply_to_last_asst(|a| a.text = content),
                    AmEvent::ThinkingDelta { delta, .. } => self.apply_to_last_asst(|a| a.thinking.push_str(&delta)),
                    AmEvent::ThinkingEnd { content, .. } => self.apply_to_last_asst(|a| a.thinking = content),
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
                self.apply_to_last_asst(|a| {
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
                self.apply_to_last_asst(|a| {
                    if let Some(row) = a.tools.iter_mut().find(|t| t.running) {
                        row.output = partial;
                    }
                });
            }
            PiEvent::ToolEnd { result, .. } => {
                self.apply_to_last_asst(|a| {
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
            PiEvent::SessionNameChanged { .. } => {}
            PiEvent::Other(_) => {}
        }
    }

    fn last_asst_mut(&mut self) -> Option<&mut AssistMsg> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Asst(a) => Some(a),
            _ => None,
        })
    }

    fn apply_to_last_asst(&mut self, f: impl FnOnce(&mut AssistMsg)) {
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

// ---------------- wizard ----------------

pub enum WizStep {
    Name,
    Model,
    Thinking,
    Face,
    Prompt,
}

pub struct Wizard {
    pub step: WizStep,
    pub is_new: bool,
    pub editing_idx: Option<usize>,
    pub name: String,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub face: Option<String>,
    pub prompt: Vec<String>,
    pub model_list: Vec<(String, String)>,
    pub model_pending: bool,
    pub sel: usize,
}

impl Wizard {
    fn new(is_new: bool, editing_idx: Option<usize>) -> Self {
        Self {
            step: WizStep::Name,
            is_new,
            editing_idx,
            name: String::new(),
            model: None,
            thinking: None,
            face: None,
            prompt: vec![String::new()],
            model_list: Vec::new(),
            model_pending: false,
            sel: 3, // new members default to thinking=medium
        }
    }

    fn prefill(&mut self, m: &Member) {
        self.name = m.name.clone();
        self.model = Some(m.model.clone());
        self.thinking = Some(m.thinking.clone());
        self.face = Some(m.face.clone());
        self.prompt = m
            .system_prompt
            .clone()
            .unwrap_or_default()
            .split('\n')
            .map(str::to_string)
            .collect();
        self.sel = THINKING_LEVELS
            .iter()
            .position(|l| *l == m.thinking.as_str())
            .unwrap_or(3);
    }
}

pub const SLASH_COMMANDS: &[&str] = &[
    "/model", "/think", "/clear", "/abort", "/compact", "/stats", "/new", "/edit", "/help", "/quit",
];

// ---------------- app ----------------

pub struct App {
    pub cfg: Config,
    pub family: Family,
    pub members: Vec<MemberState>,
    pub active: usize,
    pub input: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub history: Vec<Vec<String>>,
    pub hist_idx: Option<usize>,
    pub menu: Option<MenuItem>,
    pub wizard: Option<Wizard>,
    pub stderr_tail: VecDeque<String>,
    pub exiting: bool,
    pending_action: Option<KeyAction>,
    rec_rx: mpsc::Receiver<(usize, Rec)>,
    rec_tx: mpsc::Sender<(usize, Rec)>,
    model_list_rx: Option<mpsc::Receiver<Vec<(String, String)>>>,
    finish_wizard_pending: bool,
}

pub enum MenuItem {
    Slash { selected: usize },
    Models { list: Vec<(String, String)>, selected: usize },
    Help,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        let (rec_tx, rec_rx) = mpsc::channel(2048);
        let mut family = Family::load().unwrap_or(Family {
            members: Vec::new(),
            active: 0,
        });
        family.clamp_active();
        let members = family.members.iter().cloned().map(MemberState::new).collect();
        Self {
            cfg,
            family,
            members,
            active: 0,
            input: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            history: Vec::new(),
            hist_idx: None,
            menu: None,
            wizard: None,
            stderr_tail: VecDeque::with_capacity(5),
            exiting: false,
            pending_action: None,
            rec_rx,
            rec_tx,
            model_list_rx: None,
            finish_wizard_pending: false,
        }
    }

    /// Load family state, open the wizard when there is no family yet,
    /// and connect the active member.
    pub async fn start(&mut self) -> anyhow::Result<()> {
        if self.members.is_empty() {
            self.open_wizard(true, None);
            return Ok(());
        }
        self.ensure_active_member().await;
        Ok(())
    }

    fn active_mut(&mut self) -> Option<&mut MemberState> {
        self.members.get_mut(self.active)
    }

    fn active(&self) -> Option<&MemberState> {
        self.members.get(self.active)
    }

    // ---------------- member lifecycle ----------------

    async fn ensure_active_member(&mut self) {
        let idx = self.active;
        if self.members.get(idx).is_none() {
            return;
        }
        if self.members[idx].client.is_some() {
            return;
        }
        let member = self.members[idx].member.clone();
        let mut cfg = self.cfg.clone();
        cfg.model = Some(member.model.clone());
        cfg.thinking = Some(member.thinking.clone());
        cfg.system_prompt = member.system_prompt.clone();
        cfg.session_id = member.session_id.clone();
        let mut spawn = match PiClient::spawn(&cfg, self.rec_tx.clone(), idx).await {
            Ok(c) => c,
            Err(e) => {
                self.members[idx].error(format!("pi spawn failed: {e}"));
                self.members[idx].phase = Phase::Idle;
                return;
            }
        };
        spawn.send(CmdKind::GetState, json!({ "type": "get_state" })).await;
        self.members[idx].client = Some(spawn);
        self.members[idx].phase = Phase::Connecting;
        self.members[idx]
            .note(format!("… {} connecting", member.name));
    }

    /// (Re)start a member's client, e.g. after /edit changed its config.
    async fn restart_member(&mut self, idx: usize) {
        if let Some(old) = self.members.get_mut(idx).and_then(|m| m.client.take()) {
            tokio::spawn(async move {
                let mut old = old;
                old.shutdown().await;
            });
        }
        self.ensure_active_member().await;
    }
    fn sync_sessions(&mut self) {
        for (i, m) in self.members.iter().enumerate() {
            if let Some(fam) = self.family.member_mut(i) {
                fam.session_id = m.session_id.clone();
            }
        }
    }

    fn switch_member(&mut self, dir: i64) {
        if self.members.len() <= 1 {
            return;
        }
        let len = self.members.len() as i64;
        self.active = (((self.active as i64 + dir) % len + len) % len) as usize;
        self.family.active = self.active;
        self.sync_sessions();
        self.family.save();
        self.note_to_active(format!("— {} —", self.members[self.active].member.name));
    }

    fn note_to_active(&mut self, text: impl Into<String>) {
        if let Some(m) = self.active_mut() {
            m.note(text);
        }
    }

    // ---------------- wizard ----------------

    fn open_wizard(&mut self, is_new: bool, editing_idx: Option<usize>) {
        let mut w = Wizard::new(is_new, editing_idx);
        if let Some(idx) = editing_idx {
            if let Some(m) = self.members.get(idx) {
                w.prefill(&m.member);
            }
        }
        let cfg = self.cfg.clone();
        let (tx, rx) = mpsc::channel(1);
        self.model_list_rx = Some(rx);
        w.model_pending = true;
        tokio::spawn(async move {
            let (t2, mut r2) = mpsc::channel(64);
            let list = match PiClient::spawn(&cfg, t2, usize::MAX).await {
                Ok(mut c) => {
                    c.send(CmdKind::GetModels, json!({ "type": "get_available_models" }))
                        .await;
                    let mut out = Vec::new();
                    for _ in 0..1000 {
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            r2.recv(),
                        )
                        .await
                        {
                            Ok(Some((_, Rec::Cmd { kind: CmdKind::GetModels, resp }))) => {
                                if resp
                                    .get("success")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false)
                                {
                                    let data = resp
                                        .get("data")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    out = extract_models(&data);
                                }
                                break;
                            }
                            Ok(Some(_)) => continue,
                            _ => break,
                        }
                    }
                    c.shutdown().await;
                    out
                }
                Err(_) => Vec::new(),
            };
            let _ = tx.send(list).await;
        });
        self.wizard = Some(w);
    }

    async fn finish_wizard(&mut self) {
        let Some(w) = self.wizard.take() else { return };
        let face = w
            .face
            .clone()
            .unwrap_or_else(|| FACE_PRESETS[0].1.join("\n"));
        let prompt = w.prompt.join("\n");
        let prompt = if prompt.trim().is_empty() { None } else { Some(prompt) };
        match w.editing_idx {
            Some(idx) => {
                if let Some(m) = self.members.get_mut(idx) {
                    m.member.name = w.name.clone();
                    m.member.face = face;
                    m.member.model = w.model.clone().unwrap_or_else(|| m.member.model.clone());
                    m.member.thinking = w.thinking.clone().unwrap_or_else(|| m.member.thinking.clone());
                    m.member.system_prompt = prompt;
                    m.client = None;
                }
                self.restart_member(idx).await;
                self.note_to_active(format!("— {} updated —", w.name));
            }
            None => {
                let member = Member {
                    id: crate::family::new_member_id(),
                    name: w.name.clone(),
                    face,
                    model: w.model.clone().unwrap_or_default(),
                    thinking: w.thinking.clone().unwrap_or_else(|| "medium".into()),
                    system_prompt: prompt,
                    session_id: None,
                };
                self.members.push(MemberState::new(member.clone()));
                self.family.members.push(member);
                self.active = self.members.len() - 1;
                self.family.active = self.active;
                self.ensure_active_member().await;
                self.note_to_active("welcome to the family".to_string());
            }
        }
        self.sync_sessions();
        self.family.save();
    }

    fn wizard_key(&mut self, key: KeyEvent) -> bool {
        let Some(w) = self.wizard.as_mut() else { return false };
        if w.model_pending {
            if let Some(rx) = self.model_list_rx.as_mut() {
                if let Ok(list) = rx.try_recv() {
                    w.model_list = list;
                    w.model_pending = false;
                }
            }
        }
        match w.step {
            WizStep::Name => match key.code {
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    w.name.push(c);
                    true
                }
                KeyCode::Backspace => {
                    w.name.pop();
                    true
                }
                KeyCode::Enter => {
                    if !w.name.trim().is_empty() {
                        w.step = WizStep::Model;
                        w.sel = w
                            .model
                            .as_ref()
                            .and_then(|m| w.model_list.iter().position(|(id, _)| id == m))
                            .unwrap_or(0);
                    }
                    true
                }
                KeyCode::Esc => {
                    if w.is_new && self.members.is_empty() {
                        true
                    } else {
                        self.wizard = None;
                        true
                    }
                }
                _ => false,
            },
            WizStep::Model => {
                let n = w.model_list.len().max(1);
                match key.code {
                    KeyCode::Up => {
                        w.sel = w.sel.saturating_sub(1);
                        true
                    }
                    KeyCode::Down => {
                        w.sel = (w.sel + 1).min(n - 1);
                        true
                    }
                    KeyCode::Enter => {
                        if let Some((id, _)) = w.model_list.get(w.sel).cloned() {
                            w.model = Some(id);
                            w.step = WizStep::Thinking;
                            w.sel = w
                                .thinking
                                .as_ref()
                                .and_then(|t| {
                                    THINKING_LEVELS
                                        .iter()
                                        .position(|l| *l == t.as_str())
                                })
                                .unwrap_or(3); // default: medium
                        }
                        true
                    }
                    KeyCode::Esc => {
                        w.step = WizStep::Name;
                        true
                    }
                    _ => false,
                }
            }
            WizStep::Thinking => match key.code {
                KeyCode::Up => {
                    w.sel = w.sel.saturating_sub(1);
                    true
                }
                KeyCode::Down => {
                    w.sel = (w.sel + 1).min(THINKING_LEVELS.len() - 1);
                    true
                }
                KeyCode::Enter => {
                    w.thinking = Some(THINKING_LEVELS[w.sel].to_string());
                    w.step = WizStep::Face;
                    w.sel = 0;
                    true
                }
                KeyCode::Esc => {
                        w.step = WizStep::Model;
                        true
                    }
                _ => false,
            },
            WizStep::Face => match key.code {
                KeyCode::Up => {
                    w.sel = w.sel.saturating_sub(1);
                    true
                }
                KeyCode::Down => {
                    w.sel = (w.sel + 1).min(FACE_PRESETS.len() - 1);
                    true
                }
                KeyCode::Enter => {
                    w.face = Some(FACE_PRESETS[w.sel].1.join("\n"));
                    w.step = WizStep::Prompt;
                    true
                }
                KeyCode::Esc => {
                    w.step = WizStep::Thinking;
                    true
                }
                _ => false,
            },
            WizStep::Prompt => {
                let buf = &mut w.prompt;
                let done = match key.code {
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        buf.insert(0, String::new());
                        false
                    }
                    KeyCode::Enter => true,
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buf[0].push(c);
                        false
                    }
                    KeyCode::Backspace => {
                        if buf[0].is_empty() && buf.len() > 1 {
                            buf.remove(0);
                        } else {
                            buf[0].pop();
                        }
                        false
                    }
                    _ => false,
                };
                if done {
                    self.finish_wizard_pending = true;
                }
                true
            }
        }
    }

    async fn flush_wizard(&mut self) {
        if self.finish_wizard_pending {
            self.finish_wizard_pending = false;
            self.finish_wizard().await;
        }
    }

    // ---------------- key handling (synchronous) ----------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.wizard.is_some() && self.wizard_key(key) {
            return;
        }
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
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.pending_action = Some(KeyAction::NextMember)
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.pending_action = Some(KeyAction::PrevMember)
            }
            KeyCode::Char('t') => self.toggle_last_thinking(),
            KeyCode::Char('?') => self.menu = Some(MenuItem::Help),
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

    /// Execute the queued key action (async: may talk to pi).
    pub async fn flush_action(&mut self) {
        self.flush_wizard().await;
        match self.pending_action.take() {
            Some(KeyAction::Enter) => self.do_enter().await,
            Some(KeyAction::Esc) => self.do_esc().await,
            Some(KeyAction::PickModel(id)) => {
                if let Some(m) = self.active_mut() {
                    if let Some(c) = m.client.as_mut() {
                        c.send(CmdKind::SetModel, json!({ "type": "set_model", "model": id }))
                            .await;
                    }
                }
            }
            Some(KeyAction::NextMember) => {
                self.switch_member(1);
                self.ensure_active_member().await;
            }
            Some(KeyAction::PrevMember) => {
                self.switch_member(-1);
                self.ensure_active_member().await;
            }
            Some(KeyAction::NewMember) => self.open_wizard(true, None),
            Some(KeyAction::EditMember) => self.open_wizard(false, Some(self.active)),
            _ => {}
        }
    }

    async fn do_enter(&mut self) {
        if self.members.is_empty() {
            self.open_wizard(true, None);
            return;
        }
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
            self.members[self.active].items.push(Item::User {
                text: text.clone(),
                at: now_ts(),
            });
            if self.members[self.active].phase == Phase::Running {
                self.members[self.active].note(format!("→ steering: {text}"));
                self.send_to_active(&text, true).await;
            } else {
                self.members[self.active].phase = Phase::Running;
                self.send_to_active(&text, false).await;
            }
        }
    }

    async fn send_to_active(&mut self, text: &str, steering: bool) {
        let m = self.active_mut().unwrap();
        let mut msg = json!({ "type": "prompt", "message": text });
        if steering {
            msg["streamingBehavior"] = json!("steer");
        }
        match m.client.as_mut() {
            Some(c) => {
                if !c.send(CmdKind::Prompt, msg).await {
                    m.error("pi is gone; cannot send prompt");
                    m.phase = Phase::Idle;
                }
            }
            None => {
                m.error("member is not connected");
                m.phase = Phase::Idle;
            }
        }
    }

    async fn do_esc(&mut self) {
        let running = self
            .active()
            .map(|m| m.phase == Phase::Running)
            .unwrap_or(false);
        if running {
            if let Some(m) = self.active_mut() {
                if let Some(c) = m.client.as_mut() {
                    c.send(CmdKind::ClearQueue, json!({ "type": "clear_queue" })).await;
                    c.send(CmdKind::Abort, json!({ "type": "abort" })).await;
                }
                m.note("aborted · queued messages cleared");
            }
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
        let Some(m) = self.active_mut() else { return };
        match cmd {
            "/help" => self.menu = Some(MenuItem::Help),
            "/quit" | "/exit" => self.exiting = true,
            "/new" => self.open_wizard(true, None),
            "/edit" => self.open_wizard(false, Some(self.active)),
            "/model" => {
                if arg.is_empty() {
                    if let Some(c) = m.client.as_mut() {
                        c.send(CmdKind::GetModels, json!({ "type": "get_available_models" }))
                            .await;
                    }
                } else if let Some(c) = m.client.as_mut() {
                    c.send(CmdKind::SetModel, json!({ "type": "set_model", "model": arg }))
                        .await;
                }
            }
            "/think" => {
                if arg.is_empty() {
                    if let Some(c) = m.client.as_mut() {
                        c.send(CmdKind::CycleThinking, json!({ "type": "cycle_thinking_level" }))
                            .await;
                    }
                } else if let Some(c) = m.client.as_mut() {
                    c.send(CmdKind::SetThinking, json!({ "type": "set_thinking_level", "level": arg }))
                        .await;
                }
            }
            "/clear" => {
                if let Some(c) = m.client.as_mut() {
                    c.send(CmdKind::NewSession, json!({ "type": "new_session" })).await;
                }
            }
            "/abort" => {
                if let Some(c) = m.client.as_mut() {
                    c.send(CmdKind::Abort, json!({ "type": "abort" })).await;
                }
            }
            "/compact" => {
                if let Some(c) = m.client.as_mut() {
                    c.send(CmdKind::Compact, json!({ "type": "compact" })).await;
                }
            }
            "/stats" => {
                if let Some(c) = m.client.as_mut() {
                    c.send(CmdKind::Stats, json!({ "type": "get_session_stats" })).await;
                }
            }
            _ => {
                if self.active().map(|m| m.phase == Phase::Running).unwrap_or(false) {
                    self.send_to_active(text, true).await;
                } else {
                    self.active_mut().unwrap().phase = Phase::Running;
                    self.send_to_active(text, false).await;
                }
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
        if self.cursor_line + 1 < self.input.len()
            && self.cursor_col == self.input[self.cursor_line].chars().count()
        {
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
            self.menu = Some(MenuItem::Slash { selected: 0 });
        }
    }

    fn toggle_last_thinking(&mut self) {
        let Some(m) = self.active_mut() else { return };
        for item in m.items.iter_mut().rev() {
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
            Some(MenuItem::Slash { selected }) => match key.code {
                KeyCode::Up => {
                    self.menu = Some(MenuItem::Slash {
                        selected: selected.saturating_sub(1),
                    });
                    true
                }
                KeyCode::Down => {
                    self.menu = Some(MenuItem::Slash {
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
            Some(MenuItem::Models { list, selected }) => match key.code {
                KeyCode::Up => {
                    self.menu = Some(MenuItem::Models {
                        list: list.clone(),
                        selected: selected.saturating_sub(1),
                    });
                    true
                }
                KeyCode::Down => {
                    self.menu = Some(MenuItem::Models {
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
            Some(MenuItem::Help) => matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')),
            None => false,
        }
    }

    // ---------------- event loop ----------------

    pub async fn next(&mut self, key_rx: &mut mpsc::Receiver<KeyEvent>) -> Step {
        if let Some(w) = self.wizard.as_mut().filter(|w| w.model_pending) {
            if let Some(rx) = self.model_list_rx.as_mut() {
                if let Ok(list) = rx.try_recv() {
                    w.model_list = list;
                    w.model_pending = false;
                }
            }
        }
        tokio::select! {
            k = key_rx.recv() => Step::Key(k.expect("key channel closed")),
            r = self.rec_rx.recv() => match r {
                Some((idx, rec)) => Step::Rec(idx, rec),
                None => Step::Idle,
            },
        }
    }

    pub fn on_record(&mut self, idx: usize, rec: Rec) {
        if let Rec::Cmd { kind: CmdKind::GetModels, resp } = &rec {
            let ok = resp.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            if ok {
                let data = resp
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let list = extract_models(&data);
                if list.is_empty() {
                    if let Some(m) = self.active_mut() {
                        m.error("no models available");
                    }
                } else {
                    self.menu = Some(MenuItem::Models { list, selected: 0 });
                }
            } else if let Some(m) = self.active_mut() {
                m.error(
                    resp.get("error")
                        .and_then(|x| x.as_str())
                        .unwrap_or("get_available_models failed")
                        .to_string(),
                );
            }
            return;
        }
        if let Rec::Cmd { kind: CmdKind::ClearQueue, resp } = &rec {
            if resp
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                if let Some(texts) = resp
                    .get("data")
                    .and_then(|d| d.get("steering"))
                    .and_then(|x| x.as_array())
                {
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
        if self.members.get(idx).is_none() {
            return;
        }
        let m = self.members.get_mut(idx).unwrap();
        m.on_record(rec);
    }

    /// Shut down every member's pi process and persist the family.
    pub async fn shutdown_all(mut self) {
        for m in &mut self.members {
            if let Some(mut c) = m.client.take() {
                c.shutdown().await;
            }
        }
        let mut fam = self.family.clone();
        fam.active = self.active;
        for (i, m) in self.members.iter().enumerate() {
            if let Some(fm) = fam.member_mut(i) {
                fm.session_id = m.session_id.clone();
            }
        }
        fam.save();
    }
}

/// One step of the event loop.
pub enum Step {
    Key(KeyEvent),
    Rec(usize, Rec),
    Idle,
}
