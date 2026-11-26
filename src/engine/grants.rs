//! Workspace- and conversation-scoped single-use relay grant store with mtime/size cache and bilingual prompt builders.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::i18n::{parse_lang_tag, resolve_active_lang, Lang};
use crate::settings::{
    default_data_root, default_plugin_data_dir, expand_tilde, normalize_workspace_uri,
    ACP_PROXY_ENV, GRANTS_PATH_ENV,
};

pub const GRANT_TTL_SEC: f64 = 43_200.0;
pub const GRANT_PRUNE_SEC: f64 = 86_400.0;
pub const RELAY_DISPLAY_SEC: f64 = 600.0;

#[inline]
pub fn is_acp_proxy_active() -> bool {
    env::var(ACP_PROXY_ENV)
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}

#[cfg(unix)]
mod file_lock {
    use super::*;

    const LOCK_EX: i32 = 2;
    const LOCK_UN: i32 = 8;

    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    const O_NOFOLLOW: i32 = 0x0100;
    #[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
    const O_NOFOLLOW: i32 = 0o100000;
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "arm"))
    ))]
    const O_NOFOLLOW: i32 = 0o400000;

    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    pub struct GrantFileLock {
        file: File,
    }

    fn lock_path_for(store_path: &Path) -> PathBuf {
        let mut os = store_path.as_os_str().to_os_string();
        os.push(".lock");
        PathBuf::from(os)
    }

    impl GrantFileLock {
        pub fn acquire(store_path: &Path) -> io::Result<Self> {
            if let Some(parent) = store_path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                    let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
                }
            }
            let lpath = lock_path_for(store_path);
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(O_NOFOLLOW)
                .open(&lpath)?;
            loop {
                let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
                if rc == 0 {
                    return Ok(Self { file });
                }
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
        }
    }

    impl Drop for GrantFileLock {
        fn drop(&mut self) {
            unsafe {
                let _ = flock(self.file.as_raw_fd(), LOCK_UN);
            }
        }
    }
}

pub fn default_grants_path(data_root: Option<&Path>) -> PathBuf {
    default_grants_path_in(None, data_root)
}

pub fn default_grants_path_in(
    plugin_data_dir: Option<&Path>,
    data_root: Option<&Path>,
) -> PathBuf {
    if let Ok(ov) = env::var(GRANTS_PATH_ENV) {
        let trimmed = ov.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(expand_tilde(trimmed));
        }
    }
    if let Some(pdir) = plugin_data_dir {
        if !pdir.as_os_str().is_empty() {
            return pdir.join("relay_grants.json");
        }
    }
    let root = data_root
        .map(Path::to_path_buf)
        .unwrap_or_else(default_data_root);
    default_plugin_data_dir(&root).join("relay_grants.json")
}

pub fn paths_overlap(a: &str, b: &str) -> bool {
    let na = normalize_workspace_uri(a);
    let nb = normalize_workspace_uri(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na == nb {
        return true;
    }
    let pa = Path::new(&na);
    let pb = Path::new(&nb);
    pa.starts_with(pb) || pb.starts_with(pa)
}

pub fn workspaces_overlap(grant_ws: &str, grant_all: &[String], scope_ws: &str) -> bool {
    let ns = normalize_workspace_uri(scope_ws);
    if ns.is_empty() {
        return true;
    }
    if !grant_ws.trim().is_empty() && paths_overlap(grant_ws, &ns) {
        return true;
    }
    grant_all.iter().any(|w| paths_overlap(w, &ns))
}

/// Validate and sanitize an optional `scratch_note_path` to prevent path traversal,
/// symlink abuse, or control-character injection into handoff prompts.
pub fn sanitize_scratch_note_path(raw: &str) -> Result<String, String> {
    sanitize_scratch_note_path_for_lang(raw, resolve_active_lang(""))
}

pub fn sanitize_scratch_note_path_for_lang(raw: &str, lang: Lang) -> Result<String, String> {
    let zh = lang.is_zh();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.len() > 512 {
        return Err(if zh {
            "scratch_note_path 过长（上限 512 字节）".into()
        } else {
            "scratch_note_path exceeds 512-byte maximum".into()
        });
    }
    if trimmed.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(if zh {
            "scratch_note_path 包含非法控制字符".into()
        } else {
            "scratch_note_path contains disallowed control characters".into()
        });
    }
    let p = Path::new(trimmed);
    for comp in p.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(if zh {
                "scratch_note_path 禁止包含路径穿越段 '..'".into()
            } else {
                "scratch_note_path must not contain '..' path traversal segments".into()
            });
        }
    }
    if let Ok(meta) = fs::symlink_metadata(p) {
        if meta.file_type().is_symlink() {
            return Err(if zh {
                "scratch_note_path 禁止指向符号链接".into()
            } else {
                "scratch_note_path must not point to a symbolic link".into()
            });
        }
    }
    Ok(trimmed.to_string())
}

#[inline]
fn now_sec() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn random_hex_16() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0) as u64;
        bytes = (nanos ^ (std::process::id() as u64)).to_le_bytes();
    }
    let mut out = String::with_capacity(16);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

pub fn build_compact_command(preserve_points: &str, resume_task: &str) -> String {
    build_compact_command_for_lang(preserve_points, resume_task, resolve_active_lang(""))
}

pub fn build_compact_command_for_lang(
    preserve_points: &str,
    resume_task: &str,
    lang: Lang,
) -> String {
    let zh = lang.is_zh();
    let pts = preserve_points
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let nxt = resume_task
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    match (!pts.is_empty(), !nxt.is_empty()) {
        (true, true) => {
            if zh {
                format!("/compact 保留关键上下文与约束：{pts}。压缩完成后请继续执行：{nxt}")
            } else {
                format!("/compact Preserve key context and constraints: {pts}. After compaction, continue with: {nxt}")
            }
        }
        (true, false) => {
            if zh {
                format!("/compact 保留关键上下文与约束：{pts}。压缩完成后请简要确认已保留要点并等待后续指令。")
            } else {
                format!("/compact Preserve key context and constraints: {pts}. After compaction, briefly confirm preserved points and stop to await further instructions.")
            }
        }
        (false, true) => format!("/compact {nxt}"),
        (false, false) => "/compact".to_string(),
    }
}

pub fn build_handoff_prompt(
    conversation_id: &str,
    preserve_points: &str,
    resume_task: &str,
    scratch_note_path: &str,
    title: &str,
) -> String {
    build_handoff_prompt_for_lang(
        conversation_id,
        preserve_points,
        resume_task,
        scratch_note_path,
        title,
        resolve_active_lang(""),
    )
}

pub fn build_handoff_prompt_for_lang(
    conversation_id: &str,
    preserve_points: &str,
    resume_task: &str,
    scratch_note_path: &str,
    title: &str,
    lang: Lang,
) -> String {
    let zh = lang.is_zh();
    let pts = preserve_points.trim();
    let nxt = resume_task.trim();
    let note = scratch_note_path.trim();
    let src = if !title.trim().is_empty() && !conversation_id.trim().is_empty() {
        format!("{} ({})", title.trim(), conversation_id.trim())
    } else if !conversation_id.trim().is_empty() {
        conversation_id.trim().to_string()
    } else if !title.trim().is_empty() {
        title.trim().to_string()
    } else if zh {
        "上一会话".to_string()
    } else {
        "previous session".to_string()
    };

    if zh {
        let mut lines = vec![
            format!(
                "【上下文接力】本会话承接自高水位会话 `{src}`（通过官方 MCP App `target:new` 切换以释放上下文窗口）。"
            ),
            String::new(),
            "## 必须保留的关键上下文、已改文件与约束".to_string(),
            if pts.is_empty() {
                "（未提供额外要点）".to_string()
            } else {
                pts.to_string()
            },
        ];
        if !note.is_empty() {
            lines.push(String::new());
            lines.push("## 切片摘要备忘（按需读取）".to_string());
            lines.push(format!("- `{note}`"));
        }
        lines.push(String::new());
        if !nxt.is_empty() {
            lines.push("## 下一步任务".to_string());
            lines.push(nxt.to_string());
        } else {
            lines.push("## 下一步".to_string());
            lines.push("请确认以上接力上下文已就绪，并简要汇报当前状态等待指令。".to_string());
        }
        lines.join("\n")
    } else {
        let mut lines = vec![
            format!(
                "[Context Handoff] Continuing from high-context session `{src}` (launched via official MCP App `target:new` to reset the context window)."
            ),
            String::new(),
            "## Key Context, Modified Files & Constraints to Preserve".to_string(),
            if pts.is_empty() {
                "(No additional notes provided)".to_string()
            } else {
                pts.to_string()
            },
        ];
        if !note.is_empty() {
            lines.push(String::new());
            lines.push("## Distilled Slice Summary (read on demand)".to_string());
            lines.push(format!("- `{note}`"));
        }
        lines.push(String::new());
        if !nxt.is_empty() {
            lines.push("## Next Task to Execute".to_string());
            lines.push(nxt.to_string());
        } else {
            lines.push("## Next Step".to_string());
            lines.push(
                "Confirm the handoff context above is clear and report readiness for the next instruction."
                    .to_string(),
            );
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanelPrefs {
    #[serde(default = "default_true")]
    pub allow_auto_handoff: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lang: String,
}

fn default_true() -> bool {
    true
}

impl Default for PanelPrefs {
    fn default() -> Self {
        Self {
            allow_auto_handoff: true,
            lang: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayGrant {
    pub grant_id: String,
    pub kind: String,
    pub conversation_id: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub title: String,
    pub created_at: f64,
    #[serde(default = "default_ttl")]
    pub ttl_sec: f64,
    #[serde(default)]
    pub created_turns: usize,
    #[serde(default)]
    pub created_checkpoints: usize,
    #[serde(default = "default_true")]
    pub auto_fire: bool,
    #[serde(default)]
    pub claimed: bool,
    #[serde(default)]
    pub claimed_at: f64,
    #[serde(default)]
    pub dismissed: bool,
    #[serde(default)]
    pub preserve_points: String,
    #[serde(default)]
    pub resume_task: String,
    #[serde(default)]
    pub scratch_note_path: String,
    #[serde(default)]
    pub compact_command: String,
    #[serde(default)]
    pub handoff_prompt: String,
    #[serde(default)]
    pub handoff_conversation_id: String,
    #[serde(default)]
    pub handoff_at: f64,
    #[serde(default)]
    pub compact_status: String,
    #[serde(default)]
    pub compact_dispatched_at: f64,
    #[serde(default)]
    pub acp_proxy_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_subagent_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_auto_handoff: Option<bool>,
}

fn default_ttl() -> f64 {
    GRANT_TTL_SEC
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    settings: PanelPrefs,
    #[serde(default)]
    grants: HashMap<String, RelayGrant>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimOutcome {
    pub claimed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_subagent_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionOutcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_conversation_id: Option<String>,
}

pub struct RelayGrantStore {
    pub path: PathBuf,
    cache_sig: Option<(u64, u64, i128, u64)>,
    cache_data: Option<StoreFile>,
}

impl RelayGrantStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache_sig: None,
            cache_data: None,
        }
    }

    fn load(&mut self) -> StoreFile {
        let Ok(meta) = fs::symlink_metadata(&self.path) else {
            self.cache_sig = None;
            self.cache_data = None;
            return StoreFile::default();
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            self.cache_sig = None;
            self.cache_data = None;
            return StoreFile::default();
        }
        let sig = (
            meta.dev(),
            meta.ino(),
            (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128),
            meta.size(),
        );
        if self.cache_sig == Some(sig) {
            if let Some(ref cached) = self.cache_data {
                return cached.clone();
            }
        }
        if let Ok(raw) = fs::read_to_string(&self.path) {
            if let Ok(parsed) = serde_json::from_str::<StoreFile>(&raw) {
                self.cache_sig = Some(sig);
                self.cache_data = Some(parsed.clone());
                return parsed;
            }
        }
        StoreFile::default()
    }

    fn save(&mut self, data: &StoreFile) {
        if let Ok(smeta) = fs::symlink_metadata(&self.path) {
            if smeta.file_type().is_symlink() {
                return;
            }
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        let parent_dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let tmp_name = format!(".grants-{}-{}.tmp", std::process::id(), random_hex_16());
        let tmp_path = parent_dir.join(tmp_name);

        let Ok(payload) = serde_json::to_vec(data) else { return };
        let write_ok = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)
            .and_then(|mut f| f.write_all(&payload));

        if write_ok.is_ok() && fs::rename(&tmp_path, &self.path).is_ok() {
            if let Ok(meta) = fs::metadata(&self.path) {
                let sig = (
                    meta.dev(),
                    meta.ino(),
                    (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128),
                    meta.size(),
                );
                self.cache_sig = Some(sig);
                self.cache_data = Some(data.clone());
                return;
            }
        }
        let _ = fs::remove_file(&tmp_path);
        self.cache_sig = None;
        self.cache_data = None;
    }

    pub fn get_settings(&mut self) -> PanelPrefs {
        self.load().settings
    }

    pub fn update_settings(&mut self, allow_auto_handoff: Option<bool>) -> PanelPrefs {
        self.update_settings_with_lang(allow_auto_handoff, None)
    }

    pub fn update_settings_with_lang(
        &mut self,
        allow_auto_handoff: Option<bool>,
        lang: Option<&str>,
    ) -> PanelPrefs {
        let Ok(_lock) = file_lock::GrantFileLock::acquire(&self.path) else {
            return self.load().settings;
        };
        let mut data = self.load();
        if let Some(v) = allow_auto_handoff {
            data.settings.allow_auto_handoff = v;
        }
        if let Some(raw_lang) = lang {
            let trimmed = raw_lang.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
                data.settings.lang.clear();
            } else if let Some(parsed) = parse_lang_tag(trimmed) {
                data.settings.lang = parsed.as_str().to_string();
            }
        }
        self.save(&data);
        data.settings
    }

    pub fn find_grant(&mut self, grant_id: &str) -> Option<RelayGrant> {
        self.load().grants.get(grant_id).cloned()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_grant(
        &mut self,
        preserve_points: &str,
        resume_task: &str,
        auto_fire: bool,
        conversation_id: &str,
        turns_count: usize,
        checkpoint_count: usize,
        kind: &str,
        scratch_note_path: &str,
        title: &str,
    ) -> RelayGrant {
        self.create_grant_scoped(
            preserve_points,
            resume_task,
            auto_fire,
            conversation_id,
            "",
            &[],
            turns_count,
            checkpoint_count,
            kind,
            scratch_note_path,
            title,
            resolve_active_lang(""),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_grant_scoped(
        &mut self,
        preserve_points: &str,
        resume_task: &str,
        auto_fire: bool,
        conversation_id: &str,
        workspace: &str,
        workspaces: &[String],
        turns_count: usize,
        checkpoint_count: usize,
        kind: &str,
        scratch_note_path: &str,
        title: &str,
        lang: Lang,
    ) -> RelayGrant {
        let now = now_sec();
        let lock = file_lock::GrantFileLock::acquire(&self.path).ok();
        let mut data = self.load();
        data.grants
            .retain(|_, g| (now - g.created_at) <= GRANT_PRUNE_SEC);

        let grant_id = random_hex_16();
        let compact_command =
            build_compact_command_for_lang(preserve_points, resume_task, lang);
        let handoff_prompt = build_handoff_prompt_for_lang(
            conversation_id,
            preserve_points,
            resume_task,
            scratch_note_path,
            title,
            lang,
        );
        let norm_kind = if kind == "handoff" || kind == "compact" {
            kind.to_string()
        } else {
            "handoff".to_string()
        };

        let norm_ws = normalize_workspace_uri(workspace);
        let norm_all: Vec<String> = workspaces
            .iter()
            .map(|w| normalize_workspace_uri(w))
            .filter(|w| !w.is_empty())
            .collect();

        let entry = RelayGrant {
            grant_id: grant_id.clone(),
            kind: norm_kind,
            conversation_id: conversation_id.to_string(),
            workspace: norm_ws,
            workspaces: norm_all,
            title: title.trim().to_string(),
            created_at: now,
            ttl_sec: GRANT_TTL_SEC,
            created_turns: turns_count,
            created_checkpoints: checkpoint_count,
            auto_fire,
            claimed: false,
            claimed_at: 0.0,
            dismissed: false,
            preserve_points: preserve_points.trim().to_string(),
            resume_task: resume_task.trim().to_string(),
            scratch_note_path: scratch_note_path.trim().to_string(),
            compact_command,
            handoff_prompt,
            handoff_conversation_id: String::new(),
            handoff_at: 0.0,
            compact_status: String::new(),
            compact_dispatched_at: 0.0,
            acp_proxy_active: is_acp_proxy_active(),
            running_subagent_count: None,
            allow_auto_handoff: None,
        };
        if lock.is_some() {
            data.grants.insert(grant_id, entry.clone());
            self.save(&data);
        }
        entry
    }

    pub fn get_pending_grant(&mut self, now_override: Option<f64>) -> Option<RelayGrant> {
        self.get_pending_grant_scoped(None, None, now_override)
    }

    pub fn get_pending_grant_scoped(
        &mut self,
        scope_cid: Option<&str>,
        scope_workspace: Option<&str>,
        now_override: Option<f64>,
    ) -> Option<RelayGrant> {
        let ref_now = now_override.unwrap_or_else(now_sec);
        let cid_f = scope_cid.map(str::trim).filter(|s| !s.is_empty());
        let ws_f = scope_workspace
            .map(normalize_workspace_uri)
            .filter(|s| !s.is_empty());
        let data = self.load();
        let mut cands: Vec<RelayGrant> = data
            .grants
            .values()
            .filter(|rec| {
                if rec.dismissed {
                    return false;
                }
                let age = ref_now - rec.created_at;
                if age > rec.ttl_sec {
                    return false;
                }
                if let Some(cid_req) = cid_f {
                    if rec.conversation_id != cid_req {
                        return false;
                    }
                }
                if let Some(ref ws_req) = ws_f {
                    let has_grant_ws = !rec.workspace.is_empty() || !rec.workspaces.is_empty();
                    if has_grant_ws {
                        if !workspaces_overlap(&rec.workspace, &rec.workspaces, ws_req) {
                            return false;
                        }
                    } else if cid_f.is_none() {
                        // Never expose an unscoped grant to a workspace-bound side pane without an explicit conversation match.
                        return false;
                    }
                }
                if !rec.claimed {
                    return true;
                }
                if !rec.handoff_conversation_id.is_empty() {
                    let h_ts = if rec.handoff_at > 0.0 {
                        rec.handoff_at
                    } else {
                        rec.claimed_at
                    };
                    return (ref_now - h_ts) <= RELAY_DISPLAY_SEC;
                }
                if !rec.compact_status.is_empty() {
                    let c_ts = if rec.compact_dispatched_at > 0.0 {
                        rec.compact_dispatched_at
                    } else {
                        rec.claimed_at
                    };
                    return (ref_now - c_ts) <= RELAY_DISPLAY_SEC;
                }
                false
            })
            .cloned()
            .collect();

        if cands.is_empty() {
            return None;
        }
        cands.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        cands.into_iter().next()
    }

    pub fn claim_grant(
        &mut self,
        grant_id: &str,
        current_turns: Option<usize>,
        current_checkpoints: Option<usize>,
        running_subagent_count: Option<usize>,
        require_auto_handoff: bool,
    ) -> ClaimOutcome {
        self.claim_grant_scoped(
            grant_id,
            None,
            None,
            current_turns,
            current_checkpoints,
            running_subagent_count,
            require_auto_handoff,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_grant_scoped(
        &mut self,
        grant_id: &str,
        expected_cid: Option<&str>,
        expected_workspace: Option<&str>,
        current_turns: Option<usize>,
        current_checkpoints: Option<usize>,
        running_subagent_count: Option<usize>,
        require_auto_handoff: bool,
    ) -> ClaimOutcome {
        if grant_id.is_empty() {
            return ClaimOutcome {
                claimed: false,
                reason: Some("missing_grant_id".into()),
                grant_id: None,
                conversation_id: None,
                workspace: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: None,
                running_subagent_count: None,
            };
        }
        let Ok(_lock) = file_lock::GrantFileLock::acquire(&self.path) else {
            return ClaimOutcome {
                claimed: false,
                reason: Some("lock_error".into()),
                grant_id: None,
                conversation_id: None,
                workspace: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: None,
                running_subagent_count: None,
            };
        };
        let now = now_sec();
        let mut data = self.load();
        let allow_auto = data.settings.allow_auto_handoff;

        let Some(rec) = data.grants.get_mut(grant_id) else {
            return ClaimOutcome {
                claimed: false,
                reason: Some("not_found".into()),
                grant_id: None,
                conversation_id: None,
                workspace: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: None,
                running_subagent_count: None,
            };
        };

        if let Some(ecid) = expected_cid.map(str::trim).filter(|s| !s.is_empty()) {
            if !rec.conversation_id.is_empty() && rec.conversation_id != ecid {
                return ClaimOutcome {
                    claimed: false,
                    reason: Some("conversation_mismatch".into()),
                    grant_id: None,
                    conversation_id: Some(rec.conversation_id.clone()),
                    workspace: Some(rec.workspace.clone()),
                    compact_command: None,
                    handoff_prompt: None,
                    handoff_conversation_id: None,
                    running_subagent_count: None,
                };
            }
        }

        if let Some(ews) = expected_workspace
            .map(normalize_workspace_uri)
            .filter(|s| !s.is_empty())
        {
            let has_grant_ws = !rec.workspace.is_empty() || !rec.workspaces.is_empty();
            if has_grant_ws && !workspaces_overlap(&rec.workspace, &rec.workspaces, &ews) {
                return ClaimOutcome {
                    claimed: false,
                    reason: Some("workspace_mismatch".into()),
                    grant_id: None,
                    conversation_id: Some(rec.conversation_id.clone()),
                    workspace: Some(rec.workspace.clone()),
                    compact_command: None,
                    handoff_prompt: None,
                    handoff_conversation_id: None,
                    running_subagent_count: None,
                };
            }
        }

        if rec.claimed {
            return ClaimOutcome {
                claimed: false,
                reason: Some("already_claimed".into()),
                grant_id: None,
                conversation_id: None,
                workspace: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: Some(rec.handoff_conversation_id.clone()),
                running_subagent_count: None,
            };
        }

        if (now - rec.created_at) > rec.ttl_sec {
            return ClaimOutcome {
                claimed: false,
                reason: Some("expired".into()),
                grant_id: None,
                conversation_id: None,
                workspace: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: None,
                running_subagent_count: None,
            };
        }

        if let Some(cur_ckpts) = current_checkpoints {
            if cur_ckpts > rec.created_checkpoints {
                rec.claimed = true;
                self.save(&data);
                return ClaimOutcome {
                    claimed: false,
                    reason: Some("already_compacted".into()),
                    grant_id: None,
                    conversation_id: None,
                    workspace: None,
                    compact_command: None,
                    handoff_prompt: None,
                    handoff_conversation_id: None,
                    running_subagent_count: None,
                };
            }
        }

        if let Some(cur_turns) = current_turns {
            if rec.created_turns > 0 && cur_turns.saturating_sub(rec.created_turns) > 4 {
                rec.claimed = true;
                self.save(&data);
                return ClaimOutcome {
                    claimed: false,
                    reason: Some("session_advanced".into()),
                    grant_id: None,
                    conversation_id: None,
                    workspace: None,
                    compact_command: None,
                    handoff_prompt: None,
                    handoff_conversation_id: None,
                    running_subagent_count: None,
                };
            }
        }

        if let Some(subs) = running_subagent_count {
            if subs > 0 {
                return ClaimOutcome {
                    claimed: false,
                    reason: Some("running_subagents_active".into()),
                    grant_id: None,
                    conversation_id: None,
                    workspace: None,
                    compact_command: None,
                    handoff_prompt: None,
                    handoff_conversation_id: None,
                    running_subagent_count: Some(subs),
                };
            }
        }

        if require_auto_handoff && !allow_auto {
            return ClaimOutcome {
                claimed: false,
                reason: Some("auto_handoff_disabled".into()),
                grant_id: None,
                conversation_id: None,
                workspace: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: None,
                running_subagent_count: None,
            };
        }

        rec.claimed = true;
        rec.claimed_at = now;
        let out = ClaimOutcome {
            claimed: true,
            reason: None,
            grant_id: Some(grant_id.to_string()),
            conversation_id: Some(rec.conversation_id.clone()),
            workspace: Some(rec.workspace.clone()),
            compact_command: Some(rec.compact_command.clone()),
            handoff_prompt: Some(rec.handoff_prompt.clone()),
            handoff_conversation_id: None,
            running_subagent_count: None,
        };
        self.save(&data);
        out
    }

    pub fn record_relay(&mut self, grant_id: &str, handoff_cid: &str) -> ActionOutcome {
        if grant_id.is_empty() {
            return ActionOutcome {
                ok: false,
                reason: Some("missing_grant_id".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        }
        let Ok(_lock) = file_lock::GrantFileLock::acquire(&self.path) else {
            return ActionOutcome {
                ok: false,
                reason: Some("lock_error".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        };
        let mut data = self.load();
        let Some(rec) = data.grants.get_mut(grant_id) else {
            return ActionOutcome {
                ok: false,
                reason: Some("not_found".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        };
        rec.claimed = true;
        rec.handoff_conversation_id = handoff_cid.trim().to_string();
        rec.handoff_at = now_sec();
        let saved_cid = rec.handoff_conversation_id.clone();
        self.save(&data);
        ActionOutcome {
            ok: true,
            reason: None,
            grant_id: Some(grant_id.to_string()),
            handoff_conversation_id: Some(saved_cid),
        }
    }

    pub fn record_compact(&mut self, grant_id: &str, status: &str) -> ActionOutcome {
        if grant_id.is_empty() {
            return ActionOutcome {
                ok: false,
                reason: Some("missing_grant_id".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        }
        let Ok(_lock) = file_lock::GrantFileLock::acquire(&self.path) else {
            return ActionOutcome {
                ok: false,
                reason: Some("lock_error".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        };
        let mut data = self.load();
        let Some(rec) = data.grants.get_mut(grant_id) else {
            return ActionOutcome {
                ok: false,
                reason: Some("not_found".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        };
        rec.claimed = true;
        if rec.claimed_at <= 0.0 {
            rec.claimed_at = now_sec();
        }
        rec.compact_status = status.trim().to_string();
        rec.compact_dispatched_at = now_sec();
        let saved = rec.compact_status.clone();
        self.save(&data);
        ActionOutcome {
            ok: true,
            reason: Some(saved),
            grant_id: Some(grant_id.to_string()),
            handoff_conversation_id: None,
        }
    }

    /// Find the latest unclaimed, non-expired `kind == "compact"` grant scoped to `conversation_id`.
    /// Used by official ACP orchestrators (`acp-proxy` / `acp-run`) at clean turn boundaries.
    pub fn find_pending_compact_grant(
        &mut self,
        conversation_id: &str,
        workspace: Option<&str>,
        require_auto: bool,
        now_override: Option<f64>,
    ) -> Option<RelayGrant> {
        let cid_req = conversation_id.trim();
        if cid_req.is_empty() {
            return None;
        }
        let ref_now = now_override.unwrap_or_else(now_sec);
        let ws_f = workspace
            .map(normalize_workspace_uri)
            .filter(|s| !s.is_empty());
        let data = self.load();
        if require_auto && !data.settings.allow_auto_handoff {
            return None;
        }
        let mut cands: Vec<RelayGrant> = data
            .grants
            .values()
            .filter(|rec| {
                if rec.dismissed || rec.claimed || rec.kind != "compact" {
                    return false;
                }
                if require_auto && !rec.auto_fire {
                    return false;
                }
                if (ref_now - rec.created_at) > rec.ttl_sec {
                    return false;
                }
                if rec.conversation_id != cid_req {
                    return false;
                }
                if let Some(ref ws_req) = ws_f {
                    let has_grant_ws = !rec.workspace.is_empty() || !rec.workspaces.is_empty();
                    if has_grant_ws && !workspaces_overlap(&rec.workspace, &rec.workspaces, ws_req) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        cands.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        cands.into_iter().next()
    }

    pub fn dismiss_grant(&mut self, grant_id: &str) -> ActionOutcome {
        if grant_id.is_empty() {
            return ActionOutcome {
                ok: false,
                reason: Some("missing_grant_id".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        }
        let Ok(_lock) = file_lock::GrantFileLock::acquire(&self.path) else {
            return ActionOutcome {
                ok: false,
                reason: Some("lock_error".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        };
        let mut data = self.load();
        let Some(rec) = data.grants.get_mut(grant_id) else {
            return ActionOutcome {
                ok: false,
                reason: Some("not_found".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        };
        rec.dismissed = true;
        self.save(&data);
        ActionOutcome {
            ok: true,
            reason: None,
            grant_id: Some(grant_id.to_string()),
            handoff_conversation_id: None,
        }
    }
}
