//! JSON-RPC 2.0 MCP stdio server with startup pre-warm, zero-block SWR cache, SSR boot-data injection, scoped relay grants, and digest deduplication.

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
    default_grants_path_in, sanitize_scratch_note_path_for_lang, RelayGrantStore,
};
use crate::engine::pruner::distill_slice_with_lang;
use crate::engine::telemetry::{compute_snapshot_digest, OverviewSnapshot, TelemetryEngine};
use crate::i18n::select_display_cost;
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
    scope_cid: Option<&str>,
    scope_ws_override: Option<&str>,
) {
    let settings = grants.get_settings();
    snap.settings = Some(settings.clone());
    snap.pending_grant = None;

    let daemon_ws = engine.effective_daemon_workspace();
    let eff_ws = scope_ws_override
        .filter(|s| !s.trim().is_empty())
        .or(daemon_ws.as_deref());

    if let Some(mut pending) = grants.get_pending_grant_scoped(scope_cid, eff_ws, None) {
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
                    let mut snap = bg_engine.build_overview_snapshot(25, false, false, "", None);
                    attach_grant_and_rehash_inner(bg_engine, bg_grants, &mut snap, None, None);
                    let rev_after = hub.grants_rev();
                    if rev_after != rev_before {
                        attach_grant_and_rehash_inner(bg_engine, bg_grants, &mut snap, None, None);
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

    fn attach_grant_and_rehash(
        &mut self,
        snap: &mut OverviewSnapshot,
        scope_cid: Option<&str>,
        scope_ws: Option<&str>,
    ) {
        attach_grant_and_rehash_inner(
            &mut self.engine,
            &mut self.grants,
            snap,
            scope_cid,
            scope_ws,
        );
    }

    fn on_grants_mutated(&mut self) {
        let new_rev = self.swr.bump_grants_rev();
        if let Some(mut cached) = self.swr.get_cached() {
            self.attach_grant_and_rehash(&mut cached.snap, None, None);
            if let Ok(mut g) = self.swr.cached.write() {
                cached.grants_rev = new_rev;
                *g = Some(cached);
            }
        }
    }

    fn get_fresh_default_snapshot(&mut self) -> Option<OverviewSnapshot> {
        let mut cached = self.swr.get_cached()?;
        if cached.built_at.elapsed() >= SWR_STALE_TTL {
            return None;
        }
        let active_lang = self.engine.rates.get_lang().as_str();
        if cached.snap.lang != active_lang {
            return None;
        }
        let cur_rev = self.swr.grants_rev();
        if cached.grants_rev != cur_rev {
            self.attach_grant_and_rehash(&mut cached.snap, None, None);
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
        hint_ws: &str,
        claim_mode: &str,
    ) -> Value {
        let grant_rec = self.grants.find_grant(grant_id);
        let cid = if !hint_cid.is_empty() {
            hint_cid.to_string()
        } else {
            grant_rec
                .as_ref()
                .map(|g| g.conversation_id.clone())
                .unwrap_or_default()
        };

        let daemon_ws = self.engine.effective_daemon_workspace();
        let expected_ws = if !hint_ws.is_empty() {
            Some(hint_ws)
        } else {
            daemon_ws.as_deref()
        };
        let expected_cid = if !hint_cid.is_empty() {
            Some(hint_cid)
        } else {
            None
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
        let outcome = self.grants.claim_grant_scoped(
            grant_id,
            expected_cid,
            expected_ws,
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
        let lang = self.engine.rates.get_lang();
        let zh = lang.is_zh();
        match act {
            "set_prefs" => {
                let allow = args.get("allow_auto_handoff").and_then(Value::as_bool);
                let lang_pref = args.get("lang").and_then(Value::as_str);
                let updated = self.grants.update_settings_with_lang(allow, lang_pref);
                self.on_grants_mutated();
                let msg = if zh {
                    format!(
                        "已更新面板偏好：allow_auto_handoff={}",
                        updated.allow_auto_handoff
                    )
                } else {
                    format!(
                        "Updated panel preferences: allow_auto_handoff={}",
                        updated.allow_auto_handoff
                    )
                };
                json!({
                    "content": [{ "type": "text", "text": msg }],
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
                &str_arg(args, "workspace"),
                &str_arg_or(args, "claim_mode", "manual"),
            ),
            "distill_slice" => {
                let cid_arg = str_arg(args, "conversation_id");
                let target = self.engine.resolve_session_fast(&cid_arg, None);
                let cid = target
                    .as_ref()
                    .map(|t| t.id.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(cid_arg);
                if cid.is_empty() {
                    return json!({
                        "content": [{ "type": "text", "text": if zh { "未找到目标会话" } else { "Target session not found" } }],
                        "isError": true
                    });
                }
                let turns = self.engine.ledger.read_turns(&cid, None);
                let start_step = args.get("start_step").and_then(Value::as_i64);
                let end_step = args.get("end_step").and_then(Value::as_i64);
                let root = self.engine.ledger.data_root.clone();
                let res = distill_slice_with_lang(
                    &root,
                    &cid,
                    &turns,
                    "distill",
                    start_step,
                    end_step,
                    true,
                    8,
                    &mut self.engine.ledger.journal_cache,
                    lang,
                );
                let is_err = !res.found;
                let txt = if is_err {
                    res.error
                        .clone()
                        .unwrap_or_else(|| if zh { "提炼失败".into() } else { "Distillation failed".into() })
                } else {
                    res.saved_note_path.clone().unwrap_or_default()
                };
                json!({
                    "content": [{ "type": "text", "text": txt }],
                    "structuredContent": res,
                    "isError": is_err
                })
            }
            "create_handoff" => {
                let mut synth = args.clone();
                let cid_arg = str_arg(args, "conversation_id");
                if str_arg(args, "preserve_points").is_empty() {
                    let target = self.engine.resolve_session_fast(&cid_arg, None);
                    let title = target
                        .as_ref()
                        .map(|t| t.title.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("current session");
                    let default_pts = if zh {
                        format!("保留会话《{title}》当前进展、已修改文件、验证结果与工程约束")
                    } else {
                        format!("Preserve progress, modified files, verification state, and constraints from '{title}'")
                    };
                    synth.insert("preserve_points".into(), Value::String(default_pts));
                }
                if !synth.contains_key("auto_fire") {
                    synth.insert("auto_fire".into(), Value::Bool(false));
                }
                let kind = str_arg_or(&synth, "kind", "handoff");
                self.prepare_commit_handoff(&synth, &kind)
            }
            _ => json!({
                "content": [{ "type": "text", "text": if zh { format!("未知 UI 操作：{act}") } else { format!("Unknown UI action: {act}") } }],
                "isError": true
            }),
        }
    }

    fn prepare_commit_handoff(&mut self, args: &Map<String, Value>, kind: &str) -> Value {
        let lang = self.engine.rates.get_lang();
        let zh = lang.is_zh();
        let preserve = str_arg(args, "preserve_points");
        if preserve.is_empty() {
            return json!({
                "content": [{ "type": "text", "text": if zh { "preserve_points 不能为空" } else { "preserve_points must not be empty" } }],
                "isError": true
            });
        }
        let raw_scratch = str_arg(args, "scratch_note_path");
        let scratch_path = match sanitize_scratch_note_path_for_lang(&raw_scratch, lang) {
            Ok(p) => p,
            Err(err) => {
                return json!({
                    "content": [{ "type": "text", "text": err }],
                    "isError": true
                });
            }
        };

        let cid_arg = str_arg(args, "conversation_id");
        let target = self.engine.resolve_session_fast(&cid_arg, None);

        // Guard against ambiguous multi-window `commit_handoff` when conversation_id was omitted
        // and >=2 main conversations are concurrently RUNNING in the same workspace scope.
        if cid_arg.is_empty() && target.as_ref().map(|t| !t.is_subagent).unwrap_or(true) {
            let concurrent_mains = self.engine.ambiguous_running_mains_in_scope(None);
            if concurrent_mains.len() >= 2 {
                let joined = concurrent_mains.join(", ");
                let reason = if zh {
                    format!(
                        "检测到同一工作区有多个主会话同时运行（{joined}）；请显式传入 conversation_id 以免签发到错误窗口。"
                    )
                } else {
                    format!(
                        "Multiple main sessions are concurrently running in this workspace ({joined}); pass conversation_id explicitly to avoid targeting the wrong chat."
                    )
                };
                let blocked = json!({
                    "mode": "handoff_blocked",
                    "kind": kind,
                    "blocked": true,
                    "conversation_id": "",
                    "current_context_tokens": 0,
                    "running_subagent_count": 0,
                    "reasons": [reason],
                });
                return json!({
                    "content": [{ "type": "text", "text": reason }],
                    "structuredContent": blocked,
                    "isError": false
                });
            }
        }

        let rem_turns = u64_arg(args, "estimated_remaining_turns", 12) as usize;
        let gate = evaluate_handoff_gate(target.as_ref(), rem_turns, &mut self.engine.rates);

        if !gate.allowed {
            let blocked = json!({
                "mode": "handoff_blocked",
                "kind": kind,
                "blocked": true,
                "conversation_id": target.as_ref().map(|t| t.id.as_str()).unwrap_or(""),
                "workspace": target.as_ref().map(|t| t.workspace.as_str()).unwrap_or(""),
                "current_context_tokens": target.as_ref().map(|t| t.latest_context_tokens).unwrap_or(0),
                "running_subagent_count": target.as_ref().map(|t| t.running_subagent_count).unwrap_or(0),
                "reasons": gate.reasons,
                "profile": gate.profile
            });
            let msg_txt = if zh {
                format!(
                    "【服务端校验拦截：暂不执行全量压缩/接力】\n- {}\n请继续复用当前热前缀执行任务，或调用 context_radar_hotspots 将早期片段旁路提炼到 scratch/。",
                    gate.reasons.join("\n- ")
                )
            } else {
                format!(
                    "[Handoff Gate Blocked]\n- {}\nKeep using the warm KV prefix, or call context_radar_hotspots to distill older slices to scratch/.",
                    gate.reasons.join("\n- ")
                )
            };
            return json!({
                "content": [{ "type": "text", "text": msg_txt }],
                "structuredContent": blocked,
                "isError": false
            });
        }

        let cid = target.as_ref().map(|t| t.id.clone()).unwrap_or_default();
        let workspace = target
            .as_ref()
            .map(|t| t.workspace.clone())
            .unwrap_or_default();
        let workspaces = target
            .as_ref()
            .map(|t| t.workspaces.clone())
            .unwrap_or_default();
        let title = target.as_ref().map(|t| t.title.clone()).unwrap_or_default();
        let turns_count = target.as_ref().map(|t| t.turns).unwrap_or(0);
        let ckpt_count = target.as_ref().map(|t| t.checkpoint_count).unwrap_or(0);
        let running_subs = target
            .as_ref()
            .map(|t| t.running_subagent_count)
            .unwrap_or(0);
        let resume_task = str_arg(args, "resume_task");
        let auto_fire = bool_arg(args, "auto_fire", true);

        let grant = self.grants.create_grant_scoped(
            &preserve,
            &resume_task,
            auto_fire,
            &cid,
            &workspace,
            &workspaces,
            turns_count,
            ckpt_count,
            kind,
            &scratch_path,
            &title,
            lang,
        );
        self.on_grants_mutated();
        let settings = self.grants.get_settings();
        let allow_auto = settings.allow_auto_handoff;

        let card = json!({
            "mode": "handoff_card",
            "kind": kind,
            "blocked": false,
            "lang": lang.as_str(),
            "conversation_id": cid,
            "workspace": grant.workspace,
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

        let guide = if zh {
            let auto_line = if allow_auto && grant.auto_fire {
                "面板「自动接力」已开启且 running_subagent_count == 0：当本工具内联卡片或同工作区 Context Radar 侧边栏处于可见挂载状态时，前端会自动核销票据并通过官方 target:new 拉起新会话；若当前在无界面的 agy CLI、折叠卡片或后台标签页中运行，请展开卡片/侧边栏点击拉起，或直接复制下方 `/compact` 指令。"
            } else {
                "本次未启用自动拉起（auto_fire=false 或面板开关已关闭）：已保留当前会话窗口，可在卡片或侧边栏复制 `/compact` 指令，或手动点击拉起新会话。"
            };
            format!(
                "已通过模型档位、冷却、回本轮数与 running_subagent_count == 0 校验，票据已绑定至当前会话与工作区。\n- `/compact` 指令：`{}`\n- 官方接力说明：{auto_line}",
                grant.compact_command
            )
        } else {
            let auto_line = if allow_auto && grant.auto_fire {
                "Auto-handoff is enabled and running_subagent_count == 0: when the inline handoff card or a matching-workspace Context Radar side pane is mounted and visible, the webview will claim this single-use grant and launch a background chat via official `target:new`. Note: in headless `agy` CLI, collapsed tool cards, or hidden tabs, `target:new` waits until a UI pane opens—expand the card/pane or paste the `/compact` command below."
            } else {
                "Auto-launch is off for this grant (`auto_fire=false` or paused in panel): the current window stays active; copy the `/compact` command or click 'Launch New Chat' in the panel when ready."
            };
            format!(
                "Passed all model-tier, cooldown, break-even, and running_subagent_count == 0 gates (grant scoped to this conversation and workspace).\n- Ready `/compact` command: `{}`\n- Relay status: {auto_line}",
                grant.compact_command
            )
        };
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
                    "set_prefs" | "record_relay" | "dismiss_grant" | "claim_grant"
                    | "distill_slice" | "create_handoff" => Ok(self.handle_ui_action(&act, args)),
                    "turns" => Ok(self.handle_turns_tool(args)),
                    _ => {
                        let t0 = Instant::now();
                        let show_history = bool_arg(args, "show_history", false);
                        let include_archived = bool_arg(args, "include_archived", false);
                        let limit = u64_arg(args, "limit", 25) as usize;
                        let ws_filter = str_arg(args, "workspace_filter");
                        let cid_scope = str_arg(args, "conversation_id");
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
                            && ws_filter.is_empty()
                            && cid_scope.is_empty();

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

                        if is_default_query && !if_digest.is_empty() && !fresh {
                            let cur_lang = self.engine.rates.get_lang().as_str();
                            if let Some(cached) = self.swr.get_cached() {
                                if cached.grants_rev == self.swr.grants_rev()
                                    && cached.snap.lang == cur_lang
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
                        let cid_opt = if cid_scope.is_empty() {
                            None
                        } else {
                            Some(cid_scope.as_str())
                        };
                        let ws_opt = if ws_filter.is_empty() {
                            None
                        } else {
                            Some(ws_filter.as_str())
                        };
                        self.attach_grant_and_rehash(&mut snap, cid_opt, ws_opt);

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
                let lang = self.engine.rates.get_lang();
                let zh = lang.is_zh();
                let cid_arg = str_arg(args, "conversation_id");
                let target = self.engine.resolve_session_fast(&cid_arg, None);
                let cid = target
                    .as_ref()
                    .map(|t| t.id.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(cid_arg);
                if cid.is_empty() {
                    return Ok(json!({
                        "content": [{ "type": "text", "text": if zh { "未找到目标会话" } else { "Target session not found" } }],
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
                let res = distill_slice_with_lang(
                    &root,
                    &cid,
                    &turns,
                    &mode,
                    start_step,
                    end_step,
                    save_note,
                    top_k,
                    &mut self.engine.ledger.journal_cache,
                    lang,
                );
                if !res.found {
                    return Ok(json!({
                        "content": [{ "type": "text", "text": res.error.clone().unwrap_or_else(|| if zh { "提炼失败".into() } else { "Distillation failed".into() }) }],
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
                            if zh {
                                format!(
                                    "  · 轮 #{} (step {}，归因步 {}–{}): +{} → {} | {}",
                                    h.turn,
                                    h.step_idx,
                                    h.cause_step_range[0],
                                    h.cause_step_range[1],
                                    fmt_k(h.delta_tokens),
                                    fmt_k(h.context_after),
                                    h.cause
                                )
                            } else {
                                format!(
                                    "  · Turn #{} (step {}, caused by steps {}–{}): +{} → {} | {}",
                                    h.turn,
                                    h.step_idx,
                                    h.cause_step_range[0],
                                    h.cause_step_range[1],
                                    fmt_k(h.delta_tokens),
                                    fmt_k(h.context_after),
                                    h.cause
                                )
                            }
                        })
                        .collect();
                    if zh {
                        format!(
                            "会话 {cid} 上下文膨胀热点（活跃步 {}–{}，建议旁路提炼区间 start_step={}, end_step={}）：\n{}",
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
                        format!(
                            "Context bloat hotspots for {cid} (active steps {}–{}, recommended slice start_step={}, end_step={}):\n{}",
                            active_rng[0],
                            active_rng[1],
                            rec_rng[0],
                            rec_rng[1],
                            if rows.is_empty() {
                                "  · No significant single-turn spikes since last checkpoint".to_string()
                            } else {
                                rows.join("\n")
                            }
                        )
                    }
                } else {
                    let mut s = res.digest.clone().unwrap_or_default();
                    if let Some(p) = res.saved_note_path.as_deref().filter(|x| !x.is_empty()) {
                        if zh {
                            s.push_str(&format!("\n- 已存备忘：{p}"));
                        } else {
                            s.push_str(&format!("\n- Saved note: {p}"));
                        }
                    }
                    if let Some(h) = res.handoff_hint.as_deref().filter(|x| !x.is_empty()) {
                        if zh {
                            s.push_str(&format!("\n- 复用建议：{h}"));
                        } else {
                            s.push_str(&format!("\n- Next step: {h}"));
                        }
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
                let zh = plan.lang == "zh-CN";
                let prof = plan.profile.as_ref().unwrap();
                let rcp = plan.partial_compaction_recipes.as_ref().unwrap();
                let sym = if plan.currency_symbol.is_empty() {
                    if zh { "¥" } else { "$" }
                } else {
                    plan.currency_symbol.as_str()
                };
                let ovh = select_display_cost(
                    plan.compact_overhead_usd,
                    plan.compact_overhead_cny,
                    &plan.currency_code,
                );
                let sav = select_display_cost(
                    plan.per_turn_saving_usd,
                    plan.per_turn_saving_cny,
                    &plan.currency_code,
                );
                let econ_line = if plan.pricing_known {
                    if zh {
                        format!(
                            "冷缓存+摘要约 {sym}{ovh:.2}，后续每轮省 {sym}{sav:.3}，需 >= {} 轮回本。",
                            plan.break_even_turns
                        )
                    } else {
                        format!(
                            "compaction + rebuild ~{sym}{ovh:.2}, saves ~{sym}{sav:.3}/turn, break-even >= {} turns.",
                            plan.break_even_turns
                        )
                    }
                } else if zh {
                    format!(
                        "模型单价未配置，按 KV 缓存物理模型估算需 >= {} 轮回本。",
                        plan.break_even_turns
                    )
                } else {
                    format!(
                        "unpriced model; token-physics break-even >= {} turns.",
                        plan.break_even_turns
                    )
                };

                let txt = if zh {
                    format!(
                        "{}\n- 会话：{} ({}) | {} | 档位：{}\n- 上下文：{} / {}（下限 {} · 推荐 {} · 紧急 {}）\n- 回本估算：{econ_line}\n- 切片摘要：{}\n- 分支隔离：{}\n- 接力新会话：{}",
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
                        rcp.past_slice_distill,
                        rcp.future_branch_fold,
                        rcp.official_handoff_new
                    )
                } else {
                    format!(
                        "{}\n- Session: {} ({}) | {} | Tier: {}\n- Context: {} / {} (floor {} · target {} · urgent {})\n- Break-even: {econ_line}\n- Slice notes: {}\n- Branch isolation: {}\n- Session handoff: {}",
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
                        rcp.past_slice_distill,
                        rcp.future_branch_fold,
                        rcp.official_handoff_new
                    )
                };
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
                        &str_arg(args, "workspace"),
                        &str_arg_or(args, "claim_mode", "manual"),
                    ));
                }
                let kind = str_arg_or(args, "kind", "handoff");
                Ok(self.prepare_commit_handoff(args, &kind))
            }
            _ => {
                let zh = self.engine.rates.get_lang().is_zh();
                Ok(json!({
                    "content": [{ "type": "text", "text": if zh { format!("未知工具：{name}") } else { format!("Unknown tool: {name}") } }],
                    "isError": true
                }))
            }
        }
    }

    fn handle_turns_tool(&mut self, args: &Map<String, Value>) -> Value {
        let zh = self.engine.rates.get_lang().is_zh();
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
        let mid_note = detail
            .mid_loop_breakdown
            .as_ref()
            .filter(|m| m.slow_turn_self_expiry > 0 || m.replica_spillover > 0 || m.model_switch_cold > 0)
            .map(|m| {
                if zh {
                    format!(
                        "（中途断缓：慢轮 {} / 副本溢出 {} / 切换模型 {}）",
                        m.slow_turn_self_expiry, m.replica_spillover, m.model_switch_cold
                    )
                } else {
                    format!(
                        " (mid-loop: slow-turn {}, replica-spillover {}, model-switch {})",
                        m.slow_turn_self_expiry, m.replica_spillover, m.model_switch_cold
                    )
                }
            })
            .unwrap_or_default();
        let text = if zh {
            format!("会话 {short} 最近 {} 轮明细{mid_note}", detail.turns.len())
        } else {
            format!("Last {} turn(s) for session {short}{mid_note}", detail.turns.len())
        };
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
                    self.attach_grant_and_rehash(&mut s, None, None);
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
            let lang = self.engine.rates.get_lang().as_str();
            inject_boot_html(&template, &json!({ "view": "handoff", "lang": lang }))
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

pub fn run_stdio_daemon(
    panel_override: Option<PathBuf>,
    plugin_data_dir: Option<PathBuf>,
) -> io::Result<()> {
    let mut daemon = McpDaemon::new_with_plugin_data(None, None, plugin_data_dir, panel_override);
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

pub fn panel_path_exists(p: &Path) -> bool {
    p.is_file()
}
