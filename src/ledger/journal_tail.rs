//! Incremental inode/offset-tracked reader for transcript.jsonl (+ targeted transcript_full.jsonl hydration) with 256 KiB reverse CHECKPOINT seek.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::ledger::is_safe_id;

const BACKWARD_CHUNK: usize = 256 * 1024;
const MAX_CACHED_JOURNALS: usize = 32;
const MAX_CARRY_BYTES: usize = 4 * 1024 * 1024;

fn ansi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("valid ansi regex"))
}

pub fn clean_snippet(raw: &str, max_chars: usize) -> String {
    if raw.is_empty() || max_chars == 0 {
        return String::new();
    }
    let stripped = ansi_regex().replace_all(raw, "");
    let cut = match stripped.find("<ADDITIONAL_METADATA>") {
        Some(idx) => &stripped[..idx],
        None => stripped.as_ref(),
    };
    let no_tags = cut
        .replace("<USER_REQUEST>", "")
        .replace("</USER_REQUEST>", "");

    let mut collapsed = String::with_capacity(no_tags.len().min(max_chars * 4));
    let mut prev_ws = true;
    for ch in no_tags.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                collapsed.push(' ');
                prev_ws = true;
            }
        } else {
            collapsed.push(ch);
            prev_ws = false;
        }
    }
    if collapsed.ends_with(' ') {
        collapsed.pop();
    }

    let char_count = collapsed.chars().count();
    if char_count <= max_chars {
        return collapsed;
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let mut out: String = collapsed.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub step_index: i64,
    pub step_type: String,
    pub status: String,
    pub raw_chars: usize,
    pub snippet_short: String,
    pub snippet_long: String,
    pub tool_names: Vec<String>,
    pub tool_summaries: Vec<String>,
    pub touched_files: Vec<String>,
    pub has_truncated_fields: bool,
}

#[derive(Debug, Clone)]
struct WindowState {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    byte_offset: u64,
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone)]
struct StepTypesState {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    byte_offset: u64,
    step_types: HashMap<i64, String>,
}

pub fn transcript_path(data_root: &Path, cid: &str) -> PathBuf {
    data_root
        .join("brain")
        .join(cid)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl")
}

pub fn transcript_full_path(data_root: &Path, cid: &str) -> PathBuf {
    data_root
        .join("brain")
        .join(cid)
        .join(".system_generated")
        .join("logs")
        .join("transcript_full.jsonl")
}

fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .rposition(|w| w == needle)
}

fn find_byte(haystack: &[u8], byte: u8, start: usize) -> Option<usize> {
    haystack[start..]
        .iter()
        .position(|&b| b == byte)
        .map(|off| start + off)
}

fn rfind_byte(haystack: &[u8], byte: u8) -> Option<usize> {
    haystack.iter().rposition(|&b| b == byte)
}

/// Scan backwards in 256 KiB chunks to locate the byte offset right after the last `CHECKPOINT` line.
pub fn find_after_last_checkpoint_offset(file: &mut File, file_size: u64) -> u64 {
    let mut pos = file_size;
    let mut carry: Vec<u8> = Vec::new();
    let mut block = vec![0u8; BACKWARD_CHUNK];

    while pos > 0 {
        let read_size = (BACKWARD_CHUNK as u64).min(pos) as usize;
        pos -= read_size as u64;
        if file.seek(SeekFrom::Start(pos)).is_err() {
            return 0;
        }
        if file.read_exact(&mut block[..read_size]).is_err() {
            return 0;
        }

        let mut buf = Vec::with_capacity(read_size + carry.len());
        buf.extend_from_slice(&block[..read_size]);
        buf.extend_from_slice(&carry);

        let mut search_end = buf.len();
        while let Some(hit) = rfind_bytes(&buf[..search_end], b"CHECKPOINT") {
            let line_start_opt = rfind_byte(&buf[..hit], b'\n');
            if line_start_opt.is_none() && pos > 0 {
                // Split across chunk boundary; inspect on previous chunk iteration.
                break;
            }
            let ls = line_start_opt.map(|i| i + 1).unwrap_or(0);
            let (line_end, next_offset) = match find_byte(&buf, b'\n', hit) {
                Some(nl) => (nl, pos + (nl as u64) + 1),
                None => (buf.len(), pos + (buf.len() as u64)),
            };
            let raw_line = trim_ascii(&buf[ls..line_end]);
            if !raw_line.is_empty() {
                if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(raw_line) {
                    if map.get("type").and_then(Value::as_str) == Some("CHECKPOINT") {
                        return next_offset;
                    }
                }
            }
            search_end = ls;
        }

        if let Some(first_nl) = find_byte(&buf, b'\n', 0) {
            carry = buf[..=first_nl].to_vec();
        } else if buf.len() < MAX_CARRY_BYTES {
            carry = buf;
        } else {
            carry.clear();
        }
    }
    0
}

#[inline]
fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let Some(&b) = s.first() {
        if b.is_ascii_whitespace() {
            s = &s[1..];
        } else {
            break;
        }
    }
    while let Some(&b) = s.last() {
        if b.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

fn short_basename(raw_path: &str) -> String {
    let trimmed = raw_path.trim().trim_end_matches('/');
    Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

fn strip_cd_prefix(cmd: &str) -> &str {
    let trimmed = cmd.trim();
    if let Some(rest) = trimmed.strip_prefix("cd ") {
        if let Some(idx) = rest.find("&&") {
            let after = rest[idx + 2..].trim();
            if !after.is_empty() {
                return after;
            }
        }
    }
    trimmed
}

fn summarize_tool_call(
    name: &str,
    args: Option<&serde_json::Map<String, Value>>,
    touched_files: &mut Vec<String>,
    seen_files: &mut HashSet<String>,
) -> String {
    let Some(args) = args else {
        return name.to_string();
    };

    let mut path_labels: Vec<String> = Vec::new();
    for key in ["Path", "path", "ScriptPath", "TargetFile", "file"] {
        if let Some(val) = args.get(key).and_then(Value::as_str).map(str::trim) {
            if !val.is_empty() {
                if seen_files.insert(val.to_string()) {
                    touched_files.push(val.to_string());
                }
                path_labels.push(short_basename(val));
            }
        }
    }
    for key in ["paths", "Paths", "files"] {
        if let Some(arr) = args.get(key).and_then(Value::as_array) {
            for item in arr.iter().filter_map(Value::as_str).map(str::trim) {
                if !item.is_empty() {
                    if seen_files.insert(item.to_string()) {
                        touched_files.push(item.to_string());
                    }
                    path_labels.push(short_basename(item));
                }
            }
        }
    }
    if !path_labels.is_empty() {
        let head: Vec<String> = path_labels.iter().take(2).cloned().collect();
        let extra = path_labels.len().saturating_sub(2);
        let joined = if extra > 0 {
            format!("{}+{}", head.join(","), extra)
        } else {
            head.join(",")
        };
        return format!("{name}({})", clean_snippet(&joined, 36));
    }

    for key in ["CommandLine", "command", "cmd"] {
        if let Some(val) = args.get(key).and_then(Value::as_str) {
            let cleaned = clean_snippet(strip_cd_prefix(val), 36);
            if !cleaned.is_empty() {
                return format!("{name}({cleaned})");
            }
        }
    }

    if let Some(arr) = args.get("queries").and_then(Value::as_array) {
        let qs: Vec<String> = arr
            .iter()
            .filter_map(Value::as_str)
            .map(|s| clean_snippet(s, 18))
            .filter(|s| !s.is_empty())
            .take(2)
            .collect();
        if !qs.is_empty() {
            return format!("{name}({})", qs.join(","));
        }
    }
    for key in ["query", "Query", "pattern", "Pattern"] {
        if let Some(val) = args.get(key).and_then(Value::as_str) {
            let cleaned = clean_snippet(val, 28);
            if !cleaned.is_empty() {
                return format!("{name}({cleaned})");
            }
        }
    }

    for key in ["url", "Url", "Uri", "uri"] {
        if let Some(val) = args.get(key).and_then(Value::as_str) {
            let cleaned = clean_snippet(val, 32);
            if !cleaned.is_empty() {
                return format!("{name}({cleaned})");
            }
        }
    }

    for key in ["Recipient", "recipient", "SubagentName", "AgentName"] {
        if let Some(val) = args.get(key).and_then(Value::as_str) {
            let cleaned = clean_snippet(val, 24);
            if !cleaned.is_empty() {
                return format!("{name}({cleaned})");
            }
        }
    }

    if let Some(act) = args.get("action").and_then(Value::as_str).map(str::trim) {
        if !act.is_empty() {
            if let Some(nm) = args.get("name").and_then(Value::as_str).map(str::trim) {
                if !nm.is_empty() {
                    return format!("{name}({act}:{nm})");
                }
            }
            return format!("{name}({act})");
        }
    }

    name.to_string()
}

fn project_entry(obj: &serde_json::Map<String, Value>) -> Option<JournalEntry> {
    let step_index = obj.get("step_index")?.as_i64()?;
    let step_type = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let status = obj
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let content = obj.get("content").and_then(Value::as_str).unwrap_or("");
    let thinking = obj.get("thinking").and_then(Value::as_str).unwrap_or("");
    let raw_chars = content.chars().count().saturating_add(thinking.chars().count() / 4);
    let has_truncated_fields = obj
        .get("truncated_fields")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    let mut snippet_short = String::new();
    let mut snippet_long = String::new();
    if step_type == "USER_INPUT" {
        snippet_short = clean_snippet(content, 70);
        snippet_long = clean_snippet(content, 140);
    } else if step_type == "PLANNER_RESPONSE" && !content.trim().is_empty() {
        snippet_long = clean_snippet(content, 200);
    } else if status == "ERROR" && !content.trim().is_empty() {
        snippet_long = clean_snippet(content, 140);
    }

    let mut tool_names = Vec::new();
    let mut tool_summaries = Vec::new();
    let mut touched_files = Vec::new();
    let mut seen_files = HashSet::new();
    if let Some(tcs) = obj.get("tool_calls").and_then(Value::as_array) {
        for tc in tcs {
            let Some(tco) = tc.as_object() else { continue };
            let name = tco
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown");
            tool_names.push(name.to_string());
            let args_obj = tco
                .get("args")
                .or_else(|| tco.get("arguments"))
                .and_then(Value::as_object);
            let summary = summarize_tool_call(name, args_obj, &mut touched_files, &mut seen_files);
            tool_summaries.push(summary);
        }
    }

    Some(JournalEntry {
        step_index,
        step_type,
        status,
        raw_chars,
        snippet_short,
        snippet_long,
        tool_names,
        tool_summaries,
        touched_files,
        has_truncated_fields,
    })
}

/// Fast byte-slice scan on the first <=512 bytes of a JSONL line to extract `step_index` and `type`.
fn fast_extract_step_type(head: &[u8]) -> Option<(i64, String)> {
    let idx = extract_json_int_field(head, b"\"step_index\"")?;
    let stype = extract_json_upper_str_field(head, b"\"type\"")?;
    Some((idx, stype))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn extract_json_int_field(buf: &[u8], key: &[u8]) -> Option<i64> {
    let kpos = find_subslice(buf, key)?;
    let mut i = kpos + key.len();
    while i < buf.len() && buf[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= buf.len() || buf[i] != b':' {
        return None;
    }
    i += 1;
    while i < buf.len() && buf[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < buf.len() && buf[i] == b'-' {
        i += 1;
    }
    while i < buf.len() && buf[i].is_ascii_digit() {
        i += 1;
    }
    if i == start || (i == start + 1 && buf[start] == b'-') {
        return None;
    }
    std::str::from_utf8(&buf[start..i]).ok()?.parse::<i64>().ok()
}

fn extract_json_upper_str_field(buf: &[u8], key: &[u8]) -> Option<String> {
    let kpos = find_subslice(buf, key)?;
    let mut i = kpos + key.len();
    while i < buf.len() && buf[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= buf.len() || buf[i] != b':' {
        return None;
    }
    i += 1;
    while i < buf.len() && buf[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= buf.len() || buf[i] != b'"' {
        return None;
    }
    i += 1;
    let start = i;
    while i < buf.len() {
        let b = buf[i];
        if b == b'"' {
            if i == start {
                return None;
            }
            return std::str::from_utf8(&buf[start..i]).ok().map(str::to_string);
        }
        if !(b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
            return None;
        }
        i += 1;
    }
    None
}

#[derive(Debug)]
pub struct JournalTailCache {
    max_entries: usize,
    windows: HashMap<PathBuf, WindowState>,
    windows_order: VecDeque<PathBuf>,
    step_types: HashMap<PathBuf, StepTypesState>,
    step_types_order: VecDeque<PathBuf>,
}

impl Default for JournalTailCache {
    fn default() -> Self {
        Self::new(MAX_CACHED_JOURNALS)
    }
}

impl JournalTailCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(4),
            windows: HashMap::new(),
            windows_order: VecDeque::new(),
            step_types: HashMap::new(),
            step_types_order: VecDeque::new(),
        }
    }

    fn touch_order(order: &mut VecDeque<PathBuf>, key: &PathBuf) {
        if let Some(pos) = order.iter().position(|k| k == key) {
            order.remove(pos);
        }
        order.push_back(key.clone());
    }

    fn evict_windows(&mut self) {
        while self.windows.len() > self.max_entries {
            if let Some(oldest) = self.windows_order.pop_front() {
                self.windows.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn evict_step_types(&mut self) {
        while self.step_types.len() > self.max_entries {
            if let Some(oldest) = self.step_types_order.pop_front() {
                self.step_types.remove(&oldest);
            } else {
                break;
            }
        }
    }

    /// Return compact entries strictly after the latest `CHECKPOINT` using incremental tail reads.
    pub fn get_active_window(&mut self, data_root: &Path, cid: &str) -> Vec<JournalEntry> {
        if !is_safe_id(cid) {
            return Vec::new();
        }
        let tpath = transcript_path(data_root, cid);
        let Ok(meta) = fs::symlink_metadata(&tpath) else {
            self.windows.remove(&tpath);
            return Vec::new();
        };
        if !meta.file_type().is_file() {
            self.windows.remove(&tpath);
            return Vec::new();
        }
        let dev = meta.dev();
        let ino = meta.ino();
        let size = meta.size();
        let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);

        if let Some(cached) = self.windows.get(&tpath) {
            if cached.dev == dev
                && cached.ino == ino
                && cached.size == size
                && cached.mtime_ns == mtime_ns
            {
                return cached.entries.clone();
            }
        }

        let Ok(mut file) = File::open(&tpath) else {
            return Vec::new();
        };

        let (mut entries, start_offset) = match self.windows.remove(&tpath) {
            Some(prev) if prev.dev == dev && prev.ino == ino && size >= prev.byte_offset => {
                (prev.entries, prev.byte_offset)
            }
            _ => {
                let off = find_after_last_checkpoint_offset(&mut file, size);
                (Vec::new(), off)
            }
        };

        if file.seek(SeekFrom::Start(start_offset)).is_err() {
            return Vec::new();
        }
        let mut reader = BufReader::new(file);
        let mut pos = start_offset;
        let mut raw_line = Vec::with_capacity(4096);

        loop {
            raw_line.clear();
            let n = match reader.read_until(b'\n', &mut raw_line) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let is_complete = raw_line.ends_with(b"\n");
            let stripped = trim_ascii(&raw_line);
            if stripped.is_empty() {
                pos += n as u64;
                continue;
            }
            let parsed = match serde_json::from_slice::<Value>(stripped) {
                Ok(v) => v,
                Err(_) => {
                    if !is_complete {
                        // Incomplete mid-write trailing line; defer to next poll.
                        break;
                    }
                    pos += n as u64;
                    continue;
                }
            };
            pos += n as u64;
            if let Value::Object(map) = parsed {
                if map.get("type").and_then(Value::as_str) == Some("CHECKPOINT") {
                    entries.clear();
                    continue;
                }
                if let Some(proj) = project_entry(&map) {
                    entries.push(proj);
                }
            }
        }

        let result = entries.clone();
        self.windows.insert(
            tpath.clone(),
            WindowState {
                dev,
                ino,
                size,
                mtime_ns,
                byte_offset: pos,
                entries,
            },
        );
        Self::touch_order(&mut self.windows_order, &tpath);
        self.evict_windows();
        result
    }

    /// Hydrate ONLY entries within `s_idx..=e_idx` that set `truncated_fields` in `transcript.jsonl`
    /// by reading the corresponding full lines from `transcript_full.jsonl`.
    pub fn hydrate_truncated_entries(
        &self,
        data_root: &Path,
        cid: &str,
        entries: &mut [JournalEntry],
        s_idx: i64,
        e_idx: i64,
    ) {
        if !is_safe_id(cid) {
            return;
        }
        let mut needed: HashSet<i64> = entries
            .iter()
            .filter(|e| e.step_index >= s_idx && e.step_index <= e_idx && e.has_truncated_fields)
            .map(|e| e.step_index)
            .collect();
        if needed.is_empty() {
            return;
        }

        let fpath = transcript_full_path(data_root, cid);
        let Ok(meta) = fs::symlink_metadata(&fpath) else {
            return;
        };
        if !meta.file_type().is_file() {
            return;
        }
        let Ok(mut file) = File::open(&fpath) else {
            return;
        };
        let start_offset = find_after_last_checkpoint_offset(&mut file, meta.size());
        if file.seek(SeekFrom::Start(start_offset)).is_err() {
            return;
        }

        let max_needed = needed.iter().copied().max().unwrap_or(e_idx);
        let mut hydrated: HashMap<i64, JournalEntry> = HashMap::with_capacity(needed.len());
        let mut reader = BufReader::new(file);
        let mut raw_line = Vec::with_capacity(8192);

        while !needed.is_empty() {
            raw_line.clear();
            let n = match reader.read_until(b'\n', &mut raw_line) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let stripped = trim_ascii(&raw_line);
            if stripped.is_empty() {
                continue;
            }
            let head_len = stripped.len().min(512);
            if let Some(sidx) = extract_json_int_field(&stripped[..head_len], b"\"step_index\"") {
                if !needed.contains(&sidx) {
                    if sidx > max_needed {
                        break;
                    }
                    continue;
                }
            }
            if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(stripped) {
                if let Some(sidx) = map.get("step_index").and_then(Value::as_i64) {
                    if needed.remove(&sidx) {
                        if let Some(full_entry) = project_entry(&map) {
                            hydrated.insert(sidx, full_entry);
                        }
                    } else if sidx > max_needed {
                        break;
                    }
                }
            }
        }

        if hydrated.is_empty() {
            return;
        }
        for entry in entries.iter_mut() {
            if let Some(full) = hydrated.remove(&entry.step_index) {
                *entry = full;
            }
        }
    }

    /// Return `(dev, ino, mtime_ns, size)` for a conversation's `transcript.jsonl`, or `None` if absent/non-file.
    pub fn stat_transcript_stamp(data_root: &Path, cid: &str) -> Option<(u64, u64, i128, u64)> {
        if !is_safe_id(cid) {
            return None;
        }
        let tpath = transcript_path(data_root, cid);
        let meta = fs::symlink_metadata(&tpath).ok()?;
        if !meta.file_type().is_file() {
            return None;
        }
        let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);
        Some((meta.dev(), meta.ino(), mtime_ns, meta.size()))
    }

    /// Return `step_index -> step_type` map using fast header extraction and incremental tailing.
    pub fn get_step_types(&mut self, data_root: &Path, cid: &str) -> HashMap<i64, String> {
        if !is_safe_id(cid) {
            return HashMap::new();
        }
        let tpath = transcript_path(data_root, cid);
        let Ok(meta) = fs::symlink_metadata(&tpath) else {
            self.step_types.remove(&tpath);
            return HashMap::new();
        };
        if !meta.file_type().is_file() {
            self.step_types.remove(&tpath);
            return HashMap::new();
        }
        let dev = meta.dev();
        let ino = meta.ino();
        let size = meta.size();
        let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);

        if let Some(cached) = self.step_types.get(&tpath) {
            if cached.dev == dev
                && cached.ino == ino
                && cached.size == size
                && cached.mtime_ns == mtime_ns
            {
                return cached.step_types.clone();
            }
        }

        let Ok(mut file) = File::open(&tpath) else {
            return HashMap::new();
        };

        let (mut map, start_offset) = match self.step_types.remove(&tpath) {
            Some(prev) if prev.dev == dev && prev.ino == ino && size >= prev.byte_offset => {
                (prev.step_types, prev.byte_offset)
            }
            _ => (HashMap::new(), 0),
        };

        if file.seek(SeekFrom::Start(start_offset)).is_err() {
            return HashMap::new();
        }
        let mut reader = BufReader::new(file);
        let mut pos = start_offset;
        let mut raw_line = Vec::with_capacity(4096);

        loop {
            raw_line.clear();
            let n = match reader.read_until(b'\n', &mut raw_line) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let is_complete = raw_line.ends_with(b"\n");
            let stripped = trim_ascii(&raw_line);
            if stripped.is_empty() {
                pos += n as u64;
                continue;
            }
            let head_len = stripped.len().min(512);
            if let Some((sidx, stype)) = fast_extract_step_type(&stripped[..head_len]) {
                map.insert(sidx, stype);
                pos += n as u64;
                continue;
            }
            match serde_json::from_slice::<Value>(stripped) {
                Ok(Value::Object(obj)) => {
                    pos += n as u64;
                    if let (Some(sidx), Some(stype)) = (
                        obj.get("step_index").and_then(Value::as_i64),
                        obj.get("type").and_then(Value::as_str),
                    ) {
                        if !stype.is_empty() {
                            map.insert(sidx, stype.to_string());
                        }
                    }
                }
                Ok(_) => {
                    pos += n as u64;
                }
                Err(_) => {
                    if !is_complete {
                        break;
                    }
                    pos += n as u64;
                }
            }
        }

        let result = map.clone();
        self.step_types.insert(
            tpath.clone(),
            StepTypesState {
                dev,
                ino,
                size,
                mtime_ns,
                byte_offset: pos,
                step_types: map,
            },
        );
        Self::touch_order(&mut self.step_types_order, &tpath);
        self.evict_step_types();
        result
    }
}
