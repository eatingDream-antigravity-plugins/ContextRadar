//! Zero-copy slice Protobuf wire cursor with early-exit decoding.

#[derive(Debug, Clone)]
pub struct StepMetaDecoded {
    pub began: f64,
    pub first: f64,
    pub ended: f64,
    pub input: u64,
    pub output: u64,
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
        let Some((fnum, wtype)) = cur.read_tag() else { break };
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
        let Some((fnum, wtype)) = outer.read_tag() else { break };
        if wtype == 2 {
            let Some(sub) = outer.read_bytes() else { break };
            if fnum == 1 && !sub.is_empty() {
                let mut inner = PbCursor::new(sub);
                while !inner.is_eof() {
                    let Some((fn2, wt2)) = inner.read_tag() else { break };
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

    let s19 = f19.map(|s| String::from_utf8_lossy(s).trim().to_string()).unwrap_or_default();
    let s21 = f21.map(|s| String::from_utf8_lossy(s).trim().to_string()).unwrap_or_default();
    let s22 = f22.map(|s| String::from_utf8_lossy(s).trim().to_string()).unwrap_or_default();

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
        let Some((fnum, wtype)) = cur.read_tag() else { break };
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
                            let Some((fn2, wt2)) = inner.read_tag() else { break };
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
    let mut cw_tok = 0u64;
    let mut cr_tok = 0u64;
    let mut resp_bytes: &[u8] = &[];

    let mut ucur = PbCursor::new(usage_buf);
    while !ucur.is_eof() {
        let Some((fnum, wtype)) = ucur.read_tag() else { break };
        match wtype {
            0 => {
                let Some(val) = ucur.read_varint() else { break };
                match fnum {
                    2 => in_tok = val,
                    3 => out_tok = val,
                    4 => cw_tok = val,
                    5 => cr_tok = val,
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

    if in_tok.saturating_add(out_tok).saturating_add(cw_tok).saturating_add(cr_tok) == 0 {
        return None;
    }

    Some(StepMetaDecoded {
        began: start_ts,
        first: first_ts,
        ended: end_ts,
        input: in_tok,
        output: out_tok,
        cache_write: cw_tok,
        cache_read: cr_tok,
        response_id: String::from_utf8_lossy(resp_bytes).into_owned(),
        gen_idx,
    })
}
