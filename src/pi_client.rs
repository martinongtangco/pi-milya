//! Thin wrapper around a `pi --mode rpc` child process.
//!
//! The stdout read loop runs in a detached task that tags every record with
//! an index (the member) and pushes it into a channel; the client itself owns
//! stdin (for sending commands) and the child handle (for shutdown).
//! pi stdout is JSONL, so we split on LF only (never on U+2028/U+2029).
//! Response lines are correlated back to their command via a pending-id map.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufWriter};
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::protocol::{CmdKind, Rec};

pub struct PiClient {
    child: Option<Child>,
    stdin: Option<BufWriter<tokio::process::ChildStdin>>,
    pending: Arc<tokio::sync::Mutex<HashMap<String, CmdKind>>>,
}

impl PiClient {
    /// Spawn `pi --mode rpc` and start its reader tasks.
    pub async fn spawn(
        cfg: &Config,
        records: mpsc::Sender<(usize, Rec)>,
        idx: usize,
    ) -> Result<Self> {
        let mut cmd = Command::new(&cfg.pi_binary);
        cmd.arg("--mode").arg("rpc");
        if let Some(model) = &cfg.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(thinking) = &cfg.thinking {
            cmd.arg("--thinking").arg(thinking);
        }
        if let Some(prompt) = &cfg.system_prompt {
            cmd.arg("--append-system-prompt").arg(prompt);
        }
        if let Some(name) = &cfg.session_name {
            cmd.arg("--name").arg(name);
        }
        if let Some(dir) = &cfg.session_dir {
            cmd.arg("--session-dir").arg(dir);
        }
        if cfg.no_session {
            cmd.arg("--no-session");
        } else if let Some(id) = &cfg.session_id {
            cmd.arg("--session-id").arg(id);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&cfg.cwd)
            .spawn()
            .context(format!("failed to spawn {} (is it on PATH?)", cfg.pi_binary))?;
        let stdin = child.stdin.take().context("stdin pipe missing")?;
        let stdout = child.stdout.take().context("stdout pipe missing")?;
        let stderr = child.stderr.take().context("stderr pipe missing")?;
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        // stdout: JSONL → records channel (tagged); responses correlated by id
        let records_out = records.clone();
        let pending_out = pending.clone();
        tokio::spawn(async move {
                let reader = tokio::io::BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some(v) = serde_json::from_str::<Value>(line).ok() else {
                    continue;
                };
                let rec = if v.get("type").and_then(|t| t.as_str()) == Some("response") {
                    let id = v
                        .get("id")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                    let kind = match id {
                        Some(id) => {
                            let mut map = pending_out.lock().await;
                            map.remove(&id)
                        }
                        None => None,
                    };
                    match kind {
                        Some(kind) => Rec::Cmd { kind, resp: v },
                        None => Rec::Event(v), // response w/o known id → generic
                    }
                } else {
                    Rec::Event(v)
                };
                if records_out.send((idx, rec)).await.is_err() {
                    break;
                }
            }
        });

        // stderr: lines → records channel
        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if records.send((idx, Rec::Stderr(line))).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            child: Some(child),
            stdin: Some(BufWriter::new(stdin)),
            pending,
        })
    }

    /// Send a command. Returns false when the stdin write fails.
    pub async fn send(&mut self, kind: CmdKind, value: Value) -> bool {
        let id = uuid_v4();
        let mut v = value;
        v["id"] = json!(id);
        {
            let mut map = self.pending.lock().await;
            map.insert(id, kind);
        }
        if let Some(w) = self.stdin.as_mut() {
            match serde_json::to_string(&v) {
                Ok(mut bytes) => {
                    bytes.push('\n');
                    let wr = w.write_all(bytes.as_bytes()).await;
                    match wr {
                        Ok(()) => match w.flush().await {
                            Ok(()) => true,
                            Err(_e) => {
                                false
                            }
                        },
                        Err(_e) => {
                            false
                        }
                    }
                }
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Orderly shutdown: close stdin, then kill if needed.
    pub async fn shutdown(&mut self) {
        if let Some(w) = self.stdin.as_mut() {
            let _ = w.flush().await;
        }
        self.stdin = None;
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill().await;
            let _ = c.wait().await;
        }
    }
}

/// Tiny v4 UUID (no dependency needed).
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let mut filled = false;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut b).is_ok() {
            filled = true;
        }
    }
    if !filled {
        // fallback: mix of time + counter
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let c = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut mix = t ^ ((c as u128) << 64);
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((mix >> ((i % 8) * 8)) & 0xff) as u8 ^ (i as u8).wrapping_mul(31);
            mix = mix.wrapping_mul(6364136223847).wrapping_add(1442695040888963407);
        }
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}
