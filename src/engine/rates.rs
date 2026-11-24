//! Config-driven model pricing, currency FX conversion, and lazy external catalog loader.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, OnceLock};

use regex::Regex;
use serde_json::Value;

use crate::i18n::{infer_currency_symbol, resolve_active_lang, Lang};
use crate::settings::{
    RateOverrideRule, Settings, SettingsWatcher, DEFAULT_CONTEXT_LIMIT, DEFAULT_FX_RATE,
};

#[derive(Debug, Clone, PartialEq)]
pub struct RateCard {
    pub input_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_write_per_m: f64,
    pub output_per_m: f64,
    pub context_limit: u64,
    pub source: String,
    pub pricing_known: bool,
    pub tier_threshold: u64,
    pub tier_input: f64,
    pub tier_cache_read: f64,
    pub tier_cache_write: f64,
    pub tier_output: f64,
}

impl RateCard {
    #[allow(clippy::too_many_arguments)]
    fn builtin(
        input_per_m: f64,
        cache_read_per_m: f64,
        cache_write_per_m: f64,
        output_per_m: f64,
        context_limit: u64,
        tier_threshold: u64,
        tier_input: f64,
        tier_cache_read: f64,
        tier_cache_write: f64,
        tier_output: f64,
    ) -> Self {
        Self {
            input_per_m,
            cache_read_per_m,
            cache_write_per_m,
            output_per_m,
            context_limit,
            source: "builtin".to_string(),
            pricing_known: true,
            tier_threshold,
            tier_input,
            tier_cache_read,
            tier_cache_write,
            tier_output,
        }
    }
}

fn builtin_rates_table() -> &'static (HashMap<String, RateCard>, Vec<(String, RateCard)>) {
    static TABLE: OnceLock<(HashMap<String, RateCard>, Vec<(String, RateCard)>)> = OnceLock::new();
    TABLE.get_or_init(|| {
        let entries: Vec<(&str, RateCard)> = vec![
            // Current Antigravity Gemini 4 & Orchestrator families
            (
                "gemini-4-neon",
                RateCard::builtin(2.50, 0.25, 2.50, 15.0, DEFAULT_CONTEXT_LIMIT, 200_000, 5.0, 0.50, 5.0, 22.5),
            ),
            (
                "gemini-4-carbon",
                RateCard::builtin(1.50, 0.15, 1.50, 10.0, DEFAULT_CONTEXT_LIMIT, 200_000, 3.0, 0.30, 3.0, 15.0),
            ),
            (
                "gemini-4-argon",
                RateCard::builtin(1.50, 0.15, 1.50, 10.0, DEFAULT_CONTEXT_LIMIT, 200_000, 3.0, 0.30, 3.0, 15.0),
            ),
            (
                "gemini-4-hydrogen",
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, DEFAULT_CONTEXT_LIMIT, 200_000, 1.0, 0.10, 1.0, 6.0),
            ),
            (
                "gemini-4-pro",
                RateCard::builtin(2.50, 0.25, 2.50, 15.0, DEFAULT_CONTEXT_LIMIT, 200_000, 5.0, 0.50, 5.0, 22.5),
            ),
            (
                "gemini-4-flash-lite",
                RateCard::builtin(0.15, 0.015, 0.15, 0.60, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gemini-4-flash",
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, DEFAULT_CONTEXT_LIMIT, 200_000, 1.0, 0.10, 1.0, 6.0),
            ),
            (
                "gemini-pro-orchestrator-neon",
                RateCard::builtin(2.50, 0.25, 2.50, 15.0, DEFAULT_CONTEXT_LIMIT, 200_000, 5.0, 0.50, 5.0, 22.5),
            ),
            (
                "gemini-pro-orchestrator",
                RateCard::builtin(1.50, 0.15, 1.50, 10.0, DEFAULT_CONTEXT_LIMIT, 200_000, 3.0, 0.30, 3.0, 15.0),
            ),
            // Gemini 3.x & 2.5 families
            (
                "gemini-3.8-pro",
                RateCard::builtin(2.0, 0.20, 2.0, 12.0, DEFAULT_CONTEXT_LIMIT, 200_000, 4.0, 0.40, 4.0, 18.0),
            ),
            (
                "gemini-3.8-flash",
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gemini-3.5-pro",
                RateCard::builtin(2.0, 0.20, 2.0, 12.0, DEFAULT_CONTEXT_LIMIT, 200_000, 4.0, 0.40, 4.0, 18.0),
            ),
            (
                "gemini-3.5-flash",
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gemini-3.1-pro",
                RateCard::builtin(2.0, 0.20, 2.0, 12.0, DEFAULT_CONTEXT_LIMIT, 200_000, 4.0, 0.40, 4.0, 18.0),
            ),
            (
                "gemini-3.1-flash",
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gemini-3-pro",
                RateCard::builtin(2.0, 0.20, 2.0, 12.0, DEFAULT_CONTEXT_LIMIT, 200_000, 4.0, 0.40, 4.0, 18.0),
            ),
            (
                "gemini-3-flash-lite",
                RateCard::builtin(0.15, 0.015, 0.15, 0.60, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gemini-3-flash",
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gemini-2.5-pro",
                RateCard::builtin(1.25, 0.125, 1.25, 10.0, DEFAULT_CONTEXT_LIMIT, 200_000, 2.5, 0.25, 2.5, 15.0),
            ),
            (
                "gemini-2.5-flash-lite",
                RateCard::builtin(0.10, 0.01, 0.10, 0.40, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gemini-2.5-flash",
                RateCard::builtin(0.30, 0.03, 0.30, 2.50, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            // Anthropic Claude 4 / 4.5 / 4.6 / 5.x / 3.7 / 3.5 families
            (
                "claude-opus-5-5",
                RateCard::builtin(5.0, 0.50, 6.25, 25.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-opus-5",
                RateCard::builtin(5.0, 0.50, 6.25, 25.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-opus-4-6",
                RateCard::builtin(5.0, 0.50, 6.25, 25.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-opus-4-5",
                RateCard::builtin(5.0, 0.50, 6.25, 25.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-opus-4",
                RateCard::builtin(15.0, 1.50, 18.75, 75.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-sonnet-5-5",
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-sonnet-5",
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-sonnet-4-6",
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-sonnet-4-5",
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-sonnet-4",
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-3-7-sonnet",
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-3-5-sonnet",
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-haiku-4-6",
                RateCard::builtin(1.0, 0.10, 1.25, 5.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-haiku-4-5",
                RateCard::builtin(1.0, 0.10, 1.25, 5.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-haiku-4",
                RateCard::builtin(0.80, 0.08, 1.00, 4.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "claude-3-5-haiku",
                RateCard::builtin(0.80, 0.08, 1.00, 4.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            // OpenAI GPT-5 / GPT-OSS / GPT-4.1 / o3 / o4 families
            (
                "gpt-5-pro",
                RateCard::builtin(15.0, 1.50, 15.0, 120.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gpt-5-mini",
                RateCard::builtin(0.25, 0.025, 0.25, 2.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gpt-5-nano",
                RateCard::builtin(0.05, 0.005, 0.05, 0.40, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gpt-5",
                RateCard::builtin(1.25, 0.125, 1.25, 10.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gpt-oss",
                RateCard::builtin(0.15, 0.03, 0.15, 0.60, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gpt-4.1-mini",
                RateCard::builtin(0.40, 0.10, 0.40, 1.60, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gpt-4.1",
                RateCard::builtin(2.0, 0.50, 2.0, 8.0, DEFAULT_CONTEXT_LIMIT, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "gpt-4o",
                RateCard::builtin(2.50, 1.25, 2.50, 10.0, 128_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "o3-pro",
                RateCard::builtin(20.0, 5.0, 20.0, 80.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "o3-mini",
                RateCard::builtin(1.10, 0.55, 1.10, 4.40, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "o3",
                RateCard::builtin(2.0, 0.50, 2.0, 8.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "o4-mini",
                RateCard::builtin(1.10, 0.275, 1.10, 4.40, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
            (
                "o4",
                RateCard::builtin(2.0, 0.50, 2.0, 8.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0),
            ),
        ];

        let mut map = HashMap::with_capacity(entries.len());
        let mut sorted = Vec::with_capacity(entries.len());
        for (k, v) in entries {
            map.insert(k.to_string(), v.clone());
            sorted.push((k.to_string(), v));
        }
        sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        (map, sorted)
    })
}

fn family_fallback_rate(cands: &[String], default_limit: u64) -> Option<RateCard> {
    for cand in cands {
        if cand.starts_with("gemini-4") {
            let mut card = if cand.contains("hydrogen") || cand.contains("flash") || cand.contains("lite") {
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, default_limit, 200_000, 1.0, 0.10, 1.0, 6.0)
            } else if cand.contains("neon") || cand.contains("pro") || cand.contains("ultra") {
                RateCard::builtin(2.50, 0.25, 2.50, 15.0, default_limit, 200_000, 5.0, 0.50, 5.0, 22.5)
            } else {
                RateCard::builtin(1.50, 0.15, 1.50, 10.0, default_limit, 200_000, 3.0, 0.30, 3.0, 15.0)
            };
            card.source = "builtin-family".to_string();
            return Some(card);
        }
        if cand.starts_with("gemini-3") {
            let mut card = if cand.contains("flash") || cand.contains("lite") {
                RateCard::builtin(0.50, 0.05, 0.50, 3.0, default_limit, 0, 0.0, 0.0, 0.0, 0.0)
            } else {
                RateCard::builtin(2.0, 0.20, 2.0, 12.0, default_limit, 200_000, 4.0, 0.40, 4.0, 18.0)
            };
            card.source = "builtin-family".to_string();
            return Some(card);
        }
        if cand.starts_with("claude-") {
            let mut card = if cand.contains("opus") {
                RateCard::builtin(5.0, 0.50, 6.25, 25.0, default_limit, 0, 0.0, 0.0, 0.0, 0.0)
            } else if cand.contains("haiku") {
                RateCard::builtin(1.0, 0.10, 1.25, 5.0, default_limit, 0, 0.0, 0.0, 0.0, 0.0)
            } else {
                RateCard::builtin(3.0, 0.30, 3.75, 15.0, default_limit, 0, 0.0, 0.0, 0.0, 0.0)
            };
            card.source = "builtin-family".to_string();
            return Some(card);
        }
        if cand.starts_with("gpt-5") {
            let mut card = if cand.contains("nano") {
                RateCard::builtin(0.05, 0.005, 0.05, 0.40, default_limit, 0, 0.0, 0.0, 0.0, 0.0)
            } else if cand.contains("mini") {
                RateCard::builtin(0.25, 0.025, 0.25, 2.0, default_limit, 0, 0.0, 0.0, 0.0, 0.0)
            } else {
                RateCard::builtin(1.25, 0.125, 1.25, 10.0, default_limit, 0, 0.0, 0.0, 0.0, 0.0)
            };
            card.source = "builtin-family".to_string();
            return Some(card);
        }
        if cand.starts_with("o3") || cand.starts_with("o4") {
            let mut card = if cand.contains("mini") {
                RateCard::builtin(1.10, 0.275, 1.10, 4.40, 200_000, 0, 0.0, 0.0, 0.0, 0.0)
            } else {
                RateCard::builtin(2.0, 0.50, 2.0, 8.0, 200_000, 0, 0.0, 0.0, 0.0, 0.0)
            };
            card.source = "builtin-family".to_string();
            return Some(card);
        }
    }
    None
}

fn effort_suffix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:-(?:tiered|xhigh|high|medium|low|trial|thinking|preview|exp|latest))+$")
            .unwrap()
    })
}

fn paren_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s*\([^)]*\)\s*$").unwrap())
}

fn gemini_deploy_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(gemini-.+)-[a-z]$").unwrap())
}

fn ws_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

fn match_override(rule: &RateOverrideRule, m: &str, combined: &str) -> bool {
    if rule.match_exact.iter().any(|ex| m == ex) {
        return true;
    }
    if rule.match_prefix.iter().any(|pre| m.starts_with(pre.as_str())) {
        return true;
    }
    if rule.match_contains.iter().any(|sub| combined.contains(sub.as_str())) {
        return true;
    }
    false
}

fn candidate_keys(model: &str, model_display: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    let mut add = |raw: &str| {
        let s = raw.trim().to_ascii_lowercase();
        if s.is_empty() {
            return;
        }
        let mut variants = vec![s.clone()];
        let stripped = paren_tag_re().replace(&s, "").trim().to_string();
        if stripped != s && !stripped.is_empty() {
            variants.push(stripped);
        }
        let base_len = variants.len();
        for i in 0..base_len {
            let slug = ws_re().replace_all(&variants[i], "-").into_owned();
            if slug.starts_with("claude-") {
                let dashed = slug.replace('.', "-");
                variants.push(dashed);
            }
            variants.push(slug);
        }
        let mid_len = variants.len();
        for i in 0..mid_len {
            let no_eff = effort_suffix_re().replace(&variants[i], "").into_owned();
            if no_eff != variants[i] {
                variants.push(no_eff.clone());
            }
            if let Some(caps) = gemini_deploy_re().captures(&no_eff) {
                if let Some(g1) = caps.get(1) {
                    variants.push(g1.as_str().to_string());
                }
            }
        }
        for v in variants {
            if !v.is_empty() && seen.insert(v.clone()) {
                out.push(v);
            }
        }
    };

    add(model);
    add(model_display);
    out
}

fn parse_external_model_entry(
    info: &serde_json::Map<String, Value>,
    source: String,
    default_limit: u64,
) -> Option<RateCard> {
    let cost = info.get("cost")?.as_object()?;
    let in_p = cost.get("input").and_then(Value::as_f64).unwrap_or(0.0);
    let out_p = cost.get("output").and_then(Value::as_f64).unwrap_or(0.0);
    let cr_p = cost.get("cache_read").and_then(Value::as_f64).unwrap_or(in_p * 0.1);
    let cw_p = cost.get("cache_write").and_then(Value::as_f64).unwrap_or(in_p);
    if in_p <= 0.0 && out_p <= 0.0 {
        return None;
    }

    let mut ctx_limit = default_limit;
    if let Some(lim) = info.get("limit").and_then(Value::as_object) {
        if let Some(raw_ctx) = lim.get("context").and_then(Value::as_u64) {
            if raw_ctx > 0 {
                ctx_limit = raw_ctx.min(default_limit);
            }
        }
    }

    let mut tier_threshold = 0u64;
    let (mut t_in, mut t_cr, mut t_cw, mut t_out) = (0.0, 0.0, 0.0, 0.0);
    if let Some(tiers) = cost.get("tiers").and_then(Value::as_array) {
        if let Some(first) = tiers.first().and_then(Value::as_object) {
            if let Some(tmeta) = first.get("tier").and_then(Value::as_object) {
                if tmeta.get("type").and_then(Value::as_str) == Some("context") {
                    if let Some(sz) = tmeta.get("size").and_then(Value::as_u64) {
                        tier_threshold = sz;
                        t_in = first.get("input").and_then(Value::as_f64).unwrap_or(in_p);
                        t_cr = first.get("cache_read").and_then(Value::as_f64).unwrap_or(cr_p);
                        t_cw = first.get("cache_write").and_then(Value::as_f64).unwrap_or(t_in);
                        t_out = first.get("output").and_then(Value::as_f64).unwrap_or(out_p);
                    }
                }
            }
        }
    }

    Some(RateCard {
        input_per_m: in_p,
        cache_read_per_m: cr_p,
        cache_write_per_m: cw_p,
        output_per_m: out_p,
        context_limit: ctx_limit,
        source,
        pricing_known: true,
        tier_threshold,
        tier_input: t_in,
        tier_cache_read: t_cr,
        tier_cache_write: t_cw,
        tier_output: t_out,
    })
}

pub struct RateTable {
    pub watcher: SettingsWatcher,
    explicit_models_path: Option<String>,
    explicit_fxrate_path: Option<String>,

    ext_models_path: String,
    ext_models_sig: Option<(i128, u64)>,
    ext_models_map: HashMap<String, RateCard>,
    ext_models_sorted: Vec<(String, RateCard)>,

    ext_fx_path: String,
    ext_fx_sig: Option<(i128, u64)>,
    ext_fx_rate: Option<f64>,

    user_models_gen: u64,
    user_models_synced: bool,
    user_models_map: HashMap<String, RateCard>,
    user_models_sorted: Vec<(String, RateCard)>,

    resolve_cache_ver: (u64, Option<(i128, u64)>),
    resolve_cache: HashMap<(String, String), RateCard>,
}

impl RateTable {
    pub fn new(
        watcher: SettingsWatcher,
        models_path: Option<String>,
        fxrate_path: Option<String>,
    ) -> Self {
        Self {
            watcher,
            explicit_models_path: models_path,
            explicit_fxrate_path: fxrate_path,
            ext_models_path: String::new(),
            ext_models_sig: None,
            ext_models_map: HashMap::new(),
            ext_models_sorted: Vec::new(),
            ext_fx_path: String::new(),
            ext_fx_sig: None,
            ext_fx_rate: None,
            user_models_gen: 0,
            user_models_synced: false,
            user_models_map: HashMap::new(),
            user_models_sorted: Vec::new(),
            resolve_cache_ver: (u64::MAX, None),
            resolve_cache: HashMap::new(),
        }
    }

    pub fn current_settings(&mut self) -> Arc<Settings> {
        self.watcher.current()
    }

    pub fn external_models_loaded(&self) -> bool {
        self.ext_models_sig.is_some()
    }

    pub fn get_lang(&mut self) -> Lang {
        let cfg = self.watcher.current();
        resolve_active_lang(&cfg.lang)
    }

    pub fn is_currency_explicit(&mut self) -> bool {
        self.watcher.current().pricing.currency_configured
    }

    pub fn get_currency(&mut self) -> (String, String) {
        let cfg = self.watcher.current();
        if cfg.pricing.currency_configured {
            let code = if cfg.pricing.currency_code.is_empty() {
                "USD".to_string()
            } else {
                cfg.pricing.currency_code.clone()
            };
            let sym = if cfg.pricing.currency_symbol.is_empty() {
                infer_currency_symbol(&code).to_string()
            } else {
                cfg.pricing.currency_symbol.clone()
            };
            return (code, sym);
        }
        let (code, sym) = resolve_active_lang(&cfg.lang).default_currency();
        (code.to_string(), sym.to_string())
    }

    fn sync_user_models(&mut self, cfg: &Settings) {
        if self.user_models_synced && self.user_models_gen == cfg.generation {
            return;
        }
        let mut map = HashMap::with_capacity(cfg.pricing.models.len());
        let mut sorted = Vec::with_capacity(cfg.pricing.models.len());
        for (k, info) in &cfg.pricing.models {
            let card = RateCard {
                input_per_m: info.input_per_m,
                cache_read_per_m: info.cache_read_per_m,
                cache_write_per_m: info.cache_write_per_m,
                output_per_m: info.output_per_m,
                context_limit: if info.context_limit > 0 {
                    info.context_limit
                } else {
                    cfg.context.default_limit
                },
                source: info.source.clone(),
                pricing_known: info.input_per_m > 0.0 || info.output_per_m > 0.0,
                tier_threshold: info.tier_threshold,
                tier_input: info.tier_input,
                tier_cache_read: info.tier_cache_read,
                tier_cache_write: info.tier_cache_write,
                tier_output: info.tier_output,
            };
            map.insert(k.clone(), card.clone());
            sorted.push((k.clone(), card));
        }
        sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        self.user_models_map = map;
        self.user_models_sorted = sorted;
        self.user_models_gen = cfg.generation;
        self.user_models_synced = true;
        self.resolve_cache.clear();
    }

    pub fn get_fx_rate(&mut self) -> (f64, String) {
        let cfg = self.watcher.current();
        let fx_path = self
            .explicit_fxrate_path
            .clone()
            .unwrap_or_else(|| cfg.pricing.external_fxrate_path.clone());
        if !fx_path.is_empty() {
            if let Ok(meta) = fs::metadata(&fx_path) {
                let sig = (
                    (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128),
                    meta.size(),
                );
                if self.ext_fx_path != fx_path || self.ext_fx_sig != Some(sig) {
                    self.ext_fx_rate = fs::read_to_string(&fx_path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .and_then(|v| v.get("rate").and_then(Value::as_f64))
                        .filter(|&r| r > 0.01 && r < 1000.0);
                    self.ext_fx_path = fx_path;
                    self.ext_fx_sig = Some(sig);
                }
                if let Some(rate) = self.ext_fx_rate {
                    return (rate, "external-fx".to_string());
                }
            }
        }
        if cfg.pricing.fx_rate_configured {
            return (cfg.pricing.fx_rate, "user-config".to_string());
        }
        (DEFAULT_FX_RATE, "default".to_string())
    }

    fn ensure_external_models_loaded(&mut self, cfg: &Settings) {
        let mpath = self
            .explicit_models_path
            .clone()
            .unwrap_or_else(|| cfg.pricing.external_models_path.clone());
        if mpath.is_empty() {
            self.ext_models_map.clear();
            self.ext_models_sorted.clear();
            self.ext_models_sig = None;
            return;
        }
        let Ok(meta) = fs::metadata(&mpath) else {
            return;
        };
        let sig = (
            (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128),
            meta.size(),
        );
        if self.ext_models_path == mpath && self.ext_models_sig == Some(sig) {
            return;
        }
        let Ok(raw_text) = fs::read_to_string(&mpath) else {
            return;
        };
        let Ok(Value::Object(root)) = serde_json::from_str::<Value>(&raw_text) else {
            return;
        };

        const PREFERRED: &[&str] = &["anthropic", "google", "openai"];
        let mut order: Vec<String> = root
            .keys()
            .filter(|k| !PREFERRED.contains(&k.as_str()))
            .cloned()
            .collect();
        order.sort();
        for p in PREFERRED {
            if root.contains_key(*p) {
                order.push((*p).to_string());
            }
        }

        let mut parsed: HashMap<String, RateCard> = HashMap::new();
        for prov in order {
            let Some(pinfo) = root.get(&prov).and_then(Value::as_object) else {
                continue;
            };
            let Some(models) = pinfo.get("models").and_then(Value::as_object) else {
                continue;
            };
            for (mid, minfo) in models {
                let Some(mo) = minfo.as_object() else { continue };
                if let Some(card) =
                    parse_external_model_entry(mo, format!("external:{prov}"), cfg.context.default_limit)
                {
                    parsed.insert(mid.to_ascii_lowercase(), card);
                }
            }
        }

        let mut sorted: Vec<(String, RateCard)> =
            parsed.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

        self.ext_models_path = mpath;
        self.ext_models_sig = Some(sig);
        self.ext_models_map = parsed;
        self.ext_models_sorted = sorted;
        self.resolve_cache.clear();
    }

    pub fn short_model_label(&mut self, model_disp: &str, model_id: &str) -> String {
        let cfg = self.watcher.current();
        let lang = resolve_active_lang(&cfg.lang);
        let raw = if !model_disp.trim().is_empty() {
            model_disp.trim()
        } else {
            model_id.trim()
        };
        if raw.is_empty() || raw == "unknown" || raw == "未知模型" {
            return if lang.is_zh() {
                "未知".to_string()
            } else {
                "Unknown".to_string()
            };
        }
        let low = raw.to_ascii_lowercase();
        for (needle, label) in &cfg.pricing.short_labels {
            if low.contains(needle.as_str()) {
                return label.clone();
            }
        }
        let compact = raw
            .replace("(XHigh)", "XH")
            .replace("(High)", "")
            .replace("(Medium)", "M")
            .replace("(Low)", "L");
        let mut compact = ws_re().replace_all(&compact, " ").trim().to_string();
        if compact.chars().count() > 18 {
            for prefix in ["Google ", "Anthropic ", "OpenAI ", "Gemini ", "Claude "] {
                if let Some(rest) = compact.strip_prefix(prefix) {
                    if rest.trim().chars().count() >= 4 {
                        compact = rest.trim().to_string();
                        break;
                    }
                }
            }
        }
        if compact.chars().count() > 18 {
            compact.chars().take(18).collect()
        } else {
            compact
        }
    }

    pub fn resolve_rate(&mut self, model: &str, model_display: &str) -> RateCard {
        let cfg = self.watcher.current();
        self.sync_user_models(&cfg);

        let ver = (cfg.generation, self.ext_models_sig);
        if ver != self.resolve_cache_ver {
            self.resolve_cache.clear();
            self.resolve_cache_ver = ver;
        }

        let key = (model.to_string(), model_display.to_string());
        if let Some(hit) = self.resolve_cache.get(&key) {
            return hit.clone();
        }

        let resolved = self.resolve_uncached(&cfg, model, model_display);
        self.resolve_cache.insert(key, resolved.clone());
        resolved
    }

    fn resolve_uncached(&mut self, cfg: &Settings, model: &str, model_display: &str) -> RateCard {
        let m = model.trim().to_ascii_lowercase();
        let d = model_display.trim().to_ascii_lowercase();
        let combined = format!("{m} {d}");

        // 1. User high-priority override rules
        for rule in &cfg.pricing.overrides {
            if match_override(rule, &m, &combined) {
                return RateCard {
                    input_per_m: rule.input_per_m,
                    cache_read_per_m: rule.cache_read_per_m,
                    cache_write_per_m: rule.cache_write_per_m,
                    output_per_m: rule.output_per_m,
                    context_limit: if rule.context_limit > 0 {
                        rule.context_limit
                    } else {
                        cfg.context.default_limit
                    },
                    source: rule.source.clone(),
                    pricing_known: rule.input_per_m > 0.0 || rule.output_per_m > 0.0,
                    tier_threshold: rule.tier_threshold,
                    tier_input: rule.tier_input,
                    tier_cache_read: rule.tier_cache_read,
                    tier_cache_write: rule.tier_cache_write,
                    tier_output: rule.tier_output,
                };
            }
        }

        let cands = candidate_keys(model, model_display);

        // 2. User configured exact & prefix models
        for cand in &cands {
            if let Some(card) = self.user_models_map.get(cand) {
                return card.clone();
            }
        }
        for cand in &cands {
            for (mkey, card) in &self.user_models_sorted {
                if cand.starts_with(mkey.as_str()) {
                    return card.clone();
                }
            }
        }

        // 3. Built-in public & Antigravity models
        let (b_map, b_sorted) = builtin_rates_table();
        for cand in &cands {
            if let Some(card) = b_map.get(cand) {
                return card.clone();
            }
        }
        for cand in &cands {
            for (bkey, card) in b_sorted {
                if cand.starts_with(bkey.as_str()) {
                    return card.clone();
                }
            }
        }

        // 4. Lazy external catalog (loaded ONLY on lookup miss!)
        self.ensure_external_models_loaded(cfg);
        if !self.ext_models_map.is_empty() {
            for cand in &cands {
                if let Some(card) = self.ext_models_map.get(cand) {
                    return card.clone();
                }
            }
            for cand in &cands {
                for (ekey, card) in &self.ext_models_sorted {
                    if ekey.len() >= 6 && cand.starts_with(ekey.as_str()) {
                        return card.clone();
                    }
                }
            }
        }

        // 5. Family prefix heuristic for future minor versions (gemini-4*, gemini-3*, claude-*, gpt-5*, o3*/o4*)
        if let Some(family_card) = family_fallback_rate(&cands, cfg.context.default_limit) {
            return family_card;
        }

        RateCard {
            input_per_m: 0.0,
            cache_read_per_m: 0.0,
            cache_write_per_m: 0.0,
            output_per_m: 0.0,
            context_limit: cfg.context.default_limit,
            source: "unknown".to_string(),
            pricing_known: false,
            tier_threshold: 0,
            tier_input: 0.0,
            tier_cache_read: 0.0,
            tier_cache_write: 0.0,
            tier_output: 0.0,
        }
    }

    pub fn calculate_cost_usd(
        &mut self,
        model: &str,
        input_tokens: u64,
        cache_read: u64,
        cache_write: u64,
        output_tokens: u64,
        model_display: &str,
    ) -> f64 {
        let p = self.resolve_rate(model, model_display);
        let total_prompt = input_tokens
            .saturating_add(cache_read)
            .saturating_add(cache_write);
        let (in_p, cr_p, cw_p, out_p) = if p.tier_threshold > 0 && total_prompt > p.tier_threshold {
            (p.tier_input, p.tier_cache_read, p.tier_cache_write, p.tier_output)
        } else {
            (p.input_per_m, p.cache_read_per_m, p.cache_write_per_m, p.output_per_m)
        };
        ((input_tokens as f64) * in_p
            + (cache_read as f64) * cr_p
            + (cache_write as f64) * cw_p
            + (output_tokens as f64) * out_p)
            / 1_000_000.0
    }
}
