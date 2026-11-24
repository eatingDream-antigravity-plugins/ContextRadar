pub mod index_db;
pub mod journal_tail;
pub mod sqlite_ro;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnSample {
    pub turn_index: usize,
    pub step_idx: i64,
    pub timestamp: f64,
    pub model: String,
    pub model_display: String,
    pub input_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output_tokens: u64,
    pub ttft_ms: u64,
    pub total_ms: u64,
    pub response_id: String,
    pub step_type: i32,
    pub is_checkpoint: bool,
    pub after_turn_start: Option<bool>,
    pub after_checkpoint: bool,
}

impl TurnSample {
    #[inline]
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    #[inline]
    pub fn cache_hit_rate(&self) -> f64 {
        let ctx = self.context_tokens();
        if ctx > 0 {
            (self.cache_read as f64) / (ctx as f64) * 100.0
        } else {
            0.0
        }
    }

    #[inline]
    pub fn is_cold_cache(&self) -> bool {
        !self.is_checkpoint && self.context_tokens() > 0 && self.cache_read == 0
    }

    /// Prefill start timestamp (`began`) in epoch seconds.
    #[inline]
    pub fn began_ts(&self) -> f64 {
        self.timestamp
    }

    /// Generation end timestamp (`ended`) in epoch seconds (`began + total_ms / 1000.0`).
    #[inline]
    pub fn ended_ts(&self) -> f64 {
        if self.timestamp > 0.0 && self.total_ms > 0 {
            self.timestamp + (self.total_ms as f64) / 1000.0
        } else {
            self.timestamp
        }
    }

    /// Prefill-to-prefill elapsed time (`curr.began - prev.began`) in seconds.
    #[inline]
    pub fn start_delta_sec(&self, prev: &TurnSample) -> Option<f64> {
        if self.timestamp > 0.0 && prev.timestamp > 0.0 {
            Some((self.timestamp - prev.timestamp).max(0.0))
        } else {
            None
        }
    }

    /// Idle gap between previous generation end and current prefill start (`curr.began - prev.ended`).
    #[inline]
    pub fn idle_gap_sec(&self, prev: &TurnSample) -> Option<f64> {
        if self.timestamp > 0.0 && prev.timestamp > 0.0 {
            Some((self.timestamp - prev.ended_ts()).max(0.0))
        } else {
            None
        }
    }

    /// Returns true if model ID or reasoning-effort display label changed between consecutive steps.
    pub fn is_model_or_effort_switch(&self, prev: &TurnSample) -> bool {
        let pm = prev.model.trim().to_ascii_lowercase();
        let cm = self.model.trim().to_ascii_lowercase();
        if !pm.is_empty() && !cm.is_empty() && pm != "unknown" && cm != "unknown" && pm != cm {
            return true;
        }
        let pd = prev.model_display.trim().to_ascii_lowercase();
        let cd = self.model_display.trim().to_ascii_lowercase();
        !pd.is_empty()
            && !cd.is_empty()
            && pd != "未知模型"
            && cd != "未知模型"
            && pd != cd
    }

    #[inline]
    pub fn stream_tps(&self) -> f64 {
        if self.output_tokens == 0 || self.total_ms == 0 {
            return 0.0;
        }
        let gen_ms = if self.total_ms > self.ttft_ms {
            self.total_ms - self.ttft_ms
        } else {
            self.total_ms
        };
        if gen_ms == 0 {
            return 0.0;
        }
        (self.output_tokens as f64) / ((gen_ms as f64) / 1000.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRecord {
    pub conversation_id: String,
    pub title: String,
    pub preview: String,
    pub workspace: String,
    pub workspaces: Vec<String>,
    pub status: String,
    pub agent_name: String,
    pub parent_conversation_id: String,
    pub nesting_depth: i64,
    pub not_fully_idle: bool,
    pub killed: bool,
    pub last_modified_time: String,
    pub last_user_input_step_index: i64,
    pub archived: bool,
    pub pinned: bool,
}

impl SummaryRecord {
    #[inline]
    pub fn is_subagent(&self) -> bool {
        self.nesting_depth > 0
            || !self.parent_conversation_id.is_empty()
            || (!self.agent_name.is_empty() && self.last_user_input_step_index < 0)
    }

    pub fn matches_workspace_filter(&self, needle_lower: &str) -> bool {
        if needle_lower.is_empty() {
            return true;
        }
        if self.workspace.to_ascii_lowercase().contains(needle_lower) {
            return true;
        }
        self.workspaces
            .iter()
            .any(|w| w.to_ascii_lowercase().contains(needle_lower))
    }
}

pub fn is_safe_id(cid: &str) -> bool {
    let b = cid.as_bytes();
    if b.is_empty() || b.len() > 128 {
        return false;
    }
    if b[0] == b'.' || cid.contains("..") {
        return false;
    }
    b.iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-')
}
