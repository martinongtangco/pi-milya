//! Pi RPC client: spawns `pi --mode rpc` and speaks JSONL over the pipes.
//! Framing rules per docs/rpc.md: split on LF only (never on U+2028/29),
//! correlate responses by `id`, close stdin for an orderly shutdown.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::protocol::{CmdKind, Rec};

pub struct PiClient {
    child: tokio::process::Child,
    cmd_tx: Option<mpsc::Sender<String>>,
    event_rx: mpsc::Receiver<Value>,
    stderr_rx: mpsc::Receiver<String>,
    pending: HashMap<String, CmdKind>,
    next_id: u64,
}

impl PiClient {
    pub async fn spawn(cfg: &Config) -> Result<Self> {
        let mut cmd = Command::new(&cfg.pi_binary);
        cmd.arg("--mode").arg("rpc");
        if cfg.no_session {
            cmd.arg("--no-session");
        }
        if let Some(m) = &cfg.model {
            cmd.arg("--model").arg(m);
        }
        if let Some(t) = &cfg.thinking {
            cmd.arg("--thinking").arg(t);
        }
        if let Some(s) = &cfg.session_id {
            cmd.arg("--session-id").arg(s);
        }
        if let Some(d) = &cfg.session_dir {
            cmd.arg("--session-dir").arg(d);
        }
        if let Some(n) = &cfg.session_name {
            cmd.arg("-n").arg(n);
        }
        if let Some(t) = &cfg.tools {
            cmd.arg("-t").arg(t);
        }
        cmd.current_dir(&cfg.cwd);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to spawn '{}' — is pi installed? ({e})", cfg.pi_binary))?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let stdin = child.stdin.take().expect("stdin piped");

        let (record_tx, event_rx) = mpsc::channel(1024);
        tokio::spawn(json_line_reader(BufReader::new(stdout), record_tx));

        let (stderr_tx, stderr_rx) = mpsc::channel(64);
        tokio::spawn(raw_line_reader(BufReader::new(stderr), stderr_tx));

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<String>(256);
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = cmd_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
            // channel dropped -> close stdin: orderly pi shutdown
            let _ = stdin.shutdown().await;
        });

        Ok(Self {
            child,
            cmd_tx: Some(cmd_tx),
            event_rx,
            stderr_rx,
            pending: HashMap::new(),
            next_id: 0,
        })
    }

    /// Queue a command. Its response arrives later as `Rec::Cmd`.
    pub async fn send(&mut self, kind: CmdKind, obj: Value) -> bool {
        match &self.cmd_tx {
            Some(tx) => {
                self.next_id += 1;
                let id = format!("milya-{}", self.next_id);
                let mut v = obj;
                v["id"] = json!(id);
                self.pending.insert(id, kind);
                tx.send(v.to_string()).await.is_ok()
            }
            None => false,
        }
    }

    /// Next stdout record, demultiplexed: responses are tagged with their
    /// `CmdKind`, everything else is a session event.
    pub async fn next_event(&mut self) -> Option<Rec> {
        loop {
            let rec = self.event_rx.recv().await?;
            if rec.get("type").and_then(|v| v.as_str()) == Some("response") {
                if let Some(id) = rec.get("id").and_then(|v| v.as_str()) {
                    if let Some(kind) = self.pending.remove(id) {
                        return Some(Rec::Cmd { kind, resp: rec });
                    }
                }
                // uncorrelated response (e.g. a parse error) -> treat as event
            }
            return Some(Rec::Event(rec));
        }
    }

    pub async fn next_stderr(&mut self) -> Option<String> {
        self.stderr_rx.recv().await
    }

    pub async fn wait_exit(&mut self) -> Option<i32> {
        self.child.wait().await.ok().map(|s| s.code().unwrap_or(-1))
    }

    /// Next protocol record of any kind (events, command responses, stderr).
    pub async fn next(&mut self) -> Option<Rec> {
        loop {
            tokio::select! {
                rec = self.event_rx.recv() => {
                    let rec = rec?;
                    if rec.get("type").and_then(|v| v.as_str()) == Some("response") {
                        if let Some(id) = rec.get("id").and_then(|v| v.as_str()) {
                            if let Some(kind) = self.pending.remove(id) {
                                return Some(Rec::Cmd { kind, resp: rec });
                            }
                        }
                        // uncorrelated response (e.g. a parse error) -> event
                        return Some(Rec::Event(rec));
                    }
                    return Some(Rec::Event(rec));
                }
                s = self.stderr_rx.recv() => return s.map(Rec::Stderr),
            }
        }
    }

    /// Close stdin (orderly shutdown) and wait for pi to exit.
    pub async fn shutdown(mut self) {
        self.cmd_tx.take();
        let _ = self.child.wait().await;
    }
}

/// Read pi's stdout as JSONL. Split on LF only; a preceding CR is stripped.
async fn json_line_reader(mut reader: BufReader<impl AsyncRead + Unpin>, tx: mpsc::Sender<Value>) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {
                let mut s = String::from_utf8_lossy(&buf).into_owned();
                if s.ends_with('\n') {
                    s.pop();
                }
                if s.ends_with('\r') {
                    s.pop();
                }
                if s.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&s) {
                    Ok(v) => {
                        if tx.send(v).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("[pi-milya] malformed record ({e}): {s}"),
                }
            }
            Err(e) => {
                eprintln!("[pi-milya] stdout read error: {e}");
                break;
            }
        }
    }
}

async fn raw_line_reader(mut reader: BufReader<impl AsyncRead + Unpin>, tx: mpsc::Sender<String>) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {
                let mut s = String::from_utf8_lossy(&buf).into_owned();
                if s.ends_with('\n') {
                    s.pop();
                }
                if s.ends_with('\r') {
                    s.pop();
                }
                if s.trim().is_empty() {
                    continue;
                }
                if tx.send(s).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}
