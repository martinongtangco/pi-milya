//! pi-milya — a Grok-style chat TUI on the pi agent (`pi --mode rpc`).

mod app;
mod config;
mod family;
mod pi_client;
mod protocol;
mod ui;

use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::json;
use tokio::sync::mpsc;

use app::App;
use config::parse_args;
use pi_client::PiClient;
use protocol::{AmEvent, CmdKind, PiEvent, Rec};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cfg = parse_args(&args)?;

    if cfg.smoke {
        return smoke(&cfg).await;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &cfg).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, cfg: &config::Config) -> Result<()> {
    let mut app = App::new(cfg.clone());
    app.start().await?;
    terminal.draw(|f| ui::draw(f, &app))?;

    let (key_tx, mut key_rx) = mpsc::channel(64);
    // crossterm's poll is blocking: hop it off the async executor.
    tokio::spawn(async move {
        loop {
            let polled =
                tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(250)))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or(false);
            if !polled {
                continue;
            }
            match event::read() {
                Ok(Event::Key(k)) => {
                    if key_tx.send(k).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    loop {
        let step = app.next(&mut key_rx).await;
        match step {
            app::Step::Key(k) => app.on_key(k),
            app::Step::Rec(idx, rec) => app.on_record(idx, rec),
            app::Step::Idle => {}
        }
        app.flush_action().await;
        if app.exiting {
            break;
        }
        terminal.draw(|f| ui::draw(f, &app))?;
    }

    app.shutdown_all().await;
    Ok(())
}

/// Non-TUI end-to-end check: spawn pi, get_state, prompt, stream, settle, exit.
async fn smoke(cfg: &config::Config) -> Result<()> {
    let (rec_tx, mut rec_rx) = mpsc::channel(256);
    let mut client = PiClient::spawn(cfg, rec_tx, 0).await?;
    eprintln!("smoke: pi spawned");
    client.send(CmdKind::GetState, json!({ "type": "get_state" })).await;

    let mut settled = false;

    for _ in 0..2400 {
        let rec = tokio::time::timeout(Duration::from_secs(60), rec_rx.recv())
            .await
            .map_err(|_| anyhow::anyhow!("smoke: timed out waiting for pi"))?;
        let rec = match rec {
            Some((_, r)) => r,
            None => anyhow::bail!("smoke: pi exited early"),
        };
        match rec {
            Rec::Cmd { kind: CmdKind::GetState, resp } => {
                let ok = resp.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                if !ok {
                    anyhow::bail!("smoke: get_state failed: {}", resp.get("error").and_then(|v| v.as_str()).unwrap_or("?"));
                }
                eprintln!("smoke: state ok (model: {})", resp["data"].get("model").and_then(|m| m.get("modelId")).and_then(|v| v.as_str()).unwrap_or("?"));
                // now send the prompt
                client
                    .send(
                        CmdKind::Prompt,
                        json!({ "type": "prompt", "message": "Reply with exactly the word: ok" }),
                    )
                    .await;
            }
            Rec::Cmd { .. } => {}
            Rec::Event(v) => {
                let e = protocol::parse_event(&v);
                if let PiEvent::MessageUpdate { ame, .. } = &e {
                    if let AmEvent::TextDelta { delta, .. } = ame {
                        print!("{delta}");
                        io::stdout().flush().ok();
                    }
                }
                if matches!(e, PiEvent::AgentSettled { .. }) {
                    settled = true;
                    break;
                }
            }
            Rec::Stderr(line) => eprintln!("pi: {line}"),
        }
    }
    if !settled {
        anyhow::bail!("smoke: agent did not settle in time");
    }
    client.shutdown().await;
    eprintln!("\nsmoke: OK");
    Ok(())
}
