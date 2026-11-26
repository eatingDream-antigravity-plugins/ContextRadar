//! JSON-RPC 2.0 MCP stdio server with startup pre-warm, zero-block SWR cache, SSR boot-data injection, scoped relay grants, and digest deduplication.

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::engine::budget::{evaluate_handoff_gate, prepare_handoff_plan};
use crate::engine::grants::{
    default_grants_path_in, is_acp_proxy_active, paths_overlap,
    sanitize_scratch_note_path_for_lang, workspaces_overlap, ClaimOutcome, GrantFileSignature,
    RelayGrantStore,
};
use crate::engine::pruner::distill_slice_with_lang;
use crate::engine::telemetry::{
    compute_snapshot_digest, now_epoch_sec, OverviewSnapshot, TelemetryEngine,
};
use crate::i18n::{parse_lang_tag, select_display_cost};
use crate::rpc::tools::{
    fmt_k, format_live_status_tag, resources_spec, summarize_snapshot_text, tools_spec, APP_MIME,
    HANDOFF_URI, PANEL_URI,
};
use crate::settings::{expand_tilde, PANEL_HTML_ENV};

pub const SERVER_NAME: &str = "context-radar-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2025-11-25", // Latest supported version.
    "2025-06-18",
    "2025-03-26", // Antigravity clients.
    "2024-11-05",
];
pub const PROTOCOL_VERSION: &str = SUPPORTED_PROTOCOL_VERSIONS[0];
const APPS_EXTENSION: &str = "io.modelcontextprotocol/ui";
const MAX_HTML_BYTES: usize = 10 * 1024 * 1024;
const BOOT_MARKER: &str = "/*__CONTEXT_RADAR_BOOT__*/";
/// Short burst-coalescing TTL (paired with live disk-activity validation so any new LLM
/// request invalidates the cache immediately without waiting for TTL expiry).
pub const SWR_STALE_TTL: Duration = Duration::from_millis(150);
// Conditional polls can reuse a snapshot until a known transition or this safety limit.
const POLL_CACHE_MAX_AGE: Duration = Duration::from_secs(15);

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

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct DiskActivitySig {
    sum_mtime_ns: i128,
    sum_size: u64,
    wal_mtime_ns: i128,
    wal_size: u64,
    conv_mtime_ns: i128,
    annotations_mtime_ns: i128,
    active_wal_hash: u64,
}

fn quick_disk_activity_sig(data_root: &Path) -> DiskActivitySig {
    let stat_pair = |p: &Path| -> (i128, u64) {
        match fs::metadata(p) {
            Ok(m) => (
                (m.mtime() as i128) * 1_000_000_000 + (m.mtime_nsec() as i128),
                m.size(),
            ),
            Err(_) => (0, 0),
        }
    };
    let (sum_mtime_ns, sum_size) = stat_pair(&data_root.join("conversation_summaries.db"));
    let (wal_mtime_ns, wal_size) = stat_pair(&data_root.join("conversation_summaries.db-wal"));
    let conv_dir = data_root.join("conversations");
    let (conv_mtime_ns, _) = stat_pair(&conv_dir);
    let (annotations_mtime_ns, _) = stat_pair(&data_root.join("annotations"));
    // Also fold every active `.db-wal` (and `.db` mtime/size) so new LLM turns written
    // directly into an existing conversation SQLite WAL immediately invalidate burst cache
    // even when parent directory mtime does not change on APFS/ext4.
    let mut active_wal_hash: u64 = 0xcbf2_9ce4_8422_2325;
    if let Ok(entries) = fs::read_dir(&conv_dir) {
        for entry in entries.flatten() {
            let name_os = entry.file_name();
            let Some(name) = name_os.to_str() else {
                continue;
            };
            if name.ends_with(".db-wal") || name.ends_with(".db") {
                if let Ok(m) = entry.metadata() {
                    let ns = (m.mtime() as i128) * 1_000_000_000 + (m.mtime_nsec() as i128);
                    let mix = (ns as u64) ^ m.size().rotate_left(17) ^ (m.ino().rotate_left(7));
                    // Directory enumeration order is unspecified.
                    active_wal_hash = active_wal_hash
                        .wrapping_add((mix ^ (mix >> 32)).wrapping_mul(0x100000001b3));
                }
            }
        }
    }
    DiskActivitySig {
        sum_mtime_ns,
        sum_size,
        wal_mtime_ns,
        wal_size,
        conv_mtime_ns,
        annotations_mtime_ns,
        active_wal_hash,
    }
}

#[derive(PartialEq, Eq)]
struct SnapshotInputs {
    disk: DiskActivitySig,
    grants: Option<GrantFileSignature>,
    settings_generation: u64,
}

impl SnapshotInputs {
    fn capture(engine: &mut TelemetryEngine, grants: &RelayGrantStore) -> Self {
        Self {
            disk: quick_disk_activity_sig(&engine.ledger.data_root),
            grants: grants.disk_signature(),
            settings_generation: engine.rates.current_settings().generation,
        }
    }
}

struct CachedDefaultSnap {
    snap: Arc<OverviewSnapshot>,
    built_at: Instant,
    inputs: SnapshotInputs,
}

struct SwrHub {
    cached: RwLock<Option<CachedDefaultSnap>>,
    html: RwLock<Option<HtmlCache>>,
    rendered_panel: RwLock<Option<RenderedPanelCache>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl SwrHub {
    fn new() -> Self {
        Self {
            cached: RwLock::new(None),
            html: RwLock::new(None),
            rendered_panel: RwLock::new(None),
            worker: Mutex::new(None),
        }
    }

    fn valid_snapshot(
        &self,
        inputs: &SnapshotInputs,
        lang: &str,
        max_age: Duration,
        now: f64,
    ) -> Option<Arc<OverviewSnapshot>> {
        let guard = self.cached.read().ok()?;
        let cached = guard.as_ref()?;
        (cached.built_at.elapsed() < max_age
            && now < cached.snap.refresh_at
            && cached.snap.lang == lang
            && &cached.inputs == inputs)
            .then(|| Arc::clone(&cached.snap))
    }

    fn put_cached(&self, snap: Arc<OverviewSnapshot>, inputs: SnapshotInputs, built_at: Instant) {
        if now_epoch_sec() >= snap.refresh_at {
            return;
        }
        if let Ok(mut guard) = self.cached.write() {
            // A background pre-warm can finish after a synchronous build.
            if guard
                .as_ref()
                .is_none_or(|cached| cached.built_at <= built_at)
            {
                *guard = Some(CachedDefaultSnap {
                    snap,
                    inputs,
                    built_at,
                });
            }
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
        if let Some(deadline) = pending.display_until() {
            snap.refresh_at = snap.refresh_at.min(deadline);
        }
        let cid = pending.conversation_id.clone();
        let mut telemetry_ok = true;
        let mut valid = true;
        if !cid.is_empty() && !pending.claimed {
            match engine.try_get_turn_and_checkpoint_counts(&cid) {
                Ok((cur_turns, cur_ckpts)) => {
                    valid = cur_ckpts <= pending.created_checkpoints
                        && (pending.created_turns == 0
                            || cur_turns.saturating_sub(pending.created_turns) <= 4);
                }
                Err(_) => {
                    telemetry_ok = false;
                }
            }
        }
        if valid {
            let live_subs = if !cid.is_empty() {
                match engine.try_get_running_subagent_count(&cid, None) {
                    Ok(n) => Some(n),
                    Err(_) => {
                        telemetry_ok = false;
                        None
                    }
                }
            } else {
                Some(0)
            };
            pending.running_subagent_count = live_subs;
            pending.allow_auto_handoff = Some(settings.allow_auto_handoff && telemetry_ok);
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
    initialized: bool,
    protocol_version: String,
    client_supports_apps: bool,
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
        let mut eng = engine.unwrap_or_else(|| {
            TelemetryEngine::new_with_plugin_data(None, plugin_data_dir.clone(), None, None)
        });
        let mut gstore = grants.unwrap_or_else(|| {
            RelayGrantStore::new(default_grants_path_in(
                plugin_data_dir.as_deref(),
                Some(&eng.ledger.data_root),
            ))
        });
        eng.rates
            .set_lang_override(parse_lang_tag(&gstore.get_settings().lang));
        Self {
            engine: eng,
            grants: gstore,
            plugin_data_dir,
            panel_html_override,
            swr: Arc::new(SwrHub::new()),
            initialized: false,
            protocol_version: PROTOCOL_VERSION.to_string(),
            client_supports_apps: false,
        }
    }

    fn sync_panel_lang(&mut self) {
        let lang_pref = self.grants.get_settings().lang;
        self.engine
            .rates
            .set_lang_override(parse_lang_tag(&lang_pref));
    }

    /// Run a one-shot background startup pre-warm so initial panel opens hit RAM immediately,
    /// then immediately close and drop the temporary `bg_engine`/`bg_grants` handles.
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
                bg_engine
                    .rates
                    .set_lang_override(parse_lang_tag(&bg_grants.get_settings().lang));

                let panel_path = panel_override
                    .clone()
                    .unwrap_or_else(default_panel_html_path);
                let html_loaded = hub.load_html(&panel_path).ok();

                let built_at = Instant::now();
                let inputs = SnapshotInputs::capture(&mut bg_engine, &bg_grants);
                let mut snap = bg_engine.build_overview_snapshot(25, false, false, "", None);
                attach_grant_and_rehash_inner(
                    &mut bg_engine,
                    &mut bg_grants,
                    &mut snap,
                    None,
                    None,
                );
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
                hub.put_cached(Arc::new(snap), inputs, built_at);

                bg_engine.close();
                drop(bg_grants);
                drop(bg_engine);
            });

        if let Ok(h) = handle {
            *slot = Some(h);
        }
    }

    pub fn swr_is_warm(&self) -> bool {
        self.swr.cached.read().is_ok_and(|cache| cache.is_some())
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

    fn claim_with_live_check(
        &mut self,
        grant_id: &str,
        hint_cid: &str,
        hint_ws: &str,
        claim_mode: &str,
    ) -> Value {
        let is_auto = matches!(
            claim_mode,
            "auto_handoff" | "auto_compact" | "acp_auto_compact"
        );
        let is_manual = matches!(claim_mode, "manual" | "manual_handoff" | "manual_compact");
        if !is_auto && !is_manual {
            return claim_outcome_response(ClaimOutcome::denied("invalid_claim_mode"));
        }

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
        if let Some(dws) = daemon_ws.as_deref() {
            if !hint_ws.is_empty() && !paths_overlap(dws, hint_ws) {
                return claim_outcome_response(ClaimOutcome::denied("workspace_mismatch"));
            }
            if !hint_ws.is_empty() {
                if let Some(ref g) = grant_rec {
                    let has_grant_ws = !g.workspace.is_empty() || !g.workspaces.is_empty();
                    if has_grant_ws && !workspaces_overlap(&g.workspace, &g.workspaces, hint_ws) {
                        return claim_outcome_response(ClaimOutcome::denied("workspace_mismatch"));
                    }
                }
            }
        }
        let expected_ws = daemon_ws
            .as_deref()
            .or_else(|| (!hint_ws.is_empty()).then_some(hint_ws));
        let expected_cid = (!hint_cid.is_empty()).then_some(hint_cid);

        let (cur_turns, cur_ckpts) = if !cid.is_empty() {
            match self.engine.try_get_turn_and_checkpoint_counts(&cid) {
                Ok((t, c)) => (Some(t), Some(c)),
                Err(_) => {
                    return claim_outcome_response(ClaimOutcome::denied("telemetry_read_error"));
                }
            }
        } else {
            (None, None)
        };
        let running_subs = if !cid.is_empty() {
            match self.engine.try_get_running_subagent_count(&cid, None) {
                Ok(n) => n,
                Err(_) => {
                    return claim_outcome_response(ClaimOutcome::denied("telemetry_read_error"));
                }
            }
        } else {
            0
        };
        let outcome = self.grants.claim_grant_scoped(
            grant_id,
            expected_cid,
            expected_ws,
            cur_turns,
            cur_ckpts,
            Some(running_subs),
            is_auto,
        );
        claim_outcome_response(outcome)
    }

    fn handle_ui_action(&mut self, act: &str, args: &Map<String, Value>) -> Value {
        let lang = self.engine.rates.get_lang();
        let zh = lang.is_zh();
        match act {
            "set_prefs" => {
                let allow = args.get("allow_auto_handoff").and_then(Value::as_bool);
                let lang_pref = args.get("lang").and_then(Value::as_str);
                let Ok(updated) = self.grants.update_settings_with_lang(allow, lang_pref) else {
                    return grant_storage_error(zh);
                };
                self.engine
                    .rates
                    .set_lang_override(parse_lang_tag(&updated.lang));
                let zh = self.engine.rates.get_lang().is_zh();
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
            "record_compact" => {
                let res = self.grants.record_compact(
                    &str_arg(args, "grant_id"),
                    &str_arg_or(args, "status", "sent"),
                );
                let txt = if res.ok {
                    res.reason.clone().unwrap_or_else(|| "ok".to_string())
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
                let is_err = !res.found || res.error.is_some();
                let txt = if is_err {
                    res.error.clone().unwrap_or_else(|| {
                        if zh {
                            "提炼失败".into()
                        } else {
                            "Distillation failed".into()
                        }
                    })
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

        let auto_fire = bool_arg(args, "auto_fire", true);
        let acp_env_active = is_acp_proxy_active();
        let acp_strict_error = acp_env_active && kind == "compact" && auto_fire;

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
                    "isError": acp_strict_error
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
                "isError": acp_strict_error
            });
        }

        let Ok(settings) = self.grants.try_get_settings() else {
            return grant_storage_error(zh);
        };
        let allow_auto = settings.allow_auto_handoff;
        if acp_strict_error && !allow_auto {
            let reason = if zh {
                "面板「自动接力」开关当前已关闭（allow_auto_handoff=false）；已阻止 ACP 自动触发 `/compact`。"
            } else {
                "Auto-handoff is currently disabled in Context Radar preferences (allow_auto_handoff=false); blocking autonomous ACP `/compact`."
            };
            let blocked = json!({
                "mode": "handoff_blocked",
                "kind": kind,
                "blocked": true,
                "allow_auto_handoff": false,
                "conversation_id": target.as_ref().map(|t| t.id.as_str()).unwrap_or(""),
                "workspace": target.as_ref().map(|t| t.workspace.as_str()).unwrap_or(""),
                "current_context_tokens": target.as_ref().map(|t| t.latest_context_tokens).unwrap_or(0),
                "running_subagent_count": target.as_ref().map(|t| t.running_subagent_count).unwrap_or(0),
                "reasons": [reason],
            });
            return json!({
                "content": [{ "type": "text", "text": reason }],
                "structuredContent": blocked,
                "isError": true
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

        let Ok(grant) = self.grants.create_grant_scoped(
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
        ) else {
            return grant_storage_error(zh);
        };
        let acp_proxy_active = grant.acp_proxy_active;

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
            "acp_proxy_active": acp_proxy_active,
            "running_subagent_count": running_subs,
            "preserve_points": grant.preserve_points,
            "resume_task": grant.resume_task,
            "scratch_note_path": grant.scratch_note_path,
            "compact_command": grant.compact_command,
            "handoff_prompt": grant.handoff_prompt,
            "handoff_conversation_id": "",
            "compact_status": ""
        });

        let guide = if zh {
            let auto_line = if kind == "compact"
                && acp_proxy_active
                && allow_auto
                && grant.auto_fire
            {
                "检测到官方 ACP 自动压缩代理（acp-proxy / acp-run）已挂载且 running_subagent_count == 0：请立即结束本轮回复（不要再调用额外工具），代理将在本轮结束后通过官方 ACP `session/prompt` 原地执行下方 `/compact` 指令并无缝续跑。"
            } else if allow_auto && grant.auto_fire {
                "面板「自动接力」已开启且 running_subagent_count == 0：当本工具内联卡片或同工作区 Context Radar 侧边栏处于可见挂载状态时，前端会自动核销票据并触发官方接力/压缩流程；若当前在无界面的 agy CLI、折叠卡片或后台标签页中运行，请使用 `context-radar-mcp acp-proxy` / `acp-run`，或展开面板点击/复制下方 `/compact` 指令。"
            } else {
                "本次未启用自动触发（auto_fire=false 或面板开关已关闭）：已保留当前会话窗口，可在卡片或侧边栏一键执行/复制 `/compact` 指令，或手动点击拉起新会话。"
            };
            format!(
                "已通过模型档位、冷却、回本轮数与 running_subagent_count == 0 校验，票据已绑定至当前会话与工作区。\n- `/compact` 指令：`{}`\n- 官方执行说明：{auto_line}",
                grant.compact_command
            )
        } else {
            let auto_line = if kind == "compact"
                && acp_proxy_active
                && allow_auto
                && grant.auto_fire
            {
                "Official ACP auto-compact proxy (`acp-proxy` / `acp-run`) is active and running_subagent_count == 0: end this turn cleanly now (avoid further tool calls) so the proxy can execute native ACP `session/prompt` `/compact` in-place on the same session and resume automatically."
            } else if allow_auto && grant.auto_fire {
                "Auto-handoff is enabled and running_subagent_count == 0: when the inline card or matching-workspace Context Radar side pane is mounted and visible, the webview will claim this single-use grant automatically; in headless `agy` CLI or ACP editors, run via `context-radar-mcp acp-proxy` / `acp-run`, or copy the `/compact` command below."
            } else {
                "Auto-launch is off for this grant (`auto_fire=false` or paused in panel): the current window stays active; run or copy the `/compact` command or click 'Launch New Chat' in the panel when ready."
            };
            format!(
                "Passed all model-tier, cooldown, break-even, and running_subagent_count == 0 gates (grant scoped to this conversation and workspace).\n- Ready `/compact` command: `{}`\n- Dispatch status: {auto_line}",
                grant.compact_command
            )
        };
        json!({
            "content": [{ "type": "text", "text": guide }],
            "structuredContent": card,
            "isError": false
        })
    }

    fn available_tools(&self) -> Vec<Value> {
        let mut tools = tools_spec().as_array().cloned().unwrap_or_default();
        if !self.client_supports_apps {
            let app_only = json!(["app"]);
            tools.retain(|tool| tool["_meta"]["ui"]["visibility"] != app_only);
        }
        tools
    }

    fn is_tool_available(&self, name: &str) -> bool {
        self.available_tools()
            .iter()
            .any(|tool| tool["name"].as_str() == Some(name))
    }

    pub fn handle_message(&mut self, msg: &Value) -> Option<Value> {
        match msg {
            Value::Object(_) => self.handle_request(msg),
            Value::Array(items) => {
                if self.initialized
                    && !matches!(self.protocol_version.as_str(), "2025-03-26" | "2024-11-05")
                {
                    return Some(rpc_err(
                        &Value::Null,
                        -32600,
                        "JSON-RPC batching is not supported for this protocol version",
                    ));
                }
                if items.is_empty() {
                    return Some(rpc_err(
                        &Value::Null,
                        -32600,
                        "Invalid Request: empty batch",
                    ));
                }
                let mut responses = Vec::new();
                for item in items {
                    if item.is_object() {
                        if let Some(resp) = self.handle_request(item) {
                            responses.push(resp);
                        }
                    } else {
                        responses.push(rpc_err(&Value::Null, -32600, "Invalid Request"));
                    }
                }
                if responses.is_empty() {
                    None
                } else {
                    Some(Value::Array(responses))
                }
            }
            _ => Some(rpc_err(&Value::Null, -32600, "Invalid Request")),
        }
    }

    pub fn handle_request(&mut self, msg: &Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(rpc_err(&Value::Null, -32600, "Invalid Request"));
        };
        let req_id_opt = obj.get("id");
        let err_id = req_id_opt
            .filter(|id| id.is_string() || id.is_number())
            .unwrap_or(&Value::Null);
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(rpc_err(
                err_id,
                -32600,
                "Invalid Request: jsonrpc must be '2.0'",
            ));
        }
        let Some(method) = obj
            .get("method")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        else {
            return Some(rpc_err(
                err_id,
                -32600,
                "Invalid Request: method must be a non-empty string",
            ));
        };
        let req_id = req_id_opt?;
        if !(req_id.is_string() || req_id.is_number()) {
            return Some(rpc_err(
                &Value::Null,
                -32600,
                "Invalid Request: id must be a string or number",
            ));
        }
        let params = match obj.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(map)) => map.clone(),
            Some(_) => {
                return Some(rpc_err(req_id, -32602, "Invalid params: expected object"));
            }
        };

        match method {
            "initialize" => {
                if !self.initialized {
                    let ver = params
                        .get("protocolVersion")
                        .and_then(Value::as_str)
                        .filter(|ver| SUPPORTED_PROTOCOL_VERSIONS.contains(ver))
                        .unwrap_or(PROTOCOL_VERSION);
                    self.protocol_version = ver.to_string();
                    self.client_supports_apps = params
                        .get("capabilities")
                        .and_then(|caps| caps.get("extensions"))
                        .and_then(|extensions| extensions.get(APPS_EXTENSION))
                        .and_then(|extension| extension.get("mimeTypes"))
                        .and_then(Value::as_array)
                        .is_some_and(|types| {
                            types.iter().any(|mime| mime.as_str() == Some(APP_MIME))
                        });
                    self.initialized = true;
                }
                Some(rpc_ok(
                    req_id,
                    json!({
                        "protocolVersion": self.protocol_version,
                        "capabilities": {
                            "tools": { "listChanged": false },
                            "resources": { "subscribe": false, "listChanged": false }
                        },
                        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
                    }),
                ))
            }
            "ping" => Some(rpc_ok(req_id, json!({}))),
            "tools/list" => Some(rpc_ok(req_id, json!({ "tools": self.available_tools() }))),
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
        self.sync_panel_lang();
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| (-32602, "name must be a non-empty string".to_string()))?;
        if !self.is_tool_available(name) {
            return Err((-32602, format!("Unknown tool: {name}")));
        }
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

                        let inputs = is_default_query
                            .then(|| SnapshotInputs::capture(&mut self.engine, &self.grants));
                        let max_age = if if_digest.is_empty() {
                            SWR_STALE_TTL
                        } else {
                            POLL_CACHE_MAX_AGE
                        };
                        let cached = inputs.as_ref().filter(|_| !fresh).and_then(|inputs| {
                            self.swr.valid_snapshot(
                                inputs,
                                self.engine.rates.get_lang().as_str(),
                                max_age,
                                now_epoch_sec(),
                            )
                        });
                        let snap = if let Some(snap) = cached {
                            snap
                        } else {
                            let mut snap = self.engine.build_overview_snapshot(
                                limit,
                                show_history,
                                include_archived,
                                &ws_filter,
                                None,
                            );
                            let cid_opt = (!cid_scope.is_empty()).then_some(cid_scope.as_str());
                            let ws_opt = (!ws_filter.is_empty()).then_some(ws_filter.as_str());
                            self.attach_grant_and_rehash(&mut snap, cid_opt, ws_opt);
                            let snap = Arc::new(snap);
                            if let Some(inputs) = inputs {
                                self.swr.put_cached(Arc::clone(&snap), inputs, t0);
                            }
                            snap
                        };

                        let server_ms = t0.elapsed().as_millis() as u64;
                        if if_digest == snap.digest {
                            return Ok(json!({
                                "content": [{ "type": "text", "text": "unchanged" }],
                                "structuredContent": {
                                    "mode": "snapshot",
                                    "unchanged": true,
                                    "digest": snap.digest,
                                    "server_ms": server_ms
                                },
                                "isError": false
                            }));
                        }

                        let text = summarize_snapshot_text(&snap);
                        let mut structured = serde_json::to_value(snap.as_ref())
                            .map_err(|err| (-32603, err.to_string()))?;
                        structured["server_ms"] = json!(server_ms);
                        Ok(json!({
                            "content": [{ "type": "text", "text": text }],
                            "structuredContent": structured,
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
                if !res.found || res.error.is_some() {
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
                let plan =
                    prepare_handoff_plan(target.as_ref(), rem_turns, noisy, &mut self.engine.rates);
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
                    if zh {
                        "¥"
                    } else {
                        "$"
                    }
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
                let kind = str_arg_or(args, "kind", "handoff");
                Ok(self.prepare_commit_handoff(args, &kind))
            }
            _ => Err((-32602, format!("Unknown tool: {name}"))),
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
        let mut extra_parts: Vec<String> = Vec::new();
        if let Some(ref sess) = detail.session {
            let live_tag = format_live_status_tag(sess, zh);
            if !live_tag.is_empty()
                && (matches!(sess.live_status.as_str(), "error" | "waiting")
                    || sess.live_phase == "compacting")
            {
                extra_parts.push(live_tag.trim().to_string());
            }
            if sess.context_limit > 0 {
                extra_parts.push(if zh {
                    format!("余 {}", fmt_k(sess.remaining_context_tokens))
                } else {
                    format!("{} left", fmt_k(sess.remaining_context_tokens))
                });
            }
            if sess.total_thinking_tokens > 0 {
                extra_parts.push(if zh {
                    format!("思考 {:.0}%", sess.thinking_share_pct)
                } else {
                    format!("think {:.0}%", sess.thinking_share_pct)
                });
            }
            if sess.cross_model_compact {
                extra_parts.push(if zh {
                    "跨模型压缩".to_string()
                } else {
                    "cross-model compact".to_string()
                });
            }
        }
        let extra_note = if extra_parts.is_empty() {
            String::new()
        } else {
            format!(" | {}", extra_parts.join(" · "))
        };
        let mid_note = detail
            .mid_loop_breakdown
            .as_ref()
            .filter(|m| {
                m.slow_turn_self_expiry > 0 || m.replica_spillover > 0 || m.model_switch_cold > 0
            })
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
            format!(
                "会话 {short} 最近 {} 轮明细{extra_note}{mid_note}",
                detail.turns.len()
            )
        } else {
            format!(
                "Last {} turn(s) for session {short}{extra_note}{mid_note}",
                detail.turns.len()
            )
        };
        json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": detail,
            "isError": false
        })
    }

    fn read_resource(&mut self, params: &Map<String, Value>) -> Result<Value, (i64, String)> {
        self.sync_panel_lang();
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        if uri.is_empty() {
            return Err((-32602, "uri must be a non-empty string".to_string()));
        }
        if uri != PANEL_URI && uri != HANDOFF_URI {
            return Err((-32002, format!("Resource not found: {uri}")));
        }

        let panel_path = self.resolve_panel_path();

        let html = if uri == PANEL_URI {
            // Opening the panel ALWAYS computes a live synchronous incremental snapshot (<1 ms warm)
            // so initial SSR boot data never lags behind a just-completed LLM request.
            let built_at = Instant::now();
            let inputs = SnapshotInputs::capture(&mut self.engine, &self.grants);
            let mut snap = self
                .engine
                .build_overview_snapshot(25, false, false, "", None);
            self.attach_grant_and_rehash(&mut snap, None, None);
            snap.stale = false;
            let snap = Arc::new(snap);
            self.swr.put_cached(Arc::clone(&snap), inputs, built_at);

            let (template, mtime_ns, size) =
                self.swr.load_html(&panel_path).map_err(|e| (-32603, e))?;

            if let Some(cached_html) =
                self.swr
                    .get_rendered_panel(&panel_path, mtime_ns, size, &snap.digest, false)
            {
                cached_html
            } else {
                let boot_val = serde_json::to_value(snap.as_ref()).unwrap_or(Value::Null);
                let rendered = inject_boot_html(&template, &boot_val);
                self.swr.put_rendered_panel(
                    panel_path,
                    mtime_ns,
                    size,
                    snap.digest.clone(),
                    false,
                    rendered.clone(),
                );
                rendered
            }
        } else {
            let (template, _, _) = self.swr.load_html(&panel_path).map_err(|e| (-32603, e))?;
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

fn claim_outcome_response(outcome: ClaimOutcome) -> Value {
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

fn grant_storage_error(zh: bool) -> Value {
    let text = if zh {
        "授权存储写入失败，请重试。"
    } else {
        "Could not update grant storage. Retry the request."
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "reason": "storage_error" },
        "isError": true
    })
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
            Ok(msg) => daemon.handle_message(&msg),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::rates::RateTable;
    use crate::settings::SettingsWatcher;

    fn daemon(root: &Path) -> McpDaemon {
        let engine = TelemetryEngine::new(
            Some(root.to_path_buf()),
            Some(RateTable::new(
                SettingsWatcher::new(Some("none".into())),
                None,
                None,
            )),
            Some(None),
        );
        let grants = RelayGrantStore::new(root.join("plugin/relay_grants.json"));
        McpDaemon::new(Some(engine), Some(grants), None)
    }

    fn initialize(server: &mut McpDaemon, version: &str, capabilities: Value) -> Value {
        server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": version,
                    "capabilities": capabilities
                }
            }))
            .unwrap()
    }

    fn list_tools(server: &mut McpDaemon) -> Vec<Value> {
        let response = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/list"
            }))
            .unwrap();
        response["result"]["tools"].as_array().unwrap().clone()
    }

    #[test]
    fn initialize_negotiates_supported_protocol_versions() {
        let root = tempfile::tempdir().unwrap();
        for version in SUPPORTED_PROTOCOL_VERSIONS {
            let mut server = daemon(root.path());
            let response = initialize(&mut server, version, json!({}));
            assert_eq!(response["result"]["protocolVersion"], *version);
        }
        for version in ["2099-01-01", "arbitrary-version", ""] {
            let mut server = daemon(root.path());
            let response = initialize(&mut server, version, json!({}));
            assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
        }
    }

    #[test]
    fn tools_list_hides_app_only_tools_for_plain_text_hosts() {
        let root = tempfile::tempdir().unwrap();
        for caps in [
            json!({}),
            json!({ "extensions": {
            APPS_EXTENSION: { "mimeTypes": ["text/html"] }
        } }),
        ] {
            let mut server = daemon(root.path());
            initialize(&mut server, "2025-03-26", caps);
            let tools = list_tools(&mut server);
            let names: Vec<_> = tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect();
            assert_eq!(
                names,
                [
                    "context_radar_overview",
                    "context_radar_turns",
                    "context_radar_hotspots",
                    "context_radar_prepare_handoff",
                    "context_radar_commit_handoff"
                ]
            );
        }
    }

    #[test]
    fn ui_action_is_gated_on_apps_capability_and_frozen_after_first_initialize() {
        let root = tempfile::tempdir().unwrap();

        // 1. Plain session (no Apps capability): tools/call rejects context_radar_ui_action with -32602
        let mut plain = daemon(root.path());
        initialize(&mut plain, "2025-03-26", json!({}));
        let denied = plain
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "allow_auto_handoff": false } }
            }))
            .unwrap();
        assert_eq!(denied["error"]["code"], json!(-32602));
        assert!(plain.grants.get_settings().allow_auto_handoff);

        // 2. Repeated initialize CANNOT escalate client_supports_apps or alter protocol version
        let second_init = initialize(
            &mut plain,
            "2025-11-25",
            json!({ "extensions": { APPS_EXTENSION: { "mimeTypes": [APP_MIME] } } }),
        );
        assert_eq!(second_init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(list_tools(&mut plain).len(), 5);
        let still_denied = plain
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "allow_auto_handoff": false } }
            }))
            .unwrap();
        assert_eq!(still_denied["error"]["code"], json!(-32602));

        // 3. Legitimate SEP-1865 App host succeeds, and repeated initialize cannot downgrade it
        let mut app = daemon(root.path());
        initialize(
            &mut app,
            "2025-03-26",
            json!({ "extensions": { APPS_EXTENSION: { "mimeTypes": [APP_MIME] } } }),
        );
        let allowed = app
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "allow_auto_handoff": false } }
            }))
            .unwrap();
        assert_eq!(
            allowed["result"]["structuredContent"]["settings"]["allow_auto_handoff"],
            json!(false)
        );
        initialize(&mut app, "2025-06-18", json!({}));
        assert_eq!(list_tools(&mut app).len(), 6);
    }

    #[test]
    fn batch_requests_and_jsonrpc_validation() {
        let root = tempfile::tempdir().unwrap();
        let mut server = daemon(root.path());
        initialize(&mut server, "2025-03-26", json!({}));

        // Empty batch returns -32600
        let empty = server.handle_message(&json!([])).unwrap();
        assert_eq!(empty["error"]["code"], json!(-32600));

        // Notification-only batch omits response
        let notif_only = server.handle_message(&json!([
            { "jsonrpc": "2.0", "method": "notifications/initialized" }
        ]));
        assert!(notif_only.is_none());

        // Mixed batch returns responses only for requests
        let batch = server
            .handle_message(&json!([
                { "jsonrpc": "2.0", "id": 10, "method": "ping" },
                { "jsonrpc": "2.0", "method": "notifications/initialized" },
                { "jsonrpc": "2.0", "id": 11, "method": "ping" }
            ]))
            .unwrap();
        let arr = batch.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], json!(10));
        assert_eq!(arr[1]["id"], json!(11));

        // Unknown tool -> -32602; unknown resource -> -32002; invalid params -> -32602
        let unk_tool = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 12, "method": "tools/call",
                "params": { "name": "no_such_tool", "arguments": {} }
            }))
            .unwrap();
        assert_eq!(unk_tool["error"]["code"], json!(-32602));
        let unk_res = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 13, "method": "resources/read",
                "params": { "uri": "ui://context-radar-mcp/missing" }
            }))
            .unwrap();
        assert_eq!(unk_res["error"]["code"], json!(-32002));
        let bad_params = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 14, "method": "ping", "params": "invalid"
            }))
            .unwrap();
        assert_eq!(bad_params["error"]["code"], json!(-32602));

        // Newer protocol version (2025-11-25) rejects batch arrays with -32600
        let mut newer = daemon(root.path());
        initialize(&mut newer, "2025-11-25", json!({}));
        let rejected = newer
            .handle_message(&json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }]))
            .unwrap();
        assert_eq!(rejected["error"]["code"], json!(-32600));
    }

    #[test]
    fn claim_enforces_daemon_workspace_auto_fire_and_claim_mode() {
        let root = tempfile::tempdir().unwrap();
        let mut server = daemon(root.path());
        server
            .engine
            .set_preferred_workspace(Some("/workspace/bound-a".into()), true);
        initialize(
            &mut server,
            "2025-03-26",
            json!({ "extensions": { APPS_EXTENSION: { "mimeTypes": [APP_MIME] } } }),
        );

        let grant_b = server
            .grants
            .create_grant_scoped(
                "Keep B",
                "Resume B",
                true,
                "",
                "/workspace/other-b",
                &["/workspace/other-b".into()],
                0,
                0,
                "handoff",
                "",
                "Other B",
                crate::i18n::Lang::En,
            )
            .unwrap();

        // Caller cannot override daemon_workspace with hint workspace="/workspace/other-b"
        let cross_ws = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 20, "method": "tools/call",
                "params": {
                    "name": "context_radar_ui_action",
                    "arguments": {
                        "action": "claim_grant",
                        "grant_id": grant_b.grant_id,
                        "workspace": "/workspace/other-b",
                        "claim_mode": "manual_handoff"
                    }
                }
            }))
            .unwrap();
        assert_eq!(
            cross_ws["result"]["structuredContent"]["claimed"],
            json!(false)
        );
        assert_eq!(
            cross_ws["result"]["structuredContent"]["reason"],
            json!("workspace_mismatch")
        );

        // Unknown claim_mode is rejected
        let grant_a = server
            .grants
            .create_grant_scoped(
                "Keep A",
                "Resume A",
                false,
                "",
                "/workspace/bound-a",
                &["/workspace/bound-a".into()],
                0,
                0,
                "handoff",
                "",
                "Bound A",
                crate::i18n::Lang::En,
            )
            .unwrap();
        let bad_mode = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 21, "method": "tools/call",
                "params": {
                    "name": "context_radar_ui_action",
                    "arguments": {
                        "action": "claim_grant",
                        "grant_id": grant_a.grant_id,
                        "claim_mode": "bypass_checks"
                    }
                }
            }))
            .unwrap();
        assert_eq!(
            bad_mode["result"]["structuredContent"]["reason"],
            json!("invalid_claim_mode")
        );

        // Auto claim rejects auto_fire=false grant even when global allow_auto_handoff=true
        let auto_denied = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 22, "method": "tools/call",
                "params": {
                    "name": "context_radar_ui_action",
                    "arguments": {
                        "action": "claim_grant",
                        "grant_id": grant_a.grant_id,
                        "claim_mode": "auto_handoff"
                    }
                }
            }))
            .unwrap();
        assert_eq!(
            auto_denied["result"]["structuredContent"]["reason"],
            json!("auto_fire_disabled")
        );
    }

    #[test]
    fn tools_list_includes_app_only_tools_for_sep_1865_hosts() {
        let root = tempfile::tempdir().unwrap();
        let mut server = daemon(root.path());
        initialize(
            &mut server,
            "2025-03-26",
            json!({
                "extensions": { APPS_EXTENSION: { "mimeTypes": [APP_MIME] } }
            }),
        );
        let tools = list_tools(&mut server);
        let app_tools: Vec<_> = tools
            .iter()
            .filter(|tool| tool["_meta"]["ui"]["visibility"] == json!(["app"]))
            .collect();
        assert_eq!(tools.len(), 6);
        assert_eq!(app_tools.len(), 1);
        assert_eq!(app_tools[0]["name"], "context_radar_ui_action");
    }

    #[test]
    fn cached_overview_expires_when_pending_grant_expires() {
        let root = tempfile::tempdir().unwrap();
        let mut server = daemon(root.path());
        let grant = server
            .grants
            .create_grant("Keep state", "Resume", true, "", 0, 0, "handoff", "", "")
            .unwrap();
        let built = server
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "context_radar_overview", "arguments": {} }
            }))
            .unwrap();
        let snapshot = &built["result"]["structuredContent"];
        assert_eq!(snapshot["pending_grant"]["grant_id"], grant.grant_id);

        let inputs = SnapshotInputs::capture(&mut server.engine, &server.grants);
        let lang = snapshot["lang"].as_str().unwrap();
        let deadline = grant.created_at + grant.ttl_sec;
        assert!(server
            .swr
            .valid_snapshot(&inputs, lang, POLL_CACHE_MAX_AGE, deadline - 1.0)
            .is_some());
        assert!(server
            .swr
            .valid_snapshot(&inputs, lang, POLL_CACHE_MAX_AGE, deadline)
            .is_none());
    }

    #[test]
    fn unchanged_poll_reuses_valid_snapshot_and_rebuilds_after_invalidation() {
        let root = tempfile::tempdir().unwrap();
        let mut server = daemon(root.path());
        let overview = |server: &mut McpDaemon, args: Value| {
            server
                .handle_request(&json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "context_radar_overview", "arguments": args }
                }))
                .unwrap()["result"]["structuredContent"]
                .clone()
        };

        let initial = overview(&mut server, json!({}));
        let digest = initial["digest"].as_str().unwrap();
        assert_eq!(server.engine.overview_build_count, 1);

        let changed_for_client = overview(&mut server, json!({"if_digest": "older"}));
        assert_eq!(changed_for_client["digest"], digest);
        assert_eq!(changed_for_client["unchanged"], false);
        server.backdate_swr_cache(Duration::from_secs(2));
        for _ in 0..3 {
            let poll = overview(&mut server, json!({"if_digest": digest}));
            assert_eq!(poll["unchanged"], true);
            assert_eq!(poll["digest"], digest);
            assert!(poll.get("sessions").is_none());
        }
        assert_eq!(server.engine.overview_build_count, 1);

        fs::create_dir(&server.engine.ledger.conv_dir).unwrap();
        overview(&mut server, json!({"if_digest": digest}));
        assert_eq!(server.engine.overview_build_count, 2);

        // A separate process can update the grant store without a local mutation.
        let mut other_store = RelayGrantStore::new(server.grants.path.clone());
        other_store.update_settings(Some(false)).unwrap();
        let after_grant_change = overview(&mut server, json!({"if_digest": digest}));
        assert_eq!(after_grant_change["settings"]["allow_auto_handoff"], false);
        assert_eq!(server.engine.overview_build_count, 3);

        let next_digest = after_grant_change["digest"].as_str().unwrap();
        server.backdate_swr_cache(POLL_CACHE_MAX_AGE);
        assert_eq!(
            overview(&mut server, json!({"if_digest": next_digest}))["unchanged"],
            true
        );
        assert_eq!(server.engine.overview_build_count, 4);

        overview(
            &mut server,
            json!({"if_digest": next_digest, "fresh": true}),
        );
        assert_eq!(server.engine.overview_build_count, 5);
    }
}
