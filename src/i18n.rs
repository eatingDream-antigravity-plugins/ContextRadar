//! Locale auto-detection (`en` / `zh-CN`) and adaptive currency helpers.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::settings::{default_data_root, is_safe_id, LANG_ENV};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Lang {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN", alias = "zh", alias = "zh_CN", alias = "zh-cn")]
    ZhCn,
}

impl Lang {
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    #[inline]
    pub fn is_zh(self) -> bool {
        matches!(self, Self::ZhCn)
    }

    #[inline]
    pub fn default_currency(self) -> (&'static str, &'static str) {
        match self {
            Self::En => ("USD", "$"),
            Self::ZhCn => ("CNY", "¥"),
        }
    }
}

/// Parse an explicit or POSIX locale tag into `Some(Lang)`, returning `None` when unset, `"auto"`,
/// or encoding-only (`"UTF-8"`). Any `zh*` tag maps to `ZhCn`; all other real tags map to `En`.
pub fn parse_lang_tag(raw: &str) -> Option<Lang> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() {
        return None;
    }
    let low = trimmed.to_ascii_lowercase();
    if matches!(
        low.as_str(),
        "auto" | "default" | "system" | "utf-8" | "utf8"
    ) {
        return None;
    }
    let base = low.split(['.', '@']).next().unwrap_or(low.as_str()).trim();
    if base.is_empty() {
        return None;
    }
    if base == "zh" || base.starts_with("zh_") || base.starts_with("zh-") {
        Some(Lang::ZhCn)
    } else {
        Some(Lang::En)
    }
}

pub fn resolve_lang_from_sources(
    settings_lang: &str,
    env_override: Option<&str>,
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang_env: Option<&str>,
    apple_lang: Option<&str>,
) -> Lang {
    if let Some(l) = parse_lang_tag(settings_lang) {
        return l;
    }
    if let Some(ov) = env_override.and_then(parse_lang_tag) {
        return ov;
    }
    for candidate in [lc_all, lc_messages, lang_env].into_iter().flatten() {
        if let Some(l) = parse_lang_tag(candidate) {
            return l;
        }
    }
    if let Some(al) = apple_lang.and_then(parse_lang_tag) {
        return al;
    }
    Lang::En
}

fn detect_macos_apple_language() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        static APPLE_LANG: OnceLock<Option<String>> = OnceLock::new();
        APPLE_LANG
            .get_or_init(|| {
                let out = Command::new("/usr/bin/defaults")
                    .args(["read", "-g", "AppleLanguages"])
                    .output()
                    .ok()?;
                if !out.status.success() {
                    return None;
                }
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let cleaned = line
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .trim_end_matches(',')
                        .trim()
                        .trim_matches('"')
                        .trim();
                    if !cleaned.is_empty() {
                        return Some(cleaned.to_string());
                    }
                }
                None
            })
            .clone()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub fn resolve_active_lang(settings_lang: &str) -> Lang {
    if let Some(l) = parse_lang_tag(settings_lang) {
        return l;
    }
    let env_ov = env::var(LANG_ENV).ok();
    if let Some(ref ov) = env_ov {
        if let Some(l) = parse_lang_tag(ov) {
            return l;
        }
    }
    let lc_all = env::var("LC_ALL").ok();
    let lc_msg = env::var("LC_MESSAGES").ok();
    let lang_var = env::var("LANG").ok();
    for locale in [&lc_all, &lc_msg, &lang_var].into_iter().flatten() {
        if let Some(lang) = parse_lang_tag(locale) {
            return lang;
        }
    }
    let apple = detect_macos_apple_language();
    resolve_lang_from_sources(
        settings_lang,
        env_ov.as_deref(),
        lc_all.as_deref(),
        lc_msg.as_deref(),
        lang_var.as_deref(),
        apple.as_deref(),
    )
}

#[inline]
pub fn infer_currency_symbol(code: &str) -> &'static str {
    match code.trim().to_ascii_uppercase().as_str() {
        "USD" => "$",
        "CNY" | "RMB" | "JPY" => "¥",
        "EUR" => "€",
        "GBP" => "£",
        _ => "$",
    }
}

#[inline]
pub fn select_display_cost(cost_usd: f64, cost_cny: f64, currency_code: &str) -> f64 {
    if currency_code.trim().eq_ignore_ascii_case("USD") {
        cost_usd
    } else {
        cost_cny
    }
}

#[inline]
pub fn pick<'a>(lang: Lang, en: &'a str, zh: &'a str) -> &'a str {
    if lang.is_zh() {
        zh
    } else {
        en
    }
}

#[inline]
pub fn ttl_window_label(lang: Lang, ttl_sec: u64) -> &'static str {
    match (lang.is_zh(), ttl_sec) {
        (true, 180) => "3 分钟",
        (false, 180) => "3m",
        (true, 240) => "4 分钟",
        (false, 240) => "4m",
        (true, _) => "5 分钟",
        (false, _) => "5m",
    }
}

pub fn carbon_concurrency_warning_text(lang: Lang, active_carbon_count: usize) -> String {
    if lang.is_zh() {
        format!(
            "提示：现在有 {active_carbon_count} 个大模型代理同时运行，建议不超过 2 个，以免互相拖慢。"
        )
    } else {
        format!(
            "Tip: {active_carbon_count} large-model agents are running at once. Keep it to 2 or fewer so they don't slow each other down."
        )
    }
}

fn normalize_macos_alias(p: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        for alias in ["/tmp", "/var"] {
            if let Ok(rest) = p.strip_prefix(alias) {
                return Path::new("/private").join(&alias[1..]).join(rest);
            }
        }
    }
    p.to_path_buf()
}

fn validate_brain_scratch_rel(
    rel: &Path,
    expected_cid: Option<&str>,
    zh: bool,
) -> Result<String, String> {
    let mut comps = rel.components();
    let Some(Component::Normal(b)) = comps.next() else {
        return Err(if zh {
            "scratch_note_path 必须位于 brain/<cid>/scratch/ 下".into()
        } else {
            "scratch_note_path must stay inside brain/<cid>/scratch/".into()
        });
    };
    if b != "brain" {
        return Err(if zh {
            "scratch_note_path 必须位于 brain/<cid>/scratch/ 下".into()
        } else {
            "scratch_note_path must stay inside brain/<cid>/scratch/".into()
        });
    }
    let Some(cid) = comps.next().and_then(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    }) else {
        return Err(if zh {
            "scratch_note_path 缺少有效的会话目录".into()
        } else {
            "scratch_note_path is missing a valid conversation ID directory".into()
        });
    };
    if !is_safe_id(cid) {
        return Err(if zh {
            "scratch_note_path 包含无效会话 ID".into()
        } else {
            "scratch_note_path contains an invalid conversation ID".into()
        });
    }
    if let Some(ecid) = expected_cid.map(str::trim).filter(|s| !s.is_empty()) {
        if cid != ecid {
            return Err(if zh {
                "scratch_note_path 必须属于当前会话的 scratch/ 目录".into()
            } else {
                "scratch_note_path must belong to the active conversation's scratch/ directory"
                    .into()
            });
        }
    }
    let Some(Component::Normal(sc)) = comps.next() else {
        return Err(if zh {
            "scratch_note_path 必须位于 brain/<cid>/scratch/ 下".into()
        } else {
            "scratch_note_path must stay inside brain/<cid>/scratch/".into()
        });
    };
    if sc != "scratch" {
        return Err(if zh {
            "scratch_note_path 必须位于 brain/<cid>/scratch/ 下".into()
        } else {
            "scratch_note_path must stay inside brain/<cid>/scratch/".into()
        });
    }
    let rest: Vec<_> = comps.collect();
    if rest.is_empty() || !rest.iter().all(|c| matches!(c, Component::Normal(_))) {
        return Err(if zh {
            "scratch_note_path 必须指向 scratch/ 下的有效文件".into()
        } else {
            "scratch_note_path must point to a valid file inside scratch/".into()
        });
    }
    Ok(cid.to_string())
}

fn check_no_symlinks_under(base: &Path, rel: &Path, zh: bool) -> Result<(), String> {
    let mut cur = base.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        if let Ok(meta) = fs::symlink_metadata(&cur) {
            if meta.file_type().is_symlink() {
                return Err(if zh {
                    "scratch_note_path 禁止指向或经过符号链接".into()
                } else {
                    "scratch_note_path must not point to or traverse a symbolic link".into()
                });
            }
        }
    }
    Ok(())
}

/// Pure, zero-IO validation of an optional `scratch_note_path` to prevent path traversal,
/// arbitrary filesystem references, or control-character injection into `/compact` and handoff prompts.
pub fn sanitize_scratch_note_path_pure_for_lang(raw: &str, lang: Lang) -> Result<String, String> {
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
    if trimmed.contains('\\') {
        return Err(if zh {
            "scratch_note_path 禁止包含反斜杠 '\\'".into()
        } else {
            "scratch_note_path must not contain backslashes".into()
        });
    }
    if trimmed == "~" || trimmed.starts_with("~/") {
        return Err(if zh {
            "scratch_note_path 必须位于 scratch/ 或 brain/<cid>/scratch/ 下".into()
        } else {
            "scratch_note_path must be under scratch/ or brain/<cid>/scratch/".into()
        });
    }
    let p = Path::new(trimmed);
    let comps: Vec<Component<'_>> = p.components().collect();
    for comp in &comps {
        if matches!(comp, Component::ParentDir | Component::Prefix(_)) {
            return Err(if zh {
                "scratch_note_path 禁止包含路径穿越段 '..'".into()
            } else {
                "scratch_note_path must not contain '..' path traversal segments".into()
            });
        }
    }

    if !p.is_absolute() {
        let valid_rel = comps.len() >= 2
            && matches!(comps[0], Component::Normal(s) if s == "scratch")
            && comps[1..].iter().all(|c| matches!(c, Component::Normal(_)));
        if !valid_rel {
            return Err(if zh {
                "相对 scratch_note_path 必须位于 scratch/ 目录下".into()
            } else {
                "relative scratch_note_path must stay inside scratch/".into()
            });
        }
        return Ok(trimmed.to_string());
    }

    // Absolute path must structurally end in .../brain/<safe_cid>/scratch/<file...>
    let mut matched = false;
    for i in 0..comps.len() {
        if matches!(comps[i], Component::Normal(s) if s == "brain") && i + 3 < comps.len() {
            let tail: PathBuf = comps[i..].iter().collect();
            if validate_brain_scratch_rel(&tail, None, zh).is_ok() {
                matched = true;
                break;
            }
        }
    }
    if !matched {
        return Err(if zh {
            "绝对 scratch_note_path 必须位于 brain/<cid>/scratch/ 下".into()
        } else {
            "absolute scratch_note_path must be confined under brain/<cid>/scratch/".into()
        });
    }
    Ok(trimmed.to_string())
}

/// Validate and sanitize an optional `scratch_note_path` strictly confined under
/// `<data_root>/brain/<cid>/scratch/` (or relative `scratch/<file>`), rejecting symlinks.
pub fn sanitize_scratch_note_path_in_root_for_lang(
    raw: &str,
    data_root: &Path,
    expected_cid: Option<&str>,
    lang: Lang,
) -> Result<String, String> {
    let clean = sanitize_scratch_note_path_pure_for_lang(raw, lang)?;
    if clean.is_empty() {
        return Ok(clean);
    }
    let zh = lang.is_zh();
    let p = Path::new(&clean);

    if !p.is_absolute() {
        if let Some(ecid) = expected_cid.map(str::trim).filter(|s| !s.is_empty()) {
            if is_safe_id(ecid) {
                let rel_under_root = Path::new("brain").join(ecid).join(p);
                check_no_symlinks_under(data_root, &rel_under_root, zh)?;
            }
        } else if let Ok(meta) = fs::symlink_metadata(p) {
            if meta.file_type().is_symlink() {
                return Err(if zh {
                    "scratch_note_path 禁止指向符号链接".into()
                } else {
                    "scratch_note_path must not point to a symbolic link".into()
                });
            }
        }
        return Ok(clean);
    }

    let canon_root =
        fs::canonicalize(data_root).unwrap_or_else(|_| normalize_macos_alias(data_root));
    let norm_root = normalize_macos_alias(data_root);
    let norm_p = normalize_macos_alias(p);

    let (matched_base, rel) = if let Ok(r) = p.strip_prefix(data_root) {
        (data_root.to_path_buf(), r.to_path_buf())
    } else if let Ok(r) = norm_p.strip_prefix(&norm_root) {
        (norm_root.clone(), r.to_path_buf())
    } else if let Ok(r) = norm_p.strip_prefix(&canon_root) {
        (canon_root.clone(), r.to_path_buf())
    } else {
        return Err(if zh {
            "scratch_note_path 超出当前 data_root 的 brain/<cid>/scratch/ 范围".into()
        } else {
            "scratch_note_path is outside the allowed data_root/brain/<cid>/scratch/ directory"
                .into()
        });
    };

    let cid = validate_brain_scratch_rel(&rel, expected_cid, zh)?;
    check_no_symlinks_under(&matched_base, &rel, zh)?;

    // If the target or its scratch directory exists, verify canonical confinement too.
    let expected_scratch = canon_root.join("brain").join(&cid).join("scratch");
    if let Ok(canon_target) = fs::canonicalize(p) {
        if !canon_target.starts_with(&expected_scratch) {
            return Err(if zh {
                "scratch_note_path 规范化路径超出 scratch/ 目录".into()
            } else {
                "canonical scratch_note_path escapes brain/<cid>/scratch/".into()
            });
        }
    }

    Ok(clean)
}

/// Validate and sanitize an optional `scratch_note_path` including local symlink rejection
/// and confinement under the default `<data_root>/brain/<cid>/scratch/`.
pub fn sanitize_scratch_note_path(raw: &str) -> Result<String, String> {
    sanitize_scratch_note_path_for_lang(raw, resolve_active_lang(""))
}

pub fn sanitize_scratch_note_path_for_lang(raw: &str, lang: Lang) -> Result<String, String> {
    sanitize_scratch_note_path_in_root_for_lang(raw, &default_data_root(), None, lang)
}

pub fn build_compact_command(preserve_points: &str, resume_task: &str) -> String {
    build_compact_command_for_lang(preserve_points, resume_task, resolve_active_lang(""))
}

pub fn build_compact_command_for_lang(
    preserve_points: &str,
    resume_task: &str,
    lang: Lang,
) -> String {
    build_compact_command_with_note_for_lang(preserve_points, resume_task, "", lang)
}

/// Build a single-line directed `/compact` command, optionally referencing a distilled
/// `scratch_note_path` (`scratch/slice_*.md`) for on-demand reads after compaction.
pub fn build_compact_command_with_note_for_lang(
    preserve_points: &str,
    resume_task: &str,
    scratch_note_path: &str,
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
    let note = scratch_note_path
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    match (!pts.is_empty(), !note.is_empty(), !nxt.is_empty()) {
        (true, false, true) => {
            if zh {
                format!("/compact 保留关键上下文与约束：{pts}。压缩完成后请继续执行：{nxt}")
            } else {
                format!("/compact Preserve key context and constraints: {pts}. After compaction, continue with: {nxt}")
            }
        }
        (true, false, false) => {
            if zh {
                format!("/compact 保留关键上下文与约束：{pts}。压缩完成后请简要确认已保留要点并等待后续指令。")
            } else {
                format!("/compact Preserve key context and constraints: {pts}. After compaction, briefly confirm preserved points and stop to await further instructions.")
            }
        }
        (true, true, true) => {
            if zh {
                format!("/compact 保留关键上下文与约束：{pts}。切片摘要备忘（按需读取）：`{note}`。压缩完成后请继续执行：{nxt}")
            } else {
                format!("/compact Preserve key context and constraints: {pts}. Distilled slice note (read on demand): `{note}`. After compaction, continue with: {nxt}")
            }
        }
        (true, true, false) => {
            if zh {
                format!("/compact 保留关键上下文与约束：{pts}。切片摘要备忘（按需读取）：`{note}`。压缩完成后请简要确认已保留要点并等待后续指令。")
            } else {
                format!("/compact Preserve key context and constraints: {pts}. Distilled slice note (read on demand): `{note}`. After compaction, briefly confirm preserved points and stop to await further instructions.")
            }
        }
        (false, true, true) => {
            if zh {
                format!("/compact 切片摘要备忘（按需读取）：`{note}`。压缩完成后请继续执行：{nxt}")
            } else {
                format!("/compact Distilled slice note (read on demand): `{note}`. After compaction, continue with: {nxt}")
            }
        }
        (false, true, false) => {
            if zh {
                format!("/compact 切片摘要备忘（按需读取）：`{note}`。")
            } else {
                format!("/compact Distilled slice note (read on demand): `{note}`.")
            }
        }
        (false, false, true) => format!("/compact {nxt}"),
        (false, false, false) => "/compact".to_string(),
    }
}
