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
