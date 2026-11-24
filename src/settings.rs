use std::collections::HashMap;
use std::env;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONFIG_ENV: &str = "CONTEXT_RADAR_CONFIG";
pub const DATA_ROOT_ENV: &str = "CONTEXT_RADAR_DATA_ROOT";
pub const PLUGIN_DATA_ENV: &str = "CONTEXT_RADAR_PLUGIN_DATA";
pub const DISABLE_DISK_CACHE_ENV: &str = "CONTEXT_RADAR_DISABLE_DISK_CACHE";
pub const INDEX_CACHE_PATH_ENV: &str = "CONTEXT_RADAR_INDEX_PATH";
pub const GRANTS_PATH_ENV: &str = "CONTEXT_RADAR_GRANTS_PATH";
pub const PANEL_HTML_ENV: &str = "CONTEXT_RADAR_PANEL_HTML";
pub const LANG_ENV: &str = "CONTEXT_RADAR_LANG";
pub const WORKSPACE_ENV: &str = "CONTEXT_RADAR_WORKSPACE";
pub const CONVERSATION_ID_ENV: &str = "CONTEXT_RADAR_CONVERSATION_ID";

pub const DEFAULT_CONTEXT_LIMIT: u64 = 512_000;
pub const DEFAULT_WARN_THRESHOLD: u64 = 220_000;
pub const DEFAULT_COMPACT_THRESHOLD: u64 = 270_000;
pub const DEFAULT_CRITICAL_THRESHOLD: u64 = 350_000;
pub const DEFAULT_POST_COMPACT_BASE: u64 = 20_500;
pub const DEFAULT_SUBAGENT_SILENT_TIMEOUT_SEC: f64 = 3600.0;
pub const DEFAULT_FX_RATE: f64 = 7.2;
pub const DEFAULT_CURRENCY_CODE: &str = "USD";
pub const DEFAULT_CURRENCY_SYMBOL: &str = "$";

const DATA_ROOT_CANDIDATES: &[&str] = &["antigravity", "antigravity-ide", "antigravity-cli"];
const DISABLE_TOKENS: &[&str] = &["none", "off", "disabled", "/dev/null"];

pub fn user_home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_tilde(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed == "~" {
        return user_home_dir().to_string_lossy().into_owned();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return user_home_dir().join(rest).to_string_lossy().into_owned();
    }
    trimmed.to_string()
}

fn candidate_root_activity_ns(root: &Path) -> Option<i128> {
    if !root.is_dir() {
        return None;
    }
    let mut best_ns: Option<i128> = None;
    let mut bump = |p: &Path| {
        if let Ok(m) = fs::metadata(p) {
            let ns = (m.mtime() as i128) * 1_000_000_000 + (m.mtime_nsec() as i128);
            best_ns = Some(best_ns.map_or(ns, |prev| prev.max(ns)));
        }
    };
    bump(&root.join("conversation_summaries.db"));
    bump(&root.join("conversation_summaries.db-wal"));
    bump(&root.join("conversations"));
    best_ns
}

pub fn select_data_root_from_home(home: &Path) -> PathBuf {
    let gemini_dir = home.join(".gemini");
    let mut best_active: Option<(i128, usize, PathBuf)> = None;
    let mut first_existing_dir: Option<PathBuf> = None;

    for (idx, name) in DATA_ROOT_CANDIDATES.iter().enumerate() {
        let cand = gemini_dir.join(name);
        if cand.is_dir() && first_existing_dir.is_none() {
            first_existing_dir = Some(cand.clone());
        }
        if let Some(ts) = candidate_root_activity_ns(&cand) {
            let take = match &best_active {
                None => true,
                Some((prev_ts, prev_idx, _)) => ts > *prev_ts || (ts == *prev_ts && idx < *prev_idx),
            };
            if take {
                best_active = Some((ts, idx, cand));
            }
        }
    }

    if let Some((_, _, chosen)) = best_active {
        return chosen;
    }
    first_existing_dir.unwrap_or_else(|| gemini_dir.join("antigravity"))
}

pub fn default_data_root() -> PathBuf {
    if let Ok(val) = env::var(DATA_ROOT_ENV) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(expand_tilde(trimmed));
        }
    }
    select_data_root_from_home(&user_home_dir())
}

pub fn default_plugin_data_dir(data_root: &Path) -> PathBuf {
    if let Ok(val) = env::var(PLUGIN_DATA_ENV) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(expand_tilde(trimmed));
        }
    }
    data_root.join("plugin_data").join("context-radar-mcp")
}

pub fn is_disable_token(val: &str) -> bool {
    let lower = val.trim().to_ascii_lowercase();
    DISABLE_TOKENS.iter().any(|t| *t == lower)
}

pub fn resolve_settings_path(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(raw) = explicit {
        let trimmed = raw.trim();
        if trimmed.is_empty() || is_disable_token(trimmed) {
            return None;
        }
        return Some(PathBuf::from(expand_tilde(trimmed)));
    }

    if let Ok(env_val) = env::var(CONFIG_ENV) {
        let trimmed = env_val.trim();
        if !trimmed.is_empty() {
            if is_disable_token(trimmed) {
                return None;
            }
            return Some(PathBuf::from(expand_tilde(trimmed)));
        }
    }

    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            let xdg_path = PathBuf::from(expand_tilde(trimmed))
                .join("context-radar-mcp")
                .join("settings.json");
            if xdg_path.is_file() {
                return Some(xdg_path);
            }
        }
    }

    Some(
        user_home_dir()
            .join(".config")
            .join("context-radar-mcp")
            .join("settings.json"),
    )
}

#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub default_limit: u64,
    pub warn_threshold: u64,
    pub compact_threshold: u64,
    pub critical_threshold: u64,
    pub est_post_compact_base: u64,
    pub subagent_silent_timeout_sec: f64,
    pub enable_disk_cache: bool,
    pub disk_cache_path: String,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            default_limit: DEFAULT_CONTEXT_LIMIT,
            warn_threshold: DEFAULT_WARN_THRESHOLD,
            compact_threshold: DEFAULT_COMPACT_THRESHOLD,
            critical_threshold: DEFAULT_CRITICAL_THRESHOLD,
            est_post_compact_base: DEFAULT_POST_COMPACT_BASE,
            subagent_silent_timeout_sec: DEFAULT_SUBAGENT_SILENT_TIMEOUT_SEC,
            enable_disk_cache: true,
            disk_cache_path: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RateOverrideRule {
    pub match_prefix: Vec<String>,
    pub match_exact: Vec<String>,
    pub match_contains: Vec<String>,
    pub input_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_write_per_m: f64,
    pub output_per_m: f64,
    pub context_limit: u64,
    pub source: String,
    pub tier_threshold: u64,
    pub tier_input: f64,
    pub tier_cache_read: f64,
    pub tier_cache_write: f64,
    pub tier_output: f64,
}

#[derive(Debug, Clone)]
pub struct ModelRateInput {
    pub input_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_write_per_m: f64,
    pub output_per_m: f64,
    pub context_limit: u64,
    pub source: String,
    pub tier_threshold: u64,
    pub tier_input: f64,
    pub tier_cache_read: f64,
    pub tier_cache_write: f64,
    pub tier_output: f64,
}

#[derive(Debug, Clone)]
pub struct PricingConfig {
    pub currency_code: String,
    pub currency_symbol: String,
    pub currency_configured: bool,
    pub fx_rate: f64,
    pub fx_rate_configured: bool,
    pub external_fxrate_path: String,
    pub external_models_path: String,
    pub overrides: Vec<RateOverrideRule>,
    pub models: HashMap<String, ModelRateInput>,
    pub short_labels: Vec<(String, String)>,
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            currency_code: String::new(),
            currency_symbol: String::new(),
            currency_configured: false,
            fx_rate: DEFAULT_FX_RATE,
            fx_rate_configured: false,
            external_fxrate_path: String::new(),
            external_models_path: String::new(),
            overrides: Vec::new(),
            models: HashMap::new(),
            short_labels: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetTierSpec {
    pub profile_id: String,
    pub label: String,
    pub min_ctx: u64,
    pub recommend_ctx: u64,
    pub urgent_ctx: u64,
    pub min_turns_since_ckpt: usize,
    pub min_remaining_turns: usize,
}

#[derive(Debug, Clone)]
pub struct TierMatchRule {
    pub profile: String,
    pub match_any: Vec<String>,
    pub require_all: Vec<String>,
    pub exclude_any: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BudgetsConfig {
    pub default_profile: String,
    pub profiles: HashMap<String, BudgetTierSpec>,
    pub profile_rules: Vec<TierMatchRule>,
}

impl Default for BudgetsConfig {
    fn default() -> Self {
        Self {
            default_profile: "standard".to_string(),
            profiles: HashMap::new(),
            profile_rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GuardsConfig {
    pub boot: Value,
    pub exec_gate: Value,
}

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub generation: u64,
    pub source_path: String,
    pub lang: String,
    pub context: ContextConfig,
    pub pricing: PricingConfig,
    pub budgets: BudgetsConfig,
    pub guards: GuardsConfig,
}

fn val_u64(v: Option<&Value>, default: u64, min: u64) -> u64 {
    let Some(val) = v else { return default };
    let parsed = if let Some(n) = val.as_u64() {
        Some(n)
    } else if let Some(n) = val.as_i64() {
        if n >= 0 {
            Some(n as u64)
        } else {
            None
        }
    } else if let Some(f) = val.as_f64() {
        if f.is_finite() && f >= 0.0 {
            Some(f as u64)
        } else {
            None
        }
    } else {
        None
    };
    match parsed {
        Some(n) if n >= min => n,
        _ => default,
    }
}

fn val_f64(v: Option<&Value>, default: f64, min: f64) -> f64 {
    let Some(val) = v else { return default };
    let parsed = val
        .as_f64()
        .or_else(|| val.as_i64().map(|x| x as f64))
        .or_else(|| val.as_u64().map(|x| x as f64));
    match parsed {
        Some(n) if n.is_finite() && n >= min => n,
        _ => default,
    }
}

fn val_str(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn str_vec_lower(v: Option<&Value>) -> Vec<String> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn parse_settings_value(raw: &Value, source_path: &str, generation: u64) -> Settings {
    let Some(obj) = raw.as_object() else {
        return Settings {
            generation,
            source_path: source_path.to_string(),
            ..Settings::default()
        };
    };

    let ctx_obj = obj.get("context").and_then(Value::as_object);
    let default_limit = val_u64(ctx_obj.and_then(|o| o.get("default_limit")), DEFAULT_CONTEXT_LIMIT, 1000);
    let context = ContextConfig {
        default_limit,
        warn_threshold: val_u64(ctx_obj.and_then(|o| o.get("warn_threshold")), DEFAULT_WARN_THRESHOLD, 1000),
        compact_threshold: val_u64(ctx_obj.and_then(|o| o.get("compact_threshold")), DEFAULT_COMPACT_THRESHOLD, 1000),
        critical_threshold: val_u64(ctx_obj.and_then(|o| o.get("critical_threshold")), DEFAULT_CRITICAL_THRESHOLD, 1000),
        est_post_compact_base: val_u64(ctx_obj.and_then(|o| o.get("est_post_compact_base")), DEFAULT_POST_COMPACT_BASE, 0),
        subagent_silent_timeout_sec: val_f64(
            ctx_obj.and_then(|o| o.get("subagent_silent_timeout_sec")),
            DEFAULT_SUBAGENT_SILENT_TIMEOUT_SEC,
            10.0,
        ),
        enable_disk_cache: ctx_obj
            .and_then(|o| o.get("enable_disk_cache"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        disk_cache_path: expand_tilde(&val_str(ctx_obj.and_then(|o| o.get("disk_cache_path")))),
    };

    let pr_obj = obj.get("pricing").and_then(Value::as_object);
    let fx_configured = pr_obj
        .and_then(|o| o.get("fx_rate"))
        .map(|v| !v.is_null())
        .unwrap_or(false);
    let fx_rate = if fx_configured {
        val_f64(pr_obj.and_then(|o| o.get("fx_rate")), DEFAULT_FX_RATE, 0.0001)
    } else {
        DEFAULT_FX_RATE
    };

    let mut overrides = Vec::new();
    if let Some(arr) = pr_obj.and_then(|o| o.get("overrides")).and_then(Value::as_array) {
        for item in arr {
            let Some(item_obj) = item.as_object() else { continue };
            let in_p = val_f64(item_obj.get("input_per_m"), -1.0, 0.0);
            let out_p = val_f64(item_obj.get("output_per_m"), -1.0, 0.0);
            if in_p < 0.0 || out_p < 0.0 {
                continue;
            }
            let mp = str_vec_lower(item_obj.get("match_prefix"));
            let me = str_vec_lower(item_obj.get("match_exact"));
            let mc = str_vec_lower(item_obj.get("match_contains"));
            if mp.is_empty() && me.is_empty() && mc.is_empty() {
                continue;
            }
            let src = val_str(item_obj.get("source"));
            overrides.push(RateOverrideRule {
                match_prefix: mp,
                match_exact: me,
                match_contains: mc,
                input_per_m: in_p,
                cache_read_per_m: val_f64(item_obj.get("cache_read_per_m"), in_p * 0.1, 0.0),
                cache_write_per_m: val_f64(item_obj.get("cache_write_per_m"), in_p, 0.0),
                output_per_m: out_p,
                context_limit: val_u64(item_obj.get("context_limit"), default_limit, 1000),
                source: if src.is_empty() { "user-override".to_string() } else { src },
                tier_threshold: val_u64(item_obj.get("tier_threshold"), 0, 0),
                tier_input: val_f64(item_obj.get("tier_input"), 0.0, 0.0),
                tier_cache_read: val_f64(item_obj.get("tier_cache_read"), 0.0, 0.0),
                tier_cache_write: val_f64(item_obj.get("tier_cache_write"), 0.0, 0.0),
                tier_output: val_f64(item_obj.get("tier_output"), 0.0, 0.0),
            });
        }
    }

    let mut models = HashMap::new();
    if let Some(m_obj) = pr_obj.and_then(|o| o.get("models")).and_then(Value::as_object) {
        for (k, v) in m_obj {
            let key = k.trim().to_ascii_lowercase();
            let Some(vo) = v.as_object() else { continue };
            if key.is_empty() {
                continue;
            }
            let in_p = val_f64(vo.get("input_per_m"), -1.0, 0.0);
            let out_p = val_f64(vo.get("output_per_m"), -1.0, 0.0);
            if in_p < 0.0 || out_p < 0.0 {
                continue;
            }
            let src = val_str(vo.get("source"));
            models.insert(
                key,
                ModelRateInput {
                    input_per_m: in_p,
                    cache_read_per_m: val_f64(vo.get("cache_read_per_m"), in_p * 0.1, 0.0),
                    cache_write_per_m: val_f64(vo.get("cache_write_per_m"), in_p, 0.0),
                    output_per_m: out_p,
                    context_limit: val_u64(vo.get("context_limit"), default_limit, 1000),
                    source: if src.is_empty() { "user-config".to_string() } else { src },
                    tier_threshold: val_u64(vo.get("tier_threshold"), 0, 0),
                    tier_input: val_f64(vo.get("tier_input"), 0.0, 0.0),
                    tier_cache_read: val_f64(vo.get("tier_cache_read"), 0.0, 0.0),
                    tier_cache_write: val_f64(vo.get("tier_cache_write"), 0.0, 0.0),
                    tier_output: val_f64(vo.get("tier_output"), 0.0, 0.0),
                },
            );
        }
    }

    let mut short_labels = Vec::new();
    if let Some(arr) = pr_obj.and_then(|o| o.get("short_labels")).and_then(Value::as_array) {
        for item in arr {
            let Some(io) = item.as_object() else { continue };
            let contains = val_str(io.get("contains")).to_ascii_lowercase();
            let label = val_str(io.get("label"));
            if !contains.is_empty() && !label.is_empty() {
                short_labels.push((contains, label));
            }
        }
    }

    let lang = val_str(obj.get("lang").or_else(|| obj.get("locale")));
    let c_code = val_str(pr_obj.and_then(|o| o.get("currency_code")));
    let c_sym = val_str(pr_obj.and_then(|o| o.get("currency_symbol")));
    let currency_configured = !c_code.is_empty() || !c_sym.is_empty();
    let pricing = PricingConfig {
        currency_code: c_code,
        currency_symbol: c_sym,
        currency_configured,
        fx_rate,
        fx_rate_configured: fx_configured,
        external_fxrate_path: expand_tilde(&val_str(pr_obj.and_then(|o| o.get("external_fxrate_path")))),
        external_models_path: expand_tilde(&val_str(pr_obj.and_then(|o| o.get("external_models_path")))),
        overrides,
        models,
        short_labels,
    };

    let bud_obj = obj
        .get("budgets")
        .or_else(|| obj.get("compaction"))
        .and_then(Value::as_object);
    let mut profiles = HashMap::new();
    if let Some(p_obj) = bud_obj
        .and_then(|o| o.get("tiers").or_else(|| o.get("profiles")))
        .and_then(Value::as_object)
    {
        for (pid, pval) in p_obj {
            let key = pid.trim();
            let Some(po) = pval.as_object() else { continue };
            if key.is_empty() {
                continue;
            }
            let lbl = val_str(po.get("label"));
            profiles.insert(
                key.to_string(),
                BudgetTierSpec {
                    profile_id: key.to_string(),
                    label: if lbl.is_empty() { key.to_string() } else { lbl },
                    min_ctx: val_u64(po.get("min_ctx"), 270_000, 1000),
                    recommend_ctx: val_u64(po.get("recommend_ctx"), 310_000, 1000),
                    urgent_ctx: val_u64(po.get("urgent_ctx"), 420_000, 1000),
                    min_turns_since_ckpt: val_u64(po.get("min_turns_since_ckpt"), 18, 1) as usize,
                    min_remaining_turns: val_u64(po.get("min_remaining_turns"), 8, 1) as usize,
                },
            );
        }
    }

    let mut profile_rules = Vec::new();
    if let Some(arr) = bud_obj
        .and_then(|o| o.get("rules").or_else(|| o.get("profile_rules")))
        .and_then(Value::as_array)
    {
        for item in arr {
            let Some(io) = item.as_object() else { continue };
            let target = val_str(io.get("tier").or_else(|| io.get("profile")));
            if target.is_empty() {
                continue;
            }
            profile_rules.push(TierMatchRule {
                profile: target,
                match_any: str_vec_lower(io.get("match_any")),
                require_all: str_vec_lower(io.get("require_all")),
                exclude_any: str_vec_lower(io.get("exclude_any")),
            });
        }
    }

    let def_prof = val_str(bud_obj.and_then(|o| o.get("default_tier").or_else(|| o.get("default_profile"))));
    let budgets = BudgetsConfig {
        default_profile: if def_prof.is_empty() { "standard".to_string() } else { def_prof },
        profiles,
        profile_rules,
    };

    let guards_obj = obj
        .get("guards")
        .or_else(|| obj.get("hooks"))
        .and_then(Value::as_object);
    let boot = guards_obj
        .and_then(|o| o.get("boot").or_else(|| o.get("session_start")))
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let exec_gate = guards_obj
        .and_then(|o| o.get("exec_gate").or_else(|| o.get("pre_run_command")))
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    Settings {
        generation,
        source_path: source_path.to_string(),
        lang,
        context,
        pricing,
        budgets,
        guards: GuardsConfig { boot, exec_gate },
    }
}

pub fn load_settings_once(explicit: Option<&str>) -> Settings {
    let Some(path) = resolve_settings_path(explicit) else {
        return Settings::default();
    };
    let Ok(raw_text) = fs::read_to_string(&path) else {
        return Settings::default();
    };
    let Ok(raw_val) = serde_json::from_str::<Value>(&raw_text) else {
        return Settings::default();
    };
    parse_settings_value(&raw_val, &path.to_string_lossy(), 1)
}

#[derive(Debug)]
pub struct SettingsWatcher {
    explicit_path: Option<String>,
    last_path: Option<PathBuf>,
    last_sig: Option<(i128, u64)>,
    generation: u64,
    cached: Arc<Settings>,
}

impl SettingsWatcher {
    pub fn new(explicit_path: Option<String>) -> Self {
        Self {
            explicit_path,
            last_path: None,
            last_sig: None,
            generation: 0,
            cached: Arc::new(Settings::default()),
        }
    }

    pub fn current(&mut self) -> Arc<Settings> {
        let resolved = resolve_settings_path(self.explicit_path.as_deref());
        let Some(path) = resolved else {
            if self.last_path.is_some() || self.last_sig.is_some() {
                self.generation += 1;
                self.last_path = None;
                self.last_sig = None;
                self.cached = Arc::new(Settings {
                    generation: self.generation,
                    ..Settings::default()
                });
            }
            return Arc::clone(&self.cached);
        };

        let sig = match fs::metadata(&path) {
            Ok(meta) => {
                let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);
                (mtime_ns, meta.size())
            }
            Err(_) => {
                if self.last_path.as_ref() != Some(&path) || self.last_sig.is_some() {
                    self.generation += 1;
                    self.last_path = Some(path);
                    self.last_sig = None;
                    self.cached = Arc::new(Settings {
                        generation: self.generation,
                        ..Settings::default()
                    });
                }
                return Arc::clone(&self.cached);
            }
        };

        if self.last_path.as_ref() == Some(&path) && self.last_sig == Some(sig) {
            return Arc::clone(&self.cached);
        }

        self.generation += 1;
        self.last_path = Some(path.clone());
        self.last_sig = Some(sig);

        let parsed = fs::read_to_string(&path)
            .ok()
            .and_then(|txt| serde_json::from_str::<Value>(&txt).ok())
            .map(|val| parse_settings_value(&val, &path.to_string_lossy(), self.generation))
            .unwrap_or_else(|| Settings {
                generation: self.generation,
                ..Settings::default()
            });
        self.cached = Arc::new(parsed);
        Arc::clone(&self.cached)
    }
}
