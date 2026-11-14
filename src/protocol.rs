//! pi RPC protocol records (docs/rpc.md, docs/json.md, docs/rpc-commands.md).
//! Lenient parsing: unknown shapes fall through to `Other`.

use serde_json::Value;

pub enum CmdKind {
    Prompt,
    Steer,
    GetState,
    GetModels,
    SetModel,
    CycleModel,
    SetThinking,
    CycleThinking,
    NewSession,
    Abort,
    Compact,
    Stats,
    ClearQueue,
}

pub enum Rec {
    /// A session event (or an uncorrelated response).
    Event(Value),
    /// A response to one of our commands.
    Cmd { kind: CmdKind, resp: Value },
    Stderr(String),
}

#[derive(Clone, Debug)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    pub cost: f64,
}

pub fn parse_usage(v: &Value) -> Option<Usage> {
    if v.is_null() {
        return None;
    }
    let get = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let cost = v
        .get("cost")
        .and_then(|c| c.get("total"))
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    Some(Usage {
        input: get("input"),
        output: get("output"),
        cache_read: get("cacheRead"),
        cache_write: get("cacheWrite"),
        total: get("totalTokens"),
        cost,
    })
}

/// Nested `assistantMessageEvent` of a `message_update` record.
#[derive(Clone, Debug)]
pub enum AmEvent {
    TextDelta { ci: u32, delta: String },
    TextEnd { ci: u32, content: String },
    ThinkingDelta { ci: u32, delta: String },
    ThinkingEnd { ci: u32, content: String },
    ToolCallStart { ci: u32, id: String, name: String },
    Error { msg: String },
    Other,
}

pub fn parse_am_event(v: &Value) -> AmEvent {
    let (ci, s) = (
        v.get("contentIndex").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        v.get("type").and_then(|x| x.as_str()).unwrap_or(""),
    );
    match s {
        "text_delta" => AmEvent::TextDelta {
            ci,
            delta: v.get("delta").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "text_end" => AmEvent::TextEnd {
            ci,
            content: v.get("content").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "thinking_delta" => AmEvent::ThinkingDelta {
            ci,
            delta: v.get("delta").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "thinking_end" => AmEvent::ThinkingEnd {
            ci,
            content: v.get("content").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "toolcall_start" => AmEvent::ToolCallStart {
            ci,
            id: v
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("toolName")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
        },
        "error" => AmEvent::Error {
            msg: v
                .get("error")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .or_else(|| v.get("reason").and_then(|x| x.as_str()).map(|s| s.to_string()))
                .unwrap_or_else(|| "stream error".into()),
        },
        _ => AmEvent::Other,
    }
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub is_error: bool,
    pub duration_ms: Option<u64>,
    pub output: String,
}

/// Flatten a tool `result` value ({content: [{type:"text", text}...], ...}) to text.
pub fn tool_output(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(items) = v.get("content").and_then(|c| c.as_array()) {
        for item in items {
            if let (Some("text"), Some(text)) = (
                item.get("type").and_then(|t| t.as_str()),
                item.get("text").and_then(|t| t.as_str()),
            ) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

/// A high-level session event.
#[derive(Clone, Debug)]
pub enum PiEvent {
    AgentStart,
    AgentSettled { aborted: bool },
    TurnEnd,
    MessageStart { role: String },
    MessageUpdate { usage: Option<Usage>, ame: AmEvent },
    MessageEnd,
    ToolStart { id: String, name: String, args: Value },
    ToolUpdate { id: String, partial: String },
    ToolEnd { id: String, result: ToolResult },
    QueueUpdate { steering: Vec<String>, follow_up: Vec<String> },
    SessionNameChanged { name: Option<String> },
    ThinkingLevelChanged { level: String },
    CompactionStart { reason: String },
    CompactionEnd { aborted: bool, error: Option<String>, tokens_after: Option<u64> },
    AutoRetryStart { attempt: u32, max_attempts: u32, error: Option<String> },
    AutoRetryEnd { success: bool, final_error: Option<String> },
    ExtensionError { error: String },
    Other(String),
}

pub fn parse_event(v: &Value) -> PiEvent {
    let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "agent_start" => PiEvent::AgentStart,
        "agent_settled" => PiEvent::AgentSettled {
            aborted: v.get("aborted").and_then(|x| x.as_bool()).unwrap_or(false),
        },
        "turn_end" => PiEvent::TurnEnd,
        "message_start" => PiEvent::MessageStart {
            role: v
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "message_update" => PiEvent::MessageUpdate {
            usage: parse_usage(v.get("usage").unwrap_or(&Value::Null)),
            ame: parse_am_event(v.get("assistantMessageEvent").unwrap_or(&Value::Null)),
        },
        "message_end" => PiEvent::MessageEnd,
        "tool_execution_start" => PiEvent::ToolStart {
            id: v
                .get("toolCallId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("toolName")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
            args: v.get("args").cloned().unwrap_or(Value::Null),
        },
        "tool_execution_update" => PiEvent::ToolUpdate {
            id: v
                .get("toolCallId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            partial: tool_output(v.get("partialResult").unwrap_or(&Value::Null)),
        },
        "tool_execution_end" => PiEvent::ToolEnd {
            id: v
                .get("toolCallId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            result: ToolResult {
                is_error: v.get("isError").and_then(|x| x.as_bool()).unwrap_or(false),
                duration_ms: v.get("durationMs").and_then(|x| x.as_u64()),
                output: tool_output(v.get("result").unwrap_or(&Value::Null)),
            },
        },
        "queue_update" => PiEvent::QueueUpdate {
            steering: v
                .get("steering")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            follow_up: v
                .get("followUp")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        },
        "session_info_changed" => PiEvent::SessionNameChanged {
            name: v.get("name").and_then(|x| x.as_str()).map(str::to_string),
        },
        "thinking_level_changed" => PiEvent::ThinkingLevelChanged {
            level: v
                .get("level")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
        },
        "compaction_start" => PiEvent::CompactionStart {
            reason: v
                .get("reason")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
        },
        "compaction_end" => PiEvent::CompactionEnd {
            aborted: v.get("aborted").and_then(|x| x.as_bool()).unwrap_or(false),
            error: v
                .get("errorMessage")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            tokens_after: v
                .get("result")
                .and_then(|r| r.get("estimatedTokensAfter"))
                .and_then(|x| x.as_u64()),
        },
        "auto_retry_start" => PiEvent::AutoRetryStart {
            attempt: v.get("attempt").and_then(|x| x.as_u64()).unwrap_or(1) as u32,
            max_attempts: v
                .get("maxAttempts")
                .and_then(|x| x.as_u64())
                .unwrap_or(1) as u32,
            error: v
                .get("errorMessage")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        },
        "auto_retry_end" => PiEvent::AutoRetryEnd {
            success: v.get("success").and_then(|x| x.as_bool()).unwrap_or(false),
            final_error: v
                .get("finalError")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        },
        "extension_error" => PiEvent::ExtensionError {
            error: v
                .get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("extension error")
                .to_string(),
        },
        other => PiEvent::Other(other.to_string()),
    }
}

/// Extract a compact summary of tool args (first string field, truncated).
pub fn args_summary(args: &Value) -> String {
    let s = match args {
        Value::Object(m) => {
            // common shapes: command (bash), path (read/write/edit), pattern (grep)
            for k in ["command", "path", "pattern", "query"] {
                if let Some(s) = m.get(k).and_then(|x| x.as_str()) {
                    return s.chars().take(80).collect();
                }
            }
            m.keys().next().map(|k| format!("{k}: …")).unwrap_or_default()
        }
        Value::Null => String::new(),
        other => other.to_string().chars().take(80).collect(),
    };
    s
}

/// Tolerant model list extraction from a get_available_models response.
pub fn extract_models(data: &Value) -> Vec<(String, String)> {
    let arr = data
        .get("models")
        .and_then(|x| x.as_array())
        .or_else(|| data.as_array())
        .cloned();
    match arr {
        Some(items) => items
            .iter()
            .filter_map(|m| {
                let id = m
                    .get("id")
                    .or_else(|| m.get("modelId"))
                    .and_then(|x| x.as_str())?;
                let label = m
                    .get("name")
                    .or_else(|| m.get("label"))
                    .and_then(|x| x.as_str())
                    .unwrap_or(id);
                Some((id.to_string(), label.to_string()))
            })
            .collect(),
        None => Vec::new(),
    }
}
