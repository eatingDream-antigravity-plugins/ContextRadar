//! Zero-copy slice Protobuf wire cursor with early-exit decoding.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone)]
pub struct StepMetaDecoded {
    pub began: f64,
    pub first: f64,
    pub ended: f64,
    pub input: u64,
    pub output: u64,
    pub thinking_tokens: u64,
    pub response_tokens: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub response_id: String,
    pub gen_idx: i64,
}

#[derive(Clone, Copy)]
pub struct PbCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> PbCursor<'a> {
    #[inline]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub fn is_eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    #[inline]
    pub fn read_varint(&mut self) -> Option<u64> {
        let len = self.buf.len();
        if self.pos >= len {
            return None;
        }
        let b0 = self.buf[self.pos];
        self.pos += 1;
        if b0 < 0x80 {
            return Some(b0 as u64);
        }
        let mut val = (b0 & 0x7F) as u64;
        let mut shift = 7u32;
        while self.pos < len {
            let b = self.buf[self.pos];
            self.pos += 1;
            if shift >= 64 {
                return None;
            }
            val |= ((b & 0x7F) as u64) << shift;
            if b < 0x80 {
                return Some(val);
            }
            shift += 7;
        }
        None
    }

    #[inline]
    pub fn skip_varint(&mut self) -> bool {
        let len = self.buf.len();
        let mut count = 0usize;
        while self.pos < len {
            let b = self.buf[self.pos];
            self.pos += 1;
            if b < 0x80 {
                return true;
            }
            count += 1;
            if count >= 10 {
                return false;
            }
        }
        false
    }

    #[inline]
    pub fn read_tag(&mut self) -> Option<(u32, u8)> {
        let tag = self.read_varint()?;
        let field_num = (tag >> 3) as u32;
        if field_num == 0 {
            return None;
        }
        Some((field_num, (tag & 0x07) as u8))
    }

    #[inline]
    pub fn read_bytes(&mut self) -> Option<&'a [u8]> {
        let ln = self.read_varint()? as usize;
        let end = self.pos.checked_add(ln)?;
        if end > self.buf.len() {
            return None;
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    #[inline]
    pub fn skip_field(&mut self, wire_type: u8) -> bool {
        match wire_type {
            0 => self.skip_varint(),
            1 => {
                if let Some(end) = self.pos.checked_add(8) {
                    if end <= self.buf.len() {
                        self.pos = end;
                        return true;
                    }
                }
                false
            }
            2 => self.read_bytes().is_some(),
            5 => {
                if let Some(end) = self.pos.checked_add(4) {
                    if end <= self.buf.len() {
                        self.pos = end;
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }
}

/// Decode `google.protobuf.Timestamp` directly from a borrowed slice.
#[inline]
pub fn decode_timestamp(buf: &[u8]) -> f64 {
    let mut cur = PbCursor::new(buf);
    let mut sec: u64 = 0;
    let mut nanos: u64 = 0;
    while !cur.is_eof() {
        let Some((fnum, wtype)) = cur.read_tag() else {
            break;
        };
        if wtype == 0 {
            let Some(v) = cur.read_varint() else { break };
            if fnum == 1 {
                sec = v;
            } else if fnum == 2 {
                nanos = v;
            }
        } else if !cur.skip_field(wtype) {
            break;
        }
    }
    if sec == 0 || sec > (1u64 << 40) || nanos >= 1_000_000_000 {
        return 0.0;
    }
    (sec as f64) + (nanos as f64) * 1e-9
}

/// Extract `(canonical_model_id, display_name)` from `gen_metadata.data` with immediate early-exit.
pub fn extract_gen_model(buf: &[u8]) -> (String, String) {
    let mut f19: Option<&[u8]> = None;
    let mut f21: Option<&[u8]> = None;
    let mut f22: Option<&[u8]> = None;

    let mut outer = PbCursor::new(buf);
    while !outer.is_eof() {
        let Some((fnum, wtype)) = outer.read_tag() else {
            break;
        };
        if wtype == 2 {
            let Some(sub) = outer.read_bytes() else { break };
            if fnum == 1 && !sub.is_empty() {
                let mut inner = PbCursor::new(sub);
                while !inner.is_eof() {
                    let Some((fn2, wt2)) = inner.read_tag() else {
                        break;
                    };
                    if wt2 == 2 {
                        let Some(val) = inner.read_bytes() else { break };
                        if val.len() <= 200 {
                            match fn2 {
                                19 if f19.is_none() => {
                                    let s = trim_ascii_slice(val);
                                    if !s.is_empty() {
                                        f19 = Some(s);
                                    }
                                }
                                21 if f21.is_none() => {
                                    let s = trim_ascii_slice(val);
                                    if !s.is_empty() {
                                        f21 = Some(s);
                                    }
                                }
                                22 if f22.is_none() => {
                                    let s = trim_ascii_slice(val);
                                    if !s.is_empty() {
                                        f22 = Some(s);
                                    }
                                }
                                _ => {}
                            }
                            if let (Some(a), Some(b)) = (f19, f21) {
                                return (
                                    String::from_utf8_lossy(a).trim().to_string(),
                                    String::from_utf8_lossy(b).trim().to_string(),
                                );
                            }
                        }
                    } else if !inner.skip_field(wt2) {
                        break;
                    }
                }
                if let (Some(a), Some(b)) = (f19, f21) {
                    return (
                        String::from_utf8_lossy(a).trim().to_string(),
                        String::from_utf8_lossy(b).trim().to_string(),
                    );
                }
            }
        } else if !outer.skip_field(wtype) {
            break;
        }
    }

    let s19 = f19
        .map(|s| String::from_utf8_lossy(s).trim().to_string())
        .unwrap_or_default();
    let s21 = f21
        .map(|s| String::from_utf8_lossy(s).trim().to_string())
        .unwrap_or_default();
    let s22 = f22
        .map(|s| String::from_utf8_lossy(s).trim().to_string())
        .unwrap_or_default();

    let canonical = if !s19.is_empty() {
        s19.clone()
    } else if !s21.is_empty() {
        s21.clone()
    } else {
        s22.clone()
    };
    let display = if !s21.is_empty() {
        s21
    } else if !s19.is_empty() {
        s19
    } else {
        s22
    };
    (canonical, display)
}

#[inline]
fn trim_ascii_slice(mut s: &[u8]) -> &[u8] {
    while let Some(&first) = s.first() {
        if first.is_ascii_whitespace() {
            s = &s[1..];
        } else {
            break;
        }
    }
    while let Some(&last) = s.last() {
        if last.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// Decode `steps.metadata` protobuf into timing, token usage, response_id, and gen_idx.
pub fn decode_step_metadata(buf: &[u8]) -> Option<StepMetaDecoded> {
    let mut start_ts = 0.0f64;
    let mut first_ts = 0.0f64;
    let mut end_ts = 0.0f64;
    let mut usage_slice: Option<&[u8]> = None;
    let mut gen_idx: i64 = 0;

    let mut cur = PbCursor::new(buf);
    while !cur.is_eof() {
        let Some((fnum, wtype)) = cur.read_tag() else {
            break;
        };
        if wtype == 2 {
            let Some(sub) = cur.read_bytes() else { break };
            if !sub.is_empty() {
                match fnum {
                    1 => start_ts = decode_timestamp(sub),
                    6 => first_ts = decode_timestamp(sub),
                    7 => end_ts = decode_timestamp(sub),
                    9 => usage_slice = Some(sub),
                    20 => {
                        let mut inner = PbCursor::new(sub);
                        while !inner.is_eof() {
                            let Some((fn2, wt2)) = inner.read_tag() else {
                                break;
                            };
                            if wt2 == 0 {
                                let Some(v2) = inner.read_varint() else { break };
                                if fn2 == 3 {
                                    gen_idx = v2 as i64;
                                }
                            } else if !inner.skip_field(wt2) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if !cur.skip_field(wtype) {
            break;
        }
    }

    let usage_buf = usage_slice?;
    if start_ts <= 0.0 {
        return None;
    }

    let mut in_tok = 0u64;
    let mut out_tok = 0u64;
    let mut resp_tok = 0u64;
    let mut think_tok = 0u64;
    let mut cw_tok = 0u64;
    let mut cr_tok = 0u64;
    let mut resp_bytes: &[u8] = &[];

    let mut ucur = PbCursor::new(usage_buf);
    while !ucur.is_eof() {
        let Some((fnum, wtype)) = ucur.read_tag() else {
            break;
        };
        match wtype {
            0 => {
                let Some(val) = ucur.read_varint() else { break };
                match fnum {
                    2 => in_tok = val,
                    3 => out_tok = val,
                    4 => cw_tok = val,
                    5 => cr_tok = val,
                    9 => resp_tok = val,
                    10 => think_tok = val,
                    _ => {}
                }
            }
            2 => {
                let Some(val) = ucur.read_bytes() else { break };
                if fnum == 11 {
                    resp_bytes = val;
                }
            }
            _ => {
                if !ucur.skip_field(wtype) {
                    break;
                }
            }
        }
    }

    if in_tok
        .saturating_add(out_tok)
        .saturating_add(cw_tok)
        .saturating_add(cr_tok)
        == 0
    {
        return None;
    }

    if resp_tok == 0 && think_tok == 0 && out_tok > 0 {
        resp_tok = out_tok;
    }

    Some(StepMetaDecoded {
        began: start_ts,
        first: first_ts,
        ended: end_ts,
        input: in_tok,
        output: out_tok,
        thinking_tokens: think_tok,
        response_tokens: resp_tok,
        cache_write: cw_tok,
        cache_read: cr_tok,
        response_id: String::from_utf8_lossy(resp_bytes).into_owned(),
        gen_idx,
    })
}

/// Fast single-field scan for `steps.metadata` field `1` (`google.protobuf.Timestamp`)
/// for in-flight steps that do not have `usage` populated yet.
#[inline]
pub fn extract_step_start_ts(metadata: &[u8]) -> f64 {
    let mut cur = PbCursor::new(metadata);
    while !cur.is_eof() {
        let Some((fnum, wtype)) = cur.read_tag() else {
            break;
        };
        if fnum == 1 && wtype == 2 {
            if let Some(sub) = cur.read_bytes() {
                return decode_timestamp(sub);
            }
            break;
        }
        if !cur.skip_field(wtype) {
            break;
        }
    }
    0.0
}

/// Fast scan of `steps.metadata` extracting `(start_ts, Option<gen_idx>)` for in-flight steps.
pub fn extract_step_start_and_gen_idx(metadata: &[u8]) -> (f64, Option<i64>) {
    let mut start_ts = 0.0f64;
    let mut gen_idx: Option<i64> = None;
    let mut cur = PbCursor::new(metadata);
    while !cur.is_eof() {
        let Some((fnum, wtype)) = cur.read_tag() else {
            break;
        };
        if wtype == 2 {
            let Some(sub) = cur.read_bytes() else { break };
            if !sub.is_empty() {
                match fnum {
                    1 if start_ts <= 0.0 => start_ts = decode_timestamp(sub),
                    20 if gen_idx.is_none() => {
                        let mut inner = PbCursor::new(sub);
                        while !inner.is_eof() {
                            let Some((fn2, wt2)) = inner.read_tag() else {
                                break;
                            };
                            if wt2 == 0 {
                                let Some(v2) = inner.read_varint() else { break };
                                if fn2 == 3 {
                                    gen_idx = Some(v2 as i64);
                                    break;
                                }
                            } else if !inner.skip_field(wt2) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
                if start_ts > 0.0 && gen_idx.is_some() {
                    break;
                }
            }
        } else if !cur.skip_field(wtype) {
            break;
        }
    }
    (start_ts, gen_idx)
}

/// Zero-allocation byte length of checkpoint summary (`step_payload` field `30` -> inner field `5`).
#[inline]
pub fn extract_checkpoint_summary_len(step_payload: &[u8]) -> usize {
    let mut cur = PbCursor::new(step_payload);
    while !cur.is_eof() {
        let Some((fnum, wtype)) = cur.read_tag() else {
            break;
        };
        if fnum == 30 && wtype == 2 {
            let Some(sub) = cur.read_bytes() else { break };
            let mut inner = PbCursor::new(sub);
            while !inner.is_eof() {
                let Some((fn2, wt2)) = inner.read_tag() else {
                    break;
                };
                if fn2 == 5 && wt2 == 2 {
                    if let Some(val) = inner.read_bytes() {
                        return val.len();
                    }
                    return 0;
                }
                if !inner.skip_field(wt2) {
                    break;
                }
            }
            return 0;
        }
        if !cur.skip_field(wtype) {
            break;
        }
    }
    0
}

fn is_clean_printable_str(s: &str) -> bool {
    let mut has_visible = false;
    for ch in s.chars() {
        if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
            return false;
        }
        if ch.is_alphanumeric() {
            has_visible = true;
        }
    }
    has_visible
}

fn extract_pb_utf8_leaf(buf: &[u8], depth: u8) -> Option<&str> {
    if buf.is_empty() || depth > 4 {
        return None;
    }
    let mut cur = PbCursor::new(buf);
    let mut fallback_text: Option<&str> = None;
    let mut parsed_any = false;
    while !cur.is_eof() {
        let Some((_fnum, wtype)) = cur.read_tag() else {
            return if parsed_any { fallback_text } else { None };
        };
        if wtype == 2 {
            let Some(sub) = cur.read_bytes() else {
                return if parsed_any { fallback_text } else { None };
            };
            parsed_any = true;
            if sub.is_empty() {
                continue;
            }
            // If bytes do not start with an ASCII control char, prefer clean UTF-8 leaf directly.
            if sub[0] >= 0x20 {
                if let Ok(s) = std::str::from_utf8(sub) {
                    if is_clean_printable_str(s) {
                        return Some(s);
                    }
                }
            }
            // Try recursive protobuf decode for nested length-delimited submessages.
            if depth < 4 {
                if let Some(nested) = extract_pb_utf8_leaf(sub, depth + 1) {
                    return Some(nested);
                }
            }
            // Fallback for clean UTF-8 strings starting with whitespace (\n/\r/\t).
            if fallback_text.is_none() {
                if let Ok(s) = std::str::from_utf8(sub) {
                    if is_clean_printable_str(s) {
                        fallback_text = Some(s);
                    }
                }
            }
        } else if !cur.skip_field(wtype) {
            return if parsed_any { fallback_text } else { None };
        } else {
            parsed_any = true;
        }
    }
    fallback_text
}

fn secret_redact_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:Bearer\s+[A-Za-z0-9._\-~+/]+=*|\b(?:sk-[A-Za-z0-9_\-]{12,}|AIza[0-9A-Za-z_\-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[baprs]-[A-Za-z0-9\-]{10,})\b|(?:\b(?:api_?key|access_?token|auth_?token|token|key)\s*[=:]\s*)[^\s&;,]+)",
        )
        .expect("valid secret redaction regex")
    })
}

fn home_path_redact_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:/(?:Users|home)/[^/\s]+|[A-Za-z]:\\Users\\[^\\\s]+)")
            .expect("valid home path redaction regex")
    })
}

fn sanitize_error_text(raw: &str) -> String {
    // Collapse control chars, newlines, and consecutive whitespace into single spaces.
    let mut collapsed = String::with_capacity(raw.len().min(256));
    let mut prev_space = true;
    for ch in raw.chars() {
        if ch.is_control() || ch.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
                prev_space = true;
            }
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let redacted_secrets = secret_redact_regex().replace_all(trimmed, "[REDACTED]");
    let redacted = home_path_redact_regex().replace_all(&redacted_secrets, "~");

    let max_chars = 140usize;
    let mut count = 0usize;
    let mut cut_byte: Option<usize> = None;
    for (bidx, _) in redacted.char_indices() {
        if count == max_chars - 1 {
            cut_byte = Some(bidx);
        }
        count += 1;
        if count > max_chars {
            break;
        }
    }
    if count > max_chars {
        if let Some(b) = cut_byte {
            let prefix = redacted[..b].trim_end();
            return format!("{prefix}…");
        }
    }
    redacted.into_owned()
}

/// Safely extract the first clean printable UTF-8 leaf string from `steps.error_details`
/// protobuf wire bytes (falling back to clean UTF-8 lines), sanitize control chars/newlines,
/// redact user paths and credentials, and truncate cleanly to `<= 140` chars.
pub fn extract_error_summary(error_details: &[u8]) -> String {
    if error_details.is_empty() {
        return String::new();
    }
    if let Some(leaf) = extract_pb_utf8_leaf(error_details, 0) {
        let cleaned = sanitize_error_text(leaf);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    if let Ok(text) = std::str::from_utf8(error_details) {
        for line in text.lines() {
            let t = line.trim();
            if !t.is_empty() && is_clean_printable_str(t) {
                let cleaned = sanitize_error_text(t);
                if !cleaned.is_empty() {
                    return cleaned;
                }
            }
        }
    }
    String::new()
}
