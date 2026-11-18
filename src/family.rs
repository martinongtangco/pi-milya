//! The family: named agent members with their own model/thinking/face/prompt.
//! Persisted to ~/.config/pi-milya/family.json.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Member {
    pub id: String,
    pub name: String,
    /// ASCII face, may be multi-line.
    pub face: String,
    pub model: String,
    pub thinking: String,
    pub system_prompt: Option<String>,
    /// pi session id, for resuming this member's conversation.
    pub session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Family {
    pub members: Vec<Member>,
    pub active: usize,
}

impl Family {
    pub fn path() -> PathBuf {
        let dir = if cfg!(target_os = "windows") {
            // %APPDATA%\pi-milya is the native location; keep a legacy ~/.config
            // directory if it already holds a family.json (no forced migration).
            let appdata = std::env::var_os("APPDATA")
                .map(|a| PathBuf::from(a).join("pi-milya"));
            let legacy = std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".config").join("pi-milya"));
            match (appdata, legacy) {
                (Some(_), Some(l)) if l.join("family.json").exists() => l,
                (Some(a), _) => a,
                (None, Some(l)) => l,
                (None, None) => PathBuf::from(".pi-milya"),
            }
        } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            PathBuf::from(xdg).join("pi-milya")
        } else if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(".config").join("pi-milya")
        } else {
            PathBuf::from(".pi-milya")
        };
        dir.join("family.json")
    }

    pub fn load() -> Option<Self> {
        let s = std::fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }

    pub fn clamp_active(&mut self) {
        if self.members.is_empty() {
            self.active = 0;
        } else if self.active >= self.members.len() {
            self.active %= self.members.len();
        }
    }

    pub fn member_mut(&mut self, idx: usize) -> Option<&mut Member> {
        self.members.get_mut(idx)
    }
}

pub const THINKING_LEVELS: &[&str] = &[
    "off", "minimal", "low", "medium", "high", "xhigh", "max",
];

pub const FACE_PRESETS: &[(&str, &[&str])] = &[
    ("mama",  &["  _  ", "(o o)", " ^ ^ "]),
    ("papa",  &[" |__|", "(o o)", " _||_ "]),
    ("kuya",  &[" ^_^ ", "(o o)", " _ _ "]),
    ("tita",  &[" __  ", "(>< )", " ^^  "]),
    ("bot",   &[" [||]", "(o_o)", " [||] "]),
    ("ghost", &[" .--.", "(oo o)", " ^  ^"]),
    ("cat",   &[" /\\_/\\", "( o o )", " > ^ < "]),
    ("lola",  &["  __  ", " (o o)", "  ||  "]),
];

pub fn face_of(preset: &[&str]) -> String {
    preset.join("\n")
}

pub fn new_member_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("m-{}", ms)
}
