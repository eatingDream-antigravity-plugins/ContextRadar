//! Prefix-preserving context bloat hotspot locator and out-of-band transcript slice distiller.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::Serialize;

use crate::ledger::journal_tail::JournalTailCache;
use crate::ledger::{is_safe_id, TurnSample};

#[derive(Debug, Clone, Serialize)]
pub struct BloatHotspot {
    pub turn: usize,
    pub step_idx: i64,
    pub delta_tokens: u64,
    pub context_after: u64,
    pub cause: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrunerReport {
    pub found: bool,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_step_range: Option<[i64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_distill_range: Option<[i64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotspots: Option<Vec<BloatHotspot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slice_range: Option<[i64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_raw_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_note_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

pub fn analyze_hotspots(
    data_root: &Path,
    cid: &str,
    turns: &[TurnSample],
    top_k: usize,
    journal_cache: &mut JournalTailCache,
) -> PrunerReport {
    if !is_safe_id(cid) {
        return PrunerReport {
            found: false,
            mode: "hotspots".into(),
            error: Some("无效的会话 ID".into()),
            conversation_id: cid.to_string(),
            active_step_range: None,
            recommended_distill_range: None,
            hotspots: None,
            slice_range: None,
            step_count: None,
            estimated_raw_tokens: None,
            digest_chars: None,
            saved_note_path: None,
            handoff_hint: None,
            digest: None,
        };
    }

    let mut start_pos = 0usize;
    for (idx, t) in turns.iter().enumerate().rev() {
        if t.is_checkpoint {
            start_pos = idx + 1;
            break;
        }
    }
    let active_turns = &turns[start_pos..];
    let window_entries = journal_cache.get_active_window(data_root, cid);

    let mut step_meta: HashMap<i64, String> = HashMap::new();
    for e in &window_entries {
        if !e.tool_names.is_empty() {
            let joined = e
                .tool_names
                .iter()
                .filter(|n| !n.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            step_meta.insert(e.step_index, format!("工具: {}", truncate_chars(&joined, 80)));
        } else if e.step_type == "USER_INPUT" && !e.snippet_short.is_empty() {
            step_meta.insert(e.step_index, format!("用户: {}", e.snippet_short));
        }
    }

    let mut spikes: Vec<BloatHotspot> = Vec::new();
    let mut prev_ctx = active_turns.first().map(|t| t.context_tokens()).unwrap_or(0);
    for (pos, t) in active_turns.iter().enumerate() {
        let cur_ctx = t.context_tokens();
        let delta = if pos > 0 {
            cur_ctx.saturating_sub(prev_ctx)
        } else {
            cur_ctx
        };
        prev_ctx = cur_ctx;
        if pos == 0 || delta <= 800 {
            continue;
        }
        let cause = step_meta
            .get(&t.step_idx)
            .or_else(|| step_meta.get(&(t.step_idx - 1)))
            .or_else(|| step_meta.get(&(t.step_idx - 2)))
            .cloned()
            .unwrap_or_else(|| "上下文累积".into());
        spikes.push(BloatHotspot {
            turn: t.turn_index,
            step_idx: t.step_idx,
            delta_tokens: delta,
            context_after: cur_ctx,
            cause,
        });
    }

    spikes.sort_by(|a, b| b.delta_tokens.cmp(&a.delta_tokens));
    let k = top_k.clamp(1, 15);
    spikes.truncate(k);

    let min_step = window_entries.first().map(|e| e.step_index).unwrap_or(0);
    let max_step = window_entries.last().map(|e| e.step_index).unwrap_or(0);
    let mid_step = if max_step > min_step {
        min_step + ((max_step - min_step) / 2).max(1)
    } else {
        max_step
    };

    PrunerReport {
        found: true,
        mode: "hotspots".into(),
        error: None,
        conversation_id: cid.to_string(),
        active_step_range: Some([min_step, max_step]),
        recommended_distill_range: Some([min_step, mid_step]),
        hotspots: Some(spikes),
        slice_range: None,
        step_count: None,
        estimated_raw_tokens: None,
        digest_chars: None,
        saved_note_path: None,
        handoff_hint: None,
        digest: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn distill_slice(
    data_root: &Path,
    cid: &str,
    turns: &[TurnSample],
    mode: &str,
    start_step: Option<i64>,
    end_step: Option<i64>,
    save_note: bool,
    top_k: usize,
    journal_cache: &mut JournalTailCache,
) -> PrunerReport {
    if !is_safe_id(cid) {
        return PrunerReport {
            found: false,
            mode: mode.to_string(),
            error: Some("无效的会话 ID".into()),
            conversation_id: cid.to_string(),
            active_step_range: None,
            recommended_distill_range: None,
            hotspots: None,
            slice_range: None,
            step_count: None,
            estimated_raw_tokens: None,
            digest_chars: None,
            saved_note_path: None,
            handoff_hint: None,
            digest: None,
        };
    }

    if mode == "hotspots" && start_step.is_none() && end_step.is_none() {
        return analyze_hotspots(data_root, cid, turns, top_k, journal_cache);
    }

    let entries = journal_cache.get_active_window(data_root, cid);
    if entries.is_empty() {
        return PrunerReport {
            found: false,
            mode: "distill".into(),
            error: Some("未找到可提炼的会话轨迹文件".into()),
            conversation_id: cid.to_string(),
            active_step_range: None,
            recommended_distill_range: None,
            hotspots: None,
            slice_range: None,
            step_count: None,
            estimated_raw_tokens: None,
            digest_chars: None,
            saved_note_path: None,
            handoff_hint: None,
            digest: None,
        };
    }

    let first_idx = entries[0].step_index;
    let last_idx = entries[entries.len() - 1].step_index;
    let mut s_idx = start_step.unwrap_or(first_idx);
    let mut e_idx = end_step.unwrap_or_else(|| first_idx + ((last_idx - first_idx) / 2).max(1));
    if s_idx > e_idx {
        std::mem::swap(&mut s_idx, &mut e_idx);
    }

    let mut step_count = 0usize;
    let mut user_reqs = Vec::new();
    let mut touched_files = Vec::new();
    let mut seen_files = HashSet::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut conclusions = Vec::new();
    let mut errors = Vec::new();
    let mut raw_chars = 0usize;

    for e in entries.iter().filter(|e| e.step_index >= s_idx && e.step_index <= e_idx) {
        step_count += 1;
        raw_chars += e.raw_chars;
        if e.step_type == "USER_INPUT" && !e.snippet_long.is_empty() {
            user_reqs.push(format!("[#{}] {}", e.step_index, e.snippet_long));
        } else if e.step_type == "PLANNER_RESPONSE" && !e.snippet_long.is_empty() {
            conclusions.push(format!("[#{}] {}", e.step_index, e.snippet_long));
        }
        if e.status == "ERROR" && !e.snippet_long.is_empty() {
            errors.push(format!("[#{}] {}", e.step_index, e.snippet_long));
        }
        for tname in &e.tool_names {
            *tool_counts.entry(tname.clone()).or_insert(0) += 1;
        }
        for fpath in &e.touched_files {
            if seen_files.insert(fpath.clone()) {
                touched_files.push(fpath.clone());
            }
        }
    }

    let mut sorted_tools: Vec<(String, usize)> = tool_counts.into_iter().collect();
    sorted_tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted_tools.truncate(6);
    let top_tools = sorted_tools
        .into_iter()
        .map(|(k, v)| format!("{k}×{v}"))
        .collect::<Vec<_>>()
        .join(", ");

    let est_raw_tokens = (raw_chars / 4).max(1);
    let mut lines = vec![format!(
        "### 局部切片提炼摘要（步 #{s_idx}–#{e_idx}，共 {step_count} 步，原文约 {}k tok → <0.5k tok，主干前缀零改动）",
        est_raw_tokens / 1000
    )];
    if !user_reqs.is_empty() {
        let tail = &user_reqs[user_reqs.len().saturating_sub(4)..];
        lines.push(format!("- 用户指令：{}", tail.join("；")));
    }
    if !top_tools.is_empty() {
        lines.push(format!("- 调用统计：{top_tools}"));
    }
    if !touched_files.is_empty() {
        let short_files: Vec<String> = touched_files
            .iter()
            .take(10)
            .map(|p| {
                let trimmed = p.trim_end_matches('/');
                Path::new(trimmed)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(p)
                    .to_string()
            })
            .collect();
        lines.push(format!("- 涉及文件：{}", short_files.join(", ")));
    }
    if !errors.is_empty() {
        let tail = &errors[errors.len().saturating_sub(3)..];
        lines.push(format!("- 异常记录：{}", tail.join("；")));
    }
    if !conclusions.is_empty() {
        let tail = &conclusions[conclusions.len().saturating_sub(4)..];
        lines.push(format!("- 阶段结论：{}", tail.join(" | ")));
    }

    let mut digest = lines.join("\n");
    if digest.chars().count() > 2300 {
        digest = format!("{}…", truncate_chars(&digest, 2295));
    }

    let mut saved_path = String::new();
    if save_note {
        let brain_dir = data_root.join("brain");
        let conv_brain = brain_dir.join(cid);
        let scratch_dir = conv_brain.join("scratch");
        let is_symlink = |p: &Path| {
            fs::symlink_metadata(p)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        };
        if !is_symlink(&brain_dir) && !is_symlink(&conv_brain) && !is_symlink(&scratch_dir) {
            if fs::create_dir_all(&scratch_dir).is_ok() {
                let note_file = scratch_dir.join(format!("slice_{s_idx}_{e_idx}.md"));
                if !is_symlink(&note_file) {
                    let note_body = format!(
                        "<!-- Source conversation: {cid} | steps {s_idx}-{e_idx} | prefix-cache-safe slice -->\n{digest}\n"
                    );
                    let write_ok = OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&note_file)
                        .and_then(|mut f| f.write_all(note_body.as_bytes()));
                    if write_ok.is_ok() {
                        saved_path = note_file.to_string_lossy().into_owned();
                    }
                }
            }
        }
    }

    let handoff_hint = if !saved_path.is_empty() {
        format!("可将 {saved_path} 作为 scratch_note_path 传给 context_radar_commit_handoff 或供分叉子代理精读。")
    } else {
        "可加 save_note=true 保存到 scratch/，供分叉子代理或接力新会话按需精读。".into()
    };

    PrunerReport {
        found: true,
        mode: "distill".into(),
        error: None,
        conversation_id: cid.to_string(),
        active_step_range: None,
        recommended_distill_range: None,
        hotspots: None,
        slice_range: Some([s_idx, e_idx]),
        step_count: Some(step_count),
        estimated_raw_tokens: Some(est_raw_tokens),
        digest_chars: Some(digest.chars().count()),
        saved_note_path: Some(saved_path),
        handoff_hint: Some(handoff_hint),
        digest: Some(digest),
    }
}
