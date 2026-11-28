//! Incremental inode/offset-tracked reader for transcript.jsonl (+ targeted transcript_full.jsonl hydration) with 256 KiB reverse CHECKPOINT seek.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::ledger::is_safe_id;
use crate::settings::{read_line_bounded_with_term, BoundedLine, MAX_LINE_BYTES};
#[cfg(unix)]
use crate::settings::{UNIX_O_NOFOLLOW, UNIX_O_NONBLOCK};

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
enum UncommittedWindowTail {
    None,
    Entry(JournalEntry),
    Checkpoint,
}

#[derive(Debug, Clone)]
struct WindowState {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    committed_offset: u64,
    committed_entries: Vec<JournalEntry>,
    uncommitted_tail: UncommittedWindowTail,
}

impl WindowState {
    fn snapshot(&self) -> Vec<JournalEntry> {
        match &self.uncommitted_tail {
            UncommittedWindowTail::None => self.committed_entries.clone(),
            UncommittedWindowTail::Entry(entry) => {
                let mut out = Vec::with_capacity(self.committed_entries.len() + 1);
                out.extend_from_slice(&self.committed_entries);
                out.push(entry.clone());
                out
            }
            UncommittedWindowTail::Checkpoint => Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct StepTypesState {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    committed_offset: u64,
    committed_step_types: HashMap<i64, String>,
    uncommitted_step: Option<(i64, String)>,
}

impl StepTypesState {
    fn snapshot(&self) -> HashMap<i64, String> {
        let mut out = self.committed_step_types.clone();
        if let Some((idx, ref stype)) = self.uncommitted_step {
            out.insert(idx, stype.clone());
        }
        out
    }
}

fn open_regular_file_ro(path: &Path) -> io::Result<(File, fs::Metadata)> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    opts.custom_flags(UNIX_O_NOFOLLOW | UNIX_O_NONBLOCK);
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transcript path is not a regular file",
        ));
    }
    Ok((file, meta))
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
    haystack.windows(needle.len()).rposition(|w| w == needle)
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
    let mut carry_is_eof = true;
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
                None if carry_is_eof => {
                    let eof_off = pos + (buf.len() as u64);
                    debug_assert_eq!(eof_off, file_size);
                    (buf.len(), eof_off)
                }
                None => {
                    // Oversized (> MAX_CARRY_BYTES) suffix was dropped; buf[ls..] is mid-line.
                    search_end = ls;
                    continue;
                }
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
            if first_nl < MAX_CARRY_BYTES {
                carry = buf[..=first_nl].to_vec();
            } else {
                carry.clear();
            }
            carry_is_eof = false;
        } else if carry_is_eof && buf.len() <= MAX_CARRY_BYTES {
            carry = buf;
        } else {
            carry.clear();
            carry_is_eof = false;
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
    let raw_chars = content
        .chars()
        .count()
        .saturating_add(thinking.chars().count() / 4);
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

/// Fast top-level JSON header scan on the first `<=512` bytes of a JSONL line to extract
/// `step_index` and/or `type` without matching escaped strings or nested objects.
fn fast_scan_top_header(head: &[u8], want_type: bool) -> (Option<i64>, Option<String>) {
    let mut i = 0usize;
    while i < head.len() && head[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= head.len() || head[i] != b'{' {
        return (None, None);
    }
    i += 1;

    let mut step_idx: Option<i64> = None;
    let mut step_type: Option<String> = None;

    while i < head.len() {
        while i < head.len() && (head[i].is_ascii_whitespace() || head[i] == b',') {
            i += 1;
        }
        if i >= head.len() || head[i] == b'}' {
            break;
        }
        if head[i] != b'"' {
            break;
        }
        let Some((key, next_i)) = scan_json_raw_str(head, i) else {
            break;
        };
        i = next_i;
        while i < head.len() && head[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= head.len() || head[i] != b':' {
            break;
        }
        i += 1;
        while i < head.len() && head[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= head.len() {
            break;
        }

        if key == b"step_index" && step_idx.is_none() {
            let start = i;
            if head[i] == b'-' {
                i += 1;
            }
            while i < head.len() && head[i].is_ascii_digit() {
                i += 1;
            }
            if i == start || (i == start + 1 && head[start] == b'-') {
                break;
            }
            // Ensure integer token was not truncated right at the 512-byte slice boundary.
            if i >= head.len() || matches!(head[i], b'.' | b'e' | b'E') {
                break;
            }
            step_idx = std::str::from_utf8(&head[start..i])
                .ok()
                .and_then(|s| s.parse::<i64>().ok());
        } else if want_type && key == b"type" && step_type.is_none() {
            if head[i] != b'"' {
                break;
            }
            let Some((raw_val, after_val)) = scan_json_raw_str(head, i) else {
                break;
            };
            i = after_val;
            if !raw_val.is_empty()
                && raw_val
                    .iter()
                    .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                step_type = std::str::from_utf8(raw_val).ok().map(str::to_string);
            }
        } else {
            let Some(after_val) = skip_json_value_in_slice(head, i) else {
                break;
            };
            i = after_val;
        }

        if step_idx.is_some() && (!want_type || step_type.is_some()) {
            break;
        }
    }

    (step_idx, step_type)
}

fn scan_json_raw_str(buf: &[u8], quote_pos: usize) -> Option<(&[u8], usize)> {
    if quote_pos >= buf.len() || buf[quote_pos] != b'"' {
        return None;
    }
    let start = quote_pos + 1;
    let mut i = start;
    let mut has_escape = false;
    while i < buf.len() {
        match buf[i] {
            b'\\' => {
                has_escape = true;
                i += 2;
            }
            b'"' => {
                if has_escape {
                    return Some((&[], i + 1));
                }
                return Some((&buf[start..i], i + 1));
            }
            _ => i += 1,
        }
    }
    None
}

fn skip_json_value_in_slice(buf: &[u8], start: usize) -> Option<usize> {
    if start >= buf.len() {
        return None;
    }
    match buf[start] {
        b'"' => scan_json_raw_str(buf, start).map(|(_, next)| next),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = start;
            while i < buf.len() {
                match buf[i] {
                    b'"' => {
                        let (_, next) = scan_json_raw_str(buf, i)?;
                        i = next;
                    }
                    b'{' | b'[' => {
                        depth += 1;
                        i += 1;
                    }
                    b'}' | b']' => {
                        depth = depth.saturating_sub(1);
                        i += 1;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    _ => i += 1,
                }
            }
            None
        }
        _ => {
            let mut i = start;
            while i < buf.len() {
                if matches!(buf[i], b',' | b'}' | b']') || buf[i].is_ascii_whitespace() {
                    return Some(i);
                }
                i += 1;
            }
            None
        }
    }
}

fn fast_extract_step_type(head: &[u8]) -> Option<(i64, String)> {
    let (idx, stype) = fast_scan_top_header(head, true);
    Some((idx?, stype?))
}

fn fast_extract_top_step_index(head: &[u8]) -> Option<i64> {
    fast_scan_top_header(head, false).0
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
        let Ok(lmeta) = fs::symlink_metadata(&tpath) else {
            self.windows.remove(&tpath);
            return Vec::new();
        };
        if !lmeta.file_type().is_file() {
            self.windows.remove(&tpath);
            return Vec::new();
        }
        let l_mtime_ns = (lmeta.mtime() as i128) * 1_000_000_000 + (lmeta.mtime_nsec() as i128);

        if let Some(cached) = self.windows.get(&tpath) {
            if cached.dev == lmeta.dev()
                && cached.ino == lmeta.ino()
                && cached.size == lmeta.size()
                && cached.mtime_ns == l_mtime_ns
            {
                return cached.snapshot();
            }
        }

        let Ok((mut file, meta)) = open_regular_file_ro(&tpath) else {
            self.windows.remove(&tpath);
            return Vec::new();
        };
        let dev = meta.dev();
        let ino = meta.ino();
        let size = meta.size();
        let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);

        let (mut committed_entries, mut committed_offset) = match self.windows.remove(&tpath) {
            Some(prev) if prev.dev == dev && prev.ino == ino && size >= prev.committed_offset => {
                (prev.committed_entries, prev.committed_offset)
            }
            _ => {
                let off = find_after_last_checkpoint_offset(&mut file, size);
                (Vec::new(), off)
            }
        };

        if file.seek(SeekFrom::Start(committed_offset)).is_err() {
            return Vec::new();
        }
        let mut reader = BufReader::new(file);
        let mut raw_line = Vec::with_capacity(4096);
        let mut uncommitted_tail = UncommittedWindowTail::None;

        loop {
            let (line_kind, is_complete) =
                match read_line_bounded_with_term(&mut reader, &mut raw_line, MAX_LINE_BYTES) {
                    Ok((BoundedLine::Eof, _)) => break,
                    Ok((kind, term)) => (kind, term),
                    Err(_) => break,
                };
            let n = match line_kind {
                BoundedLine::Eof => break,
                BoundedLine::Oversized(total) => {
                    raw_line.clear();
                    raw_line.shrink_to(4096);
                    if !is_complete {
                        break;
                    }
                    committed_offset += total as u64;
                    continue;
                }
                BoundedLine::Line(n) => n,
            };
            let stripped = trim_ascii(&raw_line);
            if stripped.is_empty() {
                if is_complete {
                    committed_offset += n as u64;
                    continue;
                }
                break;
            }
            let Ok(parsed) = serde_json::from_slice::<Value>(stripped) else {
                if !is_complete {
                    // Incomplete mid-write trailing line; defer to next poll.
                    break;
                }
                committed_offset += n as u64;
                continue;
            };
            if is_complete {
                committed_offset += n as u64;
                if let Value::Object(map) = parsed {
                    if map.get("type").and_then(Value::as_str) == Some("CHECKPOINT") {
                        committed_entries.clear();
                        continue;
                    }
                    if let Some(proj) = project_entry(&map) {
                        committed_entries.push(proj);
                    }
                }
            } else {
                if let Value::Object(map) = parsed {
                    if map.get("type").and_then(Value::as_str) == Some("CHECKPOINT") {
                        uncommitted_tail = UncommittedWindowTail::Checkpoint;
                    } else if let Some(proj) = project_entry(&map) {
                        uncommitted_tail = UncommittedWindowTail::Entry(proj);
                    }
                }
                break;
            }
        }

        let state = WindowState {
            dev,
            ino,
            size,
            mtime_ns,
            committed_offset,
            committed_entries,
            uncommitted_tail,
        };
        let result = state.snapshot();
        self.windows.insert(tpath.clone(), state);
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
        let Ok((mut file, meta)) = open_regular_file_ro(&fpath) else {
            return;
        };
        let start_offset = find_after_last_checkpoint_offset(&mut file, meta.size());
        if file.seek(SeekFrom::Start(start_offset)).is_err() {
            return;
        }

        let mut hydrated: HashMap<i64, JournalEntry> = HashMap::with_capacity(needed.len());
        let mut raw_line = Vec::with_capacity(8192);
        {
            let mut reader = BufReader::new(&mut file);
            Self::scan_hydration_stream(&mut reader, &mut raw_line, &mut needed, &mut hydrated);
        }

        // If transcript_full.jsonl wrote a newer CHECKPOINT ahead of transcript.jsonl, fall back
        // to scanning [0..start_offset] so pre-checkpoint entries still hydrate cleanly.
        if !needed.is_empty() && start_offset > 0 && file.seek(SeekFrom::Start(0)).is_ok() {
            let mut prefix_reader = BufReader::new((&mut file).take(start_offset));
            Self::scan_hydration_stream(
                &mut prefix_reader,
                &mut raw_line,
                &mut needed,
                &mut hydrated,
            );
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

    fn scan_hydration_stream<R: BufRead>(
        reader: &mut R,
        raw_line: &mut Vec<u8>,
        needed: &mut HashSet<i64>,
        hydrated: &mut HashMap<i64, JournalEntry>,
    ) {
        while !needed.is_empty() {
            let (line_kind, is_complete) =
                match read_line_bounded_with_term(reader, raw_line, MAX_LINE_BYTES) {
                    Ok((BoundedLine::Eof, _)) => break,
                    Ok((kind, term)) => (kind, term),
                    Err(_) => break,
                };
            let n = match line_kind {
                BoundedLine::Eof => break,
                BoundedLine::Oversized(_) => {
                    raw_line.clear();
                    raw_line.shrink_to(8192);
                    if !is_complete {
                        break;
                    }
                    continue;
                }
                BoundedLine::Line(n) => n,
            };
            if n == 0 {
                break;
            }
            let stripped = trim_ascii(raw_line);
            if stripped.is_empty() {
                if !is_complete {
                    break;
                }
                continue;
            }
            if is_complete {
                let head_len = stripped.len().min(512);
                if let Some(sidx) = fast_extract_top_step_index(&stripped[..head_len]) {
                    if !needed.contains(&sidx) {
                        continue;
                    }
                }
            }
            if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(stripped) {
                if let Some(sidx) = map.get("step_index").and_then(Value::as_i64) {
                    if needed.remove(&sidx) {
                        if let Some(full_entry) = project_entry(&map) {
                            hydrated.insert(sidx, full_entry);
                        }
                    }
                }
            }
            if !is_complete {
                break;
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
        let Ok(lmeta) = fs::symlink_metadata(&tpath) else {
            self.step_types.remove(&tpath);
            return HashMap::new();
        };
        if !lmeta.file_type().is_file() {
            self.step_types.remove(&tpath);
            return HashMap::new();
        }
        let l_mtime_ns = (lmeta.mtime() as i128) * 1_000_000_000 + (lmeta.mtime_nsec() as i128);

        if let Some(cached) = self.step_types.get(&tpath) {
            if cached.dev == lmeta.dev()
                && cached.ino == lmeta.ino()
                && cached.size == lmeta.size()
                && cached.mtime_ns == l_mtime_ns
            {
                return cached.snapshot();
            }
        }

        let Ok((mut file, meta)) = open_regular_file_ro(&tpath) else {
            self.step_types.remove(&tpath);
            return HashMap::new();
        };
        let dev = meta.dev();
        let ino = meta.ino();
        let size = meta.size();
        let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);

        let (mut committed_step_types, mut committed_offset) = match self.step_types.remove(&tpath)
        {
            Some(prev) if prev.dev == dev && prev.ino == ino && size >= prev.committed_offset => {
                (prev.committed_step_types, prev.committed_offset)
            }
            _ => (HashMap::new(), 0),
        };

        if file.seek(SeekFrom::Start(committed_offset)).is_err() {
            return HashMap::new();
        }
        let mut reader = BufReader::new(file);
        let mut raw_line = Vec::with_capacity(4096);
        let mut uncommitted_step: Option<(i64, String)> = None;

        loop {
            let (line_kind, is_complete) =
                match read_line_bounded_with_term(&mut reader, &mut raw_line, MAX_LINE_BYTES) {
                    Ok((BoundedLine::Eof, _)) => break,
                    Ok((kind, term)) => (kind, term),
                    Err(_) => break,
                };
            let n = match line_kind {
                BoundedLine::Eof => break,
                BoundedLine::Oversized(total) => {
                    if !is_complete {
                        raw_line.clear();
                        raw_line.shrink_to(4096);
                        break;
                    }
                    let stripped = trim_ascii(&raw_line);
                    let head_len = stripped.len().min(512);
                    if let Some((sidx, stype)) = fast_extract_step_type(&stripped[..head_len]) {
                        committed_step_types.insert(sidx, stype);
                    }
                    committed_offset += total as u64;
                    raw_line.clear();
                    raw_line.shrink_to(4096);
                    continue;
                }
                BoundedLine::Line(n) => n,
            };
            let stripped = trim_ascii(&raw_line);
            if stripped.is_empty() {
                if is_complete {
                    committed_offset += n as u64;
                    continue;
                }
                break;
            }
            if !is_complete {
                // Un-terminated EOF line: never advance committed_offset or trust fast header scan
                // before verifying full JSON validity at EOF.
                if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(stripped) {
                    if let (Some(sidx), Some(stype)) = (
                        obj.get("step_index").and_then(Value::as_i64),
                        obj.get("type").and_then(Value::as_str),
                    ) {
                        if !stype.is_empty() {
                            uncommitted_step = Some((sidx, stype.to_string()));
                        }
                    }
                }
                break;
            }
            let head_len = stripped.len().min(512);
            if let Some((sidx, stype)) = fast_extract_step_type(&stripped[..head_len]) {
                committed_step_types.insert(sidx, stype);
                committed_offset += n as u64;
                continue;
            }
            match serde_json::from_slice::<Value>(stripped) {
                Ok(Value::Object(obj)) => {
                    committed_offset += n as u64;
                    if let (Some(sidx), Some(stype)) = (
                        obj.get("step_index").and_then(Value::as_i64),
                        obj.get("type").and_then(Value::as_str),
                    ) {
                        if !stype.is_empty() {
                            committed_step_types.insert(sidx, stype.to_string());
                        }
                    }
                }
                Ok(_) | Err(_) => {
                    committed_offset += n as u64;
                }
            }
        }

        let state = StepTypesState {
            dev,
            ino,
            size,
            mtime_ns,
            committed_offset,
            committed_step_types,
            uncommitted_step,
        };
        let result = state.snapshot();
        self.step_types.insert(tpath.clone(), state);
        Self::touch_order(&mut self.step_types_order, &tpath);
        self.evict_step_types();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn journal_tail_skips_oversized_jsonl_line_without_oom() {
        let td = tempfile::tempdir().unwrap();
        let cid = "70000000-0000-0000-0000-000000000777";
        let tpath = transcript_path(td.path(), cid);
        fs::create_dir_all(tpath.parent().unwrap()).unwrap();
        {
            let mut f = File::create(&tpath).unwrap();
            writeln!(
                f,
                "{{\"step_index\":1,\"type\":\"USER_INPUT\",\"user_request\":\"before\"}}"
            )
            .unwrap();
            f.write_all(b"{\"step_index\":2,\"type\":\"TOOL_OUTPUT\",\"payload\":\"")
                .unwrap();
            let chunk = vec![b'a'; 1024 * 1024];
            for _ in 0..17 {
                f.write_all(&chunk).unwrap();
            }
            f.write_all(b"\"}\n").unwrap();
            writeln!(
                f,
                "{{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"response\":\"after\"}}"
            )
            .unwrap();
        }
        let mut cache = JournalTailCache::new(16);
        let entries = cache.get_active_window(td.path(), cid);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].step_index, 1);
        assert_eq!(entries[1].step_index, 3);
        let stypes = cache.get_step_types(td.path(), cid);
        assert_eq!(stypes.get(&1).map(String::as_str), Some("USER_INPUT"));
        assert_eq!(stypes.get(&2).map(String::as_str), Some("TOOL_OUTPUT"));
        assert_eq!(stypes.get(&3).map(String::as_str), Some("PLANNER_RESPONSE"));
    }

    #[test]
    fn incremental_mid_write_append_and_unterminated_eof_tail_stay_exact() {
        let td = tempfile::tempdir().unwrap();
        let cid = "70000000-0000-0000-0000-000000000778";
        let tpath = transcript_path(td.path(), cid);
        fs::create_dir_all(tpath.parent().unwrap()).unwrap();

        let line1 = b"{\"step_index\":1,\"type\":\"USER_INPUT\",\"content\":\"first request\"}\n";
        // Mid-write line 2: step_index & type already flushed, plus embedded fake header inside content
        let line2_head = b"{\"step_index\":2,\"type\":\"PLANNER_RESPONSE\",\"content\":\"streaming {\\\"step_index\\\":999,\\\"type\\\":\\\"USER_INPUT\\\"} and ";
        let line2_tail = b"more text \\\"step_index\\\":888,\\\"type\\\":\\\"CHECKPOINT\\\"\"}\n";
        let line3 = b"{\"step_index\":3,\"type\":\"USER_INPUT\",\"content\":\"follow up\"}\n";

        {
            let mut f = File::create(&tpath).unwrap();
            f.write_all(line1).unwrap();
            f.write_all(line2_head).unwrap();
            f.flush().unwrap();
        }

        let mut cache = JournalTailCache::new(16);
        let win_p1 = cache.get_active_window(td.path(), cid);
        assert_eq!(
            win_p1.iter().map(|e| e.step_index).collect::<Vec<_>>(),
            vec![1]
        );
        // Exact (dev, ino, size, mtime_ns) hit returns identical snapshot
        assert_eq!(cache.get_active_window(td.path(), cid), win_p1);

        let st_p1 = cache.get_step_types(td.path(), cid);
        assert_eq!(st_p1.len(), 1);
        assert_eq!(st_p1.get(&1).map(String::as_str), Some("USER_INPUT"));
        assert!(!st_p1.contains_key(&2));
        assert_eq!(cache.get_step_types(td.path(), cid), st_p1);

        // Phase 2: flush rest of line 2 + '\n' + line 3
        {
            let mut f = OpenOptions::new().append(true).open(&tpath).unwrap();
            f.write_all(line2_tail).unwrap();
            f.write_all(line3).unwrap();
            f.flush().unwrap();
        }

        let win_p2 = cache.get_active_window(td.path(), cid);
        assert_eq!(
            win_p2.iter().map(|e| e.step_index).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(win_p2[1].snippet_long.contains("streaming"));

        let st_p2 = cache.get_step_types(td.path(), cid);
        assert_eq!(st_p2.len(), 3);
        assert_eq!(st_p2.get(&1).map(String::as_str), Some("USER_INPUT"));
        assert_eq!(st_p2.get(&2).map(String::as_str), Some("PLANNER_RESPONSE"));
        assert_eq!(st_p2.get(&3).map(String::as_str), Some("USER_INPUT"));
        assert!(!st_p2.contains_key(&999));
        assert!(!st_p2.contains_key(&888));

        // Closed file lacking trailing '\n': valid JSON at EOF is returned on poll 1 AND not duplicated on append
        let line4_no_nl =
            b"{\"step_index\":4,\"type\":\"PLANNER_RESPONSE\",\"content\":\"no trailing newline\"}";
        {
            let mut f = OpenOptions::new().append(true).open(&tpath).unwrap();
            f.write_all(line4_no_nl).unwrap();
            f.flush().unwrap();
        }

        let win_p3 = cache.get_active_window(td.path(), cid);
        assert_eq!(
            win_p3.iter().map(|e| e.step_index).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        // Exact cache hit preserves uncommitted valid EOF entry
        assert_eq!(
            cache
                .get_active_window(td.path(), cid)
                .iter()
                .map(|e| e.step_index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );

        let st_p3 = cache.get_step_types(td.path(), cid);
        assert_eq!(st_p3.get(&4).map(String::as_str), Some("PLANNER_RESPONSE"));
        assert_eq!(cache.get_step_types(td.path(), cid), st_p3);

        // Append '\n' + step 5 '\n' -> zero duplicate step 4!
        {
            let mut f = OpenOptions::new().append(true).open(&tpath).unwrap();
            f.write_all(
                b"\n{\"step_index\":5,\"type\":\"USER_INPUT\",\"content\":\"after newline\"}\n",
            )
            .unwrap();
            f.flush().unwrap();
        }

        let win_p4 = cache.get_active_window(td.path(), cid);
        assert_eq!(
            win_p4.iter().map(|e| e.step_index).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        let st_p4 = cache.get_step_types(td.path(), cid);
        assert_eq!(st_p4.len(), 5);
        assert_eq!(st_p4.get(&5).map(String::as_str), Some("USER_INPUT"));

        // Uncommitted CHECKPOINT without '\n', followed by '\n' + step 7
        {
            let mut f = OpenOptions::new().append(true).open(&tpath).unwrap();
            f.write_all(b"{\"step_index\":6,\"type\":\"CHECKPOINT\",\"content\":\"summary\"}")
                .unwrap();
            f.flush().unwrap();
        }
        assert!(cache.get_active_window(td.path(), cid).is_empty());
        assert!(cache.get_active_window(td.path(), cid).is_empty());
        {
            let mut f = OpenOptions::new().append(true).open(&tpath).unwrap();
            f.write_all(
                b"\n{\"step_index\":7,\"type\":\"USER_INPUT\",\"content\":\"post compact\"}\n",
            )
            .unwrap();
            f.flush().unwrap();
        }
        let win_p5 = cache.get_active_window(td.path(), cid);
        assert_eq!(
            win_p5.iter().map(|e| e.step_index).collect::<Vec<_>>(),
            vec![7]
        );
        let st_p5 = cache.get_step_types(td.path(), cid);
        assert_eq!(st_p5.get(&6).map(String::as_str), Some("CHECKPOINT"));
        assert_eq!(st_p5.get(&7).map(String::as_str), Some("USER_INPUT"));
    }

    #[test]
    fn mid_write_oversized_eof_line_without_newline_does_not_park_mid_line() {
        let td = tempfile::tempdir().unwrap();
        let cid = "70000000-0000-0000-0000-000000000779";
        let tpath = transcript_path(td.path(), cid);
        fs::create_dir_all(tpath.parent().unwrap()).unwrap();

        let line1 = b"{\"step_index\":1,\"type\":\"USER_INPUT\",\"content\":\"start\"}\n";
        let giant_head = b"{\"step_index\":2,\"type\":\"TOOL_OUTPUT\",\"payload\":\"";
        {
            let mut f = File::create(&tpath).unwrap();
            f.write_all(line1).unwrap();
            f.write_all(giant_head).unwrap();
            // Write > MAX_LINE_BYTES (17 MiB) WITHOUT a terminating '\n'
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..17 {
                f.write_all(&chunk).unwrap();
            }
            f.flush().unwrap();
        }

        let mut cache = JournalTailCache::new(16);
        let win1 = cache.get_active_window(td.path(), cid);
        assert_eq!(
            win1.iter().map(|e| e.step_index).collect::<Vec<_>>(),
            vec![1]
        );
        let st1 = cache.get_step_types(td.path(), cid);
        assert_eq!(st1.len(), 1);
        assert_eq!(st1.get(&1).map(String::as_str), Some("USER_INPUT"));
        assert!(!st1.contains_key(&2));

        // Finish the oversized line with text that would look like a step if seek landed mid-line,
        // followed by '\n' and step 3.
        {
            let mut f = OpenOptions::new().append(true).open(&tpath).unwrap();
            f.write_all(b"{\\\"step_index\\\":777,\\\"type\\\":\\\"USER_INPUT\\\"}\"}\n")
                .unwrap();
            f.write_all(b"{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"content\":\"done\"}\n")
                .unwrap();
            f.flush().unwrap();
        }

        let win2 = cache.get_active_window(td.path(), cid);
        assert_eq!(
            win2.iter().map(|e| e.step_index).collect::<Vec<_>>(),
            vec![1, 3]
        );
        let st2 = cache.get_step_types(td.path(), cid);
        assert_eq!(st2.len(), 3);
        assert_eq!(st2.get(&1).map(String::as_str), Some("USER_INPUT"));
        assert_eq!(st2.get(&2).map(String::as_str), Some("TOOL_OUTPUT"));
        assert_eq!(st2.get(&3).map(String::as_str), Some("PLANNER_RESPONSE"));
        assert!(!st2.contains_key(&777));
    }

    #[test]
    fn find_after_last_checkpoint_handles_oversized_carry_and_eof_checkpoints() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("ckpt_test.jsonl");

        // 1. Real CHECKPOINT followed by a > MAX_CARRY_BYTES (~4.5 MiB) line containing "CHECKPOINT"
        let cp1 = b"{\"step_index\":1,\"type\":\"CHECKPOINT\",\"content\":\"real cp\"}\n";
        let expected_after_cp1 = cp1.len() as u64;
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(cp1).unwrap();
            // Craft prefix so chunk boundary at 256 KiB sees "CHECKPOINT" without '\n' after carry drops > 4 MiB
            f.write_all(b"{\"step_index\":2,\"type\":\"TOOL_OUTPUT\",\"note\":\"mentions CHECKPOINT here\",\"blob\":\"")
                .unwrap();
            let chunk = vec![b'z'; 512 * 1024];
            for _ in 0..9 {
                f.write_all(&chunk).unwrap();
            }
            f.write_all(b"\"}\n").unwrap();
            f.write_all(b"{\"step_index\":3,\"type\":\"USER_INPUT\",\"content\":\"tail\"}\n")
                .unwrap();
        }
        {
            let mut f = File::open(&path).unwrap();
            let sz = f.metadata().unwrap().len();
            assert_eq!(
                find_after_last_checkpoint_offset(&mut f, sz),
                expected_after_cp1
            );
        }

        // 2. Valid CHECKPOINT right at true EOF WITHOUT trailing '\n' -> returns true file_size
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"step_index\":4,\"type\":\"CHECKPOINT\",\"content\":\"eof cp\"}")
                .unwrap();
        }
        let eof_cp_size = fs::metadata(&path).unwrap().len();
        {
            let mut f = File::open(&path).unwrap();
            assert_eq!(
                find_after_last_checkpoint_offset(&mut f, eof_cp_size),
                eof_cp_size
            );
        }

        // 3. Mid-write invalid JSON CHECKPOINT right at EOF -> ignored, falls back to previous committed CHECKPOINT
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"\n{\"step_index\":5,\"type\":\"CHECKPOINT\",\"content\":\"mid-write")
                .unwrap();
        }
        {
            let mut f = File::open(&path).unwrap();
            let sz = f.metadata().unwrap().len();
            // Previous valid cp4 now has '\n' right after it (eof_cp_size + 1)
            assert_eq!(
                find_after_last_checkpoint_offset(&mut f, sz),
                eof_cp_size + 1
            );
        }
    }

    #[test]
    fn hydrate_truncated_entries_handles_out_of_order_partial_eof_and_symlinks() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let td = tempfile::tempdir().unwrap();
        let cid = "70000000-0000-0000-0000-000000000780";
        let tpath = transcript_path(td.path(), cid);
        let fpath = transcript_full_path(td.path(), cid);
        fs::create_dir_all(tpath.parent().unwrap()).unwrap();

        // Compact transcript has steps 1, 2, 3 all marked truncated_fields
        let compact = concat!(
            "{\"step_index\":1,\"type\":\"USER_INPUT\",\"content\":\"short1\",\"truncated_fields\":[\"content\"]}\n",
            "{\"step_index\":2,\"type\":\"PLANNER_RESPONSE\",\"content\":\"short2\",\"truncated_fields\":[\"content\"]}\n",
            "{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"content\":\"short3\",\"truncated_fields\":[\"content\"]}\n",
        );
        fs::write(&tpath, compact).unwrap();

        // transcript_full.jsonl has step 5 FIRST (> max_needed=3), then step 3, then step 2 (out-of-order),
        // and a mid-write partial trailing line for step 1 without '\n'.
        let full = concat!(
            "{\"step_index\":5,\"type\":\"PLANNER_RESPONSE\",\"content\":\"future step five\"}\n",
            "{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"content\":\"hydrated full three\"}\n",
            "{\"meta\":{\"step_index\":99},\"step_index\":2,\"type\":\"PLANNER_RESPONSE\",\"content\":\"hydrated full two\"}\n",
            "{\"step_index\":1,\"type\":\"USER_INPUT\",\"content\":\"mid-write partial one"
        );
        fs::write(&fpath, full).unwrap();

        let mut cache = JournalTailCache::new(16);
        let mut entries = cache.get_active_window(td.path(), cid);
        assert_eq!(entries.len(), 3);

        cache.hydrate_truncated_entries(td.path(), cid, &mut entries, 1, 3);
        // Step 1 was mid-write in transcript_full.jsonl -> safely retains compact "short1"
        assert_eq!(entries[0].snippet_short, "short1");
        // Steps 2 & 3 hydrated despite step 5 (> max_needed) appearing before them and nested meta.step_index=99
        assert_eq!(entries[1].snippet_long, "hydrated full two");
        assert_eq!(entries[2].snippet_long, "hydrated full three");

        // Also verify hydration when transcript_full.jsonl already appended a newer CHECKPOINT ahead of transcript.jsonl
        let full_ahead_ckpt = concat!(
            "{\"step_index\":1,\"type\":\"USER_INPUT\",\"content\":\"hydrated full one\"}\n",
            "{\"step_index\":2,\"type\":\"PLANNER_RESPONSE\",\"content\":\"hydrated full two\"}\n",
            "{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"content\":\"hydrated full three\"}\n",
            "{\"step_index\":4,\"type\":\"CHECKPOINT\",\"content\":\"ahead summary\"}\n",
        );
        fs::write(&fpath, full_ahead_ckpt).unwrap();
        let mut entries_ckpt = cache.get_active_window(td.path(), cid);
        cache.hydrate_truncated_entries(td.path(), cid, &mut entries_ckpt, 1, 3);
        assert_eq!(entries_ckpt[0].snippet_short, "hydrated full one");
        assert_eq!(entries_ckpt[1].snippet_long, "hydrated full two");
        assert_eq!(entries_ckpt[2].snippet_long, "hydrated full three");

        // O_NOFOLLOW symlink rejection on transcript_full.jsonl and transcript.jsonl
        #[cfg(unix)]
        {
            let outside = td.path().join("outside_full.jsonl");
            fs::rename(&fpath, &outside).unwrap();
            symlink(&outside, &fpath).unwrap();
            let mut entries_sym = cache.get_active_window(td.path(), cid);
            cache.hydrate_truncated_entries(td.path(), cid, &mut entries_sym, 1, 3);
            assert_eq!(entries_sym[0].snippet_short, "short1");

            let outside_t = td.path().join("outside_t.jsonl");
            fs::rename(&tpath, &outside_t).unwrap();
            symlink(&outside_t, &tpath).unwrap();
            assert!(cache.get_active_window(td.path(), cid).is_empty());
            assert!(cache.get_step_types(td.path(), cid).is_empty());
        }
    }
}
