//! Early-exit balanced-JSON stdin reader (zero open-pipe stall) and UTF-8 byte truncator.

use core::ffi::c_void;
use std::time::{Duration, Instant};

use serde_json::Value;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x0001;

#[cfg(target_os = "macos")]
type Nfds = core::ffi::c_uint;
#[cfg(not(target_os = "macos"))]
type Nfds = core::ffi::c_ulong;

extern "C" {
    fn poll(fds: *mut PollFd, nfds: Nfds, timeout: i32) -> i32;
    fn read(fd: i32, buf: *mut c_void, count: usize) -> isize;
}

/// Truncate UTF-8 string cleanly at a char boundary so byte length <= `max_bytes`.
pub fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || text.is_empty() {
        return String::new();
    }
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes <= 3 {
        return ".".repeat(max_bytes);
    }
    let mut end = max_bytes - 3;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// Streaming balanced-JSON object/array detector.
#[derive(Debug, Default)]
pub struct JsonBalanceScanner {
    started: bool,
    depth: usize,
    in_string: bool,
    escaped: bool,
    invalid_prefix: bool,
}

impl JsonBalanceScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes; returns `true` as soon as a balanced top-level JSON object completes.
    pub fn push(&mut self, chunk: &[u8]) -> bool {
        if self.invalid_prefix {
            return false;
        }
        for &b in chunk {
            if !self.started {
                if b.is_ascii_whitespace() {
                    continue;
                }
                if b == b'{' {
                    self.started = true;
                    self.depth = 1;
                    continue;
                }
                self.invalid_prefix = true;
                return false;
            }

            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if b == b'\\' {
                    self.escaped = true;
                } else if b == b'"' {
                    self.in_string = false;
                }
                continue;
            }

            match b {
                b'"' => self.in_string = true,
                b'{' | b'[' => self.depth += 1,
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    pub fn is_complete(&self) -> bool {
        self.started && self.depth == 0 && !self.in_string && !self.invalid_prefix
    }
}

/// Read JSON from stdin fd 0, returning immediately once a balanced top-level JSON object arrives
/// even if the parent process keeps the stdin pipe open.
pub fn read_stdin_json_fast(timeout_ms: u64) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut tmp = [0u8; 4096];
    let mut scanner = JsonBalanceScanner::new();
    const MAX_STDIN_BYTES: usize = 4 * 1024 * 1024;

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let rem_ms = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(i32::MAX as u128) as i32;

        let mut pfd = PollFd {
            fd: 0,
            events: POLLIN,
            revents: 0,
        };
        let ready = unsafe { poll(&mut pfd as *mut PollFd, 1, rem_ms) };
        if ready <= 0 {
            if ready == 0 {
                break;
            }
            // Retry briefly on EINTR until deadline
            continue;
        }

        let n = unsafe { read(0, tmp.as_mut_ptr().cast::<c_void>(), tmp.len()) };
        if n <= 0 {
            // EOF or error
            break;
        }
        let slice = &tmp[..(n as usize)];
        buf.extend_from_slice(slice);

        if scanner.push(slice) {
            if let Ok(val) = serde_json::from_slice::<Value>(&buf) {
                return Some(val);
            }
        }
        if buf.len() >= MAX_STDIN_BYTES {
            break;
        }
    }

    let trimmed = trim_bytes(&buf);
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_slice::<Value>(trimmed).ok()
}

fn trim_bytes(mut s: &[u8]) -> &[u8] {
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
