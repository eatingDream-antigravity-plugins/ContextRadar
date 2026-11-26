//! SessionStart guard: workspace-scoped state file summarizer and extra rule injector.

use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::guard::stdin_json::{read_stdin_json_fast, truncate_utf8};
use crate::settings::{expand_tilde, load_settings_once, normalize_workspace_uri};

const DEFAULT_MAX_BYTES: usize = 800;
const DEFAULT_MAX_TOTAL_BYTES: usize = 3200;
const MAX_READ_BYTES: u64 = 262_144;

fn read_bounded_safe(path: &Path, ws_root: Option<&Path>) -> Option<String> {
    let smeta = fs::symlink_metadata(path).ok()?;
    if smeta.file_type().is_symlink() || !smeta.file_type().is_file() {
        return None;
    }
    if let Some(root) = ws_root {
        let root_real = fs::canonicalize(root).ok()?;
        let path_real = fs::canonicalize(path).ok()?;
        if path_real != root_real && !path_real.starts_with(&root_real) {
            return None;
        }
    }
    let file = File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_READ_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub fn extract_workspace_candidates(payload: &Value) -> Vec<String> {
    let Some(obj) = payload.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |raw: &str| {
        let cleaned = normalize_workspace_uri(raw);
        if !cleaned.is_empty() && seen.insert(cleaned.clone()) {
            out.push(cleaned);
        }
    };

    if let Some(cwd) = obj.get("cwd").and_then(Value::as_str) {
        push(cwd);
    }
    for key in [
        "workspacePaths",
        "workspace_paths",
        "workspaceRoots",
        "workspace_roots",
    ] {
        if let Some(arr) = obj.get(key).and_then(Value::as_array) {
            for item in arr.iter().filter_map(Value::as_str) {
                push(item);
            }
        }
    }
    out
}

pub fn extract_workspace_candidate(payload: &Value) -> String {
    extract_workspace_candidates(payload)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn normalize_path(raw: &str) -> PathBuf {
    let expanded = PathBuf::from(expand_tilde(raw));
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(expanded)
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

pub fn is_inside_workspace(candidate: &str, workspace_root: &str) -> bool {
    if workspace_root.trim().is_empty() {
        return true;
    }
    if candidate.trim().is_empty() {
        return false;
    }
    let root_norm = normalize_path(workspace_root);
    let cand_norm = normalize_path(candidate);
    if cand_norm == root_norm || cand_norm.starts_with(&root_norm) {
        return true;
    }
    if let (Ok(root_real), Ok(cand_real)) =
        (fs::canonicalize(&root_norm), fs::canonicalize(&cand_norm))
    {
        return cand_real == root_real || cand_real.starts_with(&root_real);
    }
    false
}

fn resolve_file_path(raw_path: &str, workspace_root: &str) -> Option<(PathBuf, Option<PathBuf>)> {
    let expanded = expand_tilde(raw_path);
    if expanded.is_empty() {
        return None;
    }
    let p = PathBuf::from(&expanded);
    if p.is_absolute() {
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return None;
        }
        return Some((p, None));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    if !workspace_root.trim().is_empty() {
        let base = normalize_path(workspace_root);
        let joined = base.join(p);
        return Some((joined, Some(base)));
    }
    Some((normalize_path(&expanded), None))
}

fn is_dash_divider(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'-')
}

fn summarize_markdown_state_table(markdown: &str, spec: &serde_json::Map<String, Value>) -> String {
    let section_heading = spec
        .get("section_heading")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let t_cfg = spec.get("table_columns").and_then(Value::as_object);
    let th_cfg = spec.get("theme_columns").and_then(Value::as_object);

    let name_col = t_cfg
        .and_then(|o| o.get("name_col"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let min_cols = t_cfg
        .and_then(|o| o.get("min_cols"))
        .and_then(Value::as_u64)
        .unwrap_or(3) as usize;
    let ignore_main: HashSet<String> = t_cfg
        .and_then(|o| o.get("ignore_headers"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let counters: Vec<(usize, String)> = t_cfg
        .and_then(|o| o.get("counters"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let co = c.as_object()?;
                    let col = co.get("col")?.as_u64()? as usize;
                    let label = co.get("label")?.as_str()?.to_string();
                    Some((col, label))
                })
                .collect()
        })
        .unwrap_or_default();

    let theme_name_col = th_cfg
        .and_then(|o| o.get("name_col"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let theme_min_cols = th_cfg
        .and_then(|o| o.get("min_cols"))
        .and_then(Value::as_u64)
        .unwrap_or(4) as usize;
    let ignore_themes: HashSet<String> = th_cfg
        .and_then(|o| o.get("ignore_headers"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let split_sep = th_cfg
        .and_then(|o| o.get("split_sep"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("·");
    let sub_col = th_cfg
        .and_then(|o| o.get("sub_col"))
        .and_then(Value::as_u64)
        .unwrap_or(1) as usize;
    let active_col = th_cfg
        .and_then(|o| o.get("active_col"))
        .and_then(Value::as_u64)
        .unwrap_or(2) as usize;
    let repos_col = th_cfg
        .and_then(|o| o.get("repos_col"))
        .and_then(Value::as_u64)
        .unwrap_or(3) as usize;

    let mut active_items: Vec<String> = Vec::new();
    let mut themes: Vec<String> = Vec::new();
    let mut in_secondary = false;
    let mut rows_seen = 0usize;

    for raw_line in markdown.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if !section_heading.is_empty() && line.starts_with(section_heading) {
            in_secondary = true;
            continue;
        }
        if !line.starts_with('|') {
            continue;
        }
        let inner = line
            .strip_prefix('|')
            .unwrap_or(line)
            .strip_suffix('|')
            .unwrap_or_else(|| line.strip_prefix('|').unwrap_or(line));
        let cells: Vec<&str> = inner.split('|').map(str::trim).collect();

        if !in_secondary && cells.len() >= min_cols {
            let name = cells.get(name_col).copied().unwrap_or("");
            if name.is_empty() || ignore_main.contains(name) || is_dash_divider(name) {
                continue;
            }
            rows_seen += 1;
            let mut parts: Vec<String> = Vec::new();
            let mut total: i64 = 0;
            for (col, label) in &counters {
                let val = cells
                    .get(*col)
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                if val > 0 {
                    total += val;
                    parts.push(format!("{label}={val}"));
                }
            }
            if total > 0 {
                active_items.push(format!("{name}[{}]", parts.join(",")));
            }
        } else if in_secondary && cells.len() >= theme_min_cols {
            let theme = cells.get(theme_name_col).copied().unwrap_or("");
            if theme.is_empty() || ignore_themes.contains(theme) || is_dash_divider(theme) {
                continue;
            }
            let short_name = if !split_sep.is_empty() {
                theme.split(split_sep).next().unwrap_or(theme).trim()
            } else {
                theme
            };
            let s_val = cells.get(sub_col).copied().unwrap_or("");
            let a_val = cells.get(active_col).copied().unwrap_or("");
            let r_val = cells.get(repos_col).copied().unwrap_or("");
            themes.push(format!("{short_name}({s_val}/{a_val},{r_val})"));
        }
    }

    if rows_seen == 0 && themes.is_empty() {
        return String::new();
    }
    let prefix = spec
        .get("prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("[STATE]");
    let main_part = if active_items.is_empty() {
        "none".to_string()
    } else {
        active_items.join(", ")
    };
    if !section_heading.is_empty() {
        let theme_part = if themes.is_empty() {
            "none".to_string()
        } else {
            themes.join("; ")
        };
        format!("{prefix} non-zero: {main_part} | themes: {theme_part}")
    } else {
        format!("{prefix} non-zero: {main_part}")
    }
}

fn summarize_markdown_bullets(markdown: &str, spec: &serde_json::Map<String, Value>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in markdown.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            let cleaned = line.trim_start_matches('#').trim_start();
            lines.push(cleaned.to_string());
        } else if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            lines.push(format!("• {}", rest.trim_start()));
        }
        if lines.len() >= 24 {
            break;
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let prefix = spec
        .get("prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|p| format!("{p} "))
        .unwrap_or_default();
    format!("{prefix}{}", lines.join(" | "))
}

fn summarize_plain(content: &str, spec: &serde_json::Map<String, Value>) -> String {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let prefix = spec
        .get("prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|p| format!("{p} "))
        .unwrap_or_default();
    format!("{prefix}{}", lines.join(" | "))
}

pub fn evaluate_boot_guard(payload: &Value, hook_cfg: &Value) -> Value {
    let Some(cfg) = hook_cfg.as_object() else {
        return json!({});
    };
    if cfg.get("enabled").and_then(Value::as_bool) == Some(false) {
        return json!({});
    }
    let state_files = cfg
        .get("state_files")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let extra_rules = cfg
        .get("extra_rules")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if state_files.is_empty() && extra_rules.is_empty() {
        return json!({});
    }

    let candidates = extract_workspace_candidates(payload);
    let primary_cand = candidates.first().map(String::as_str).unwrap_or("");
    let matches_any_ws = |ws_root: &str| -> Option<String> {
        if ws_root.trim().is_empty() {
            return Some(primary_cand.to_string());
        }
        for c in &candidates {
            if is_inside_workspace(c, ws_root) {
                return Some(c.clone());
            }
        }
        None
    };
    let resolve_across_candidates =
        |raw_path: &str, ws_root: &str, matched_cand: &str| -> Option<(PathBuf, Option<PathBuf>)> {
            if !ws_root.trim().is_empty() {
                return resolve_file_path(raw_path, ws_root);
            }
            for c in &candidates {
                if let Some((tp, br)) = resolve_file_path(raw_path, c) {
                    if tp.exists() {
                        return Some((tp, br));
                    }
                }
            }
            resolve_file_path(raw_path, matched_cand)
        };

    let mut messages: Vec<String> = Vec::new();

    for spec_val in state_files {
        let Some(spec) = spec_val.as_object() else {
            continue;
        };
        let Some(raw_path) = spec.get("path").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if raw_path.is_empty() {
            continue;
        }
        let ws_root = spec
            .get("workspace_root")
            .and_then(Value::as_str)
            .unwrap_or("");
        let Some(matched_cand) = matches_any_ws(ws_root) else {
            continue;
        };
        let Some((target_path, bound_root)) =
            resolve_across_candidates(raw_path, ws_root, &matched_cand)
        else {
            continue;
        };
        let Some(raw) = read_bounded_safe(&target_path, bound_root.as_deref()) else {
            continue;
        };
        let fmt = spec
            .get("format")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("plain");
        let summary = match fmt {
            "markdown_state_table" => summarize_markdown_state_table(&raw, spec),
            "markdown_bullets" => summarize_markdown_bullets(&raw, spec),
            _ => summarize_plain(&raw, spec),
        };
        if summary.is_empty() {
            continue;
        }
        let max_bytes = spec
            .get("max_bytes")
            .and_then(Value::as_u64)
            .filter(|&n| n >= 64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_BYTES);
        messages.push(truncate_utf8(&summary, max_bytes));
    }

    for rule_val in extra_rules {
        let Some(rule) = rule_val.as_object() else {
            continue;
        };
        let ws_root = rule
            .get("workspace_root")
            .and_then(Value::as_str)
            .unwrap_or("");
        let Some(matched_cand) = matches_any_ws(ws_root) else {
            continue;
        };
        let body = if let Some(f) = rule
            .get("file")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let Some((fpath, bound_root)) = resolve_across_candidates(f, ws_root, &matched_cand)
            else {
                continue;
            };
            match read_bounded_safe(&fpath, bound_root.as_deref()) {
                Some(txt) => txt.trim().to_string(),
                None => continue,
            }
        } else if let Some(lines) = rule.get("lines").and_then(Value::as_array) {
            lines
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else if let Some(txt) = rule.get("text").and_then(Value::as_str) {
            txt.trim().to_string()
        } else {
            String::new()
        };
        if body.is_empty() {
            continue;
        }
        let tag = rule
            .get("tag")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|t| format!("[{t}] "))
            .unwrap_or_default();
        let max_bytes = rule
            .get("max_bytes")
            .and_then(Value::as_u64)
            .filter(|&n| n >= 64)
            .map(|n| n as usize)
            .unwrap_or(1200);
        messages.push(truncate_utf8(&format!("{tag}{body}"), max_bytes));
    }

    if messages.is_empty() {
        return json!({});
    }
    let max_total = cfg
        .get("max_total_bytes")
        .and_then(Value::as_u64)
        .filter(|&n| n >= 128)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_TOTAL_BYTES);
    let combined = truncate_utf8(&messages.join("\n"), max_total);
    if combined.is_empty() {
        return json!({});
    }
    json!({
        "injectSteps": [{ "systemMessage": { "systemMessage": combined } }]
    })
}

pub fn run_boot_guard_cli() {
    let Some(payload) = read_stdin_json_fast(350) else {
        println!("{{}}");
        return;
    };
    let settings = load_settings_once(None);
    let out = evaluate_boot_guard(&payload, &settings.guards.boot);
    println!(
        "{}",
        serde_json::to_string(&out).unwrap_or_else(|_| "{}".into())
    );
}
