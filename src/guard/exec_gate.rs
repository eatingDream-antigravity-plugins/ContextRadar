//! PreToolUse(run_command) guard: raises low sync WaitMsBeforeAsync while exempting daemons & background commands.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::guard::stdin_json::read_stdin_json_fast;
use crate::settings::load_settings_once;

const DEFAULT_MIN_SYNC_WAIT_MS: u64 = 5000;
const DEFAULT_TARGET_SYNC_WAIT_MS: u64 = 10000;
const DEFAULT_ASYNC_BUILD_MAX_WAIT_MS: u64 = 550;

const DEFAULT_EXEMPT_PATTERNS: &[&str] = &[
    r"(?:^|[;&|]\s*)nohup\b",
    r"(?:^|\s)docker\s+compose\s+up\s+-d\b",
    r"(?:^|[;&|]\s*)sleep\s+(?:[1-9]\d{1,})\b",
    r"(?:^|[;&|]\s*)tail\s+-[a-zA-Z]*f\b",
    r"(?:^|[;&|]\s*)watch\b",
    r"\b(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?(?:dev|start|watch)\b",
    r"\b(?:vite|next\s+dev|cargo\s+watch|uvicorn|gunicorn|flask\s+run)\b",
    r"(?:^|[;&|]\s*)ssh\s+.*-(?:[a-zA-Z]*[fN])\b",
    r"[^>&]\s*&\s*$",
];

/// Finite heavy build/test/install commands that Antigravity agents intentionally background
/// using `WaitMsBeforeAsync: 500` (`IsDaemon: false`).
const DEFAULT_FINITE_BG_BUILD_PATTERNS: &[&str] = &[
    r"\bcargo\s+(?:build|test|check|clippy|bench|install|doc)\b",
    r"\b(?:pytest|py\.test|tox|nox)\b",
    r"\bpython3?\s+-m\s+(?:pytest|unittest)\b",
    r"\b(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?(?:build|test|ci|install|pack|lint|typecheck)\b",
    r"\bgo\s+(?:build|test|install)\b",
    r"\b(?:gradle|./gradlew|mvn|./mvnw|cmake|ninja|bazel|xcodebuild)\b",
    r"(?:^|[;&|]\s*)make(?:\s+|$)",
    r"\bswift\s+(?:build|test)\b",
    r"\bdocker\s+(?:build|buildx\s+build|pull|compose\s+build)\b",
    r"\b(?:pip|pip3|conda|mamba|uv)\s+(?:install|sync|build)\b",
    r"\b(?:tsc|webpack|rollup|esbuild|vite\s+build|next\s+build)\b",
];

fn default_exempt_regexes() -> &'static Vec<Regex> {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        DEFAULT_EXEMPT_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("valid default exempt regex"))
            .collect()
    })
}

fn finite_bg_build_regexes() -> &'static Vec<Regex> {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        DEFAULT_FINITE_BG_BUILD_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("valid finite bg build regex"))
            .collect()
    })
}

pub fn is_deliberate_async_build(
    wait_ms: u64,
    command_line: &str,
    tool_summary: &str,
    hook_cfg: Option<&Value>,
) -> bool {
    let cfg = hook_cfg.and_then(Value::as_object);
    if cfg
        .and_then(|o| o.get("respect_async_builds"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return false;
    }
    let max_async_wait = cfg
        .and_then(|o| o.get("async_build_max_wait_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_ASYNC_BUILD_MAX_WAIT_MS);
    if wait_ms > max_async_wait {
        return false;
    }
    let sum_low = tool_summary.trim().to_ascii_lowercase();
    if sum_low.contains("background") || sum_low.contains("async") {
        return true;
    }
    let trimmed = command_line.trim();
    if trimmed.is_empty() {
        return false;
    }
    finite_bg_build_regexes()
        .iter()
        .any(|re| re.is_match(trimmed))
}

pub fn is_explicit_background_command(command_line: &str, hook_cfg: Option<&Value>) -> bool {
    let trimmed = command_line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let cfg_obj = hook_cfg.and_then(Value::as_object);
    let replace_defaults = cfg_obj
        .and_then(|o| o.get("replace_default_exempt_patterns"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let user_patterns = cfg_obj
        .and_then(|o| o.get("exempt_patterns"))
        .and_then(Value::as_array);

    if !replace_defaults {
        for re in default_exempt_regexes() {
            if re.is_match(trimmed) {
                return true;
            }
        }
    }

    if let Some(arr) = user_patterns {
        for pat_val in arr {
            let Some(pat) = pat_val.as_str().map(str::trim) else {
                continue;
            };
            if pat.is_empty() {
                continue;
            }
            if let Ok(re) = Regex::new(pat) {
                if re.is_match(trimmed) {
                    return true;
                }
            }
        }
    }

    false
}

fn parse_wait_ms(val: Option<&Value>) -> Option<u64> {
    let v = val?;
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    if let Some(n) = v.as_i64() {
        return if n >= 0 { Some(n as u64) } else { None };
    }
    if let Some(f) = v.as_f64() {
        return if f.is_finite() && f >= 0.0 {
            Some(f as u64)
        } else {
            None
        };
    }
    if let Some(s) = v.as_str() {
        let t = s.trim();
        if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
            return t.parse::<u64>().ok();
        }
    }
    None
}

pub fn evaluate_exec_gate(payload: &Value, hook_cfg: Option<&Value>) -> Value {
    let pass = || json!({ "decision": "ask" });

    let Some(call) = payload.get("toolCall").and_then(Value::as_object) else {
        return pass();
    };
    if call.get("name").and_then(Value::as_str) != Some("run_command") {
        return pass();
    }
    let Some(args) = call.get("args").and_then(Value::as_object) else {
        return pass();
    };

    let cfg = hook_cfg.and_then(Value::as_object);
    if cfg.and_then(|o| o.get("enabled")).and_then(Value::as_bool) == Some(false) {
        return pass();
    }

    let respect_is_daemon = cfg
        .and_then(|o| o.get("respect_is_daemon"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if respect_is_daemon {
        let is_daemon = match args.get("IsDaemon") {
            Some(Value::Bool(true)) => true,
            Some(Value::String(s)) if s == "true" => true,
            _ => false,
        };
        if is_daemon {
            return pass();
        }
    }

    let min_wait = cfg
        .and_then(|o| o.get("min_sync_wait_ms"))
        .and_then(Value::as_u64)
        .map(|v| v.min(10_000))
        .unwrap_or(DEFAULT_MIN_SYNC_WAIT_MS);
    let target_wait = cfg
        .and_then(|o| o.get("target_sync_wait_ms"))
        .and_then(Value::as_u64)
        .filter(|&v| v >= 500)
        .map(|v| v.min(10_000))
        .unwrap_or(DEFAULT_TARGET_SYNC_WAIT_MS);

    let Some(wait_ms) = parse_wait_ms(args.get("WaitMsBeforeAsync")) else {
        return pass();
    };
    if wait_ms >= min_wait {
        return pass();
    }

    let cmd = args
        .get("CommandLine")
        .and_then(Value::as_str)
        .unwrap_or("");
    let summary = args
        .get("toolSummary")
        .and_then(Value::as_str)
        .unwrap_or("");
    if is_explicit_background_command(cmd, hook_cfg)
        || is_deliberate_async_build(wait_ms, cmd, summary, hook_cfg)
    {
        return pass();
    }

    json!({
        "decision": "ask",
        "overwrite": {
            "WaitMsBeforeAsync": target_wait
        }
    })
}

pub fn run_exec_gate_cli() {
    let Some(payload) = read_stdin_json_fast(350) else {
        println!("{{\"decision\":\"ask\"}}");
        return;
    };
    let settings = load_settings_once(None);
    let out = evaluate_exec_gate(&payload, Some(&settings.guards.exec_gate));
    println!(
        "{}",
        serde_json::to_string(&out).unwrap_or_else(|_| "{\"decision\":\"ask\"}".into())
    );
}
