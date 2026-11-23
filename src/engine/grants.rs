//! Single-use replay-protected relay grant store with mtime/size cache and prompt builders.

use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::settings::{
    default_data_root, default_plugin_data_dir, expand_tilde, GRANTS_PATH_ENV,
};

pub const GRANT_TTL_SEC: f64 = 43_200.0;
pub const GRANT_PRUNE_SEC: f64 = 86_400.0;
pub const RELAY_DISPLAY_SEC: f64 = 600.0;

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

/// Validate and sanitize an optional `scratch_note_path` to prevent path traversal,
/// symlink abuse, or control-character injection into handoff prompts.
pub fn sanitize_scratch_note_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.len() > 512 {
        return Err("scratch_note_path 过长（上限 512 字节）".into());
    }
    if trimmed.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err("scratch_note_path 包含非法控制字符".into());
    }
    let p = Path::new(trimmed);
    for comp in p.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err("scratch_note_path 禁止包含路径穿越段 '..'".into());
        }
    }
    if let Ok(meta) = fs::symlink_metadata(p) {
        if meta.file_type().is_symlink() {
            return Err("scratch_note_path 禁止指向符号链接".into());
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
        (true, true) => format!("/compact 保留关键上下文与约束：{pts}。压缩完成后请立即继续执行：{nxt}"),
        (true, false) => format!("/compact 保留关键上下文与约束：{pts}"),
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
    let pts = preserve_points.trim();
    let nxt = resume_task.trim();
    let note = scratch_note_path.trim();
    let src = if !title.trim().is_empty() && !conversation_id.trim().is_empty() {
        format!("{} ({})", title.trim(), conversation_id.trim())
    } else if !conversation_id.trim().is_empty() {
        conversation_id.trim().to_string()
    } else if !title.trim().is_empty() {
        title.trim().to_string()
    } else {
        "上一会话".to_string()
    };

    let mut lines = vec![
        format!(
            "【上下文压缩接力】本会话承接自高上下文会话 `{src}`（通过官方 MCP App `target:new` 无缝切换以释放上下文窗口）。"
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
        lines.push("## 历史提纯摘要文件（按需精读）".to_string());
        lines.push(format!("- `{note}`"));
    }
    lines.push(String::new());
    if !nxt.is_empty() {
        lines.push("## 请立即继续执行的下一步任务".to_string());
        lines.push(nxt.to_string());
    } else {
        lines.push("## 下一步".to_string());
        lines.push("请确认以上接力上下文已就绪，并汇报当前状态等待进一步指令。".to_string());
    }
    lines.join("\n")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanelPrefs {
    #[serde(default = "default_true")]
    pub allow_auto_handoff: bool,
}

fn default_true() -> bool {
    true
}

impl Default for PanelPrefs {
    fn default() -> Self {
        Self {
            allow_auto_handoff: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayGrant {
    pub grant_id: String,
    pub kind: String,
    pub conversation_id: String,
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
    cache_sig: Option<(i128, u64)>,
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
        let mut data = self.load();
        if let Some(v) = allow_auto_handoff {
            data.settings.allow_auto_handoff = v;
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
        let now = now_sec();
        let mut data = self.load();
        data.grants
            .retain(|_, g| (now - g.created_at) <= GRANT_PRUNE_SEC);

        let grant_id = random_hex_16();
        let compact_command = build_compact_command(preserve_points, resume_task);
        let handoff_prompt = build_handoff_prompt(
            conversation_id,
            preserve_points,
            resume_task,
            scratch_note_path,
            title,
        );
        let norm_kind = if kind == "handoff" || kind == "compact" {
            kind.to_string()
        } else {
            "handoff".to_string()
        };

        let entry = RelayGrant {
            grant_id: grant_id.clone(),
            kind: norm_kind,
            conversation_id: conversation_id.to_string(),
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
            running_subagent_count: None,
            allow_auto_handoff: None,
        };
        data.grants.insert(grant_id, entry.clone());
        self.save(&data);
        entry
    }

    pub fn get_pending_grant(&mut self, now_override: Option<f64>) -> Option<RelayGrant> {
        let ref_now = now_override.unwrap_or_else(now_sec);
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
        if grant_id.is_empty() {
            return ClaimOutcome {
                claimed: false,
                reason: Some("missing_grant_id".into()),
                grant_id: None,
                conversation_id: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: None,
                running_subagent_count: None,
            };
        }
        let now = now_sec();
        let mut data = self.load();
        let allow_auto = data.settings.allow_auto_handoff;

        let Some(rec) = data.grants.get_mut(grant_id) else {
            return ClaimOutcome {
                claimed: false,
                reason: Some("not_found".into()),
                grant_id: None,
                conversation_id: None,
                compact_command: None,
                handoff_prompt: None,
                handoff_conversation_id: None,
                running_subagent_count: None,
            };
        };

        if rec.claimed {
            return ClaimOutcome {
                claimed: false,
                reason: Some("already_claimed".into()),
                grant_id: None,
                conversation_id: None,
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

    pub fn dismiss_grant(&mut self, grant_id: &str) -> ActionOutcome {
        if grant_id.is_empty() {
            return ActionOutcome {
                ok: false,
                reason: Some("missing_grant_id".into()),
                grant_id: None,
                handoff_conversation_id: None,
            };
        }
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
