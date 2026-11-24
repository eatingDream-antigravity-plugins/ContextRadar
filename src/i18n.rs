//! Locale auto-detection (`en` / `zh-CN`) and adaptive currency helpers.

use std::env;
use std::process::Command;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::settings::LANG_ENV;

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
    if matches!(low.as_str(), "auto" | "default" | "system" | "utf-8" | "utf8") {
        return None;
    }
    let base = low
        .split(['.', '@'])
        .next()
        .unwrap_or(low.as_str())
        .trim();
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
    for v in [&lc_all, &lc_msg, &lang_var] {
        if let Some(ref s) = v {
            if let Some(l) = parse_lang_tag(s) {
                return l;
            }
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
        (true, _) => "5 分钟",
        (false, _) => "5m",
    }
}

pub fn carbon_concurrency_warning_text(lang: Lang, active_carbon_count: usize) -> String {
    if lang.is_zh() {
        format!(
            "并发提示：当前 {active_carbon_count} 个活跃会话同时运行 Gemini 4 Carbon，建议将并发 Carbon 控制在 <= 2，避免单副本 HBM 缓存溢出。"
        )
    } else {
        format!(
            "Concurrency tip: {active_carbon_count} active conversations are running Gemini 4 Carbon simultaneously; cap concurrent Carbon workers <= 2 to avoid per-replica HBM spillover."
        )
    }
}
