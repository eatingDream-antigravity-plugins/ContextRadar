//! Prefix-preserving context bloat hotspot locator and out-of-band transcript slice distiller.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use crate::i18n::{resolve_active_lang, Lang};
use crate::ledger::journal_tail::{JournalEntry, JournalTailCache};
use crate::ledger::{is_safe_id, TurnSample};

mod note_file;

#[derive(Debug, Clone, Serialize)]
pub struct BloatHotspot {
    pub turn: usize,
    pub step_idx: i64,
    pub cause_step_range: [i64; 2],
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

fn attribute_spike_cause(
    window_entries: &[JournalEntry],
    prev_step: i64,
    cur_step: i64,
    lang: Lang,
) -> String {
    let zh = lang.is_zh();
    let mut tools: Vec<String> = Vec::new();
    let mut seen_tools: HashSet<String> = HashSet::new();
    let mut user_inputs: Vec<String> = Vec::new();

    // Attribute ONLY to steps executed BEFORE `cur_step`:
    // - `prev_step`'s outgoing `tool_calls` (which executed right after `prev_step` and produced outputs before `cur_step`),
    // - any intermediate tool/user steps in `(prev_step + 1) .. cur_step`.
    // Never attribute to `cur_step`'s own unexecuted outgoing `tool_calls`!
    for e in window_entries {
        if e.step_index < prev_step || e.step_index >= cur_step {
            continue;
        }
        let summaries = if !e.tool_summaries.is_empty() {
            &e.tool_summaries
        } else {
            &e.tool_names
        };
        for ts in summaries {
            if !ts.is_empty() && seen_tools.insert(ts.clone()) {
                tools.push(ts.clone());
            }
        }
        if e.step_index > prev_step && e.step_type == "USER_INPUT" && !e.snippet_short.is_empty() {
            user_inputs.push(e.snippet_short.clone());
        }
    }

    let mut parts: Vec<String> = Vec::new();
    if !tools.is_empty() {
        let joined = tools.join(", ");
        let prefix = if zh { "工具" } else { "Tools" };
        parts.push(format!("{prefix}: {}", truncate_chars(&joined, 84)));
    }
    if !user_inputs.is_empty() {
        let joined = user_inputs.join("; ");
        let prefix = if zh { "用户" } else { "User" };
        parts.push(format!("{prefix}: {}", truncate_chars(&joined, 64)));
    }

    if parts.is_empty() {
        if zh {
            "逐步累积".to_string()
        } else {
            "Gradual buildup".to_string()
        }
    } else {
        truncate_chars(&parts.join(" · "), 110)
    }
}

pub fn analyze_hotspots(
    data_root: &Path,
    cid: &str,
    turns: &[TurnSample],
    top_k: usize,
    journal_cache: &mut JournalTailCache,
) -> PrunerReport {
    analyze_hotspots_with_lang(
        data_root,
        cid,
        turns,
        top_k,
        journal_cache,
        resolve_active_lang(""),
    )
}

pub fn analyze_hotspots_with_lang(
    data_root: &Path,
    cid: &str,
    turns: &[TurnSample],
    top_k: usize,
    journal_cache: &mut JournalTailCache,
    lang: Lang,
) -> PrunerReport {
    let zh = lang.is_zh();
    if !is_safe_id(cid) {
        return PrunerReport {
            found: false,
            mode: "hotspots".into(),
            error: Some(if zh {
                "无效的会话 ID".into()
            } else {
                "Invalid conversation ID".into()
            }),
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

    let mut spikes: Vec<BloatHotspot> = Vec::new();
    let mut prev_ctx = active_turns
        .first()
        .map(|t| t.context_tokens())
        .unwrap_or(0);
    let mut prev_step = active_turns.first().map(|t| t.step_idx).unwrap_or(0);
    for (pos, t) in active_turns.iter().enumerate() {
        let cur_ctx = t.context_tokens();
        let cur_step = t.step_idx;
        let delta = if pos > 0 {
            cur_ctx.saturating_sub(prev_ctx)
        } else {
            cur_ctx
        };
        let p_step = prev_step;
        prev_ctx = cur_ctx;
        prev_step = cur_step;
        if pos == 0 || delta <= 800 {
            continue;
        }
        let cause = attribute_spike_cause(&window_entries, p_step, cur_step, lang);
        spikes.push(BloatHotspot {
            turn: t.turn_index,
            step_idx: cur_step,
            cause_step_range: [p_step, (cur_step - 1).max(p_step)],
            delta_tokens: delta,
            context_after: cur_ctx,
            cause,
        });
    }

    spikes.sort_by_key(|spike| std::cmp::Reverse(spike.delta_tokens));
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
    distill_slice_with_lang(
        data_root,
        cid,
        turns,
        mode,
        start_step,
        end_step,
        save_note,
        top_k,
        journal_cache,
        resolve_active_lang(""),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn distill_slice_with_lang(
    data_root: &Path,
    cid: &str,
    turns: &[TurnSample],
    mode: &str,
    start_step: Option<i64>,
    end_step: Option<i64>,
    save_note: bool,
    top_k: usize,
    journal_cache: &mut JournalTailCache,
    lang: Lang,
) -> PrunerReport {
    let zh = lang.is_zh();
    if !is_safe_id(cid) {
        return PrunerReport {
            found: false,
            mode: mode.to_string(),
            error: Some(if zh {
                "无效的会话 ID".into()
            } else {
                "Invalid conversation ID".into()
            }),
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
        return analyze_hotspots_with_lang(data_root, cid, turns, top_k, journal_cache, lang);
    }

    let mut entries = journal_cache.get_active_window(data_root, cid);
    if entries.is_empty() {
        return PrunerReport {
            found: false,
            mode: "distill".into(),
            error: Some(if zh {
                "找不到这个会话的记录文件。".into()
            } else {
                "Couldn't find this chat's transcript.".into()
            }),
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

    // Hydrate truncated compact lines from transcript_full.jsonl on demand
    journal_cache.hydrate_truncated_entries(data_root, cid, &mut entries, s_idx, e_idx);

    let mut step_count = 0usize;
    let mut user_reqs = Vec::new();
    let mut touched_files = Vec::new();
    let mut seen_files = HashSet::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut notable_targets: Vec<String> = Vec::new();
    let mut seen_targets: HashSet<String> = HashSet::new();
    let mut conclusions = Vec::new();
    let mut errors = Vec::new();
    let mut raw_chars = 0usize;

    for e in entries
        .iter()
        .filter(|e| e.step_index >= s_idx && e.step_index <= e_idx)
    {
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
        for ts in &e.tool_summaries {
            if ts.contains('(') && notable_targets.len() < 8 && seen_targets.insert(ts.clone()) {
                notable_targets.push(ts.clone());
            }
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
    let mut lines = vec![if zh {
        format!(
            "### 要点摘要（第 {s_idx}–{e_idx} 步，共 {step_count} 步，约 {}k → <0.5k，未改动当前缓存）",
            est_raw_tokens / 1000
        )
    } else {
        format!(
            "### Key points (steps #{s_idx}–#{e_idx}, {step_count} steps, ~{}k → <0.5k, live cache untouched)",
            est_raw_tokens / 1000
        )
    }];

    if !user_reqs.is_empty() {
        let tail = &user_reqs[user_reqs.len().saturating_sub(4)..];
        lines.push(if zh {
            format!("- 用户指令：{}", tail.join("；"))
        } else {
            format!("- User requests: {}", tail.join("; "))
        });
    }
    if !top_tools.is_empty() {
        lines.push(if zh {
            format!("- 调用统计：{top_tools}")
        } else {
            format!("- Tool calls: {top_tools}")
        });
    }
    if !notable_targets.is_empty() {
        lines.push(if zh {
            format!("- 关键操作：{}", notable_targets.join(", "))
        } else {
            format!("- Key actions: {}", notable_targets.join(", "))
        });
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
        lines.push(if zh {
            format!("- 涉及文件：{}", short_files.join(", "))
        } else {
            format!("- Files touched: {}", short_files.join(", "))
        });
    }
    if !errors.is_empty() {
        let tail = &errors[errors.len().saturating_sub(3)..];
        lines.push(if zh {
            format!("- 异常记录：{}", tail.join("；"))
        } else {
            format!("- Errors: {}", tail.join("; "))
        });
    }
    if !conclusions.is_empty() {
        let tail = &conclusions[conclusions.len().saturating_sub(4)..];
        lines.push(if zh {
            format!("- 阶段结论：{}", tail.join(" | "))
        } else {
            format!("- Conclusions: {}", tail.join(" | "))
        });
    }

    let mut digest = lines.join("\n");
    if digest.chars().count() > 2300 {
        digest = format!("{}…", truncate_chars(&digest, 2295));
    }

    let mut saved_path = String::new();
    let mut save_error: Option<String> = None;
    if save_note {
        let name = format!("slice_{s_idx}_{e_idx}.md");
        let body = format!(
            "<!-- Source conversation: {cid} | steps {s_idx}-{e_idx} | out-of-band scratch slice -->\n{digest}\n"
        );
        match note_file::save(data_root, cid, &name, &body) {
            Ok(path) => {
                saved_path = path.to_string_lossy().into_owned();
            }
            Err(err) => {
                save_error = Some(if zh {
                    format!("要点文件保存失败：{err}")
                } else {
                    format!("Couldn't save notes file: {err}")
                });
            }
        }
    }

    let handoff_hint = if !saved_path.is_empty() {
        Some(if zh {
            format!(
                "可将 {saved_path} 传给 radar_commit_handoff，或在换新会话 / 新子代理时按需读取。"
            )
        } else {
            format!(
                "Pass {saved_path} as scratch_note_path to radar_commit_handoff, or read it from a fresh subagent or new chat."
            )
        })
    } else if save_error.is_some() {
        None
    } else {
        Some(if zh {
            "可加 save_note=true 把要点存进笔记文件，供新子代理或新会话按需读取。".into()
        } else {
            "Pass save_note=true to save these notes for fresh subagents or a new chat.".into()
        })
    };

    PrunerReport {
        found: true,
        mode: "distill".into(),
        error: save_error,
        conversation_id: cid.to_string(),
        active_step_range: None,
        recommended_distill_range: None,
        hotspots: None,
        slice_range: Some([s_idx, e_idx]),
        step_count: Some(step_count),
        estimated_raw_tokens: Some(est_raw_tokens),
        digest_chars: Some(digest.chars().count()),
        saved_note_path: Some(saved_path),
        handoff_hint,
        digest: Some(digest),
    }
}
