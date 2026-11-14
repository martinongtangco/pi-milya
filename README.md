# pi-milya

A **Grok-style chat TUI on the [pi](https://pi.dev) agent**. One Rust binary,
zero JS at runtime: pi runs as a child process in `--mode rpc` (JSONL over the
pipes) and pi-milya renders the conversation with ratatui.

*pilya* — a play on *pamilya* (family in Tagalog): your model, your rig, one
terminal.

## How it works

```
┌────────────┐  JSONL commands   ┌────────────────────┐
│  pi-milya  │ ────────────────▶ │  pi --mode rpc     │
│ (ratatui)  │ ◀──────────────── │  (agent + tools +  │
└────────────┘  events/responses │   session)         │
                                └────────────────────┘
```

- **Framing**: strict JSONL (LF-split only), responses correlated by `id`
- **Run state**: `prompt` → wait for `agent_settled` (not just `agent_end`)
- **Steering**: a prompt sent while the agent is running uses
  `streamingBehavior: "steer"`; Esc = `clear_queue` + `abort` (queued text is
  restored to the editor, per pi's docs)
- **Shutdown**: closing stdin = orderly pi shutdown

See [pi docs: rpc.md](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md),
`rpc-commands.md`, `json.md` for the full protocol.

## Run

```sh
cargo run --                     # in the cwd where the session should live
cargo run -- --no-session        # in-memory session
cargo run -- --model "qwen3.8-27b-a" --thinking max
cargo run -- --smoke             # non-TUI end-to-end check
```

All normal pi CLI options pass through: `--pi`, `--cwd`, `--model`,
`--thinking`, `--session-id`, `--session-dir`, `--name`, `--tools`.

## Keys

| key | action |
|---|---|
| `enter` | send (steers while the agent is running) |
| `shift+enter` / `ctrl+j` | newline |
| `esc` | abort run + clear queue (idle: clear input) |
| `up` / `down` | history |
| `tab` | complete slash commands |
| `t` | toggle the last thinking block |
| `?` | help |
| `ctrl+c` | quit |

Slash commands: `/model [id]` (no arg = picker), `/think [level]` (no arg =
cycle), `/clear` (new session), `/abort`, `/compact`, `/stats`, `/help`,
`/quit`. Unknown `/foo` goes to pi (extension/skill/template commands).

## Layout

- **Header**: model (from `get_state`), thinking level, session tokens + cost
- **Scrollback**: `you>` prompts, assistant text (live `text_delta` streaming,
  replaced by authoritative `text_end`/`message_end` content), collapsible
  thinking blocks, tool rows (`✓/✗/…`, name + args summary + tail of output)
- **Input**: multi-line editor with history
- **Status bar**: phase, steering/follow-up queue sizes, session id, pi stderr tail

## Code layout

| module | role |
|---|---|
| `src/main.rs` | CLI args, TUI event loop, `--smoke` |
| `src/config.rs` | flag parsing |
| `src/pi_client.rs` | spawn + JSONL framing + command/response correlation |
| `src/protocol.rs` | lenient record parsing (commands, events, usage) |
| `src/app.rs` | state machine, editor, slash commands, event handling |
| `src/ui.rs` | ratatui rendering |
