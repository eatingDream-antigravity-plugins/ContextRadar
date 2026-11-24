//! Official Agent Client Protocol (ACP v1) transparent proxy and headless runner.
//!
//! Enables autonomous in-place `/compact` (and compact-and-continue) on `agy acp`
//! sessions without concurrent session conflicts or non-official internal RPCs:
//! - `acp-proxy`: sits transparently over stdio JSON-RPC 2.0 between any ACP client
//!   (Zed, JetBrains, Xcode, Android Studio, VS Code ACP Client) and `agy acp`.
//!   When a `session/prompt` turn completes (`stopReason == "end_turn"`) and an
//!   unclaimed `kind == "compact"` Context Radar grant (or opt-in token threshold)
//!   is ready for that session, `acp-proxy` atomically claims the grant and issues
//!   a native ACP `session/prompt` with `/compact ...` on the exact same connection
//!   before resolving the client's original prompt response.
//! - `acp-run`: standalone headless ACP v1 client for CLI / CI automation with the
//!   same gated autonomous `/compact` loop.

use std::collections::HashMap;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use serde::Serialize;
use serde_json::{json, Value};

use crate::engine::grants::RelayGrantStore;
use crate::engine::telemetry::TelemetryEngine;
use crate::rpc::protocol::{SERVER_NAME, SERVER_VERSION};
use crate::settings::{
    default_plugin_data_dir, expand_tilde, resolve_agy_bin, user_home_dir, ACP_PROXY_ENV,
    DATA_ROOT_ENV, GRANTS_PATH_ENV,
};

pub const DEFAULT_MAX_AUTO_COMPACTS: usize = 2;

#[derive(Debug, Clone)]
pub struct AcpProxyConfig {
    pub agy_bin: PathBuf,
    pub agy_args: Vec<String>,
    pub data_root: PathBuf,
    pub grants_path: PathBuf,
    pub max_auto_compacts: usize,
    pub auto_compact_threshold: u64,
    pub threshold_compact_prompt: String,
    pub quiet_notices: bool,
    pub log_file: Option<PathBuf>,
}

impl AcpProxyConfig {
    pub fn default_with_args(agy_args: Vec<String>) -> Self {
        let data_root = resolve_cli_data_root_from_args(&agy_args);
        let grants_path = default_plugin_data_dir(&data_root).join("relay_grants.json");
        Self {
            agy_bin: resolve_agy_bin(None),
            agy_args,
            data_root,
            grants_path,
            max_auto_compacts: DEFAULT_MAX_AUTO_COMPACTS,
            auto_compact_threshold: 0,
            threshold_compact_prompt: "/compact".to_string(),
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

/// Resolve the `agy` CLI data root respecting `--gemini-dir` and `--app-data-dir` flags.
pub fn resolve_cli_data_root_from_args(agy_args: &[String]) -> PathBuf {
    let mut gemini_dir = ".gemini".to_string();
    let mut app_data_dir = "antigravity-cli".to_string();
    let mut i = 0;
    while i < agy_args.len() {
        let arg = agy_args[i].as_str();
        if (arg == "--gemini-dir" || arg == "--gemini_dir") && i + 1 < agy_args.len() {
            gemini_dir = agy_args[i + 1].clone();
            i += 2;
        } else if let Some(val) = arg
            .strip_prefix("--gemini-dir=")
            .or_else(|| arg.strip_prefix("--gemini_dir="))
        {
            if !val.trim().is_empty() {
                gemini_dir = val.trim().to_string();
            }
            i += 1;
        } else if (arg == "--app-data-dir" || arg == "--app_data_dir") && i + 1 < agy_args.len() {
            app_data_dir = agy_args[i + 1].clone();
            i += 2;
        } else if let Some(val) = arg
            .strip_prefix("--app-data-dir=")
            .or_else(|| arg.strip_prefix("--app_data_dir="))
        {
            if !val.trim().is_empty() {
                app_data_dir = val.trim().to_string();
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    let app_path = PathBuf::from(expand_tilde(&app_data_dir));
    if app_path.is_absolute() {
        return app_path;
    }
    let gem_path = PathBuf::from(expand_tilde(&gemini_dir));
    let base = if gem_path.is_absolute() {
        gem_path
    } else {
        user_home_dir().join(gem_path)
    };
    base.join(app_path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    ToAgent(Value),
    ToClient(Value),
}

#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub session_id: String,
    pub cwd: String,
    pub last_used_tokens: u64,
    pub context_size: u64,
    pub total_auto_compacts: usize,
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
    grant_id: Option<String>,
    compacts_in_turn: usize,
    cancelled_by_client: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LiveSessionCheck {
    pub turns: Option<usize>,
    pub checkpoints: Option<usize>,
    pub running_subagents: Option<usize>,
}

/// Deterministic, transport-independent ACP v1 proxy coordinator.
pub struct ProxyCoordinator {
    max_auto_compacts: usize,
    auto_compact_threshold: u64,
    threshold_compact_prompt: String,
    quiet_notices: bool,
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

pub fn prompt_starts_with_compact(params: &Value) -> bool {
    let Some(blocks) = params.get("prompt").and_then(Value::as_array) else {
        return false;
    };
    for b in blocks {
        if b.get("type").and_then(Value::as_str) == Some("text") {
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
        // Any non-text block before text means `/compact` is not leading.
        return false;
    }
    false
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
            next_internal_seq: 1,
            sessions: HashMap::new(),
            pending_client: HashMap::new(),
            internal_by_id: HashMap::new(),
            internal_by_session: HashMap::new(),
        }
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

    pub fn on_agent_message<F>(
        &mut self,
        msg: Value,
        grants: &mut RelayGrantStore,
        mut live_check: F,
    ) -> Vec<Outbound>
    where
        F: FnMut(&str) -> LiveSessionCheck,
    {
        // 1. Handle session/update notifications (track live context usage).
        if msg.get("id").is_none() {
            if msg.get("method").and_then(Value::as_str) == Some("session/update") {
                if let Some(params) = msg.get("params") {
                    let sid = params
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if let Some(upd) = params.get("update") {
                        if upd.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
                        {
                            let used = upd.get("used").and_then(Value::as_u64).unwrap_or(0);
                            let size = upd.get("size").and_then(Value::as_u64).unwrap_or(0);
                            if !sid.is_empty() {
                                let st = self.sessions.entry(sid.to_string()).or_insert_with(|| {
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
            let is_err = msg.get("error").is_some();
            let stop_reason = msg
                .get("result")
                .and_then(|r| r.get("stopReason"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(ref gid) = internal.grant_id {
                let status = if is_err {
                    "acp_error"
                } else if stop_reason == "cancelled" || internal.cancelled_by_client {
                    "acp_cancelled"
                } else {
                    "acp_compacted"
                };
                let _ = grants.record_compact(gid, status);
            }

            // Allow chaining another agent-issued compact grant only if clean end_turn and within cap.
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
                    grants,
                    &mut live_check,
                ) {
                    return outbound;
                }
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
                    if let Some(res) = msg.get("result") {
                        let sid = res
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or(requested_session_id)
                            .unwrap_or_default();
                        if !sid.is_empty() {
                            let st = self.sessions.entry(sid.clone()).or_insert_with(|| {
                                SessionState {
                                    session_id: sid,
                                    ..SessionState::default()
                                }
                            });
                            if !cwd.is_empty() {
                                st.cwd = cwd;
                            }
                        }
                    }
                    return vec![Outbound::ToClient(msg)];
                }
                PendingClientReq::Prompt {
                    session_id,
                    user_initiated_compact,
                } => {
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
                        if let Some(outbound) = self.try_inject_compact(
                            &session_id,
                            orig_id,
                            0,
                            true,
                            grants,
                            &mut live_check,
                        ) {
                            return outbound;
                        }
                    }
                    return vec![Outbound::ToClient(msg)];
                }
            }
        }

        vec![Outbound::ToClient(msg)]
    }

    fn try_inject_compact<F>(
        &mut self,
        session_id: &str,
        original_client_id: Value,
        compacts_so_far: usize,
        allow_threshold_fallback: bool,
        grants: &mut RelayGrantStore,
        live_check: &mut F,
    ) -> Option<Vec<Outbound>>
    where
        F: FnMut(&str) -> LiveSessionCheck,
    {
        let cwd_opt = self
            .sessions
            .get(session_id)
            .map(|s| s.cwd.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Priority 1: Agent-issued `kind == "compact"` grant from Context Radar MCP.
        let mut chosen: Option<(Option<String>, String)> = None;
        if let Some(grant) =
            grants.find_pending_compact_grant(session_id, cwd_opt.as_deref(), true, None)
        {
            let live = live_check(session_id);
            let claim = grants.claim_grant_scoped(
                &grant.grant_id,
                Some(session_id),
                cwd_opt.as_deref(),
                live.turns,
                live.checkpoints,
                live.running_subagents,
                true,
            );
            if claim.claimed {
                let cmd = claim
                    .compact_command
                    .unwrap_or(grant.compact_command)
                    .trim()
                    .to_string();
                if !cmd.is_empty() {
                    chosen = Some((Some(grant.grant_id), cmd));
                }
            }
        }

        // Priority 2: Opt-in token threshold safety net using official ACP `usage_update`.
        if chosen.is_none()
            && allow_threshold_fallback
            && self.auto_compact_threshold > 0
            && compacts_so_far == 0
        {
            if let Some(st) = self.sessions.get(session_id) {
                if st.last_used_tokens >= self.auto_compact_threshold {
                    let live = live_check(session_id);
                    if live.running_subagents.unwrap_or(0) == 0 {
                        chosen = Some((None, self.threshold_compact_prompt.clone()));
                    }
                }
            }
        }

        let (grant_id, compact_cmd) = chosen?;
        let seq = self.next_internal_seq;
        self.next_internal_seq += 1;
        let internal_id = format!("__cr_acp_compact_{seq}");
        let internal_id_val = Value::String(internal_id.clone());
        let internal_key = id_key(&internal_id_val)?;

        self.internal_by_session
            .insert(session_id.to_string(), internal_key.clone());
        self.internal_by_id.insert(
            internal_key,
            InternalCompactState {
                original_client_id,
                session_id: session_id.to_string(),
                grant_id,
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
    cmd.env(DATA_ROOT_ENV, &cfg.data_root);
    cmd.env(GRANTS_PATH_ENV, &cfg.grants_path);
    cmd.env(ACP_PROXY_ENV, "1");
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

fn append_log(log_file: &Option<PathBuf>, line: &str) {
    let Some(path) = log_file else { return };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

enum ProxyEvent {
    ClientMsg(Value),
    ClientEof,
    AgentMsg(Value),
    AgentEof,
}

/// Run transparent official ACP v1 proxy over stdio.
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

    // Pump child stderr directly to proxy stderr.
    let stderr_handle = thread::spawn(move || {
        let mut reader = BufReader::new(child_stderr);
        let mut err_out = io::stderr().lock();
        let mut buf = String::new();
        while let Ok(n) = reader.read_line(&mut buf) {
            if n == 0 {
                break;
            }
            let _ = err_out.write_all(buf.as_bytes());
            let _ = err_out.flush();
            buf.clear();
        }
    });

    let (tx, rx) = mpsc::channel::<ProxyEvent>();
    let tx_client = tx.clone();
    thread::spawn(move || {
        let stdin = io::stdin();
        let reader = BufReader::new(stdin.lock());
        for line in reader.lines() {
            let Ok(raw) = line else { break };
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                if tx_client.send(ProxyEvent::ClientMsg(val)).is_err() {
                    break;
                }
            }
        }
        let _ = tx_client.send(ProxyEvent::ClientEof);
    });

    let tx_agent = tx;
    thread::spawn(move || {
        let reader = BufReader::new(child_stdout);
        for line in reader.lines() {
            let Ok(raw) = line else { break };
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                if tx_agent.send(ProxyEvent::AgentMsg(val)).is_err() {
                    break;
                }
            }
        }
        let _ = tx_agent.send(ProxyEvent::AgentEof);
    });

    let mut grants = RelayGrantStore::new(cfg.grants_path.clone());
    let mut telemetry = TelemetryEngine::new_with_plugin_data(
        Some(cfg.data_root.clone()),
        cfg.grants_path.parent().map(Path::to_path_buf),
        None,
        Some(None),
    );
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
                let actions = coord.on_agent_message(msg, &mut grants, |sid| {
                    let (t, c) = telemetry.get_turn_and_checkpoint_counts(sid);
                    let subs = telemetry.get_running_subagent_count(sid, None);
                    LiveSessionCheck {
                        turns: if t > 0 { Some(t) } else { None },
                        checkpoints: if t > 0 || c > 0 { Some(c) } else { None },
                        running_subagents: Some(subs),
                    }
                });
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

    telemetry.close();
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
    options
        .first()
        .and_then(|o| o.get("optionId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Standalone headless official ACP v1 runner with autonomous `/compact` orchestration.
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
    let cwd_abs = cfg
        .cwd
        .canonicalize()
        .unwrap_or_else(|_| cfg.cwd.clone());
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

    let stderr_handle = thread::spawn(move || {
        let mut reader = BufReader::new(child_stderr);
        let mut buf = String::new();
        while let Ok(n) = reader.read_line(&mut buf) {
            if n == 0 {
                break;
            }
            eprint!("{buf}");
            buf.clear();
        }
    });

    let mut reader = BufReader::new(child_stdout);
    let mut next_id: u64 = 1;
    let mut alloc_id = || {
        let cur = next_id;
        next_id += 1;
        cur
    };

    let mut grants = RelayGrantStore::new(cfg.proxy.grants_path.clone());
    let mut telemetry = TelemetryEngine::new_with_plugin_data(
        Some(cfg.proxy.data_root.clone()),
        cfg.proxy.grants_path.parent().map(Path::to_path_buf),
        None,
        Some(None),
    );
    let mut coord = ProxyCoordinator::new(
        cfg.proxy.max_auto_compacts,
        cfg.proxy.auto_compact_threshold,
        cfg.proxy.threshold_compact_prompt.clone(),
        true,
    );

    let mut wait_for_id = |target_id: u64,
                           stdin_w: &mut std::process::ChildStdin,
                           coord: &mut ProxyCoordinator,
                           grants: &mut RelayGrantStore,
                           telemetry: &mut TelemetryEngine,
                           response_text: &mut String,
                           stream_text: bool|
     -> io::Result<Value> {
        let target_key = id_key(&json!(target_id)).unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "agy acp closed stdout before replying",
                ));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            append_log(&cfg.proxy.log_file, &format!("A->R {msg}"));
            let actions = coord.on_agent_message(msg, grants, |sid| {
                let (t, c) = telemetry.get_turn_and_checkpoint_counts(sid);
                let subs = telemetry.get_running_subagent_count(sid, None);
                LiveSessionCheck {
                    turns: if t > 0 { Some(t) } else { None },
                    checkpoints: if t > 0 || c > 0 { Some(c) } else { None },
                    running_subagents: Some(subs),
                }
            });
            for act in actions {
                match act {
                    Outbound::ToAgent(v) => {
                        append_log(&cfg.proxy.log_file, &format!("R->A {v}"));
                        eprintln!("[{SERVER_NAME} acp] autonomous /compact dispatched");
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
            "clientInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
        }
    });
    for a in coord.on_client_message(init_req) {
        if let Outbound::ToAgent(v) = a {
            write_json_line(&mut child_stdin, &v)?;
        }
    }
    let init_res = wait_for_id(
        init_id,
        &mut child_stdin,
        &mut coord,
        &mut grants,
        &mut telemetry,
        &mut scratch,
        false,
    )?;
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
        &mut grants,
        &mut telemetry,
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
        let _ = wait_for_id(
            mid,
            &mut child_stdin,
            &mut coord,
            &mut grants,
            &mut telemetry,
            &mut scratch,
            false,
        );
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
        let _ = wait_for_id(
            mid,
            &mut child_stdin,
            &mut coord,
            &mut grants,
            &mut telemetry,
            &mut scratch,
            false,
        );
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
        &mut grants,
        &mut telemetry,
        &mut response_text,
        !cfg.json_output,
    )?;
    if !cfg.json_output && !response_text.ends_with('\n') && !response_text.is_empty() {
        println!();
    }

    // Graceful shutdown so agy acp flushes session state cleanly.
    drop(child_stdin);
    telemetry.close();
    let _ = child.wait();
    let _ = stderr_handle.join();

    if let Some(err) = prompt_res.get("error") {
        return Err(io::Error::other(format!("ACP session/prompt failed: {err}")));
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

pub fn parse_and_run_acp_proxy(args: &[String]) -> io::Result<i32> {
    let mut agy_bin: Option<String> = None;
    let mut data_root: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut grants_path: Option<PathBuf> = None;
    let mut max_auto_compacts = DEFAULT_MAX_AUTO_COMPACTS;
    let mut auto_compact_threshold: u64 = 0;
    let mut threshold_prompt = "/compact".to_string();
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
        match a {
            "--agy-bin" if i + 1 < args.len() => {
                agy_bin = Some(args[i + 1].clone());
                i += 2;
            }
            "--data-root" if i + 1 < args.len() => {
                data_root = Some(PathBuf::from(expand_tilde(&args[i + 1])));
                i += 2;
            }
            "--data-dir" | "--data_dir" if i + 1 < args.len() => {
                data_dir = Some(PathBuf::from(expand_tilde(&args[i + 1])));
                i += 2;
            }
            "--grants-path" if i + 1 < args.len() => {
                grants_path = Some(PathBuf::from(expand_tilde(&args[i + 1])));
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
--data-root <path>                  Override Antigravity CLI data root\n  \
--data-dir <path>                   Override Context Radar plugin_data dir\n  \
--grants-path <path>                Override shared relay_grants.json path\n  \
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

    let resolved_root = data_root.unwrap_or_else(|| resolve_cli_data_root_from_args(&agy_args));
    let resolved_grants = grants_path.unwrap_or_else(|| {
        if let Some(ref dd) = data_dir {
            dd.join("relay_grants.json")
        } else {
            default_plugin_data_dir(&resolved_root).join("relay_grants.json")
        }
    });

    let cfg = AcpProxyConfig {
        agy_bin: resolve_agy_bin(agy_bin.as_deref()),
        agy_args,
        data_root: resolved_root,
        grants_path: resolved_grants,
        max_auto_compacts,
        auto_compact_threshold,
        threshold_compact_prompt: threshold_prompt,
        quiet_notices,
        log_file,
    };
    run_acp_proxy(cfg)
}

pub fn parse_and_run_acp_headless(args: &[String]) -> io::Result<i32> {
    let mut agy_bin: Option<String> = None;
    let mut data_root: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut grants_path: Option<PathBuf> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut prompt = String::new();
    let mut session_id: Option<String> = None;
    let mut model: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut auto_approve = false;
    let mut json_output = false;
    let mut max_auto_compacts = DEFAULT_MAX_AUTO_COMPACTS;
    let mut auto_compact_threshold: u64 = 0;
    let mut threshold_prompt = "/compact".to_string();
    let mut log_file: Option<PathBuf> = None;
    let mut agy_args = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            agy_args.extend_from_slice(&args[i + 1..]);
            break;
        }
        match a {
            "-p" | "--prompt" if i + 1 < args.len() => {
                prompt = args[i + 1].clone();
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
            "--data-root" if i + 1 < args.len() => {
                data_root = Some(PathBuf::from(expand_tilde(&args[i + 1])));
                i += 2;
            }
            "--data-dir" | "--data_dir" if i + 1 < args.len() => {
                data_dir = Some(PathBuf::from(expand_tilde(&args[i + 1])));
                i += 2;
            }
            "--grants-path" if i + 1 < args.len() => {
                grants_path = Some(PathBuf::from(expand_tilde(&args[i + 1])));
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
                    "Usage: context-radar-mcp acp-run --prompt <text> [options] [-- <agy acp flags...>]\n\
Options:\n  \
-p, --prompt <text>                 Prompt to send over official ACP v1\n  \
--cwd <dir>                         Workspace directory (default: current dir)\n  \
--session-id <id>                   Resume an existing ACP session via session/resume\n  \
--model <slug>                      Optional model ID via session/set_model\n  \
--mode <id>                         Optional mode ID via session/set_mode\n  \
--auto-approve                      Auto-approve tool permission requests\n  \
--json                              Emit structured JSON report to stdout\n  \
--agy-bin <path>                    Path to agy binary (default: auto-detect)\n  \
--data-root <path>                  Override Antigravity CLI data root\n  \
--grants-path <path>                Override shared relay_grants.json path\n  \
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

    if prompt.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "acp-run requires --prompt <text>",
        ));
    }

    let resolved_root = data_root.unwrap_or_else(|| resolve_cli_data_root_from_args(&agy_args));
    let resolved_grants = grants_path.unwrap_or_else(|| {
        if let Some(ref dd) = data_dir {
            dd.join("relay_grants.json")
        } else {
            default_plugin_data_dir(&resolved_root).join("relay_grants.json")
        }
    });
    let resolved_cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let run_cfg = AcpRunConfig {
        proxy: AcpProxyConfig {
            agy_bin: resolve_agy_bin(agy_bin.as_deref()),
            agy_args,
            data_root: resolved_root,
            grants_path: resolved_grants,
            max_auto_compacts,
            auto_compact_threshold,
            threshold_compact_prompt: threshold_prompt,
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
