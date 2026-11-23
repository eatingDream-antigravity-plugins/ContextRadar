//! JSON-RPC 2.0 MCP stdio server with startup pre-warm, zero-block SWR cache, SSR boot-data injection, and digest deduplication.

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::engine::budget::{evaluate_handoff_gate, prepare_handoff_plan};
use crate::engine::grants::{
    default_grants_path_in, sanitize_scratch_note_path, RelayGrantStore,
};
use crate::engine::pruner::distill_slice;
use crate::engine::telemetry::{compute_snapshot_digest, OverviewSnapshot, TelemetryEngine};
use crate::rpc::tools::{
    fmt_k, resources_spec, summarize_snapshot_text, tools_spec, APP_MIME, HANDOFF_URI, PANEL_URI,
};
use crate::settings::{expand_tilde, PANEL_HTML_ENV};

pub const SERVER_NAME: &str = "context-radar-mcp";
pub const SERVER_VERSION: &str = "1.0.0";
pub const PROTOCOL_VERSION: &str = "2025-03-26";
const MAX_HTML_BYTES: usize = 10 * 1024 * 1024;
const BOOT_MARKER: &str = "/*__CONTEXT_RADAR_BOOT__*/";
/// Short burst-coalescing TTL so back-to-back `resources/read` + `context_radar_overview`
/// calls during the same pane open share one snapshot without ever serving stale data.
pub const SWR_STALE_TTL: Duration = Duration::from_millis(500);

fn serialize_boot_json(val: &Value) -> String {
    let raw = serde_json::to_string(val).unwrap_or_else(|_| "null".to_string());
    let mut out = String::with_capacity(raw.len() + 32);
    for ch in raw.chars() {
        match ch {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn default_panel_html_path() -> PathBuf {
    if let Ok(ov) = env::var(PANEL_HTML_ENV) {
        let trimmed = ov.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(expand_tilde(trimmed));
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(root) = bin_dir.parent() {
                let cand = root.join("panel").join("dist").join("panel.html");
                if cand.is_file() {
                    return cand;
                }
            }
        }
    }
    PathBuf::from("panel").join("dist").join("panel.html")
}

#[derive(Clone)]
struct HtmlCache {
    path: PathBuf,
    mtime_ns: i128,
    size: u64,
    content: String,
}

#[derive(Clone)]
struct RenderedPanelCache {
    path: PathBuf,
    mtime_ns: i128,
    size: u64,
    digest: String,
    stale: bool,
    html: String,
}

#[derive(Clone)]
struct CachedDefaultSnap {
    snap: OverviewSnapshot,
    built_at: Instant,
    grants_rev: u64,
}

#[derive(Default)]
struct RefreshSignal {
    requested: bool,
    in_flight: bool,
    shutdown: bool,
}

struct SwrHub {
    cached: RwLock<Option<CachedDefaultSnap>>,
    html: RwLock<Option<HtmlCache>>,
    rendered_panel: RwLock<Option<RenderedPanelCache>>,
    grants_rev: AtomicU64,
    signal: (Mutex<RefreshSignal>, Condvar),
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl SwrHub {
    fn new() -> Self {
        Self {
            cached: RwLock::new(None),
            html: RwLock::new(None),
            rendered_panel: RwLock::new(None),
            grants_rev: AtomicU64::new(0),
            signal: (Mutex::new(RefreshSignal::default()), Condvar::new()),
            worker: Mutex::new(None),
        }
    }

    #[inline]
    fn grants_rev(&self) -> u64 {
        self.grants_rev.load(Ordering::Acquire)
    }

    fn bump_grants_rev(&self) -> u64 {
        let next = self.grants_rev.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut rp) = self.rendered_panel.write() {
            *rp = None;
        }
        next
    }

    fn get_cached(&self) -> Option<CachedDefaultSnap> {
        self.cached.read().ok().and_then(|g| g.clone())
    }

    fn put_cached(&self, snap: OverviewSnapshot, grants_rev: u64) {
        if let Ok(mut g) = self.cached.write() {
            *g = Some(CachedDefaultSnap {
                snap,
                built_at: Instant::now(),
                grants_rev,
            });
        }
    }

    fn get_rendered_panel(
        &self,
        path: &Path,
        mtime_ns: i128,
        size: u64,
        digest: &str,
        stale: bool,
    ) -> Option<String> {
        let g = self.rendered_panel.read().ok()?;
        let c = g.as_ref()?;
        if c.path == path
            && c.mtime_ns == mtime_ns
            && c.size == size
            && c.digest == digest
            && c.stale == stale
        {
            return Some(c.html.clone());
        }
        None
    }

    fn put_rendered_panel(
        &self,
        path: PathBuf,
        mtime_ns: i128,
        size: u64,
        digest: String,
        stale: bool,
        html: String,
    ) {
        if let Ok(mut g) = self.rendered_panel.write() {
            *g = Some(RenderedPanelCache {
                path,
                mtime_ns,
                size,
                digest,
                stale,
                html,
            });
        }
    }

    fn load_html(&self, path: &Path) -> Result<(String, i128, u64), String> {
        let meta = fs::metadata(path)
            .map_err(|e| format!("Failed to stat panel HTML {}: {e}", path.display()))?;
        let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);
        let size = meta.size();
        if let Ok(g) = self.html.read() {
            if let Some(ref c) = *g {
                if c.path == path && c.mtime_ns == mtime_ns && c.size == size {
                    return Ok((c.content.clone(), mtime_ns, size));
                }
            }
        }
        let html = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read panel HTML {}: {e}", path.display()))?;
        if html.len() > MAX_HTML_BYTES {
            return Err("panel.html exceeds 10 MiB MCP App resource limit".to_string());
        }
        if let Ok(mut g) = self.html.write() {
            *g = Some(HtmlCache {
                path: path.to_path_buf(),
                mtime_ns,
                size,
                content: html.clone(),
            });
        }
        if let Ok(mut rp) = self.rendered_panel.write() {
            *rp = None;
        }
        Ok((html, mtime_ns, size))
    }

    #[allow(dead_code)]
    fn trigger_refresh(&self) {
        let (lock, cvar) = &self.signal;
        if let Ok(mut sig) = lock.lock() {
            if sig.shutdown || sig.requested || sig.in_flight {
                return;
            }
            sig.requested = true;
            cvar.notify_one();
        }
    }

    fn backdate_cached(&self, age: Duration) {
        if let Ok(mut g) = self.cached.write() {
            if let Some(ref mut c) = *g {
                if let Some(earlier) = Instant::now().checked_sub(age) {
                    c.built_at = earlier;
                }
            }
        }
        if let Ok(mut rp) = self.rendered_panel.write() {
            *rp = None;
        }
    }

    fn shutdown(&self) {
        let (lock, cvar) = &self.signal;
        if let Ok(mut sig) = lock.lock() {
            sig.shutdown = true;
            cvar.notify_all();
        }
        if let Ok(mut w) = self.worker.lock() {
            if let Some(handle) = w.take() {
                let _ = handle.join();
            }
        }
    }
}

fn attach_grant_and_rehash_inner(
    engine: &mut TelemetryEngine,
    grants: &mut RelayGrantStore,
    snap: &mut OverviewSnapshot,
) {
    let settings = grants.get_settings();
    snap.settings = Some(settings.clone());
    snap.pending_grant = None;
    if let Some(mut pending) = grants.get_pending_grant(None) {
        let cid = pending.conversation_id.clone();
        let mut valid = true;
        if !cid.is_empty() && !pending.claimed {
            let (cur_turns, cur_ckpts) = engine.get_turn_and_checkpoint_counts(&cid);
            if cur_ckpts > pending.created_checkpoints {
                valid = false;
            } else if pending.created_turns > 0
                && cur_turns.saturating_sub(pending.created_turns) > 4
            {
                valid = false;
            }
        }
        if valid {
            let live_subs = if !cid.is_empty() {
                engine.get_running_subagent_count(&cid, None)
            } else {
                0
            };
            pending.running_subagent_count = Some(live_subs);
            pending.allow_auto_handoff = Some(settings.allow_auto_handoff);
            snap.pending_grant = Some(pending);
        }
    }
    snap.digest = compute_snapshot_digest(snap);
}

fn inject_boot_html(template: &str, boot_val: &Value) -> String {
    if template.contains(BOOT_MARKER) {
        let script_assign = format!(
            "window.__CONTEXT_RADAR_BOOT__={};",
            serialize_boot_json(boot_val)
        );
        template.replacen(BOOT_MARKER, &script_assign, 1)
    } else {
        template.to_string()
    }
}

pub struct McpDaemon {
    pub engine: TelemetryEngine,
    pub grants: RelayGrantStore,
    plugin_data_dir: Option<PathBuf>,
    panel_html_override: Option<PathBuf>,
    swr: Arc<SwrHub>,
}

impl Drop for McpDaemon {
    fn drop(&mut self) {
        self.swr.shutdown();
    }
}

impl McpDaemon {
    pub fn new(
        engine: Option<TelemetryEngine>,
        grants: Option<RelayGrantStore>,
        panel_html_override: Option<PathBuf>,
    ) -> Self {
        Self::new_with_plugin_data(engine, grants, None, panel_html_override)
    }

    pub fn new_with_plugin_data(
        engine: Option<TelemetryEngine>,
        grants: Option<RelayGrantStore>,
        plugin_data_dir: Option<PathBuf>,
        panel_html_override: Option<PathBuf>,
    ) -> Self {
        let eng = engine.unwrap_or_else(|| {
            TelemetryEngine::new_with_plugin_data(None, plugin_data_dir.clone(), None, None)
        });
        let gstore = grants.unwrap_or_else(|| {
            RelayGrantStore::new(default_grants_path_in(
                plugin_data_dir.as_deref(),
                Some(&eng.ledger.data_root),
            ))
        });
        Self {
            engine: eng,
            grants: gstore,
            plugin_data_dir,
            panel_html_override,
            swr: Arc::new(SwrHub::new()),
        }
    }

    /// Spawn the single-flight background pre-warm & Stale-While-Revalidate refresh thread.
    /// Immediately pre-loads `panel/dist/panel.html`, opens host SQLite strictly read-only,
    /// warms the active sessions snapshot + SSR boot HTML into RAM, and then sleeps until
    /// woken by a stale read (zero idle CPU burn).
    pub fn start_prewarm(&self) {
        let Ok(mut slot) = self.swr.worker.lock() else {
            return;
        };
        if slot.is_some() {
            return;
        }
        let hub = Arc::clone(&self.swr);
        let data_root = self.engine.ledger.data_root.clone();
        let plugin_data_dir = self.plugin_data_dir.clone();
        let grants_path = self.grants.path.clone();
        let panel_override = self.panel_html_override.clone();

        if let Ok(mut sig) = hub.signal.0.lock() {
            sig.in_flight = true;
        }

        let handle = thread::Builder::new()
            .name("context-radar-swr".to_string())
            .spawn(move || {
                let mut bg_engine = TelemetryEngine::new_with_plugin_data(
                    Some(data_root),
                    plugin_data_dir,
                    None,
                    None,
                );
                let mut bg_grants = RelayGrantStore::new(grants_path);

                let run_refresh = |bg_engine: &mut TelemetryEngine,
                                   bg_grants: &mut RelayGrantStore,
                                   hub: &SwrHub,
                                   panel_override: &Option<PathBuf>| {
                    let panel_path = panel_override
                        .clone()
                        .unwrap_or_else(default_panel_html_path);
                    let html_loaded = hub.load_html(&panel_path).ok();

                    let rev_before = hub.grants_rev();
                    let mut snap =
                        bg_engine.build_overview_snapshot(25, false, false, "", None);
                    attach_grant_and_rehash_inner(bg_engine, bg_grants, &mut snap);
                    let rev_after = hub.grants_rev();
                    if rev_after != rev_before {
                        attach_grant_and_rehash_inner(bg_engine, bg_grants, &mut snap);
                    }
                    let final_rev = hub.grants_rev();
                    snap.stale = false;

                    if let Some((template, mtime_ns, size)) = html_loaded {
                        if let Ok(val) = serde_json::to_value(&snap) {
                            let rendered = inject_boot_html(&template, &val);
                            hub.put_rendered_panel(
                                panel_path,
                                mtime_ns,
                                size,
                                snap.digest.clone(),
                                false,
                                rendered,
                            );
                        }
                    }
                    hub.put_cached(snap, final_rev);
                };

                // Immediate startup pre-warm pass before waiting for SWR triggers.
                run_refresh(&mut bg_engine, &mut bg_grants, &hub, &panel_override);

                let (lock, cvar) = &hub.signal;
                loop {
                    let mut sig = match lock.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    sig.in_flight = false;
                    while !sig.requested && !sig.shutdown {
                        sig = match cvar.wait(sig) {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                    }
                    if sig.shutdown {
                        break;
                    }
                    sig.requested = false;
                    sig.in_flight = true;
                    drop(sig);

                    run_refresh(&mut bg_engine, &mut bg_grants, &hub, &panel_override);
                }

                bg_engine.close();
            });

        if let Ok(h) = handle {
            *slot = Some(h);
        } else if let Ok(mut sig) = self.swr.signal.0.lock() {
            sig.in_flight = false;
        }
    }

    pub fn swr_is_warm(&self) -> bool {
        self.swr.get_cached().is_some()
    }

    pub fn backdate_swr_cache(&self, age: Duration) {
        self.swr.backdate_cached(age);
    }

    pub fn close(&mut self) {
        self.swr.shutdown();
        self.engine.close();
    }

    fn resolve_panel_path(&self) -> PathBuf {
        self.panel_html_override
            .clone()
            .unwrap_or_else(default_panel_html_path)
    }

    fn attach_grant_and_rehash(&mut self, snap: &mut OverviewSnapshot) {
        attach_grant_and_rehash_inner(&mut self.engine, &mut self.grants, snap);
    }

    fn on_grants_mutated(&mut self) {
        let new_rev = self.swr.bump_grants_rev();
        if let Some(mut cached) = self.swr.get_cached() {
            self.attach_grant_and_rehash(&mut cached.snap);
            if let Ok(mut g) = self.swr.cached.write() {
                cached.grants_rev = new_rev;
                *g = Some(cached);
            }
        }
    }

    /// Return a cached default overview snapshot ONLY when strictly fresh (`< SWR_STALE_TTL`).
    /// Never serves stale data so the panel never displays delayed numbers or jumps after boot.
    fn get_fresh_default_snapshot(&mut self) -> Option<OverviewSnapshot> {
        let mut cached = self.swr.get_cached()?;
        if cached.built_at.elapsed() >= SWR_STALE_TTL {
            return None;
        }
        let cur_rev = self.swr.grants_rev();
        if cached.grants_rev != cur_rev {
            self.attach_grant_and_rehash(&mut cached.snap);
            if let Ok(mut g) = self.swr.cached.write() {
                cached.grants_rev = cur_rev;
                *g = Some(cached.clone());
            }
        }
        cached.snap.stale = false;
        Some(cached.snap)
    }

    fn claim_with_live_check(
        &mut self,
        grant_id: &str,
        hint_cid: &str,
        claim_mode: &str,
    ) -> Value {
        let cid = if !hint_cid.is_empty() {
            hint_cid.to_string()
        } else {
            self.grants
                .find_grant(grant_id)
                .map(|g| g.conversation_id)
                .unwrap_or_default()
        };
        let (cur_turns, cur_ckpts) = if !cid.is_empty() {
            let (t, c) = self.engine.get_turn_and_checkpoint_counts(&cid);
            (Some(t), Some(c))
        } else {
            (None, None)
        };
        let running_subs = if !cid.is_empty() {
            self.engine.get_running_subagent_count(&cid, None)
        } else {
            0
        };
        let require_auto = claim_mode == "auto_handoff";
        let check_subs = if matches!(claim_mode, "auto_handoff" | "manual_handoff") {
            Some(running_subs)
        } else {
            None
        };
        let outcome = self.grants.claim_grant(
            grant_id,
            cur_turns,
            cur_ckpts,
            check_subs,
            require_auto,
        );
        if outcome.claimed
            || matches!(
                outcome.reason.as_deref(),
                Some("already_compacted" | "session_advanced")
            )
        {
            self.on_grants_mutated();
        }
        let text = if outcome.claimed {
            "ok".to_string()
        } else {
            outcome
                .reason
                .clone()
                .unwrap_or_else(|| "denied".to_string())
        };
        json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": outcome,
            "isError": false
        })
    }

    fn handle_ui_action(&mut self, act: &str, args: &Map<String, Value>) -> Value {
        match act {
            "set_prefs" => {
                let allow = args.get("allow_auto_handoff").and_then(Value::as_bool);
                let updated = self.grants.update_settings(allow);
                self.on_grants_mutated();
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("已更新面板偏好：allow_auto_handoff={}", updated.allow_auto_handoff)
                    }],
                    "structuredContent": { "mode": "prefs", "settings": updated },
                    "isError": false
                })
            }
            "record_relay" => {
                let res = self.grants.record_relay(
                    &str_arg(args, "grant_id"),
                    &str_arg(args, "handoff_conversation_id"),
                );
                if res.ok {
                    self.on_grants_mutated();
                }
                let txt = if res.ok {
                    "ok".to_string()
                } else {
                    res.reason.clone().unwrap_or_else(|| "error".to_string())
                };
                json!({
                    "content": [{ "type": "text", "text": txt }],
                    "structuredContent": res,
                    "isError": false
                })
            }
            "dismiss_grant" => {
                let res = self.grants.dismiss_grant(&str_arg(args, "grant_id"));
                if res.ok {
                    self.on_grants_mutated();
                }
                let txt = if res.ok {
                    "ok".to_string()
                } else {
                    res.reason.clone().unwrap_or_else(|| "error".to_string())
                };
                json!({
                    "content": [{ "type": "text", "text": txt }],
                    "structuredContent": res,
                    "isError": false
                })
            }
            "claim_grant" => self.claim_with_live_check(
                &str_arg(args, "grant_id"),
                &str_arg(args, "conversation_id"),
                &str_arg_or(args, "claim_mode", "manual"),
            ),
            _ => json!({
                "content": [{ "type": "text", "text": format!("未知 UI 操作：{act}") }],
                "isError": true
            }),
        }
    }

    fn prepare_commit_handoff(&mut self, args: &Map<String, Value>, kind: &str) -> Value {
        let preserve = str_arg(args, "preserve_points");
        if preserve.is_empty() {
            return json!({
                "content": [{ "type": "text", "text": "preserve_points 不能为空" }],
                "isError": true
            });
        }
        let raw_scratch = str_arg(args, "scratch_note_path");
        let scratch_path = match sanitize_scratch_note_path(&raw_scratch) {
            Ok(p) => p,
            Err(err) => {
                return json!({
                    "content": [{ "type": "text", "text": err }],
                    "isError": true
                });
            }
        };

        let cid_arg = str_arg(args, "conversation_id");
        let rem_turns = u64_arg(args, "estimated_remaining_turns", 12) as usize;
        let target = self.engine.resolve_session_fast(&cid_arg, None);
        let gate = evaluate_handoff_gate(target.as_ref(), rem_turns, &mut self.engine.rates);

        if !gate.allowed {
            let blocked = json!({
                "mode": "handoff_blocked",
                "kind": kind,
                "blocked": true,
                "conversation_id": target.as_ref().map(|t| t.id.as_str()).unwrap_or(""),
                "current_context_tokens": target.as_ref().map(|t| t.latest_context_tokens).unwrap_or(0),
                "running_subagent_count": target.as_ref().map(|t| t.running_subagent_count).unwrap_or(0),
                "reasons": gate.reasons,
                "profile": gate.profile
            });
            let msg_txt = format!(
                "【服务端硬约束拦截：禁止本次全量压缩/接力】\n- {}\n请继续直接复用当前热前缀执行任务，或改调 context_radar_hotspots 做保前缀局部提炼。",
                gate.reasons.join("\n- ")
            );
            return json!({
                "content": [{ "type": "text", "text": msg_txt }],
                "structuredContent": blocked,
                "isError": false
            });
        }

        let cid = target.as_ref().map(|t| t.id.clone()).unwrap_or_default();
        let title = target.as_ref().map(|t| t.title.clone()).unwrap_or_default();
        let turns_count = target.as_ref().map(|t| t.turns).unwrap_or(0);
        let ckpt_count = target.as_ref().map(|t| t.checkpoint_count).unwrap_or(0);
        let running_subs = target
            .as_ref()
            .map(|t| t.running_subagent_count)
            .unwrap_or(0);
        let resume_task = str_arg(args, "resume_task");
        let auto_fire = bool_arg(args, "auto_fire", true);

        let grant = self.grants.create_grant(
            &preserve,
            &resume_task,
            auto_fire,
            &cid,
            turns_count,
            ckpt_count,
            kind,
            &scratch_path,
            &title,
        );
        self.on_grants_mutated();
        let settings = self.grants.get_settings();
        let allow_auto = settings.allow_auto_handoff;

        let card = json!({
            "mode": "handoff_card",
            "kind": kind,
            "blocked": false,
            "conversation_id": cid,
            "title": title,
            "grant_id": grant.grant_id,
            "auto_fire": grant.auto_fire,
            "allow_auto_handoff": allow_auto,
            "running_subagent_count": running_subs,
            "preserve_points": grant.preserve_points,
            "resume_task": grant.resume_task,
            "scratch_note_path": grant.scratch_note_path,
            "compact_command": grant.compact_command,
            "handoff_prompt": grant.handoff_prompt,
            "handoff_conversation_id": ""
        });

        let auto_line = if allow_auto && grant.auto_fire {
            "当前面板「自主接力新会话（官方 target:new）」已开启且满足 running_subagent_count == 0：前端将自动核销单次票据并调用官方 _meta: {\"com.google.antigravity/message\": {\"target\": \"new\"}} 拉起接力新会话并记录新 conversationId。"
        } else {
            "本次未启用自动拉起新会话（auto_fire=false 或面板开关已手动关闭）：已保留当前会话窗口，可在卡片或侧边栏一键复制 /compact 原地压缩，或手动点击「拉起接力新会话」。"
        };
        let guide = format!(
            "已通过模型×强度、冷却、经济回本及 running_subagent_count == 0 全部硬拦截校验。\n- 原地压缩说明：Antigravity 同会话原地 /compact 仅支持用户在输入框首词手动提交；本工具已在内联卡片与侧边栏监控面板中准备好一键复制的完整 `/compact` 指令（含保留要点与续跑指令）。\n- 官方接力状态：{auto_line}"
        );
        json!({
            "content": [{ "type": "text", "text": guide }],
            "structuredContent": card,
            "isError": false
        })
    }

    pub fn handle_request(&mut self, msg: &Value) -> Option<Value> {
        let obj = msg.as_object()?;
        let req_id = obj.get("id")?;
        if req_id.is_null() {
            return None;
        }
        let method = obj.get("method").and_then(Value::as_str).unwrap_or("");
        let params = obj
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        match method {
            "initialize" => {
                let ver = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(PROTOCOL_VERSION);
                Some(rpc_ok(
                    req_id,
                    json!({
                        "protocolVersion": ver,
                        "capabilities": {
                            "tools": { "listChanged": false },
                            "resources": { "subscribe": false, "listChanged": false }
                        },
                        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
                    }),
                ))
            }
            "ping" => Some(rpc_ok(req_id, json!({}))),
            "tools/list" => Some(rpc_ok(req_id, json!({ "tools": tools_spec() }))),
            "tools/call" => match self.call_tool(&params) {
                Ok(res) => Some(rpc_ok(req_id, res)),
                Err((code, message)) => Some(rpc_err(req_id, code, &message)),
            },
            "resources/list" => Some(rpc_ok(req_id, json!({ "resources": resources_spec() }))),
            "resources/templates/list" => Some(rpc_ok(req_id, json!({ "resourceTemplates": [] }))),
            "resources/read" => match self.read_resource(&params) {
                Ok(res) => Some(rpc_ok(req_id, res)),
                Err((code, message)) => Some(rpc_err(req_id, code, &message)),
            },
            "prompts/list" => Some(rpc_ok(req_id, json!({ "prompts": [] }))),
            _ => Some(rpc_err(
                req_id,
                -32601,
                &format!("Method not found: {method}"),
            )),
        }
    }

    fn call_tool(&mut self, params: &Map<String, Value>) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args_val = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let args = args_val
            .as_object()
            .ok_or_else(|| (-32602, "arguments must be an object".to_string()))?;

        match name {
            "context_radar_ui_action" => {
                let act = str_arg(args, "action");
                Ok(self.handle_ui_action(&act, args))
            }
            "context_radar_overview" => {
                let act = str_arg_or(args, "action", "snapshot");
                match act.as_str() {
                    "set_prefs" | "record_relay" | "dismiss_grant" | "claim_grant" => {
                        Ok(self.handle_ui_action(&act, args))
                    }
                    "turns" => Ok(self.handle_turns_tool(args)),
                    _ => {
                        let t0 = Instant::now();
                        let show_history = bool_arg(args, "show_history", false);
                        let include_archived = bool_arg(args, "include_archived", false);
                        let limit = u64_arg(args, "limit", 25) as usize;
                        let ws_filter = str_arg(args, "workspace_filter");
                        let fresh = bool_arg(args, "fresh", false);
                        let if_digest = {
                            let d = str_arg(args, "if_digest");
                            if !d.is_empty() {
                                d
                            } else {
                                str_arg(args, "if_hash")
                            }
                        };
                        let is_default_query = !show_history
                            && !include_archived
                            && limit == 25
                            && ws_filter.is_empty();

                        // Burst-coalescing fresh hit (<500ms) for initial entrypoint open (empty args / no if_digest).
                        if is_default_query && if_digest.is_empty() && !fresh {
                            if let Some(mut snap) = self.get_fresh_default_snapshot() {
                                snap.server_ms = t0.elapsed().as_millis() as u64;
                                let text = summarize_snapshot_text(&snap);
                                return Ok(json!({
                                    "content": [{ "type": "text", "text": text }],
                                    "structuredContent": snap,
                                    "isError": false
                                }));
                            }
                        }

                        // If SWR background refresh just completed with a newer digest than if_digest, serve immediately.
                        if is_default_query && !if_digest.is_empty() && !fresh {
                            if let Some(cached) = self.swr.get_cached() {
                                if cached.grants_rev == self.swr.grants_rev()
                                    && cached.snap.digest != if_digest
                                    && cached.built_at.elapsed() < SWR_STALE_TTL
                                {
                                    let mut snap = cached.snap;
                                    snap.stale = false;
                                    snap.server_ms = t0.elapsed().as_millis() as u64;
                                    let text = summarize_snapshot_text(&snap);
                                    return Ok(json!({
                                        "content": [{ "type": "text", "text": text }],
                                        "structuredContent": snap,
                                        "isError": false
                                    }));
                                }
                            }
                        }

                        let mut snap = self.engine.build_overview_snapshot(
                            limit,
                            show_history,
                            include_archived,
                            &ws_filter,
                            None,
                        );
                        self.attach_grant_and_rehash(&mut snap);

                        if is_default_query {
                            let rev = self.swr.grants_rev();
                            self.swr.put_cached(snap.clone(), rev);
                        }

                        if !if_digest.is_empty() && if_digest == snap.digest {
                            return Ok(json!({
                                "content": [{ "type": "text", "text": "unchanged" }],
                                "structuredContent": {
                                    "mode": "snapshot",
                                    "unchanged": true,
                                    "digest": snap.digest,
                                    "server_ms": snap.server_ms
                                },
                                "isError": false
                            }));
                        }

                        let text = summarize_snapshot_text(&snap);
                        Ok(json!({
                            "content": [{ "type": "text", "text": text }],
                            "structuredContent": snap,
                            "isError": false
                        }))
                    }
                }
            }
            "context_radar_turns" => Ok(self.handle_turns_tool(args)),
            "context_radar_hotspots" => {
                let cid_arg = str_arg(args, "conversation_id");
                let target = self.engine.resolve_session_fast(&cid_arg, None);
                let cid = target
                    .as_ref()
                    .map(|t| t.id.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(cid_arg);
                if cid.is_empty() {
                    return Ok(json!({
                        "content": [{ "type": "text", "text": "未找到目标会话" }],
                        "isError": true
                    }));
                }
                let turns = self.engine.ledger.read_turns(&cid, None);
                let mode = str_arg_or(args, "mode", "hotspots");
                let start_step = args.get("start_step").and_then(Value::as_i64);
                let end_step = args.get("end_step").and_then(Value::as_i64);
                let save_note = bool_arg(args, "save_note", false);
                let top_k = u64_arg(args, "top_k", 8) as usize;
                let root = self.engine.ledger.data_root.clone();
                let res = distill_slice(
                    &root,
                    &cid,
                    &turns,
                    &mode,
                    start_step,
                    end_step,
                    save_note,
                    top_k,
                    &mut self.engine.ledger.journal_cache,
                );
                if !res.found {
                    return Ok(json!({
                        "content": [{ "type": "text", "text": res.error.clone().unwrap_or_else(|| "提炼失败".into()) }],
                        "structuredContent": res,
                        "isError": true
                    }));
                }
                let txt = if res.mode == "hotspots" {
                    let active_rng = res.active_step_range.unwrap_or([0, 0]);
                    let rec_rng = res.recommended_distill_range.unwrap_or([0, 0]);
                    let rows: Vec<String> = res
                        .hotspots
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .map(|h| {
                            format!(
                                "  · 轮 #{} (step {}): +{} → {} | {}",
                                h.turn,
                                h.step_idx,
                                fmt_k(h.delta_tokens),
                                fmt_k(h.context_after),
                                h.cause
                            )
                        })
                        .collect();
                    format!(
                        "会话 {cid} 上下文膨胀热点（活跃步 {}–{}，建议局部提炼区间 start_step={}, end_step={}）：\n{}",
                        active_rng[0],
                        active_rng[1],
                        rec_rng[0],
                        rec_rng[1],
                        if rows.is_empty() {
                            "  · 当前检查点后尚无明显单步膨胀".to_string()
                        } else {
                            rows.join("\n")
                        }
                    )
                } else {
                    let mut s = res.digest.clone().unwrap_or_default();
                    if let Some(p) = res.saved_note_path.as_deref().filter(|x| !x.is_empty()) {
                        s.push_str(&format!("\n- 已存备忘：{p}"));
                    }
                    if let Some(h) = res.handoff_hint.as_deref().filter(|x| !x.is_empty()) {
                        s.push_str(&format!("\n- 复用建议：{h}"));
                    }
                    s
                };
                Ok(json!({
                    "content": [{ "type": "text", "text": txt }],
                    "structuredContent": res,
                    "isError": false
                }))
            }
            "context_radar_prepare_handoff" => {
                let cid_arg = str_arg(args, "conversation_id");
                let rem_turns = u64_arg(args, "estimated_remaining_turns", 12) as usize;
                let noisy = u64_arg(args, "upcoming_noisy_tokens", 0);
                let target = self.engine.resolve_session_fast(&cid_arg, None);
                let plan = prepare_handoff_plan(
                    target.as_ref(),
                    rem_turns,
                    noisy,
                    &mut self.engine.rates,
                );
                if !plan.found {
                    return Ok(json!({
                        "content": [{ "type": "text", "text": plan.reason.clone().unwrap_or_else(|| plan.headline.clone()) }],
                        "structuredContent": plan,
                        "isError": false
                    }));
                }
                let prof = plan.profile.as_ref().unwrap();
                let rcp = plan.partial_compaction_recipes.as_ref().unwrap();
                let sym = if plan.currency_symbol.is_empty() {
                    "¥"
                } else {
                    plan.currency_symbol.as_str()
                };
                let txt = format!(
                    "{}\n- 会话：{} ({}) | {} | 档位：{}\n- 上下文：{} / {}（硬下限 {} · 推荐 {} · 紧急 {}）\n- 经济回本：冷缓存+摘要约 {}{:.2}，后续每轮省 {}{:.3}，需 >= {} 轮回本。\n- 历史局部提纯：{}\n- 后续分叉折叠：{}\n- 官方接力新会话：{}",
                    plan.headline,
                    plan.title,
                    plan.conversation_id,
                    plan.model_display,
                    prof.label,
                    fmt_k(plan.current_context_tokens),
                    fmt_k(plan.context_limit),
                    fmt_k(prof.min_ctx),
                    fmt_k(prof.recommend_ctx),
                    fmt_k(prof.urgent_ctx),
                    sym,
                    plan.compact_overhead_cny,
                    sym,
                    plan.per_turn_saving_cny,
                    plan.break_even_turns,
                    rcp.past_slice_distill,
                    rcp.future_branch_fold,
                    rcp.official_handoff_new
                );
                Ok(json!({
                    "content": [{ "type": "text", "text": txt }],
                    "structuredContent": plan,
                    "isError": false
                }))
            }
            "context_radar_commit_handoff" => {
                let act = str_arg_or(args, "action", "create");
                if act == "claim" {
                    return Ok(self.claim_with_live_check(
                        &str_arg(args, "grant_id"),
                        &str_arg(args, "conversation_id"),
                        &str_arg_or(args, "claim_mode", "manual"),
                    ));
                }
                let kind = str_arg_or(args, "kind", "handoff");
                Ok(self.prepare_commit_handoff(args, &kind))
            }
            _ => Ok(json!({
                "content": [{ "type": "text", "text": format!("未知工具：{name}") }],
                "isError": true
            })),
        }
    }

    fn handle_turns_tool(&mut self, args: &Map<String, Value>) -> Value {
        let cid = str_arg(args, "conversation_id");
        let limit = u64_arg(args, "limit", 30) as usize;
        let detail = self.engine.get_session_turns(&cid, limit);
        if let Some(ref err) = detail.error {
            return json!({
                "content": [{ "type": "text", "text": err }],
                "structuredContent": detail,
                "isError": true
            });
        }
        let short: String = detail.conversation_id.chars().take(8).collect();
        let text = format!("会话 {short} 最近 {} 轮明细", detail.turns.len());
        json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": detail,
            "isError": false
        })
    }

    fn read_resource(&mut self, params: &Map<String, Value>) -> Result<Value, (i64, String)> {
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        if uri != PANEL_URI && uri != HANDOFF_URI {
            return Err((-32602, format!("Unknown resource URI: {uri}")));
        }

        let panel_path = self.resolve_panel_path();

        let html = if uri == PANEL_URI {
            let snap = match self.get_fresh_default_snapshot() {
                Some(s) => s,
                None => {
                    let mut s = self.engine.build_overview_snapshot(25, false, false, "", None);
                    self.attach_grant_and_rehash(&mut s);
                    s.stale = false;
                    let rev = self.swr.grants_rev();
                    self.swr.put_cached(s.clone(), rev);
                    s
                }
            };

            let (template, mtime_ns, size) = self
                .swr
                .load_html(&panel_path)
                .map_err(|e| (-32603, e))?;

            if let Some(cached_html) =
                self.swr
                    .get_rendered_panel(&panel_path, mtime_ns, size, &snap.digest, false)
            {
                cached_html
            } else {
                let boot_val = serde_json::to_value(&snap).unwrap_or(Value::Null);
                let rendered = inject_boot_html(&template, &boot_val);
                self.swr.put_rendered_panel(
                    panel_path,
                    mtime_ns,
                    size,
                    snap.digest,
                    false,
                    rendered.clone(),
                );
                rendered
            }
        } else {
            let (template, _, _) = self
                .swr
                .load_html(&panel_path)
                .map_err(|e| (-32603, e))?;
            inject_boot_html(&template, &json!({ "view": "handoff" }))
        };

        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": APP_MIME,
                "text": html,
                "_meta": { "ui": { "prefersBorder": false } }
            }]
        }))
    }
}

fn str_arg(args: &Map<String, Value>, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn str_arg_or(args: &Map<String, Value>, key: &str, default: &str) -> String {
    let s = str_arg(args, key);
    if s.is_empty() {
        default.to_string()
    } else {
        s
    }
}

fn bool_arg(args: &Map<String, Value>, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn u64_arg(args: &Map<String, Value>, key: &str, default: u64) -> u64 {
    args.get(key)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().filter(|&n| n >= 0).map(|n| n as u64))
        })
        .unwrap_or(default)
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

/// Run line-delimited JSON-RPC 2.0 MCP daemon over stdio with immediate background pre-warm + SWR.
pub fn run_stdio_daemon(
    panel_override: Option<PathBuf>,
    plugin_data_dir: Option<PathBuf>,
) -> io::Result<()> {
    let mut daemon = McpDaemon::new_with_plugin_data(None, None, plugin_data_dir, panel_override);
    // Start immediate background pre-warm (HTML + read-only SQLite summary & sessions snapshot + SSR cache)
    // without blocking stdin JSON-RPC handshake.
    daemon.start_prewarm();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    let mut line = String::with_capacity(4096);

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(trimmed) {
            Ok(msg) => {
                if msg.is_object() {
                    daemon.handle_request(&msg)
                } else {
                    None
                }
            }
            Err(e) => Some(rpc_err(&Value::Null, -32700, &format!("Parse error: {e}"))),
        };
        if let Some(reply) = resp {
            serde_json::to_writer(&mut writer, &reply)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }

    daemon.close();
    Ok(())
}

/// Helper for unit/integration tests to verify panel HTML path resolution.
pub fn panel_path_exists(p: &Path) -> bool {
    p.is_file()
}
