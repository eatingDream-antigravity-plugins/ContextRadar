//! Pure, acyclic model-family predicates, KV-cache TTL constants, and model x effort budget tiers.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::i18n::{resolve_active_lang, Lang};
pub use crate::settings::{BudgetTierSpec, TierMatchRule};
use crate::settings::Settings;

/// Empirical Gemini 4 implicit KV cache TTL (3 minutes, anchored to prefill start `prev.began`).
pub const GEMINI_4_CACHE_TTL_SEC: f64 = 180.0;
/// Default implicit/explicit KV cache TTL for non-Gemini-4 models (5 minutes).
pub const DEFAULT_CACHE_TTL_SEC: f64 = 300.0;

#[inline]
pub fn is_gemini_4_model(model: &str, model_display: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    let d = model_display.trim().to_ascii_lowercase();
    m.starts_with("gemini-4")
        || m.contains("/gemini-4")
        || d.starts_with("gemini 4")
        || d.contains("gemini 4")
        || d.contains("gemini-4")
}

#[inline]
pub fn is_gemini_4_carbon(model: &str, model_display: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    let d = model_display.trim().to_ascii_lowercase();
    m.starts_with("gemini-4-carbon")
        || m.contains("/gemini-4-carbon")
        || (is_gemini_4_model(model, model_display) && (m.contains("carbon") || d.contains("carbon")))
}

#[inline]
pub fn model_cache_ttl_sec(model: &str, model_display: &str) -> f64 {
    if is_gemini_4_model(model, model_display) {
        GEMINI_4_CACHE_TTL_SEC
    } else {
        DEFAULT_CACHE_TTL_SEC
    }
}

fn fast_model_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|[\s_\-/])(?:flash|mini|haiku|lite|hydrogen|nano)(?:$|[\s_\-/])").unwrap()
    })
}

fn xhigh_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s_\-/(])(?:xhigh|o3)(?:$|[\s_\-/)])").unwrap())
}

fn pro_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s_\-/])pro(?:$|[\s_\-/])").unwrap())
}

fn neon_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s_\-/(])neon(?:$|[\s_\-/)])").unwrap())
}

fn carbon_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s_\-/(])(?:carbon|argon)(?:$|[\s_\-/)])").unwrap())
}

pub fn builtin_tiers_for_lang(lang: Lang) -> HashMap<String, BudgetTierSpec> {
    let zh = lang.is_zh();
    let mut m = HashMap::new();
    let specs = [
        BudgetTierSpec {
            profile_id: "high_reasoning".into(),
            label: if zh {
                "高推理档位（高思考输出）".into()
            } else {
                "High Reasoning Tier".into()
            },
            min_ctx: 250_000,
            recommend_ctx: 290_000,
            urgent_ctx: 410_000,
            min_turns_since_ckpt: 15,
            min_remaining_turns: 7,
        },
        BudgetTierSpec {
            profile_id: "flagship".into(),
            label: if zh {
                "旗舰主力档位".into()
            } else {
                "Flagship Tier".into()
            },
            min_ctx: 270_000,
            recommend_ctx: 310_000,
            urgent_ctx: 420_000,
            min_turns_since_ckpt: 18,
            min_remaining_turns: 8,
        },
        BudgetTierSpec {
            profile_id: "standard".into(),
            label: if zh {
                "标准均衡档位".into()
            } else {
                "Standard Balanced Tier".into()
            },
            min_ctx: 280_000,
            recommend_ctx: 320_000,
            urgent_ctx: 430_000,
            min_turns_since_ckpt: 18,
            min_remaining_turns: 8,
        },
        BudgetTierSpec {
            profile_id: "claude_tier".into(),
            label: if zh {
                "Claude 档位（缓存写 1.25x 溢价）".into()
            } else {
                "Claude Tier (1.25x Cache Write)".into()
            },
            min_ctx: 300_000,
            recommend_ctx: 350_000,
            urgent_ctx: 440_000,
            min_turns_since_ckpt: 22,
            min_remaining_turns: 10,
        },
        BudgetTierSpec {
            profile_id: "fast_reasoning".into(),
            label: if zh {
                "轻量高推理档位".into()
            } else {
                "Fast High-Reasoning Tier".into()
            },
            min_ctx: 300_000,
            recommend_ctx: 340_000,
            urgent_ctx: 430_000,
            min_turns_since_ckpt: 20,
            min_remaining_turns: 10,
        },
        BudgetTierSpec {
            profile_id: "fast".into(),
            label: if zh {
                "轻量高性价比档位".into()
            } else {
                "Fast Efficiency Tier".into()
            },
            min_ctx: 330_000,
            recommend_ctx: 370_000,
            urgent_ctx: 445_000,
            min_turns_since_ckpt: 25,
            min_remaining_turns: 12,
        },
    ];
    for s in specs {
        m.insert(s.profile_id.clone(), s);
    }
    m
}

pub fn builtin_tiers() -> HashMap<String, BudgetTierSpec> {
    builtin_tiers_for_lang(resolve_active_lang(""))
}

fn rule_matches(rule: &TierMatchRule, raw: &str) -> bool {
    if !rule.exclude_any.is_empty() && rule.exclude_any.iter().any(|tok| raw.contains(tok.as_str()))
    {
        return false;
    }
    if !rule.require_all.is_empty() && !rule.require_all.iter().all(|tok| raw.contains(tok.as_str()))
    {
        return false;
    }
    if !rule.match_any.is_empty() && !rule.match_any.iter().any(|tok| raw.contains(tok.as_str())) {
        return false;
    }
    !rule.match_any.is_empty() || !rule.require_all.is_empty()
}

pub fn resolve_budget_tier(
    model: &str,
    model_display: &str,
    settings: &Settings,
) -> BudgetTierSpec {
    let lang = resolve_active_lang(&settings.lang);
    let raw = format!("{} {}", model.trim(), model_display.trim())
        .trim()
        .to_ascii_lowercase();
    let mut merged = builtin_tiers_for_lang(lang);
    for (k, v) in &settings.budgets.profiles {
        merged.insert(k.clone(), v.clone());
    }

    for rule in &settings.budgets.profile_rules {
        if let Some(spec) = merged.get(&rule.profile) {
            if rule_matches(rule, &raw) {
                return spec.clone();
            }
        }
    }

    let is_xhigh = xhigh_re().is_match(&raw);
    if raw.contains("claude") || raw.contains("opus") || raw.contains("sonnet") {
        return merged.remove("claude_tier").unwrap();
    }
    if fast_model_re().is_match(&raw) {
        let key = if is_xhigh { "fast_reasoning" } else { "fast" };
        return merged.remove(key).unwrap();
    }
    if is_xhigh {
        return merged.remove("high_reasoning").unwrap();
    }
    if carbon_re().is_match(&raw) {
        return merged.remove("standard").unwrap();
    }
    if neon_re().is_match(&raw)
        || (pro_re().is_match(&raw) && !(raw.contains("medium") || raw.contains("low")))
    {
        return merged.remove("flagship").unwrap();
    }

    merged
        .remove(&settings.budgets.default_profile)
        .or_else(|| merged.remove("standard"))
        .unwrap()
}
