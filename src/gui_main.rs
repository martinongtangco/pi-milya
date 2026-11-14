//! pi-milya GUI (egui spike): the family as a real window.
//!
//! Reuses the terminal-agnostic core (family, pi_client, protocol, config).
//! All members' conversations coexist — streams keep flowing in the
//! background regardless of which tab is open; switch freely.
//!
//! Built against eframe/egui 0.29: single-string `RichText` (no spans),
//! `TopBottomPanel::bottom`, `ComboBox::show_index`, `Modifiers` fields,
//! `ViewportCommand::Close/Title`.

use std::sync::mpsc as std_mpsc;

use eframe::egui;
use egui::{Color32, RichText, ScrollArea, TextEdit};
use pulldown_cmark::{Event, Parser, Tag};
use serde_json::json;

use pi_milya::app::{AssistMsg, Phase, ToolRow};
use pi_milya::config::{self, Config};
use pi_milya::family::{self, Family, Member, FACE_PRESETS, THINKING_LEVELS};
use pi_milya::pi_client::PiClient;
use pi_milya::protocol::{
    args_summary, extract_models, parse_event, AmEvent, CmdKind, PiEvent, Rec,
};
use tokio::sync::mpsc as tokio_mpsc;

/// Grok-flavoured dark palette + Teams-flavoured layout constants.
mod theme {
    pub const BG: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(13, 13, 13);
    pub const PANEL: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(19, 19, 19);
    pub const HOVER: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(28, 28, 28);
    pub const BORDER: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(34, 34, 34);
    pub const TEXT: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(235, 235, 235);
    pub const DIM: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(150, 150, 150);
    pub const FAINT: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(95, 95, 95);
    pub const BUBBLE: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(31, 31, 31);
    pub const ACCENT: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(245, 245, 245);
    pub const ERR: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(255, 122, 122);
    pub const THINK: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(170, 160, 200);

    // presence
    pub const PRESENCE_IDLE: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(90, 200, 120);
    pub const PRESENCE_BUSY: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(255, 196, 90);
    pub const PRESENCE_CONNECT: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(170, 140, 255);
    pub const PRESENCE_OFF: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(70, 70, 70);

    pub const MEMBER_COLORS: [eframe::egui::Color32; 8] = [
        eframe::egui::Color32::from_rgb(122, 197, 255),
        eframe::egui::Color32::from_rgb(131, 216, 131),
        eframe::egui::Color32::from_rgb(255, 196, 110),
        eframe::egui::Color32::from_rgb(255, 140, 150),
        eframe::egui::Color32::from_rgb(182, 152, 255),
        eframe::egui::Color32::from_rgb(110, 220, 210),
        eframe::egui::Color32::from_rgb(255, 170, 130),
        eframe::egui::Color32::from_rgb(190, 190, 190),
    ];

    /// Teams-style per-member color from a stable name hash.
    pub fn member_color(name: &str) -> eframe::egui::Color32 {
        let h: u64 = name.bytes().fold(5381u64, |a, b| a.wrapping_mul(33).wrapping_add(b as u64));
        MEMBER_COLORS[(h as usize) % MEMBER_COLORS.len()]
    }

    /// dark tint of a member color, for avatar discs.
    pub fn avatar_fill(c: eframe::egui::Color32) -> eframe::egui::Color32 {
        let [r, g, b, _] = c.to_array();
        eframe::egui::Color32::from_rgb(r / 5, g / 5, b / 5)
    }
}

/// eframe visuals for the Grok look: near-black, hairline borders, soft rounding.
fn grok_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = theme::BG;
    v.window_fill = theme::PANEL;
    v.window_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    v.override_text_color = Some(theme::TEXT);
    v.faint_bg_color = theme::BORDER;
    v.hyperlink_color = theme::ACCENT;
    v.error_fg_color = theme::ERR;
    v.code_bg_color = Color32::from_rgb(22, 22, 25);
    // inactive widgets
    v.widgets.inactive.bg_fill = theme::PANEL;
    v.widgets.inactive.weak_bg_fill = theme::PANEL;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, theme::DIM);
    v.widgets.inactive.rounding = egui::Rounding::same(8.0);
    // hovered
    v.widgets.hovered.bg_fill = theme::HOVER;
    v.widgets.hovered.weak_bg_fill = theme::HOVER;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT);
    // active
    v.widgets.active.bg_fill = theme::BUBBLE;
    v.widgets.active.weak_bg_fill = theme::BUBBLE;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT);
    // open (combo boxes)
    v.widgets.open.weak_bg_fill = theme::HOVER;
    // noninteractive
    v.widgets.noninteractive.bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, theme::FAINT);
    // selection
    v.selection.bg_fill = theme::BUBBLE;
    v.selection.stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    v
}

fn new_assist() -> AssistMsg {
    AssistMsg {
        text: String::new(),
        thinking: String::new(),
        thinking_collapsed: true,
        thinking_open: false,
        tools: Vec::new(),
        streaming: true,
        at: pi_milya::app::now_ts(),
    }
}

fn new_tool(name: String, summary: String) -> ToolRow {
    ToolRow {
        name,
        summary,
        running: true,
        is_error: false,
        output: String::new(),
        duration_ms: None,
    }
}

/// One family member's pane: config + live state (the pi client lives in a task).
struct MemberPane {
    member: Member,
    phase: Phase,
    items: Vec<pi_milya::app::Item>,
    model: Option<String>,
    thinking: Option<String>,
    session_id: Option<String>,
    total_tokens: u64,
    total_cost: f64,
    cur_usage: Option<pi_milya::protocol::Usage>,
    cmd_tx: Option<tokio_mpsc::Sender<(CmdKind, serde_json::Value)>>,
}

impl MemberPane {
    fn new(member: Member) -> Self {
        Self {
            session_id: member.session_id.clone(),
            member,
            phase: Phase::Connecting,
            items: vec![],
            model: None,
            thinking: None,
            total_tokens: 0,
            total_cost: 0.0,
            cur_usage: None,
            cmd_tx: None,
        }
    }

    fn note(&mut self, text: impl Into<String>) {
        self.items.push(pi_milya::app::Item::Note(text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.items.push(pi_milya::app::Item::Err(text.into()));
    }

    fn last_asst_mut(&mut self) -> Option<&mut AssistMsg> {
        self.items
            .iter_mut()
            .rev()
            .find_map(|i| match i {
                pi_milya::app::Item::Asst(a) => Some(a),
                _ => None,
            })
    }

    fn apply_to_last_asst(&mut self, f: impl FnOnce(&mut AssistMsg)) {
        if self.last_asst_mut().is_none() {
            self.items.push(pi_milya::app::Item::Asst(new_assist()));
        }
        if let Some(a) = self.last_asst_mut() {
            f(a);
        }
    }

    fn apply_rec(&mut self, rec: &Rec) {
        match rec {
            Rec::Cmd { kind, resp } => self.apply_cmd(kind, resp),
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
                self.apply_event(&parse_event(v));
            }
            Rec::Stderr(line) => {
                if !line.trim().is_empty() {
                    self.note(format!("pi: {line}"));
                }
            }
        }
    }

    fn apply_cmd(&mut self, kind: &CmdKind, resp: &serde_json::Value) {
        let ok = resp.get("success").and_then(|x| x.as_bool()).unwrap_or(false);
        let data = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let err = || {
            resp.get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("command failed")
                .to_string()
        };
        match *kind {
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
                        .retain(|i| !matches!(i, pi_milya::app::Item::Note(n) if n.starts_with("…")));
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
            CmdKind::NewSession => {
                if ok {
                    let cancelled = data
                        .get("cancelled")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    if cancelled {
                        self.note("new session cancelled by extension");
                    } else {
                        self.items.retain(|i| !matches!(i, pi_milya::app::Item::Asst(_)));
                        self.items.retain(|i| !matches!(i, pi_milya::app::Item::User { .. }));
                        self.note("new session started");
                    }
                } else {
                    self.error(err());
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
                    let s = data.to_string();
                    self.note(format!(
                        "stats: {}",
                        if s.len() > 200 {
                            s.chars().take(200).collect::<String>() + "…"
                        } else {
                            s
                        }
                    ));
                } else {
                    self.error(err());
                }
            }
            _ => {}
        }
    }

    fn apply_event(&mut self, e: &PiEvent) {
        match e {
            PiEvent::AgentStart => self.phase = Phase::Running,
            PiEvent::AgentSettled { aborted } => {
                self.phase = Phase::Idle;
                if *aborted {
                    self.note("agent run aborted");
                }
            }
            PiEvent::MessageStart { role } if role == "assistant" => {
                self.items.push(pi_milya::app::Item::Asst(new_assist()));
            }
            PiEvent::MessageUpdate { usage, ame } => {
                if let Some(u) = usage {
                    self.cur_usage = Some(u.clone());
                }
                match ame {
                    AmEvent::TextDelta { delta, .. } => self.apply_to_last_asst(|a| a.text.push_str(delta)),
                    AmEvent::TextEnd { content, .. } => self.apply_to_last_asst(|a| a.text = content.clone()),
                    AmEvent::ThinkingDelta { delta, .. } => self.apply_to_last_asst(|a| a.thinking.push_str(delta)),
                    AmEvent::ThinkingEnd { content, .. } => self.apply_to_last_asst(|a| a.thinking = content.clone()),
                    AmEvent::Error { msg } => self.error(msg.clone()),
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
                }
            }
            PiEvent::ToolStart { name, args, .. } => {
                self.apply_to_last_asst(|a| {
                    a.tools.push(new_tool(name.clone(), args_summary(args)));
                });
            }
            PiEvent::ToolUpdate { partial, .. } => {
                self.apply_to_last_asst(|a| {
                    if let Some(row) = a.tools.iter_mut().find(|t| t.running) {
                        row.output = partial.clone();
                    }
                });
            }
            PiEvent::ToolEnd { result, .. } => {
                self.apply_to_last_asst(|a| {
                    let row = match a.tools.iter_mut().find(|t| t.running) {
                        Some(r) => r,
                        None => match a.tools.last_mut() {
                            Some(r) => r,
                            None => return,
                        },
                    };
                    row.running = false;
                    row.is_error = result.is_error;
                    row.duration_ms = result.duration_ms;
                    row.output = result.output.clone();
                });
            }
            PiEvent::ThinkingLevelChanged { level } => {
                self.thinking = Some(level.clone());
                self.note(format!("thinking level → {level}"));
            }
            PiEvent::CompactionStart { reason } => self.note(format!("compacting ({reason}) …")),
            PiEvent::CompactionEnd { aborted, error, tokens_after } => {
                if *aborted {
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
            PiEvent::ExtensionError { error } => self.error(error.clone()),
            _ => {}
        }
    }
}

/// First-run / new / edit member form (a real form — no key-fu).
struct WizForm {
    is_new: bool,
    editing_idx: Option<usize>,
    name: String,
    model_list: Vec<(String, String)>,
    model: Option<String>,
    model_pending: bool,
    thinking: String,
    face: String,
    prompt: String,
    save_err: String,
}

struct GuiApp {
    base_cfg: Config,
    family: Family,
    members: Vec<MemberPane>,
    active: usize,
    input: String,
    wizard: Option<WizForm>,
    model_list_rx: Option<std_mpsc::Receiver<Vec<(String, String)>>>,
    rec_rx: std_mpsc::Receiver<(usize, Rec)>,
    rec_tx: tokio_mpsc::Sender<(usize, Rec)>,
    rt: tokio::runtime::Handle,
    #[allow(dead_code)] // keeps the runtime alive for the app's whole life
    rt_keep: Option<tokio::runtime::Runtime>,
}

impl GuiApp {
    fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let (rec_tx, rec_rx_tokio) = tokio_mpsc::channel(4096);
        let (rec_tx_std, rec_rx) = std_mpsc::channel();
        rt.spawn(async move {
            let mut rx = rec_rx_tokio;
            while let Some(tag) = rx.recv().await {
                if rec_tx_std.send(tag).is_err() {
                    break;
                }
            }
        });
        let base_cfg = config::parse_args(&std::env::args().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut family = Family::load().unwrap_or(Family {
            members: Vec::new(),
            active: 0,
        });
        family.clamp_active();
        let members = family.members.iter().cloned().map(MemberPane::new).collect();
        let mut app = Self {
            base_cfg,
            family,
            members,
            active: 0,
            input: String::new(),
            wizard: None,
            model_list_rx: None,
            rec_rx,
            rec_tx,
            rt: rt.handle().clone(),
            rt_keep: Some(rt),
        };
        let rt2 = app.rt.clone();
        rt2.block_on(app.start_inner());
        app
    }

    async fn start_inner(&mut self) {
        if self.members.is_empty() {
            self.open_wizard(true, None);
            return;
        }
        self.spawn_member(self.active);
    }

    /// Spawn (or restart) the pi process for one member.
    fn spawn_member(&mut self, idx: usize) {
        let Some(m) = self.members.get_mut(idx) else { return };
        if m.cmd_tx.is_some() {
            return;
        }
        let member = m.member.clone();
        let mut cfg = self.base_cfg.clone();
        cfg.model = Some(member.model.clone());
        cfg.thinking = Some(member.thinking.clone());
        cfg.system_prompt = member.system_prompt.clone();
        cfg.session_id = member.session_id.clone();
        let rec_tx = self.rec_tx.clone();
        let (cmd_tx, mut cmd_rx) = tokio_mpsc::channel(64);
        self.rt.spawn(async move {
            let rec_tx2 = rec_tx.clone();
            let mut client = match PiClient::spawn(&cfg, rec_tx, idx).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = rec_tx2
                        .send((idx, Rec::Stderr(format!("pi spawn failed: {e}"))))
                        .await;
                    return;
                }
            };
            client
                .send(CmdKind::GetState, json!({ "type": "get_state" }))
                .await;
            loop {
                match cmd_rx.recv().await {
                    Some((kind, value)) => {
                        let _ = client.send(kind, value).await;
                    }
                    None => break,
                }
            }
            client.shutdown().await;
        });
        m.cmd_tx = Some(cmd_tx);
        m.phase = Phase::Connecting;
        m.note(format!("… {} connecting", member.name));
    }

    fn send_cmd(&self, idx: usize, kind: CmdKind, value: serde_json::Value) {
        if let Some(tx) = self.members.get(idx).and_then(|m| m.cmd_tx.clone()) {
            let rt = self.rt.clone();
            rt.spawn(async move {
                let _ = tx.send((kind, value)).await;
            });
        }
    }

    fn sync_sessions(&mut self) {
        for (i, m) in self.members.iter().enumerate() {
            if let Some(fam) = self.family.member_mut(i) {
                fam.session_id = m.session_id.clone();
            }
        }
    }

    fn save_family(&mut self) {
        self.family.active = self.active;
        self.sync_sessions();
        self.family.save();
    }

    fn switch_member(&mut self, dir: i64) {
        if self.members.len() <= 1 {
            return;
        }
        let len = self.members.len() as i64;
        self.active = (((self.active as i64 + dir) % len + len) % len) as usize;
        self.save_family();
        self.spawn_member(self.active);
    }

    fn open_wizard(&mut self, is_new: bool, editing_idx: Option<usize>) {
        let mut w = WizForm {
            is_new,
            editing_idx,
            name: String::new(),
            model_list: Vec::new(),
            model: None,
            model_pending: true,
            thinking: "medium".into(),
            face: FACE_PRESETS[0].1.join("\n"),
            prompt: String::new(),
            save_err: String::new(),
        };
        if let Some(idx) = editing_idx {
            if let Some(m) = self.members.get(idx) {
                w.name = m.member.name.clone();
                w.model = Some(m.member.model.clone());
                w.thinking = m.member.thinking.clone();
                w.face = m.member.face.clone();
                w.prompt = m.member.system_prompt.clone().unwrap_or_default();
            }
        }
        let base = self.base_cfg.clone();
        let (tx, rx) = std_mpsc::channel();
        self.model_list_rx = Some(rx);
        self.rt.spawn(async move {
            let (t2, mut r2) = tokio_mpsc::channel(64);
            let list = match PiClient::spawn(&base, t2, usize::MAX).await {
                Ok(mut c) => {
                    c.send(CmdKind::GetModels, json!({ "type": "get_available_models" }))
                        .await;
                    let mut out = Vec::new();
                    for _ in 0..1000 {
                        match tokio::time::timeout(std::time::Duration::from_secs(30), r2.recv())
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
            let _ = tx.send(list);
        });
        self.wizard = Some(w);
    }

    fn apply_wizard(&mut self, w: WizForm) {
        if w.name.trim().is_empty() {
            self.wizard = Some(w);
            return;
        }
        let prompt = if w.prompt.trim().is_empty() {
            None
        } else {
            Some(w.prompt)
        };
        match w.editing_idx {
            Some(idx) => {
                if let Some(m) = self.members.get_mut(idx) {
                    m.member.name = w.name;
                    m.member.face = w.face;
                    m.member.model = w.model.clone().unwrap_or_else(|| m.member.model.clone());
                    m.member.thinking = w.thinking;
                    m.member.system_prompt = prompt;
                    m.cmd_tx = None; // old task exits → shuts down its client
                    m.note(format!("— restarting with new config —"));
                }
                self.spawn_member(idx);
                if let Some(m) = self.members.get_mut(idx) {
                    m.note("config updated");
                }
            }
            None => {
                let member = Member {
                    id: family::new_member_id(),
                    name: w.name,
                    face: w.face,
                    model: w.model.clone().unwrap_or_default(),
                    thinking: w.thinking,
                    system_prompt: prompt,
                    session_id: None,
                };
                self.members.push(MemberPane::new(member.clone()));
                self.family.members.push(member);
                self.active = self.members.len() - 1;
                self.spawn_member(self.active);
                self.members[self.active].note("welcome to the family");
            }
        }
        self.save_family();
    }

    fn drain_records(&mut self) -> bool {
        let mut changed = false;
        for (idx, rec) in self.rec_rx.try_iter() {
            if let Some(m) = self.members.get_mut(idx) {
                let before = hash_items(&m.items);
                m.apply_rec(&rec);
                if hash_items(&m.items) != before {
                    changed = true;
                }
                if m.items.len() > 400 {
                    m.items.drain(0..m.items.len() - 400);
                }
            }
        }
        if let Some(rx) = self.model_list_rx.as_mut() {
            if let Ok(list) = rx.try_recv() {
                if let Some(w) = self.wizard.as_mut() {
                    w.model_list = list;
                    w.model_pending = false;
                }
            }
        }
        changed
    }

    fn do_send(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        let steering = self
            .members
            .get(self.active)
            .map(|m| m.phase == Phase::Running)
            .unwrap_or(false);
        if let Some(m) = self.members.get_mut(self.active) {
            m.items.push(pi_milya::app::Item::User {
                text: text.clone(),
                at: pi_milya::app::now_ts(),
            });
            if m.phase != Phase::Running {
                m.phase = Phase::Running;
            } else {
                m.note(format!("→ steering: {text}"));
            }
        }
        self.input.clear();
        let mut msg = json!({ "type": "prompt", "message": text });
        if steering {
            msg["streamingBehavior"] = json!("steer");
        }
        self.send_cmd(self.active, CmdKind::Prompt, msg);
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let changed = self.drain_records();
        let any_running = self
            .members
            .iter()
            .any(|m| m.phase == Phase::Running);
        let active_streaming = self
            .members
            .get(self.active)
            .is_some_and(|m| m.phase == Phase::Running || m.phase == Phase::Connecting);

        let (next, prev, quit) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(egui::Key::N),
                i.modifiers.ctrl && i.key_pressed(egui::Key::P),
                i.modifiers.ctrl && i.key_pressed(egui::Key::Q),
            )
        });
        if next {
            self.switch_member(1);
        }
        if prev {
            self.switch_member(-1);
        }
        if quit {
            self.save_family();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        ctx.set_visuals(grok_visuals());

        // Adaptive scale: grow fonts/margins with the window so the UI doesn't
        // look tiny on large displays (or cramped on small ones). 900pt is the
        // reference height. `min` (not max) guarantees the whole layout always
        // fits inside the window — even when the reported viewport is smaller
        // than the window (high-DPI Windows displays), so the input bar can
        // never float above a black band. Clamped to [1.0, 2.0].
        let size = ctx.input(|i| i.screen_rect.size());
        let scale = (size.y / 900.0).min(size.x / 1560.0).clamp(1.0, 2.0);
        if (ctx.pixels_per_point() - scale).abs() > 0.01 {
            ctx.set_pixels_per_point(scale);
        }

        let title = match self.members.get(self.active) {
            Some(m) => format!(
                "pi-milya — {} · {}",
                m.member.name,
                m.model.as_deref().unwrap_or("…")
            ),
            None => "pi-milya".to_string(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        if self.wizard.is_some() {
            self.draw_wizard(ctx);
        } else {
            self.draw_main(ctx, active_streaming);
        }

        // Throttled repaint: at most ~20fps, whether or not content changed.
        // Records arriving between ticks are batched into the next repaint
        // (drain_records pulls everything that is ready), so streaming stays
        // smooth without a 60fps full re-layout of the whole window.
        if changed || any_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_family();
    }
}

/// per-row scratch data (immutable snapshot; keeps borrows scoped)
struct RowData {
    active: bool,
    color: Color32,
    initial: String,
    name: String,
    sub: String,
    dot: Color32,
}

fn initial_of(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

impl GuiApp {
    fn draw_main(&mut self, ctx: &egui::Context, active_streaming: bool) {
        let resp = egui::TopBottomPanel::bottom("input").show(ctx, |ui| {
            self.draw_input_bar(ui);
        });
        let _ = resp;
        let fam = egui::SidePanel::left("family")
            .resizable(true)
            .default_width(260.0)
            .width_range(180.0..=480.0)
            .show(ctx, |ui| {
                self.draw_family_sidebar(ui);
            });
        let _ = fam;
        let cent = egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_header(ui);
            self.draw_chat(ui, active_streaming);
        });
        let _ = cent;
    }

    /// Teams-style slim channel header: face · name · model · thinking … stats · actions
    fn draw_header(&mut self, ui: &mut egui::Ui) {
        let d = self.members.get(self.active).map(|m| RowData {
            active: true,
            color: theme::member_color(&m.member.name),
            initial: initial_of(&m.member.name),
            name: m.member.name.clone(),
            sub: format!(
                "{} · {}",
                m.model.as_deref().unwrap_or(&m.member.model),
                m.member.thinking
            )
            .chars()
            .take(40)
            .collect(),
            dot: match m.phase {
                Phase::Running => theme::PRESENCE_BUSY,
                Phase::Connecting => theme::PRESENCE_CONNECT,
                _ => {
                    if m.cmd_tx.is_some() {
                        theme::PRESENCE_IDLE
                    } else {
                        theme::PRESENCE_OFF
                    }
                },
            },
        });
        let (n, total) = (if self.members.is_empty() { 0 } else { self.active + 1 }, self.members.len());
        let (tok, cost) = self
            .members
            .get(self.active)
            .map(|m| (m.total_tokens, m.total_cost))
            .unwrap_or((0, 0.0));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if let Some(d) = &d {
                ui.label(
                    RichText::new(format!("{}  ", d.name.chars().next().unwrap_or(' ')))
                        .strong()
                        .size(14.0)
                        .color(d.color),
                );
                ui.label(RichText::new(&d.name).strong().size(14.0));
                ui.add_space(10.0);
                ui.label(RichText::new(&d.sub).small().color(theme::DIM));
            } else {
                ui.label(RichText::new("the family is empty").size(14.0).color(theme::DIM));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if ui.button("edit member").clicked() {
                    self.open_wizard(false, Some(self.active));
                }
                if ui.button("new member").clicked() {
                    self.open_wizard(true, None);
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("{n}/{total} members · {tok} tokens · ${cost:.3}"))
                        .small()
                        .color(theme::FAINT),
                );
            });
        });
        ui.add_space(6.0);
        ui.add(egui::Separator::default());
    }

    /// Teams-style member rail: avatar disc, name, model caption, presence dot.
    fn draw_family_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("family").strong().size(13.0));
        });
        ui.add_space(12.0);
        for i in 0..self.members.len() {
            let d = { // scoped immutable read
                let m = &self.members[i];
                RowData {
                    active: i == self.active,
                    color: theme::member_color(&m.member.name),
                    initial: initial_of(&m.member.name),
                    name: m.member.name.clone(),
                    sub: format!(
                        "{} · {}",
                        m.model.as_deref().unwrap_or(&m.member.model),
                        m.member.thinking
                    )
                    .chars()
                    .take(26)
                    .collect(),
                    dot: match m.phase {
                        Phase::Running => theme::PRESENCE_BUSY,
                        Phase::Connecting => theme::PRESENCE_CONNECT,
                        _ => {
                            if m.cmd_tx.is_some() {
                                theme::PRESENCE_IDLE
                            } else {
                                theme::PRESENCE_OFF
                            }
                        },
                    },
                }
            };
            let (row_w, row_h) = (ui.available_width().max(140.0), 48.0);
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(row_w, row_h), egui::Sense::click());
            ui.painter().rect_filled(
                rect,
                10.0,
                if d.active { theme::HOVER } else { Color32::TRANSPARENT },
            );
            // avatar disc, left of the row
            let av_c = egui::pos2(rect.min.x + 8.0 + 17.0, rect.center().y);
            ui.painter().circle_filled(av_c, 17.0, theme::avatar_fill(d.color));
            ui.painter().text(
                av_c,
                egui::Align2::CENTER_CENTER,
                d.initial.clone(),
                egui::FontId::new(14.0, egui::FontFamily::Proportional),
                d.color,
            );
            // presence dot, bottom-right of the disc
            ui.painter().circle_filled(av_c + egui::vec2(12.0, 12.0), 5.0, d.dot);
            // name + model caption
            let tx = rect.min.x + 8.0 + 34.0 + 10.0;
            ui.painter().text(
                egui::pos2(tx, rect.min.y + 8.0),
                egui::Align2::LEFT_TOP,
                d.name.clone(),
                egui::FontId::new(13.5, egui::FontFamily::Proportional),
                if d.active { theme::TEXT } else { theme::DIM },
            );
            ui.painter().text(
                egui::pos2(tx, rect.min.y + 28.0),
                egui::Align2::LEFT_TOP,
                d.sub.clone(),
                egui::FontId::new(11.0, egui::FontFamily::Proportional),
                theme::FAINT,
            );
            if resp.clicked() {
                self.active = i;
                self.save_family();
                self.spawn_member(i);
            }
            ui.add_space(4.0);
        }
        ui.add_space(8.0);
        if ui.button("+  new member").clicked() {
            self.open_wizard(true, None);
        }
    }

    /// Grok-style floating input pill: one rounded frame, multiline field, round send.
    fn draw_input_bar(&mut self, ui: &mut egui::Ui) {
        // The send/abort row lives in a `with_layout` sub-ui whose min-size is
        // not propagated to the parent, so the panel must pin its own height
        // (in points — the adaptive pixels_per_point scales it for big displays).
        ui.set_min_height(74.0);
        // Responsive: bar spans most of the panel (capped so it doesn't get
        // absurdly wide on huge displays), centered. All inner widths are
        // derived from the actual available space — nothing fixed at 760px.
        let bar_w = ui.available_width().clamp(520.0, 1200.0);
        ui.horizontal_centered(|ui| {
            egui::Frame::none()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                .rounding(egui::Rounding::same(16.0))
                .inner_margin(egui::Margin::same(12.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_max_width(bar_w - 24.0);
                    let id = egui::Id::new("input_edit");
                    let avail = ui.available_width();
                    // Reserve room for the send button (+abort when running) so
                    // the editor never collides with the controls below it.
                    let edit_w = (avail - 48.0).max(140.0);
                    let input = TextEdit::multiline(&mut self.input)
                        .id(id)
                        .desired_width(edit_w)
                        .desired_rows(1)
                        .frame(false);
                    ui.add(input);
                    if ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                        && ui.memory(|m| m.has_focus(id))
                    {
                        self.do_send();
                    }
                    // Second row: hint left, actions right (egui's documented
                    // idiom: label first, then a right_to_left block which pins
                    // its items to the right edge — no overlap at any size).
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("enter sends · shift+enter newline · ctrl+n/p switch")
                                .size(11.0)
                                .color(theme::FAINT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let running = self
                                .members
                                .get(self.active)
                                .map(|m| m.phase == Phase::Running)
                                .unwrap_or(false);
                            if running && ui.button("abort").clicked() {
                                self.send_cmd(
                                    self.active,
                                    CmdKind::ClearQueue,
                                    json!({ "type": "clear_queue" }),
                                );
                                self.send_cmd(self.active, CmdKind::Abort, json!({ "type": "abort" }));
                                if let Some(m) = self.members.get_mut(self.active) {
                                    m.note("aborted · queued messages cleared");
                                }
                            }
                            let enabled = !self.input.trim().is_empty();
                            let send = egui::Frame::none()
                                .fill(if enabled { theme::ACCENT } else { theme::BUBBLE })
                                .rounding(egui::Rounding::same(14.0))
                                .inner_margin(egui::Margin::same(0.0))
                                .show(ui, |ui| {
                                    ui.set_min_size(egui::vec2(30.0, 30.0));
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new("➤")
                                                .strong()
                                                .color(if enabled {
                                                    theme::BG
                                                } else {
                                                    theme::FAINT
                                                }),
                                        );
                                    });
                                });
                            if send.response.interact(egui::Sense::click()).clicked() {
                                self.do_send();
                            }
                        });
                    });
                });
        });
    }

    /// Teams-style message list: avatar gutter, name + timestamp, markdown body,
    /// right-aligned user bubble, collapsible thinking, tool cards.
    fn draw_chat(&mut self, ui: &mut egui::Ui, active_streaming: bool) {
        if self.members.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.add_space(80.0);
                ui.label(RichText::new("the family is empty").size(20.0).color(theme::DIM));
                ui.add_space(8.0);
                ui.label(RichText::new("click “new member” to meet your first one").weak());
            });
            return;
        }
        let mname = self.members[self.active].member.name.clone();
        let mcolor = theme::member_color(&mname);
        let minitial = initial_of(&mname);
        ScrollArea::both()
            .stick_to_bottom(active_streaming)
            .show(ui, |ui| {
                ui.add_space(12.0);
                let n = self.members[self.active].items.len();
                for idx in 0..n {
                    let toggle_thinking = {
                        let item = &self.members[self.active].items[idx];
                        match item {
                            pi_milya::app::Item::User { text: t, at } => {
                                ui.horizontal(|ui| {
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                        egui::Frame::none()
                                            .fill(theme::BUBBLE)
                                                .rounding(egui::Rounding::same(14.0))
                                                .inner_margin(egui::Margin::same(10.0))
                                                .show(ui, |ui: &mut egui::Ui| {
                                                    ui.set_max_width(680.0);
                                                    ui.label(RichText::new(t.clone()).color(theme::TEXT));
                                                    ui.horizontal(|ui| {
                                                        ui.with_layout(
                                                            egui::Layout::right_to_left(egui::Align::Center),
                                                            |ui| {
                                                                ui.label(
                                                                    RichText::new(pi_milya::app::fmt_hm(*at))
                                                                        .size(10.0)
                                                                        .color(theme::FAINT),
                                                                );
                                                            },
                                                        );
                                                    });
                                                });
                                        ui.add_space(44.0); // avatar gutter
                                    });
                                });
                                false
                            }
                            pi_milya::app::Item::Asst(a) => {
                                let mut clicked_think = false;
                                ui.horizontal(|ui| {
                                    // avatar disc in a fixed gutter
                                    let (arect, _) =
                                        ui.allocate_exact_size(egui::vec2(44.0, 40.0), egui::Sense::hover());
                                    let av_c = egui::pos2(
                                        arect.min.x + 18.0,
                                        arect.min.y + 20.0,
                                    );
                                    ui.painter().circle_filled(av_c, 16.0, theme::avatar_fill(mcolor));
                                    ui.painter().text(
                                        av_c,
                                        egui::Align2::CENTER_CENTER,
                                        minitial.clone(),
                                        egui::FontId::new(13.0, egui::FontFamily::Proportional),
                                        mcolor,
                                    );
                                    ui.add_space(8.0);
                                    ui.vertical(|ui| {
                                        ui.set_max_width(760.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(mname.clone())
                                                    .strong()
                                                    .size(13.5)
                                                    .color(mcolor),
                                            );
                                            ui.add_space(8.0);
                                            ui.label(
                                                RichText::new(pi_milya::app::fmt_hm(a.at))
                                                    .size(10.0)
                                                    .color(theme::FAINT),
                                            );
                                        });
                                        ui.add_space(3.0);
                                        // Grok-style collapsible thinking line
                                        if !a.thinking.is_empty() {
                                            let tl = ui.add(
                                                egui::Label::new(
                                                    RichText::new(format!(
                                                        "{} · thinking · {} chars",
                                                        if a.thinking_open { "▾" } else { "▸" },
                                                        a.thinking.chars().count()
                                                    ))
                                                    .small()
                                                    .color(theme::THINK),
                                                ),
                                            );
                                            clicked_think = tl.interact(egui::Sense::click()).clicked();
                                            if a.thinking_open {
                                                for tl2 in a.thinking.lines().take(120) {
                                                    ui.label(
                                                        RichText::new(tl2.to_string())
                                                            .small()
                                                            .color(theme::THINK),
                                                    );
                                                }
                                                ui.add_space(4.0);
                                            }
                                        }
                                        // tool cards (fixed-size, painter background)
                                        for t in &a.tools {
                                            let (status, c) = if t.running {
                                                ("…", theme::PRESENCE_BUSY)
                                            } else if t.is_error {
                                                ("✕", theme::ERR)
                                            } else {
                                                ("✓", theme::PRESENCE_IDLE)
                                            };
                                            let tw = ui.available_width().min(640.0);
                                            let (trect, _) = ui.allocate_exact_size(
                                                egui::vec2(tw, 34.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter()
                                                .rect_filled(trect, 8.0, Color32::from_rgb(22, 22, 25));
                                            ui.painter()
                                                .rect_stroke(trect, 8.0, egui::Stroke::new(1.0_f32, theme::BORDER));
                                            let ty = trect.center().y;
                                            ui.painter().text(
                                                egui::pos2(trect.min.x + 10.0, ty),
                                                egui::Align2::LEFT_CENTER,
                                                status,
                                                egui::FontId::new(12.0, egui::FontFamily::Proportional),
                                                c,
                                            );
                                            ui.painter().text(
                                                egui::pos2(trect.min.x + 30.0, ty),
                                                egui::Align2::LEFT_CENTER,
                                                t.name.clone(),
                                                egui::FontId::new(12.0, egui::FontFamily::Monospace),
                                                theme::DIM,
                                            );
                                            if let Some(d) = t.duration_ms {
                                                ui.painter().text(
                                                    egui::pos2(trect.max.x - 10.0, ty),
                                                    egui::Align2::RIGHT_CENTER,
                                                    format!("{d} ms"),
                                                    egui::FontId::new(10.0, egui::FontFamily::Proportional),
                                                    theme::FAINT,
                                                );
                                            }
                                            ui.add_space(6.0);
                                        }
                                        if !a.text.is_empty() {
                                            draw_markdown(ui, &a.text);
                                        }
                                        if a.streaming && a.text.is_empty() {
                                            ui.label(RichText::new("…").color(theme::FAINT));
                                        }
                                    });
                                });
                                clicked_think
                            }
                            pi_milya::app::Item::Note(t) => {
                                ui.horizontal_centered(|ui| {
                                    ui.label(RichText::new(t.clone()).size(10.0).color(theme::FAINT));
                                });
                                false
                            }
                            pi_milya::app::Item::Err(t) => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("!  ").color(theme::ERR).strong());
                                    ui.label(RichText::new(t.clone()).small().color(theme::ERR));
                                });
                                false
                            }
                        }
                    };
                    if toggle_thinking {
                        // flip this message's thinking block
                        if let Some(mm) = self
                            .members
                            .get_mut(self.active)
                            .and_then(|m| m.items.get_mut(idx))
                        {
                            if let pi_milya::app::Item::Asst(x) = mm {
                                if !x.thinking.is_empty() {
                                    x.thinking_open = !x.thinking_open;
                                }
                            }
                        }
                    }
                    ui.add_space(14.0);
                }
            });
    }

    fn draw_wizard(&mut self, ctx: &egui::Context) {
        if self.wizard.is_none() {
            return;
        }
        let mut w = self.wizard.take().unwrap();
        let title = if w.is_new {
            "meeting the family".to_string()
        } else {
            format!("editing {}", w.name)
        };
        let mut saved = false;
        let mut cancelled = false;
        egui::TopBottomPanel::top("wiz_title").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(title).size(18.0).color(theme::TEXT));
            });
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.label("name");
                ui.add(
                    egui::TextEdit::singleline(&mut w.name)
                        .desired_width(ui.available_width().min(420.0)),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("model");
                if w.model_pending && w.model_list.is_empty() {
                    ui.label(RichText::new("fetching models from pi …").weak());
                } else if w.model_list.is_empty() {
                    ui.label(RichText::new("no models available — check llamesa").color(theme::ERR));
                } else {
                    let mut sel = w
                        .model
                        .as_ref()
                        .and_then(|m| w.model_list.iter().position(|(id, _)| id == m))
                        .unwrap_or(0);
                    let _ = egui::ComboBox::from_id_salt("model")
                        .selected_text(
                            w.model_list[sel]
                                .1
                                .clone()
                                .chars()
                                .take(60)
                                .collect::<String>(),
                        )
                        .show_index(ui, &mut sel, w.model_list.len(), |i| {
                            w.model_list[i].1.clone()
                        });
                    w.model = w.model_list.get(sel).map(|x| x.0.clone());
                    if let Some(m) = w.model_list.get(sel) {
                        ui.label(
                            RichText::new(format!("id: {}", m.0))
                                .small()
                                .weak()
                                .code(),
                        );
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("thinking");
                let mut sel = THINKING_LEVELS
                    .iter()
                    .position(|t| *t == w.thinking)
                    .unwrap_or(3);
                let _ = egui::ComboBox::from_id_salt("thinking")
                    .selected_text(w.thinking.clone())
                    .show_index(ui, &mut sel, THINKING_LEVELS.len(), |i| {
                        THINKING_LEVELS[i].to_string()
                    });
                w.thinking = THINKING_LEVELS[sel].to_string();
            });
            ui.add_space(8.0);
            ui.label("face");
            ui.horizontal(|ui| {
                for (name, art) in FACE_PRESETS {
                    let face = art.join("\n");
                    let sel = w.face == face;
                    let btn = egui::Button::new(RichText::new(format!("{face}\n{name}")).code().small())
                        .selected(sel);
                    if ui.add(btn).clicked() {
                        w.face = face;
                    }
                }
            });
            ui.add_space(8.0);
            ui.label("system prompt (appended to pi's prompt; empty is fine)");
            ui.add(
                TextEdit::multiline(&mut w.prompt)
                    .desired_width(ui.available_width().min(640.0))
                    .desired_rows(4),
            );
            ui.add_space(16.0);
            if !w.save_err.is_empty() {
                ui.label(RichText::new(&w.save_err).color(theme::ERR));
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("save member")).clicked() {
                    if w.model.as_deref().unwrap_or("").trim().is_empty() {
                        w.save_err = "choose a model first — pi cannot start without one".to_string();
                    } else {
                        saved = true;
                    }
                }
                if ui.add(egui::Button::new("cancel")).clicked() {
                    cancelled = true;
                }
            });
        });
        if saved {
            self.apply_wizard(w);
        } else if cancelled && !(w.is_new && self.members.is_empty()) {
            self.wizard = None;
        } else {
            self.wizard = Some(w);
        }
    }
}

/// Cheap content hash for change detection (len + tail bytes).
fn hash_items(items: &[pi_milya::app::Item]) -> u64 {
    let mut h: u64 = (items.len() as u64).wrapping_mul(0x9e3779b97f4a7c15);
    if let Some(last) = items.last() {
        let s = match last {
            pi_milya::app::Item::User { text: t, .. } => t.as_str(),
            pi_milya::app::Item::Asst(a) => a.text.as_str(),
            pi_milya::app::Item::Note(t) => t.as_str(),
            pi_milya::app::Item::Err(t) => t.as_str(),
        };
        for b in s.bytes().rev().take(64) {
            h = h.rotate_left(5) ^ b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// Render markdown as egui: headings, inline code, code blocks, lists.
fn draw_markdown(ui: &mut egui::Ui, text: &str) {
    let parser = Parser::new(text);
    let mut para: String = String::new();
    let mut in_code = false;
    let mut code_lines: Vec<String> = Vec::new();
    let mut code_line: String = String::new();
    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => para.clear(),
                Tag::CodeBlock(_) => {
                    in_code = true;
                    code_lines.clear();
                    code_line.clear();
                }
                Tag::Item => para.push_str("  • "),
                _ => {}
            },
            Event::End(tag) => match tag {
                pulldown_cmark::TagEnd::Paragraph => {
                    if !para.is_empty() {
                        ui.label(RichText::new(para.clone()));
                        para.clear();
                    }
                }
                pulldown_cmark::TagEnd::Heading(level) => {
                    let size = match level {
                        pulldown_cmark::HeadingLevel::H1 => 20.0,
                        pulldown_cmark::HeadingLevel::H2 => 17.0,
                        _ => 15.0,
                    };
                    if !para.is_empty() {
                        ui.label(RichText::new(para.clone()).strong().size(size));
                        para.clear();
                    }
                }
                pulldown_cmark::TagEnd::CodeBlock => {
                    in_code = false;
                    if !code_line.is_empty() {
                        code_lines.push(code_line.clone());
                        code_line.clear();
                    }
                    let block = code_lines.join("\n");
                    ui.add_space(2.0);
                    let _ = egui::Frame::none()
                        .fill(Color32::from_rgb(22, 22, 25))
                        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::symmetric(10.0, 0.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(block).monospace().small().color(theme::DIM));
                        });
                    ui.add_space(2.0);
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_code {
                    for (n, line) in t.lines().enumerate() {
                        if n > 0 {
                            code_lines.push(code_line.clone());
                            code_line.clear();
                        }
                        code_line.push_str(line);
                    }
                } else {
                    para.push_str(&t.to_string());
                }
            }
            Event::Code(t) => {
                para.push_str(&format!("`{t}`"));
            }
            _ => {}
        }
    }
    if !para.is_empty() {
        ui.label(RichText::new(para));
    }
}

fn main() {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "pi-milya",
        opts,
        Box::new(|_cc| Ok(Box::new(GuiApp::new()))),
    )
    .expect("eframe run failed");
}
