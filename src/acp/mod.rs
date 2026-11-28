//! Official Agent Client Protocol (ACP v1) pure-protocol thin client proxy and headless runner.
//!
//! Enables autonomous in-place `/compact` (and compact-and-continue) on `agy acp`
//! sessions with zero local SQLite, Protobuf, or `relay_grants.json` coupling:
//! - `acp-proxy`: sits transparently over stdio JSON-RPC 2.0 between any ACP client
//!   (Zed, JetBrains, Xcode, Android Studio, VS Code ACP Client) and `agy acp`
//!   (including `--remote-agent` / SSH setups).
//!   Observes official ACP `session/update` notifications (`tool_call`, `tool_call_update`,
//!   `usage_update`). When `radar_commit_handoff(kind: "compact", auto_fire: true)`
//!   finishes with `status: "completed"`, queues the directed `/compact` prompt and injects
//!   a native ACP `session/prompt` at the clean `stopReason == "end_turn"` boundary on the
//!   exact same connection before resolving the client's original prompt response.
//!   When the MCP server's hard gates reject compaction, it returns `isError: true`, causing
//!   `agy acp` to emit `tool_call_update(status: "failed")` so the thin client never fires.
//! - `acp-run`: standalone headless ACP v1 client for CLI / CI automation with the same
//!   pure-protocol autonomous `/compact` loop and optional `--native-auto-compact`.

use std::collections::HashMap;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::i18n::{
    build_compact_command_with_note_for_lang, resolve_active_lang,
    sanitize_scratch_note_path_pure_for_lang, Lang,
};
use crate::settings::{
    expand_tilde, read_line_bounded, read_regular_file_bounded, resolve_agy_bin, BoundedLine,
    ACP_PROXY_ENV, AGY_AUTO_COMPACT_OVERRIDE_ENV, MAX_CONFIG_BYTES, MAX_LINE_BYTES,
    UNIX_O_NOFOLLOW, UNIX_O_NONBLOCK,
};

pub const DEFAULT_MAX_AUTO_COMPACTS: usize = 2;
pub const MAX_COMPACT_COMMAND_BYTES: usize = 4096;
const ACP_CHANNEL_BOUND: usize = 256;
const CLIENT_NAME: &str = "context-radar-mcp";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct AcpProxyConfig {
    pub agy_bin: PathBuf,
    pub agy_args: Vec<String>,
    pub max_auto_compacts: usize,
    pub auto_compact_threshold: u64,
    pub threshold_compact_prompt: String,
    pub native_auto_compact: Option<String>,
    pub quiet_notices: bool,
    pub log_file: Option<PathBuf>,
}

impl AcpProxyConfig {
    pub fn default_with_args(agy_args: Vec<String>) -> Self {
        Self {
            agy_bin: resolve_agy_bin(None),
            agy_args,
            max_auto_compacts: DEFAULT_MAX_AUTO_COMPACTS,
            auto_compact_threshold: 0,
            threshold_compact_prompt: "/compact".to_string(),
            native_auto_compact: None,
            quiet_notices: false,
            log_file: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcpRunConfig {
    pub proxy: AcpProxyConfig,
    pub cwd: PathBuf,
    pub prompt: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub auto_approve: bool,
    pub json_output: bool,
    pub extra_mcp_servers: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    ToAgent(Value),
    ToClient(Value),
}

#[derive(Debug, Clone, Default)]
struct TrackedToolCall {
    name: String,
    raw_input: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCompact {
    pub tool_call_id: String,
    pub command: String,
}

#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub session_id: String,
    pub cwd: String,
    pub last_used_tokens: u64,
    pub context_size: u64,
    pub total_auto_compacts: usize,
    pub pending_compact: Option<PendingCompact>,
    in_flight_tools: HashMap<String, TrackedToolCall>,
}

#[derive(Debug, Clone)]
enum PendingClientReq {
    SessionOpen {
        cwd: String,
        requested_session_id: Option<String>,
    },
    Prompt {
        session_id: String,
        user_initiated_compact: bool,
    },
}

#[derive(Debug, Clone)]
struct InternalCompactState {
    original_client_id: Value,
    session_id: String,
    compacts_in_turn: usize,
    cancelled_by_client: bool,
}

/// Deterministic, zero-IO, transport-independent ACP v1 pure-protocol proxy coordinator.
pub struct ProxyCoordinator {
    max_auto_compacts: usize,
    auto_compact_threshold: u64,
    threshold_compact_prompt: String,
    quiet_notices: bool,
    lang: Lang,
    next_internal_seq: u64,
    sessions: HashMap<String, SessionState>,
    pending_client: HashMap<String, PendingClientReq>,
    internal_by_id: HashMap<String, InternalCompactState>,
    internal_by_session: HashMap<String, String>,
}

fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::Number(n) => Some(format!("n:{n}")),
        Value::String(s) => Some(format!("s:{s}")),
        _ => None,
    }
}

/// Matches `agy 1.5.16` `/compact` detection: skips leading non-text content blocks
/// (attachments/files/images/resources) and empty text blocks, inspecting the first
/// non-empty text block for a leading `/compact` directive.
pub fn prompt_starts_with_compact(params: &Value) -> bool {
    let Some(blocks) = params.get("prompt").and_then(Value::as_array) else {
        return false;
    };
    for b in blocks {
        if b.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let txt = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
        if txt.is_empty() {
            continue;
        }
        let lower = txt.to_ascii_lowercase();
        return lower == "/compact"
            || lower.starts_with("/compact ")
            || lower.starts_with("/compact\t")
            || lower.starts_with("/compact\n");
    }
    false
}

const CANONICAL_RADAR_SERVERS: &[&str] = &[
    "context-radar-mcp",
    "context-radar",
    "context-radar-mcp_context-radar-mcp",
    "context-radar_context-radar",
    "context_radar_mcp",
    "context_radar",
];

fn is_canonical_server_prefix(server: &str) -> bool {
    let s = server.trim();
    let stripped = s
        .strip_prefix("mcp_")
        .or_else(|| s.strip_prefix("mcp:"))
        .or_else(|| s.strip_prefix("mcp/"))
        .or_else(|| s.strip_prefix("mcp."))
        .unwrap_or(s);
    CANONICAL_RADAR_SERVERS.contains(&stripped)
}

pub fn is_commit_handoff_tool_name(name: &str) -> bool {
    let trimmed = name.trim();
    for target in ["radar_commit_handoff", "context_radar_commit_handoff"] {
        if trimmed == target {
            return true;
        }
        if let Some(prefix) = trimmed.strip_suffix(target) {
            if let Some(server_part) = prefix.strip_suffix(['_', ':', '/', '.']) {
                if is_canonical_server_prefix(server_part) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_call_mcp_tool_wrapper(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed == "call_mcp_tool"
        || trimmed.ends_with("_call_mcp_tool")
        || trimmed.ends_with(":call_mcp_tool")
        || trimmed.ends_with("/call_mcp_tool")
}

fn value_as_json_object(val: &Value) -> Option<Map<String, Value>> {
    match val {
        Value::Object(map) => Some(map.clone()),
        Value::String(s) => serde_json::from_str::<Value>(s.trim())
            .ok()
            .and_then(|v| v.as_object().cloned()),
        _ => None,
    }
}

fn extract_tool_input_object(upd: &Value) -> Option<Map<String, Value>> {
    for key in ["rawInput", "args", "arguments"] {
        if let Some(v) = upd.get(key) {
            if let Some(obj) = value_as_json_object(v) {
                return Some(obj);
            }
        }
    }
    None
}

fn extract_commit_handoff_args(name: &str, input: &Value) -> Option<Map<String, Value>> {
    if is_commit_handoff_tool_name(name) {
        return value_as_json_object(input);
    }
    if is_call_mcp_tool_wrapper(name) {
        let wrapper = value_as_json_object(input)?;
        let target_tool = wrapper
            .get("ToolName")
            .or_else(|| wrapper.get("tool_name"))
            .or_else(|| wrapper.get("toolName"))
            .and_then(Value::as_str)?;
        if !is_commit_handoff_tool_name(target_tool) {
            return None;
        }
        let server_name = wrapper
            .get("ServerName")
            .or_else(|| wrapper.get("server_name"))
            .or_else(|| wrapper.get("serverName"))
            .and_then(Value::as_str);
        match server_name {
            Some(srv) if !is_canonical_server_prefix(srv) => return None,
            None if target_tool.trim() == "radar_commit_handoff" => return None,
            _ => {}
        }
        let inner = wrapper
            .get("Arguments")
            .or_else(|| wrapper.get("arguments"))
            .or_else(|| wrapper.get("args"))?;
        return value_as_json_object(inner);
    }
    None
}

fn inspect_output_obj_blocked(obj: &Map<String, Value>) -> bool {
    if obj.get("isError").and_then(Value::as_bool) == Some(true)
        || obj.get("blocked").and_then(Value::as_bool) == Some(true)
        || obj.get("mode").and_then(Value::as_str) == Some("handoff_blocked")
    {
        return true;
    }
    if let Some(sc) = obj.get("structuredContent").and_then(Value::as_object) {
        if sc.get("blocked").and_then(Value::as_bool) == Some(true)
            || sc.get("mode").and_then(Value::as_str) == Some("handoff_blocked")
        {
            return true;
        }
    }
    false
}

fn raw_output_is_blocked(upd: &Value) -> bool {
    if let Some(raw_out) = upd.get("rawOutput") {
        if let Some(obj) = value_as_json_object(raw_out) {
            if inspect_output_obj_blocked(&obj) {
                return true;
            }
        }
    }
    false
}

fn sanitize_single_line_compact_command(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_COMPACT_COMMAND_BYTES {
        return None;
    }
    // Disallow multi-line prompt injection after `/compact`.
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return None;
    }
    let mut clean = String::with_capacity(trimmed.len());
    let mut prev_ws = false;
    for ch in trimmed.chars() {
        if ch == ' ' || ch == '\t' {
            if !clean.is_empty() && !prev_ws {
                clean.push(' ');
                prev_ws = true;
            }
        } else if !ch.is_control() {
            clean.push(ch);
            prev_ws = false;
        }
    }
    let clean = clean.trim();
    if clean.is_empty() || clean.len() > MAX_COMPACT_COMMAND_BYTES {
        return None;
    }
    let lower = clean.to_ascii_lowercase();
    if lower == "/compact" || lower.starts_with("/compact ") {
        Some(clean.to_string())
    } else {
        None
    }
}

pub fn extract_compact_command_from_output(upd: &Value) -> Option<String> {
    let raw_out = upd.get("rawOutput")?;
    let obj = value_as_json_object(raw_out)?;
    let cmd = obj
        .get("structuredContent")
        .and_then(Value::as_object)
        .and_then(|sc| sc.get("compact_command"))
        .or_else(|| obj.get("compact_command"))
        .and_then(Value::as_str)?;
    sanitize_single_line_compact_command(cmd)
}

impl ProxyCoordinator {
    pub fn new(
        max_auto_compacts: usize,
        auto_compact_threshold: u64,
        threshold_compact_prompt: String,
        quiet_notices: bool,
    ) -> Self {
        Self {
            max_auto_compacts,
            auto_compact_threshold,
            threshold_compact_prompt: if threshold_compact_prompt.trim().is_empty() {
                "/compact".to_string()
            } else {
                threshold_compact_prompt
            },
            quiet_notices,
            lang: resolve_active_lang(""),
            next_internal_seq: 1,
            sessions: HashMap::new(),
            pending_client: HashMap::new(),
            internal_by_id: HashMap::new(),
            internal_by_session: HashMap::new(),
        }
    }

    pub fn with_lang(mut self, lang: Lang) -> Self {
        self.lang = lang;
        self
    }

    pub fn session_state(&self, session_id: &str) -> Option<&SessionState> {
        self.sessions.get(session_id)
    }

    pub fn total_auto_compacts(&self) -> usize {
        self.sessions.values().map(|s| s.total_auto_compacts).sum()
    }

    pub fn on_client_message(&mut self, msg: Value) -> Vec<Outbound> {
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        if let Some(key) = msg.get("id").and_then(id_key) {
            match method.as_str() {
                "session/new" | "session/load" | "session/resume" => {
                    let cwd = params
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let requested_session_id = params
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    self.pending_client.insert(
                        key,
                        PendingClientReq::SessionOpen {
                            cwd,
                            requested_session_id,
                        },
                    );
                }
                "session/prompt" => {
                    let session_id = params
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if let Some(st) = self.sessions.get_mut(&session_id) {
                        st.pending_compact = None;
                        st.in_flight_tools.clear();
                    }
                    let user_initiated_compact = prompt_starts_with_compact(&params);
                    self.pending_client.insert(
                        key,
                        PendingClientReq::Prompt {
                            session_id,
                            user_initiated_compact,
                        },
                    );
                }
                "session/close" => {
                    if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
                        self.sessions.remove(sid);
                        if let Some(ikey) = self.internal_by_session.remove(sid) {
                            self.internal_by_id.remove(&ikey);
                        }
                    }
                }
                _ => {}
            }
        } else if method == "session/cancel" {
            if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
                if let Some(internal_key) = self.internal_by_session.get(sid).cloned() {
                    if let Some(st) = self.internal_by_id.get_mut(&internal_key) {
                        st.cancelled_by_client = true;
                    }
                }
            }
        }

        vec![Outbound::ToAgent(msg)]
    }

    fn handle_tool_call_update(&mut self, sid: &str, upd: &Value) {
        if sid.is_empty() {
            return;
        }
        let tc_id = upd
            .get("toolCallId")
            .or_else(|| upd.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if tc_id.is_empty() {
            return;
        }
        let explicit_name = upd
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let fallback_title = upd
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let incoming_input = extract_tool_input_object(upd);
        let status = upd
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();

        let st = self
            .sessions
            .entry(sid.to_string())
            .or_insert_with(|| SessionState {
                session_id: sid.to_string(),
                ..SessionState::default()
            });

        {
            let tracked = st.in_flight_tools.entry(tc_id.to_string()).or_default();
            if let Some(n) = explicit_name {
                tracked.name = n.to_string();
            } else if tracked.name.is_empty() {
                if let Some(t) = fallback_title {
                    tracked.name = t.to_string();
                }
            }
            if let Some(new_map) = incoming_input {
                match tracked.raw_input {
                    Some(Value::Object(ref mut existing)) => {
                        for (k, v) in new_map {
                            existing.insert(k, v);
                        }
                    }
                    _ => {
                        tracked.raw_input = Some(Value::Object(new_map));
                    }
                }
            }
        }

        match status {
            "failed" | "cancelled" | "error" => {
                st.in_flight_tools.remove(tc_id);
                if st.pending_compact.as_ref().map(|p| p.tool_call_id.as_str()) == Some(tc_id) {
                    st.pending_compact = None;
                }
            }
            "completed" => {
                let tracked = st.in_flight_tools.remove(tc_id).unwrap_or_default();
                if raw_output_is_blocked(upd) {
                    return;
                }
                let Some(args) = extract_commit_handoff_args(
                    &tracked.name,
                    tracked.raw_input.as_ref().unwrap_or(&Value::Null),
                ) else {
                    return;
                };
                let kind = args
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("handoff")
                    .trim();
                if kind != "compact" {
                    return;
                }
                let auto_fire = args
                    .get("auto_fire")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if !auto_fire {
                    return;
                }
                let preserve_points = args
                    .get("preserve_points")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if preserve_points.is_empty() {
                    return;
                }
                let command = extract_compact_command_from_output(upd).unwrap_or_else(|| {
                    let resume_task = args
                        .get("resume_task")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let raw_note = args
                        .get("scratch_note_path")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let note = sanitize_scratch_note_path_pure_for_lang(raw_note, self.lang)
                        .unwrap_or_default();
                    let built = build_compact_command_with_note_for_lang(
                        preserve_points,
                        resume_task,
                        &note,
                        self.lang,
                    );
                    let capped = if built.len() <= MAX_COMPACT_COMMAND_BYTES {
                        built
                    } else {
                        let mut end = MAX_COMPACT_COMMAND_BYTES;
                        while end > 0 && !built.is_char_boundary(end) {
                            end -= 1;
                        }
                        built[..end].to_string()
                    };
                    sanitize_single_line_compact_command(&capped).unwrap_or_default()
                });
                if !command.trim().is_empty() {
                    st.pending_compact = Some(PendingCompact {
                        tool_call_id: tc_id.to_string(),
                        command,
                    });
                }
            }
            _ => {}
        }
    }

    pub fn on_agent_message(&mut self, msg: Value) -> Vec<Outbound> {
        // 1. Handle session/update notifications (track live usage and tool calls).
        if msg.get("id").is_none() {
            if msg.get("method").and_then(Value::as_str) == Some("session/update") {
                if let Some(params) = msg.get("params") {
                    let sid = params
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if let Some(upd) = params.get("update") {
                        match upd.get("sessionUpdate").and_then(Value::as_str) {
                            Some("usage_update") => {
                                let used = upd.get("used").and_then(Value::as_u64).unwrap_or(0);
                                let size = upd.get("size").and_then(Value::as_u64).unwrap_or(0);
                                if !sid.is_empty() {
                                    let st =
                                        self.sessions.entry(sid.to_string()).or_insert_with(|| {
                                            SessionState {
                                                session_id: sid.to_string(),
                                                ..SessionState::default()
                                            }
                                        });
                                    st.last_used_tokens = used;
                                    if size > 0 {
                                        st.context_size = size;
                                    }
                                }
                            }
                            Some("tool_call") | Some("tool_call_update") => {
                                self.handle_tool_call_update(sid, upd);
                            }
                            _ => {}
                        }
                    }
                }
            }
            return vec![Outbound::ToClient(msg)];
        }

        // 2. Server-to-client request (has both `id` and `method`, e.g. session/request_permission).
        if msg.get("method").is_some() {
            return vec![Outbound::ToClient(msg)];
        }

        // 3. Response from Agent (`id` present, `result` or `error` present).
        let Some(key) = msg.get("id").and_then(id_key) else {
            return vec![Outbound::ToClient(msg)];
        };

        // 3a. Response to one of our internally injected `/compact` prompts.
        if let Some(internal) = self.internal_by_id.remove(&key) {
            self.internal_by_session.remove(&internal.session_id);
            if let Some(st) = self.sessions.get_mut(&internal.session_id) {
                st.in_flight_tools.clear();
            }
            let is_err = msg.get("error").is_some();
            let stop_reason = msg
                .get("result")
                .and_then(|r| r.get("stopReason"))
                .and_then(Value::as_str)
                .unwrap_or("");

            // Allow chaining another agent-issued compact only on clean end_turn within cap.
            if !is_err
                && stop_reason == "end_turn"
                && !internal.cancelled_by_client
                && internal.compacts_in_turn < self.max_auto_compacts
            {
                if let Some(outbound) = self.try_inject_compact(
                    &internal.session_id,
                    internal.original_client_id.clone(),
                    internal.compacts_in_turn,
                    false,
                ) {
                    return outbound;
                }
            } else if let Some(st) = self.sessions.get_mut(&internal.session_id) {
                st.pending_compact = None;
            }

            let mut rewritten = msg;
            if let Some(obj) = rewritten.as_object_mut() {
                obj.insert("id".to_string(), internal.original_client_id);
            }
            return vec![Outbound::ToClient(rewritten)];
        }

        // 3b. Response to a tracked client request.
        if let Some(req) = self.pending_client.remove(&key) {
            match req {
                PendingClientReq::SessionOpen {
                    cwd,
                    requested_session_id,
                } => {
                    let sid = msg
                        .get("result")
                        .and_then(|res| res.get("sessionId"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or(requested_session_id)
                        .unwrap_or_default();
                    if !sid.is_empty() {
                        if msg.get("result").is_some() {
                            let st =
                                self.sessions
                                    .entry(sid.clone())
                                    .or_insert_with(|| SessionState {
                                        session_id: sid.clone(),
                                        ..SessionState::default()
                                    });
                            if st.session_id.is_empty() {
                                st.session_id = sid;
                            }
                            if !cwd.is_empty() {
                                st.cwd = cwd;
                            }
                            // Drop any historical tool_call updates replayed during session/load or session/resume.
                            st.pending_compact = None;
                            st.in_flight_tools.clear();
                        } else if let Some(st) = self.sessions.get_mut(&sid) {
                            st.pending_compact = None;
                            st.in_flight_tools.clear();
                        }
                    }
                    return vec![Outbound::ToClient(msg)];
                }
                PendingClientReq::Prompt {
                    session_id,
                    user_initiated_compact,
                } => {
                    if let Some(st) = self.sessions.get_mut(&session_id) {
                        st.in_flight_tools.clear();
                    }
                    let is_err = msg.get("error").is_some();
                    let stop_reason = msg
                        .get("result")
                        .and_then(|r| r.get("stopReason"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !is_err
                        && stop_reason == "end_turn"
                        && !user_initiated_compact
                        && self.max_auto_compacts > 0
                        && !session_id.is_empty()
                    {
                        let orig_id = msg.get("id").cloned().unwrap_or(Value::Null);
                        if let Some(outbound) =
                            self.try_inject_compact(&session_id, orig_id, 0, true)
                        {
                            return outbound;
                        }
                    } else if let Some(st) = self.sessions.get_mut(&session_id) {
                        st.pending_compact = None;
                    }
                    return vec![Outbound::ToClient(msg)];
                }
            }
        }

        vec![Outbound::ToClient(msg)]
    }

    fn try_inject_compact(
        &mut self,
        session_id: &str,
        original_client_id: Value,
        compacts_so_far: usize,
        allow_threshold_fallback: bool,
    ) -> Option<Vec<Outbound>> {
        // Priority 1: Protocol-observed `context_radar_commit_handoff(kind="compact")` completed in turn.
        let mut chosen = self
            .sessions
            .get_mut(session_id)
            .and_then(|st| st.pending_compact.take())
            .map(|p| p.command.trim().to_string())
            .filter(|c| !c.is_empty());

        // Priority 2: Opt-in token threshold safety net using official ACP `usage_update`.
        if chosen.is_none()
            && allow_threshold_fallback
            && self.auto_compact_threshold > 0
            && compacts_so_far == 0
        {
            if let Some(st) = self.sessions.get(session_id) {
                if st.last_used_tokens >= self.auto_compact_threshold {
                    chosen = Some(self.threshold_compact_prompt.clone());
                }
            }
        }

        let compact_cmd = chosen?;
        let seq = self.next_internal_seq;
        self.next_internal_seq += 1;
        let internal_id = format!("__cr_acp_compact_{seq}");
        let internal_id_val = Value::String(internal_id);
        let internal_key = id_key(&internal_id_val)?;

        self.internal_by_session
            .insert(session_id.to_string(), internal_key.clone());
        self.internal_by_id.insert(
            internal_key,
            InternalCompactState {
                original_client_id,
                session_id: session_id.to_string(),
                compacts_in_turn: compacts_so_far + 1,
                cancelled_by_client: false,
            },
        );
        let st = self
            .sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionState {
                session_id: session_id.to_string(),
                ..SessionState::default()
            });
        st.total_auto_compacts += 1;

        let mut out = Vec::new();
        if !self.quiet_notices {
            out.push(Outbound::ToClient(json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {
                            "type": "text",
                            "text": "\n\n[Context Radar] Executing native ACP /compact checkpoint...\n\n"
                        }
                    }
                }
            })));
        }
        out.push(Outbound::ToAgent(json!({
            "jsonrpc": "2.0",
            "id": internal_id_val,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": compact_cmd }]
            }
        })));
        Some(out)
    }
}

fn spawn_agy_acp(cfg: &AcpProxyConfig, cwd: Option<&Path>) -> io::Result<Child> {
    let mut cmd = Command::new(&cfg.agy_bin);
    cmd.arg("acp");
    for a in &cfg.agy_args {
        cmd.arg(a);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.env(ACP_PROXY_ENV, "1");
    if let Some(ref ov) = cfg.native_auto_compact {
        let trimmed = ov.trim();
        if !trimmed.is_empty() {
            cmd.env(AGY_AUTO_COMPACT_OVERRIDE_ENV, trimmed);
        }
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn()
}

fn write_json_line<W: Write>(writer: &mut W, msg: &Value) -> io::Result<()> {
    let line = serde_json::to_string(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn append_proxy_log_line(path: &Path, line: &str) -> io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
            .custom_flags(UNIX_O_NOFOLLOW | UNIX_O_NONBLOCK);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = f.metadata()?;
        if !meta.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "proxy log path is not a regular file",
            ));
        }
        if (meta.permissions().mode() & 0o777) != 0o600 {
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    writeln!(f, "{line}")
}

fn append_log(log_file: &Option<PathBuf>, line: &str) {
    let Some(path) = log_file else { return };
    let _ = append_proxy_log_line(path, line);
}

enum ProxyEvent {
    ClientMsg(Value),
    ClientEof,
    AgentMsg(Value),
    AgentEof,
}

fn spawn_stderr_forwarder(mut reader: std::process::ChildStderr) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut err_out = io::stderr().lock();
            let _ = err_out.write_all(&buf[..n]);
            let _ = err_out.flush();
        }
    })
}

/// Run transparent official ACP v1 pure-protocol proxy over stdio.
pub fn run_acp_proxy(cfg: AcpProxyConfig) -> io::Result<i32> {
    let mut child = spawn_agy_acp(&cfg, None)?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("failed to open agy acp stdin"))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("failed to open agy acp stdout"))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("failed to open agy acp stderr"))?;

    // Pump child stderr directly to proxy stderr using fixed-size chunks without holding stderr lock across reads.
    let stderr_handle = spawn_stderr_forwarder(child_stderr);

    let (tx, rx) = mpsc::sync_channel::<ProxyEvent>(ACP_CHANNEL_BOUND);
    let tx_client = tx.clone();
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let mut raw = Vec::with_capacity(4096);
        loop {
            match read_line_bounded(&mut reader, &mut raw, MAX_LINE_BYTES) {
                Ok(BoundedLine::Eof) | Err(_) => break,
                Ok(BoundedLine::Oversized(_)) => {
                    eprintln!(
                        "[{CLIENT_NAME} acp] dropped oversized client JSON-RPC line (>16 MiB)"
                    );
                    raw.clear();
                    raw.shrink_to(4096);
                }
                Ok(BoundedLine::Line(_)) => {
                    let Ok(s) = std::str::from_utf8(&raw) else {
                        continue;
                    };
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                        if tx_client.send(ProxyEvent::ClientMsg(val)).is_err() {
                            break;
                        }
                    }
                }
            }
        }
        let _ = tx_client.send(ProxyEvent::ClientEof);
    });

    let tx_agent = tx;
    thread::spawn(move || {
        let mut reader = BufReader::new(child_stdout);
        let mut raw = Vec::with_capacity(4096);
        loop {
            match read_line_bounded(&mut reader, &mut raw, MAX_LINE_BYTES) {
                Ok(BoundedLine::Eof) | Err(_) => break,
                Ok(BoundedLine::Oversized(_)) => {
                    eprintln!(
                        "[{CLIENT_NAME} acp] dropped oversized agent JSON-RPC line (>16 MiB)"
                    );
                    raw.clear();
                    raw.shrink_to(4096);
                }
                Ok(BoundedLine::Line(_)) => {
                    let Ok(s) = std::str::from_utf8(&raw) else {
                        continue;
                    };
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                        if tx_agent.send(ProxyEvent::AgentMsg(val)).is_err() {
                            break;
                        }
                    }
                }
            }
        }
        let _ = tx_agent.send(ProxyEvent::AgentEof);
    });

    let mut coord = ProxyCoordinator::new(
        cfg.max_auto_compacts,
        cfg.auto_compact_threshold,
        cfg.threshold_compact_prompt.clone(),
        cfg.quiet_notices,
    );
    let stdout = io::stdout();
    let mut client_out = stdout.lock();
    let mut child_stdin_open = true;

    for ev in rx {
        match ev {
            ProxyEvent::ClientMsg(msg) => {
                append_log(&cfg.log_file, &format!("C->A {msg}"));
                let actions = coord.on_client_message(msg);
                for act in actions {
                    match act {
                        Outbound::ToAgent(v) => {
                            if child_stdin_open && write_json_line(&mut child_stdin, &v).is_err() {
                                child_stdin_open = false;
                            }
                        }
                        Outbound::ToClient(v) => {
                            let _ = write_json_line(&mut client_out, &v);
                        }
                    }
                }
            }
            ProxyEvent::AgentMsg(msg) => {
                append_log(&cfg.log_file, &format!("A->C {msg}"));
                let actions = coord.on_agent_message(msg);
                for act in actions {
                    match act {
                        Outbound::ToAgent(v) => {
                            append_log(&cfg.log_file, &format!("PROXY->A {v}"));
                            if child_stdin_open && write_json_line(&mut child_stdin, &v).is_err() {
                                child_stdin_open = false;
                            }
                        }
                        Outbound::ToClient(v) => {
                            let _ = write_json_line(&mut client_out, &v);
                        }
                    }
                }
            }
            ProxyEvent::ClientEof => {
                if child_stdin_open {
                    drop(child_stdin);
                }
                break;
            }
            ProxyEvent::AgentEof => {
                break;
            }
        }
    }

    let status = child.wait()?;
    let _ = stderr_handle.join();
    Ok(status.code().unwrap_or(0))
}

#[derive(Debug, Clone, Serialize)]
pub struct AcpRunReport {
    pub session_id: String,
    pub stop_reason: String,
    pub response: String,
    pub auto_compacts: usize,
    pub final_used_tokens: u64,
    pub context_size: u64,
    pub usage: Value,
}

fn pick_permission_option_id(params: &Value, allow: bool) -> Option<String> {
    let options = params.get("options")?.as_array()?;
    let preferred_kinds: &[&str] = if allow {
        &["allow_once", "allow_always", "allow"]
    } else {
        &["reject_once", "reject_always", "deny", "reject"]
    };
    for kind in preferred_kinds {
        for opt in options {
            if opt.get("kind").and_then(Value::as_str) == Some(*kind) {
                if let Some(oid) = opt.get("optionId").and_then(Value::as_str) {
                    return Some(oid.to_string());
                }
            }
        }
    }
    None
}

/// Standalone headless official ACP v1 pure-protocol runner with autonomous `/compact`.
pub fn run_acp_headless(mut cfg: AcpRunConfig) -> io::Result<AcpRunReport> {
    if cfg.auto_approve
        && !cfg
            .proxy
            .agy_args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions")
    {
        cfg.proxy
            .agy_args
            .push("--dangerously-skip-permissions".to_string());
    }
    let cwd_abs = cfg.cwd.canonicalize().unwrap_or_else(|_| cfg.cwd.clone());
    let mut child = spawn_agy_acp(&cfg.proxy, Some(&cwd_abs))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("failed to open agy acp stdin"))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("failed to open agy acp stdout"))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("failed to open agy acp stderr"))?;

    let stderr_handle = spawn_stderr_forwarder(child_stderr);

    let mut reader = BufReader::new(child_stdout);
    let mut next_id: u64 = 1;
    let mut alloc_id = || {
        let cur = next_id;
        next_id += 1;
        cur
    };

    let mut coord = ProxyCoordinator::new(
        cfg.proxy.max_auto_compacts,
        cfg.proxy.auto_compact_threshold,
        cfg.proxy.threshold_compact_prompt.clone(),
        true,
    );

    let mut wait_for_id = |target_id: u64,
                           stdin_w: &mut std::process::ChildStdin,
                           coord: &mut ProxyCoordinator,
                           response_text: &mut String,
                           stream_text: bool|
     -> io::Result<Value> {
        let target_key = id_key(&json!(target_id)).unwrap();
        let mut raw_line = Vec::with_capacity(4096);
        loop {
            match read_line_bounded(&mut reader, &mut raw_line, MAX_LINE_BYTES)? {
                BoundedLine::Eof => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "agy acp closed stdout before replying",
                    ));
                }
                BoundedLine::Oversized(_) => {
                    eprintln!(
                        "[{CLIENT_NAME} acp] dropped oversized agent JSON-RPC line (>16 MiB)"
                    );
                    raw_line.clear();
                    raw_line.shrink_to(4096);
                    continue;
                }
                BoundedLine::Line(_) => {}
            }
            let Ok(line) = std::str::from_utf8(&raw_line) else {
                continue;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            append_log(&cfg.proxy.log_file, &format!("A->R {msg}"));
            let actions = coord.on_agent_message(msg);
            for act in actions {
                match act {
                    Outbound::ToAgent(v) => {
                        append_log(&cfg.proxy.log_file, &format!("R->A {v}"));
                        eprintln!("[{CLIENT_NAME} acp] autonomous /compact dispatched");
                        write_json_line(stdin_w, &v)?;
                    }
                    Outbound::ToClient(v) => {
                        // Server-to-client permission request
                        if v.get("method").and_then(Value::as_str)
                            == Some("session/request_permission")
                        {
                            if let Some(req_id) = v.get("id").cloned() {
                                let opt_id = pick_permission_option_id(
                                    v.get("params").unwrap_or(&Value::Null),
                                    cfg.auto_approve,
                                );
                                let reply = if let Some(oid) = opt_id {
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": req_id,
                                        "result": {
                                            "outcome": { "outcome": "selected", "optionId": oid }
                                        }
                                    })
                                } else {
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": req_id,
                                        "result": { "outcome": { "outcome": "cancelled" } }
                                    })
                                };
                                write_json_line(stdin_w, &reply)?;
                            }
                            continue;
                        }
                        if v.get("method").and_then(Value::as_str) == Some("session/update") {
                            if let Some(upd) = v.get("params").and_then(|p| p.get("update")) {
                                if upd.get("sessionUpdate").and_then(Value::as_str)
                                    == Some("agent_message_chunk")
                                {
                                    if let Some(txt) = upd
                                        .get("content")
                                        .and_then(|c| c.get("text"))
                                        .and_then(Value::as_str)
                                    {
                                        response_text.push_str(txt);
                                        if stream_text {
                                            print!("{txt}");
                                            let _ = io::stdout().flush();
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        if v.get("id").and_then(id_key).as_deref() == Some(target_key.as_str()) {
                            return Ok(v);
                        }
                    }
                }
            }
        }
    };

    let mut scratch = String::new();

    // 1. initialize
    let init_id = alloc_id();
    let init_req = json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION }
        }
    });
    for a in coord.on_client_message(init_req) {
        if let Outbound::ToAgent(v) = a {
            write_json_line(&mut child_stdin, &v)?;
        }
    }
    let init_res = wait_for_id(init_id, &mut child_stdin, &mut coord, &mut scratch, false)?;
    if let Some(err) = init_res.get("error") {
        return Err(io::Error::other(format!("ACP initialize failed: {err}")));
    }

    // 2. session/new or session/resume
    let sess_req_id = alloc_id();
    let sess_req = if let Some(ref existing_sid) = cfg.session_id {
        json!({
            "jsonrpc": "2.0",
            "id": sess_req_id,
            "method": "session/resume",
            "params": {
                "sessionId": existing_sid,
                "cwd": cwd_abs.to_string_lossy(),
                "mcpServers": cfg.extra_mcp_servers
            }
        })
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": sess_req_id,
            "method": "session/new",
            "params": {
                "cwd": cwd_abs.to_string_lossy(),
                "mcpServers": cfg.extra_mcp_servers
            }
        })
    };
    for a in coord.on_client_message(sess_req) {
        if let Outbound::ToAgent(v) = a {
            write_json_line(&mut child_stdin, &v)?;
        }
    }
    let sess_res = wait_for_id(
        sess_req_id,
        &mut child_stdin,
        &mut coord,
        &mut scratch,
        false,
    )?;
    if let Some(err) = sess_res.get("error") {
        return Err(io::Error::other(format!("ACP session open failed: {err}")));
    }
    let session_id = sess_res
        .get("result")
        .and_then(|r| r.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| cfg.session_id.clone())
        .ok_or_else(|| io::Error::other("ACP server did not return sessionId"))?;

    // Optional model / mode selection via official ACP methods.
    if let Some(ref model_id) = cfg.model {
        let mid = alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": mid,
            "method": "session/set_model",
            "params": { "sessionId": session_id, "modelId": model_id }
        });
        for a in coord.on_client_message(req) {
            if let Outbound::ToAgent(v) = a {
                write_json_line(&mut child_stdin, &v)?;
            }
        }
        let _ = wait_for_id(mid, &mut child_stdin, &mut coord, &mut scratch, false);
    }
    if let Some(ref mode_id) = cfg.mode {
        let mid = alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": mid,
            "method": "session/set_mode",
            "params": { "sessionId": session_id, "modeId": mode_id }
        });
        for a in coord.on_client_message(req) {
            if let Outbound::ToAgent(v) = a {
                write_json_line(&mut child_stdin, &v)?;
            }
        }
        let _ = wait_for_id(mid, &mut child_stdin, &mut coord, &mut scratch, false);
    }

    // 3. session/prompt (ProxyCoordinator automatically intercepts and runs /compact if triggered!)
    let mut response_text = String::new();
    let prompt_id = alloc_id();
    let prompt_req = json!({
        "jsonrpc": "2.0",
        "id": prompt_id,
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": cfg.prompt }]
        }
    });
    for a in coord.on_client_message(prompt_req) {
        if let Outbound::ToAgent(v) = a {
            write_json_line(&mut child_stdin, &v)?;
        }
    }
    let prompt_res = wait_for_id(
        prompt_id,
        &mut child_stdin,
        &mut coord,
        &mut response_text,
        !cfg.json_output,
    )?;
    if !cfg.json_output && !response_text.ends_with('\n') && !response_text.is_empty() {
        println!();
    }

    // Graceful shutdown so agy acp flushes session state cleanly.
    drop(child_stdin);
    let _ = child.wait();
    let _ = stderr_handle.join();

    if let Some(err) = prompt_res.get("error") {
        return Err(io::Error::other(format!(
            "ACP session/prompt failed: {err}"
        )));
    }
    let result = prompt_res.get("result").cloned().unwrap_or(Value::Null);
    let stop_reason = result
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let usage = result.get("usage").cloned().unwrap_or(Value::Null);
    let st = coord.session_state(&session_id);
    Ok(AcpRunReport {
        session_id,
        stop_reason,
        response: response_text,
        auto_compacts: st.map(|s| s.total_auto_compacts).unwrap_or(0),
        final_used_tokens: st.map(|s| s.last_used_tokens).unwrap_or(0),
        context_size: st.map(|s| s.context_size).unwrap_or(0),
        usage,
    })
}

fn is_compat_noop_flag_eq(arg: &str) -> bool {
    arg.starts_with("--data-root=")
        || arg.starts_with("--data-dir=")
        || arg.starts_with("--data_dir=")
        || arg.starts_with("--grants-path=")
}

pub fn parse_and_run_acp_proxy(args: &[String]) -> io::Result<i32> {
    let mut agy_bin: Option<String> = None;
    let mut max_auto_compacts = DEFAULT_MAX_AUTO_COMPACTS;
    let mut auto_compact_threshold: u64 = 0;
    let mut threshold_prompt = "/compact".to_string();
    let mut native_auto_compact: Option<String> = None;
    let mut quiet_notices = false;
    let mut log_file: Option<PathBuf> = None;
    let mut agy_args = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            agy_args.extend_from_slice(&args[i + 1..]);
            break;
        }
        if let Some(val) = a.strip_prefix("--native-auto-compact=") {
            if !val.trim().is_empty() {
                native_auto_compact = Some(val.trim().to_string());
            }
            i += 1;
            continue;
        }
        if is_compat_noop_flag_eq(a) {
            i += 1;
            continue;
        }
        match a {
            "--agy-bin" if i + 1 < args.len() => {
                agy_bin = Some(args[i + 1].clone());
                i += 2;
            }
            // Harmless backwards-compatible no-op flags (thin client has zero local DB/grant IO).
            "--data-root" | "--data-dir" | "--data_dir" | "--grants-path" if i + 1 < args.len() => {
                i += 2;
            }
            "--native-auto-compact" if i + 1 < args.len() => {
                let v = args[i + 1].trim();
                if !v.is_empty() {
                    native_auto_compact = Some(v.to_string());
                }
                i += 2;
            }
            "--max-auto-compacts" if i + 1 < args.len() => {
                max_auto_compacts = args[i + 1].parse().unwrap_or(DEFAULT_MAX_AUTO_COMPACTS);
                i += 2;
            }
            "--auto-compact-threshold" if i + 1 < args.len() => {
                auto_compact_threshold = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--threshold-compact-prompt" if i + 1 < args.len() => {
                threshold_prompt = args[i + 1].clone();
                i += 2;
            }
            "--quiet-notices" => {
                quiet_notices = true;
                i += 1;
            }
            "--proxy-log" if i + 1 < args.len() => {
                log_file = Some(PathBuf::from(expand_tilde(&args[i + 1])));
                i += 2;
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: context-radar-mcp acp-proxy [options] [-- <agy acp flags...>]\n\
Options:\n  \
--agy-bin <path>                    Path to agy binary (default: auto-detect)\n  \
--native-auto-compact <val>         Set AGY_CLI_AUTO_COMPACT_OVERRIDE (e.g. 300K, 80%)\n  \
--max-auto-compacts <n>             Max autonomous /compact injections per turn (default: 2)\n  \
--auto-compact-threshold <tokens>   Optional token threshold fallback (default: 0 = off)\n  \
--threshold-compact-prompt <text>   Prompt for threshold compaction (default: /compact)\n  \
--quiet-notices                     Omit inline [Context Radar] status chunk\n  \
--proxy-log <path>                  Append JSON-RPC debug log to file"
                );
                return Ok(0);
            }
            _ => {
                agy_args.push(args[i].clone());
                i += 1;
            }
        }
    }

    let cfg = AcpProxyConfig {
        agy_bin: resolve_agy_bin(agy_bin.as_deref()),
        agy_args,
        max_auto_compacts,
        auto_compact_threshold,
        threshold_compact_prompt: threshold_prompt,
        native_auto_compact,
        quiet_notices,
        log_file,
    };
    run_acp_proxy(cfg)
}

fn read_prompt_from_stdin() -> io::Result<String> {
    let mut out = String::new();
    io::stdin()
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_string(&mut out)?;
    if out.len() > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "prompt from stdin exceeds 4 MiB limit",
        ));
    }
    Ok(out)
}

pub fn parse_and_run_acp_headless(args: &[String]) -> io::Result<i32> {
    let mut agy_bin: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut prompt_arg: Option<String> = None;
    let mut prompt_file: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut model: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut auto_approve = false;
    let mut json_output = false;
    let mut max_auto_compacts = DEFAULT_MAX_AUTO_COMPACTS;
    let mut auto_compact_threshold: u64 = 0;
    let mut threshold_prompt = "/compact".to_string();
    let mut native_auto_compact: Option<String> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut agy_args = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            agy_args.extend_from_slice(&args[i + 1..]);
            break;
        }
        if let Some(val) = a.strip_prefix("--native-auto-compact=") {
            if !val.trim().is_empty() {
                native_auto_compact = Some(val.trim().to_string());
            }
            i += 1;
            continue;
        }
        if let Some(val) = a.strip_prefix("--prompt-file=") {
            prompt_file = Some(val.to_string());
            i += 1;
            continue;
        }
        if is_compat_noop_flag_eq(a) {
            i += 1;
            continue;
        }
        match a {
            "-p" | "--prompt" if i + 1 < args.len() => {
                prompt_arg = Some(args[i + 1].clone());
                i += 2;
            }
            "--prompt-file" if i + 1 < args.len() => {
                prompt_file = Some(args[i + 1].clone());
                i += 2;
            }
            "--cwd" if i + 1 < args.len() => {
                cwd = Some(PathBuf::from(expand_tilde(&args[i + 1])));
                i += 2;
            }
            "--session-id" | "--conversation" if i + 1 < args.len() => {
                session_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--model" if i + 1 < args.len() => {
                model = Some(args[i + 1].clone());
                i += 2;
            }
            "--mode" if i + 1 < args.len() => {
                mode = Some(args[i + 1].clone());
                i += 2;
            }
            "--auto-approve" => {
                auto_approve = true;
                i += 1;
            }
            "--json" => {
                json_output = true;
                i += 1;
            }
            "--agy-bin" if i + 1 < args.len() => {
                agy_bin = Some(args[i + 1].clone());
                i += 2;
            }
            // Harmless backwards-compatible no-op flags (thin client has zero local DB/grant IO).
            "--data-root" | "--data-dir" | "--data_dir" | "--grants-path" if i + 1 < args.len() => {
                i += 2;
            }
            "--native-auto-compact" if i + 1 < args.len() => {
                let v = args[i + 1].trim();
                if !v.is_empty() {
                    native_auto_compact = Some(v.to_string());
                }
                i += 2;
            }
            "--max-auto-compacts" if i + 1 < args.len() => {
                max_auto_compacts = args[i + 1].parse().unwrap_or(DEFAULT_MAX_AUTO_COMPACTS);
                i += 2;
            }
            "--auto-compact-threshold" if i + 1 < args.len() => {
                auto_compact_threshold = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--threshold-compact-prompt" if i + 1 < args.len() => {
                threshold_prompt = args[i + 1].clone();
                i += 2;
            }
            "--proxy-log" if i + 1 < args.len() => {
                log_file = Some(PathBuf::from(expand_tilde(&args[i + 1])));
                i += 2;
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: context-radar-mcp acp-run (--prompt <text|-> | --prompt-file <path|->) [options] [-- <agy acp flags...>]\n\
Options:\n  \
-p, --prompt <text|->               Prompt to send over official ACP v1 ('-' reads stdin)\n  \
--prompt-file <path|->              Read prompt from regular file or '-' for stdin (avoids argv exposure)\n  \
--cwd <dir>                         Workspace directory (default: current dir)\n  \
--session-id <id>                   Resume an existing ACP session via session/resume\n  \
--model <slug>                      Optional model ID via session/set_model\n  \
--mode <id>                         Optional mode ID via session/set_mode\n  \
--auto-approve                      Auto-approve tool permission requests\n  \
--json                              Emit structured JSON report to stdout\n  \
--agy-bin <path>                    Path to agy binary (default: auto-detect)\n  \
--native-auto-compact <val>         Set AGY_CLI_AUTO_COMPACT_OVERRIDE (e.g. 300K, 80%)\n  \
--max-auto-compacts <n>             Max autonomous /compact rounds (default: 2)\n  \
--auto-compact-threshold <tokens>   Optional token threshold fallback (default: 0 = off)\n  \
--proxy-log <path>                  Append JSON-RPC debug log to file"
                );
                return Ok(0);
            }
            _ => {
                agy_args.push(args[i].clone());
                i += 1;
            }
        }
    }

    let prompt = match (prompt_arg, prompt_file) {
        (Some(_), Some(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "specify only one of --prompt or --prompt-file",
            ));
        }
        (Some(p), None) => {
            if p == "-" {
                read_prompt_from_stdin()?
            } else {
                p
            }
        }
        (None, Some(pf)) => {
            if pf == "-" {
                read_prompt_from_stdin()?
            } else {
                read_regular_file_bounded(&PathBuf::from(expand_tilde(&pf)), MAX_CONFIG_BYTES)?
            }
        }
        (None, None) => String::new(),
    };

    if prompt.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "acp-run requires --prompt <text|-> or --prompt-file <path|->",
        ));
    }

    let resolved_cwd =
        cwd.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let run_cfg = AcpRunConfig {
        proxy: AcpProxyConfig {
            agy_bin: resolve_agy_bin(agy_bin.as_deref()),
            agy_args,
            max_auto_compacts,
            auto_compact_threshold,
            threshold_compact_prompt: threshold_prompt,
            native_auto_compact,
            quiet_notices: true,
            log_file,
        },
        cwd: resolved_cwd,
        prompt,
        session_id,
        model,
        mode,
        auto_approve,
        json_output,
        extra_mcp_servers: Vec::new(),
    };

    let report = run_acp_headless(run_cfg)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
    Ok(0)
}
