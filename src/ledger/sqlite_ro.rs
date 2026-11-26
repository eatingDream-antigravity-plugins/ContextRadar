//! Strict read-only SQLite reader for Antigravity host DBs with incremental WAL delta tracking.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use regex::bytes::Regex as BytesRegex;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use crate::ledger::index_db::{DbVersionSig, IndexDbCache, ParsedLedgerState};
use crate::ledger::journal_tail::JournalTailCache;
use crate::ledger::{is_safe_id, LiveStepProbe, SummaryRecord, TurnSample};
use crate::settings::normalize_workspace_uri;
use crate::wire::pb_cursor::{
    decode_step_metadata, extract_checkpoint_summary_len, extract_error_summary, extract_gen_model,
    extract_step_start_and_gen_idx,
};

fn ann_title_regex() -> &'static BytesRegex {
    static RE: OnceLock<BytesRegex> = OnceLock::new();
    RE.get_or_init(|| BytesRegex::new(r#"title:\s*"((?:[^"\\]|\\.)*)""#).expect("valid title regex"))
}

/// Strictly read-only host DB opener. Never sets immutable so live WAL reads work,
/// and enforces `PRAGMA query_only = ON` so host files can never be mutated.
pub fn open_host_readonly(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(1500))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub sig: DbVersionSig,
    pub eff_mtime: f64,
}

#[inline]
fn meta_mtime_ns(meta: &fs::Metadata) -> i128 {
    (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128)
}

#[inline]
fn meta_mtime_sec(meta: &fs::Metadata) -> f64 {
    (meta.mtime() as f64) + (meta.mtime_nsec() as f64) * 1e-9
}

pub fn stat_db_stamp(db_path: &Path) -> Option<FileStamp> {
    let meta = fs::metadata(db_path).ok()?;
    let db_mtime_ns = meta_mtime_ns(&meta);
    let db_size = meta.size();
    let mut eff_mtime = meta_mtime_sec(&meta);

    let mut wal_path = db_path.as_os_str().to_os_string();
    wal_path.push("-wal");
    let (wal_mtime_ns, wal_size) = match fs::metadata(Path::new(&wal_path)) {
        Ok(wmeta) => {
            let wm = meta_mtime_ns(&wmeta);
            let wt = meta_mtime_sec(&wmeta);
            if wt > eff_mtime {
                eff_mtime = wt;
            }
            (wm, wmeta.size())
        }
        Err(_) => (0, 0),
    };

    Some(FileStamp {
        dev: meta.dev(),
        ino: meta.ino(),
        sig: DbVersionSig {
            db_mtime_ns,
            db_size,
            wal_mtime_ns,
            wal_size,
        },
        eff_mtime,
    })
}

/// Single-pass `read_dir` scan collecting `.db` and `.db-wal` stamps without ENOENT stats.
pub fn scan_conversations_dir(conv_dir: &Path) -> (HashMap<String, f64>, HashMap<String, FileStamp>) {
    let mut db_stats: HashMap<String, (u64, u64, i128, u64, f64)> = HashMap::new();
    let mut wal_stats: HashMap<String, (i128, u64, f64)> = HashMap::new();

    if let Ok(entries) = fs::read_dir(conv_dir) {
        for entry in entries.flatten() {
            let fname_os = entry.file_name();
            let Some(name) = fname_os.to_str() else { continue };
            if let Some(cid) = name.strip_suffix(".db") {
                if !is_safe_id(cid) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        db_stats.insert(
                            cid.to_string(),
                            (
                                meta.dev(),
                                meta.ino(),
                                meta_mtime_ns(&meta),
                                meta.size(),
                                meta_mtime_sec(&meta),
                            ),
                        );
                    }
                }
            } else if let Some(cid) = name.strip_suffix(".db-wal") {
                if let Ok(wmeta) = entry.metadata() {
                    wal_stats.insert(
                        cid.to_string(),
                        (meta_mtime_ns(&wmeta), wmeta.size(), meta_mtime_sec(&wmeta)),
                    );
                }
            }
        }
    }

    let mut file_mtimes = HashMap::with_capacity(db_stats.len());
    let mut file_stamps = HashMap::with_capacity(db_stats.len());
    for (cid, (dev, ino, dm, ds, dt)) in db_stats {
        let (wm, ws, eff) = match wal_stats.get(&cid) {
            Some(&(wm, ws, wt)) => (wm, ws, if wt > dt { wt } else { dt }),
            None => (0, 0, dt),
        };
        file_mtimes.insert(cid.clone(), eff);
        file_stamps.insert(
            cid,
            FileStamp {
                dev,
                ino,
                sig: DbVersionSig {
                    db_mtime_ns: dm,
                    db_size: ds,
                    wal_mtime_ns: wm,
                    wal_size: ws,
                },
                eff_mtime: eff,
            },
        );
    }

    (file_mtimes, file_stamps)
}

pub fn extract_workspaces(raw_uris: &str) -> (String, Vec<String>) {
    let trimmed = raw_uris.trim();
    if trimmed.is_empty() {
        return (String::new(), Vec::new());
    }
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for item in items {
            let s = item
                .as_str()
                .map(normalize_workspace_uri)
                .unwrap_or_default();
            if !s.is_empty() && seen.insert(s.clone()) {
                out.push(s);
            }
        }
        let primary = out.first().cloned().unwrap_or_default();
        return (primary, out);
    }
    let single = normalize_workspace_uri(trimmed);
    if single.is_empty() {
        (String::new(), Vec::new())
    } else {
        (single.clone(), vec![single])
    }
}

#[inline]
fn compute_times(began: f64, first: f64, ended: f64) -> (u64, u64) {
    if began <= 0.0 || ended <= 0.0 || ended < began {
        return (0, 0);
    }
    let total_ms = (((ended - began) * 1000.0).round() as i64).max(1) as u64;
    let ttft_ms = if first > 0.0 && began <= first && first <= ended {
        (((first - began) * 1000.0).round() as i64).max(1) as u64
    } else {
        0
    };
    (ttft_ms, total_ms)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[derive(Debug, Clone)]
struct AnnFileEntry {
    mtime_ns: i128,
    size: u64,
    archived: bool,
    pinned: bool,
    title: String,
}

#[derive(Debug, Default)]
struct AnnotationIndex {
    dir_mtime_ns: i128,
    file_cache: HashMap<String, AnnFileEntry>,
    archived: HashSet<String>,
    pinned: HashSet<String>,
    titles: HashMap<String, String>,
    version: u64,
}

impl AnnotationIndex {
    fn refresh(&mut self, ann_dir: &Path) -> u64 {
        let Ok(dir_meta) = fs::metadata(ann_dir) else {
            if self.dir_mtime_ns != -1 || !self.file_cache.is_empty() {
                self.dir_mtime_ns = -1;
                self.file_cache.clear();
                self.archived.clear();
                self.pinned.clear();
                self.titles.clear();
                self.version += 1;
            }
            return self.version;
        };
        let dir_mtime = meta_mtime_ns(&dir_meta);
        let mut changed = false;
        let mut seen = HashSet::new();

        if let Ok(entries) = fs::read_dir(ann_dir) {
            let mut buf = [0u8; 4096];
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let Some(name) = fname.to_str() else { continue };
                let Some(cid) = name.strip_suffix(".pbtxt") else { continue };
                if !is_safe_id(cid) {
                    continue;
                }
                let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
                if !meta.file_type().is_file() {
                    continue;
                }
                seen.insert(cid.to_string());
                let mtime_ns = meta_mtime_ns(&meta);
                let size = meta.size();
                if let Some(prev) = self.file_cache.get(cid) {
                    if prev.mtime_ns == mtime_ns && prev.size == size {
                        continue;
                    }
                }
                changed = true;
                if let Ok(mut fh) = File::open(entry.path()) {
                    let n = fh.read(&mut buf).unwrap_or(0);
                    let raw = &buf[..n];
                    let is_arch = contains_bytes(raw, b"archived:true")
                        || contains_bytes(raw, b"archived: true");
                    let is_pin = contains_bytes(raw, b"pinned:true")
                        || contains_bytes(raw, b"pinned: true");
                    let title = ann_title_regex()
                        .captures(raw)
                        .and_then(|c| c.get(1))
                        .map(|m| String::from_utf8_lossy(m.as_bytes()).trim().to_string())
                        .unwrap_or_default();
                    self.file_cache.insert(
                        cid.to_string(),
                        AnnFileEntry {
                            mtime_ns,
                            size,
                            archived: is_arch,
                            pinned: is_pin,
                            title,
                        },
                    );
                }
            }
        }

        if seen.len() != self.file_cache.len() {
            self.file_cache.retain(|k, _| seen.contains(k));
            changed = true;
        }

        if changed || dir_mtime != self.dir_mtime_ns {
            self.archived.clear();
            self.pinned.clear();
            self.titles.clear();
            for (cid, entry) in &self.file_cache {
                if entry.archived {
                    self.archived.insert(cid.clone());
                }
                if entry.pinned {
                    self.pinned.insert(cid.clone());
                }
                if !entry.title.is_empty() {
                    self.titles.insert(cid.clone(), entry.title.clone());
                }
            }
            self.dir_mtime_ns = dir_mtime;
            self.version += 1;
        }
        self.version
    }
}

#[derive(Debug, Clone)]
struct RawSummaryTuple {
    title: String,
    preview: String,
    workspace: String,
    workspaces: Vec<String>,
    status: String,
    agent_name: String,
    parent_conversation_id: String,
    nesting_depth: i64,
    not_fully_idle: bool,
    killed: bool,
    last_modified_time: String,
    last_user_input_step_index: i64,
}

#[derive(Debug, Default)]
struct SummaryIndex {
    last_stamp: Option<(u64, u64, DbVersionSig)>,
    raw_rows: HashMap<String, RawSummaryTuple>,
    fts_archived: HashSet<String>,
    last_merged_key: Option<((u64, u64, DbVersionSig), u64)>,
    merged: Arc<HashMap<String, SummaryRecord>>,
}

pub struct LedgerReader {
    pub data_root: PathBuf,
    pub conv_dir: PathBuf,
    pub ann_dir: PathBuf,
    pub summaries_db: PathBuf,
    db_cache: HashMap<String, ParsedLedgerState>,
    disk_cache: Option<IndexDbCache>,
    annotations: AnnotationIndex,
    summaries: SummaryIndex,
    pub journal_cache: JournalTailCache,
}

impl LedgerReader {
    pub fn new(data_root: PathBuf, disk_cache_path: Option<PathBuf>) -> Self {
        let conv_dir = data_root.join("conversations");
        let ann_dir = data_root.join("annotations");
        let summaries_db = data_root.join("conversation_summaries.db");
        let disk_cache = disk_cache_path
            .filter(|p| !p.as_os_str().is_empty())
            .map(IndexDbCache::new);

        Self {
            data_root,
            conv_dir,
            ann_dir,
            summaries_db,
            db_cache: HashMap::new(),
            disk_cache,
            annotations: AnnotationIndex::default(),
            summaries: SummaryIndex::default(),
            journal_cache: JournalTailCache::default(),
        }
    }

    pub fn close(&mut self) {
        if let Some(dc) = &mut self.disk_cache {
            dc.close();
        }
    }

    pub fn read_summaries(&mut self) -> Arc<HashMap<String, SummaryRecord>> {
        let Some(stamp) = stat_db_stamp(&self.summaries_db) else {
            self.summaries.last_stamp = None;
            if !self.summaries.merged.is_empty() {
                self.summaries.merged = Arc::new(HashMap::new());
            }
            return Arc::clone(&self.summaries.merged);
        };
        let db_key = (stamp.dev, stamp.ino, stamp.sig.clone());
        let ann_ver = self.annotations.refresh(&self.ann_dir);

        if self.summaries.last_stamp.as_ref() == Some(&db_key)
            && self
                .summaries
                .last_merged_key
                .as_ref()
                .map(|(k, av)| k == &db_key && *av == ann_ver)
                .unwrap_or(false)
        {
            return Arc::clone(&self.summaries.merged);
        }

        if self.summaries.last_stamp.as_ref() != Some(&db_key) {
            if let Ok(conn) = open_host_readonly(&self.summaries_db) {
                let mut parents: HashMap<String, String> = HashMap::new();
                if let Ok(mut stmt) =
                    conn.prepare("SELECT conversation_id, parent_id FROM conversation_parents")
                {
                    if let Ok(rows) = stmt.query_map([], |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                        ))
                    }) {
                        for row in rows.flatten() {
                            if let (Some(c), Some(p)) = row {
                                if !c.is_empty() && !p.is_empty() {
                                    parents.insert(c, p);
                                }
                            }
                        }
                    }
                }

                let mut fts_archived: HashSet<String> = HashSet::new();
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT DISTINCT conversation_id FROM conversations_fts WHERE archived = 1",
                ) {
                    if let Ok(rows) = stmt.query_map([], |r| r.get::<_, Option<String>>(0)) {
                        for cid in rows.flatten().flatten() {
                            if !cid.is_empty() {
                                fts_archived.insert(cid);
                            }
                        }
                    }
                }

                let mut raw_rows: HashMap<String, RawSummaryTuple> = HashMap::new();
                let query = "SELECT conversation_id, title, preview, workspace_uris, status,
                             agent_name, parent_conversation_id, nesting_depth,
                             not_fully_idle, killed, last_modified_time, last_user_input_step_index
                             FROM conversation_summaries";
                if let Ok(mut stmt) = conn.prepare(query) {
                    if let Ok(rows) = stmt.query_map([], |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                            r.get::<_, Option<String>>(6)?,
                            r.get::<_, Option<i64>>(7)?,
                            r.get::<_, Option<i64>>(8)?,
                            r.get::<_, Option<i64>>(9)?,
                            r.get::<_, Option<String>>(10)?,
                            r.get::<_, Option<i64>>(11)?,
                        ))
                    }) {
                        for row in rows.flatten() {
                            let cid = row.0.unwrap_or_default();
                            if cid.is_empty() {
                                continue;
                            }
                            let p_direct = row.6.unwrap_or_default();
                            let parent_id = if !p_direct.is_empty() {
                                p_direct
                            } else {
                                parents.get(&cid).cloned().unwrap_or_default()
                            };
                            let (ws_primary, ws_all) =
                                extract_workspaces(&row.3.unwrap_or_default());
                            raw_rows.insert(
                                cid,
                                RawSummaryTuple {
                                    title: row.1.unwrap_or_default().trim().to_string(),
                                    preview: row.2.unwrap_or_default().trim().to_string(),
                                    workspace: ws_primary,
                                    workspaces: ws_all,
                                    status: row.4.unwrap_or_default(),
                                    agent_name: row.5.unwrap_or_default().trim().to_string(),
                                    parent_conversation_id: parent_id,
                                    nesting_depth: row.7.unwrap_or(0),
                                    not_fully_idle: row.8.unwrap_or(0) != 0,
                                    killed: row.9.unwrap_or(0) != 0,
                                    last_modified_time: row.10.unwrap_or_default(),
                                    last_user_input_step_index: row.11.unwrap_or(-1),
                                },
                            );
                        }
                    }
                }

                self.summaries.raw_rows = raw_rows;
                self.summaries.fts_archived = fts_archived;
                self.summaries.last_stamp = Some(db_key.clone());
            } else {
                return Arc::clone(&self.summaries.merged);
            }
        }

        let mut merged = HashMap::with_capacity(self.summaries.raw_rows.len());
        for (cid, r) in &self.summaries.raw_rows {
            let title = self
                .annotations
                .titles
                .get(cid)
                .cloned()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| r.title.clone());
            let archived =
                self.annotations.archived.contains(cid) || self.summaries.fts_archived.contains(cid);
            let pinned = self.annotations.pinned.contains(cid);
            merged.insert(
                cid.clone(),
                SummaryRecord {
                    conversation_id: cid.clone(),
                    title,
                    preview: r.preview.clone(),
                    workspace: r.workspace.clone(),
                    workspaces: r.workspaces.clone(),
                    status: r.status.clone(),
                    agent_name: r.agent_name.clone(),
                    parent_conversation_id: r.parent_conversation_id.clone(),
                    nesting_depth: r.nesting_depth,
                    not_fully_idle: r.not_fully_idle,
                    killed: r.killed,
                    last_modified_time: r.last_modified_time.clone(),
                    last_user_input_step_index: r.last_user_input_step_index,
                    archived,
                    pinned,
                },
            );
        }

        self.summaries.merged = Arc::new(merged);
        self.summaries.last_merged_key = Some((db_key, ann_ver));
        Arc::clone(&self.summaries.merged)
    }

    pub fn read_turns(
        &mut self,
        cid: &str,
        prefetched: Option<&FileStamp>,
    ) -> Arc<Vec<TurnSample>> {
        self.read_turns_and_probe(cid, prefetched).0
    }

    pub fn read_turns_and_probe(
        &mut self,
        cid: &str,
        prefetched: Option<&FileStamp>,
    ) -> (Arc<Vec<TurnSample>>, LiveStepProbe) {
        if !is_safe_id(cid) {
            return (Arc::new(Vec::new()), LiveStepProbe::default());
        }
        let db_path = self.conv_dir.join(format!("{cid}.db"));
        let owned_stamp;
        let stamp = match prefetched {
            Some(s) => s,
            None => match stat_db_stamp(&db_path) {
                Some(s) => {
                    owned_stamp = s;
                    &owned_stamp
                }
                None => return (Arc::new(Vec::new()), LiveStepProbe::default()),
            },
        };

        if let Some(cached) = self.db_cache.get(cid) {
            if cached.dev == stamp.dev && cached.ino == stamp.ino && cached.sig == stamp.sig {
                return (Arc::clone(&cached.turns), cached.live_probe.clone());
            }
        }

        let mut base_state: Option<ParsedLedgerState> = self.db_cache.remove(cid);
        if base_state.is_none() {
            if let Some(dc) = &mut self.disk_cache {
                if let Some(disk_state) = dc.load(cid, stamp.dev, stamp.ino) {
                    if disk_state.sig == stamp.sig {
                        let out = (Arc::clone(&disk_state.turns), disk_state.live_probe.clone());
                        self.db_cache.insert(cid.to_string(), disk_state);
                        return out;
                    }
                    if stamp.sig.db_size >= disk_state.sig.db_size {
                        base_state = Some(disk_state);
                    }
                }
            }
        }

        if let Some(prev) = base_state {
            if prev.dev == stamp.dev
                && prev.ino == stamp.ino
                && stamp.sig.db_size >= prev.sig.db_size
            {
                if let Some(updated) = parse_db_incremental(&db_path, prev, stamp) {
                    let out = (Arc::clone(&updated.turns), updated.live_probe.clone());
                    if let Some(dc) = &mut self.disk_cache {
                        dc.save(cid, &updated);
                    }
                    self.db_cache.insert(cid.to_string(), updated);
                    return out;
                }
            }
        }

        let Some(state) = parse_db_full(&db_path, stamp) else {
            return (Arc::new(Vec::new()), LiveStepProbe::default());
        };
        let out = (Arc::clone(&state.turns), state.live_probe.clone());
        if let Some(dc) = &mut self.disk_cache {
            dc.save(cid, &state);
        }
        self.db_cache.insert(cid.to_string(), state);
        out
    }

    pub fn scan_dir(&self) -> (HashMap<String, f64>, HashMap<String, FileStamp>) {
        scan_conversations_dir(&self.conv_dir)
    }

    pub fn db_path(&self, cid: &str) -> PathBuf {
        self.conv_dir.join(format!("{cid}.db"))
    }
}

fn parse_db_incremental(
    db_path: &Path,
    mut prev: ParsedLedgerState,
    stamp: &FileStamp,
) -> Option<ParsedLedgerState> {
    let conn = open_host_readonly(db_path).ok()?;
    let cur_max_step: i64 = conn
        .prepare_cached("SELECT MAX(idx) FROM steps")
        .ok()?
        .query_row([], |r| r.get::<_, Option<i64>>(0))
        .ok()?
        .unwrap_or(-1);
    if cur_max_step < prev.max_step_idx {
        return None;
    }

    if let Ok(mut gstmt) = conn.prepare_cached(
        "SELECT idx, data FROM gen_metadata WHERE idx > ?1 AND data IS NOT NULL ORDER BY idx",
    ) {
        if let Ok(mut rows) = gstmt.query(params![prev.max_gen_idx]) {
            while let Ok(Some(row)) = rows.next() {
                if let Ok(gidx) = row.get::<_, i64>(0) {
                    if gidx > prev.max_gen_idx {
                        prev.max_gen_idx = gidx;
                    }
                    if let Some(bytes) = row.get_ref(1).ok().and_then(|v| v.as_bytes().ok()) {
                        let (m_id, m_disp) = extract_gen_model(bytes);
                        if !m_id.is_empty() || !m_disp.is_empty() {
                            prev.models.insert(gidx, (m_id, m_disp));
                        }
                    }
                }
            }
        }
    }

    // Look back up to 4 steps so late-arriving streaming metadata on a recent step
    // (after a subsequent tool/boundary step was inserted) is never missed.
    let start_idx = prev.max_step_idx.saturating_sub(4).max(0);
    let sql = if prev.has_step_type {
        "SELECT idx, step_type, CASE WHEN step_type IN (14, 101) THEN NULL ELSE metadata END
         FROM steps WHERE idx >= ?1 AND step_type NOT IN (17, 132) ORDER BY idx"
    } else {
        "SELECT idx, 15, metadata FROM steps WHERE idx >= ?1 AND metadata IS NOT NULL ORDER BY idx"
    };

    let mut stmt = conn.prepare_cached(sql).ok()?;
    let mut rows = stmt.query(params![start_idx]).ok()?;
    let turns_mut = Arc::make_mut(&mut prev.turns);

    let mut new_max_step = prev.max_step_idx;
    while let Ok(Some(row)) = rows.next() {
        let sidx = row.get::<_, i64>(0).unwrap_or(0);
        let stype = row.get::<_, Option<i32>>(1).unwrap_or(Some(0)).unwrap_or(0);
        let is_recheck = sidx <= prev.max_step_idx;
        if sidx > new_max_step {
            new_max_step = sidx;
        }

        if prev.has_step_type && (stype == 14 || stype == 101) {
            if !is_recheck {
                prev.saw_turn_start = true;
            }
            continue;
        }

        let parsed = row
            .get_ref(2)
            .ok()
            .and_then(|v| v.as_bytes().ok())
            .and_then(decode_step_metadata);
        let Some(parsed) = parsed else {
            if !is_recheck && prev.has_step_type && stype == 23 {
                prev.saw_ckpt_boundary = true;
            }
            continue;
        };

        let gen_idx = parsed.gen_idx;
        if !prev.models.contains_key(&gen_idx) && gen_idx <= prev.max_gen_idx {
            if let Ok(mut q) = conn.prepare_cached(
                "SELECT data FROM gen_metadata WHERE idx = ?1 AND data IS NOT NULL",
            ) {
                if let Ok(Some(blob)) = q.query_row(params![gen_idx], |r| r.get::<_, Vec<u8>>(0)).optional() {
                    let pair = extract_gen_model(&blob);
                    if !pair.0.is_empty() || !pair.1.is_empty() {
                        prev.models.insert(gen_idx, pair);
                    }
                }
            }
        }

        let (m_id, m_disp) = match prev.models.get(&gen_idx) {
            Some((a, b)) if !a.is_empty() || !b.is_empty() => {
                prev.last_known_model = (a.clone(), b.clone());
                (a.clone(), b.clone())
            }
            _ => prev.last_known_model.clone(),
        };

        let model_out = if m_id.is_empty() { "unknown".to_string() } else { m_id.clone() };
        let disp_out = if !m_disp.is_empty() {
            m_disp
        } else if !m_id.is_empty() {
            m_id
        } else {
            "未知模型".to_string()
        };

        if !parsed.response_id.is_empty() {
            if let Some(&pos) = prev.by_resp.get(&parsed.response_id) {
                let existing = &turns_mut[pos];
                let began = if existing.timestamp > 0.0 { existing.timestamp } else { parsed.began };
                let (mut ttft_ms, total_ms) = compute_times(began, parsed.first, parsed.ended);
                if existing.ttft_ms > 0 && ttft_ms == 0 {
                    ttft_ms = existing.ttft_ms;
                }
                turns_mut[pos] = TurnSample {
                    turn_index: existing.turn_index,
                    step_idx: sidx,
                    timestamp: began,
                    model: model_out,
                    model_display: disp_out,
                    input_tokens: parsed.input,
                    cache_read: parsed.cache_read,
                    cache_write: parsed.cache_write,
                    output_tokens: parsed.output,
                    thinking_tokens: parsed.thinking_tokens,
                    response_tokens: parsed.response_tokens,
                    ttft_ms,
                    total_ms,
                    response_id: parsed.response_id,
                    step_type: stype,
                    is_checkpoint: stype == 23,
                    after_turn_start: existing.after_turn_start,
                    after_checkpoint: existing.after_checkpoint,
                };
                if stype == 23 {
                    prev.saw_ckpt_boundary = true;
                }
                continue;
            }
        }

        if is_recheck {
            if let Some(pos) = turns_mut.iter().rposition(|t| t.step_idx == sidx) {
                let existing = &turns_mut[pos];
                let began = if existing.timestamp > 0.0 { existing.timestamp } else { parsed.began };
                let (mut ttft_ms, total_ms) = compute_times(began, parsed.first, parsed.ended);
                if existing.ttft_ms > 0 && ttft_ms == 0 {
                    ttft_ms = existing.ttft_ms;
                }
                let resp_id = parsed.response_id;
                if !resp_id.is_empty() {
                    prev.by_resp.insert(resp_id.clone(), pos);
                }
                turns_mut[pos] = TurnSample {
                    turn_index: existing.turn_index,
                    step_idx: sidx,
                    timestamp: began,
                    model: model_out,
                    model_display: disp_out,
                    input_tokens: parsed.input,
                    cache_read: parsed.cache_read,
                    cache_write: parsed.cache_write,
                    output_tokens: parsed.output,
                    thinking_tokens: parsed.thinking_tokens,
                    response_tokens: parsed.response_tokens,
                    ttft_ms,
                    total_ms,
                    response_id: resp_id,
                    step_type: stype,
                    is_checkpoint: stype == 23,
                    after_turn_start: existing.after_turn_start,
                    after_checkpoint: existing.after_checkpoint,
                };
                continue;
            }
        }

        let (ttft_ms, total_ms) = compute_times(parsed.began, parsed.first, parsed.ended);
        let new_idx = turns_mut.len();
        let resp_id = parsed.response_id;
        let new_turn = TurnSample {
            turn_index: new_idx + 1,
            step_idx: sidx,
            timestamp: parsed.began,
            model: model_out,
            model_display: disp_out,
            input_tokens: parsed.input,
            cache_read: parsed.cache_read,
            cache_write: parsed.cache_write,
            output_tokens: parsed.output,
            thinking_tokens: parsed.thinking_tokens,
            response_tokens: parsed.response_tokens,
            ttft_ms,
            total_ms,
            response_id: resp_id.clone(),
            step_type: stype,
            is_checkpoint: stype == 23,
            after_turn_start: if prev.has_step_type { Some(prev.saw_turn_start) } else { None },
            after_checkpoint: prev.saw_ckpt_boundary,
        };
        prev.saw_ckpt_boundary = stype == 23;
        prev.saw_turn_start = false;
        if !resp_id.is_empty() {
            prev.by_resp.insert(resp_id, new_idx);
        }
        turns_mut.push(new_turn);
    }

    let live_probe = probe_live_step_state(
        &conn,
        prev.has_step_type,
        &prev.turns,
        &prev.models,
        prev.max_gen_idx,
    );
    prev.dev = stamp.dev;
    prev.ino = stamp.ino;
    prev.sig = stamp.sig.clone();
    prev.max_step_idx = new_max_step.max(cur_max_step);
    prev.live_probe = live_probe;
    Some(prev)
}

fn parse_db_full(db_path: &Path, stamp: &FileStamp) -> Option<ParsedLedgerState> {
    let conn = open_host_readonly(db_path).ok()?;

    let mut models: HashMap<i64, (String, String)> = HashMap::new();
    let mut max_gen_idx: i64 = -1;
    if let Ok(mut gstmt) =
        conn.prepare("SELECT idx, data FROM gen_metadata WHERE data IS NOT NULL ORDER BY idx")
    {
        if let Ok(mut rows) = gstmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                if let Ok(gidx) = row.get::<_, i64>(0) {
                    if gidx > max_gen_idx {
                        max_gen_idx = gidx;
                    }
                    if let Some(bytes) = row.get_ref(1).ok().and_then(|v| v.as_bytes().ok()) {
                        let (m_id, m_disp) = extract_gen_model(bytes);
                        if !m_id.is_empty() || !m_disp.is_empty() {
                            models.insert(gidx, (m_id, m_disp));
                        }
                    }
                }
            }
        }
    }

    let mut has_step_type = true;
    let stmt_res = conn.prepare(
        "SELECT idx, step_type, CASE WHEN step_type IN (14, 101) THEN NULL ELSE metadata END
         FROM steps WHERE step_type NOT IN (17, 132) ORDER BY idx",
    );
    let mut stmt = match stmt_res {
        Ok(s) => s,
        Err(_) => {
            has_step_type = false;
            conn.prepare(
                "SELECT idx, 15, metadata FROM steps WHERE metadata IS NOT NULL ORDER BY idx",
            )
            .ok()?
        }
    };

    let mut turns: Vec<TurnSample> = Vec::new();
    let mut first_ts_by_turn: Vec<f64> = Vec::new();
    let mut by_resp: HashMap<String, usize> = HashMap::new();
    let mut last_known_model = (String::new(), String::new());
    let mut saw_ckpt_boundary = false;
    let mut saw_turn_start = false;
    let mut max_step_idx: i64 = -1;

    let mut rows = stmt.query([]).ok()?;
    while let Ok(Some(row)) = rows.next() {
        let sidx = row.get::<_, i64>(0).unwrap_or(0);
        if sidx > max_step_idx {
            max_step_idx = sidx;
        }
        let stype = row.get::<_, Option<i32>>(1).unwrap_or(Some(0)).unwrap_or(0);
        if has_step_type && (stype == 14 || stype == 101) {
            saw_turn_start = true;
            continue;
        }

        let parsed = row
            .get_ref(2)
            .ok()
            .and_then(|v| v.as_bytes().ok())
            .and_then(decode_step_metadata);
        let Some(parsed) = parsed else {
            if has_step_type && stype == 23 {
                saw_ckpt_boundary = true;
            }
            continue;
        };

        let (m_id, m_disp) = match models.get(&parsed.gen_idx) {
            Some((a, b)) if !a.is_empty() || !b.is_empty() => {
                last_known_model = (a.clone(), b.clone());
                (a.clone(), b.clone())
            }
            _ => last_known_model.clone(),
        };

        let model_out = if m_id.is_empty() { "unknown".to_string() } else { m_id.clone() };
        let disp_out = if !m_disp.is_empty() {
            m_disp
        } else if !m_id.is_empty() {
            m_id
        } else {
            "未知模型".to_string()
        };

        if !parsed.response_id.is_empty() {
            if let Some(&pos) = by_resp.get(&parsed.response_id) {
                let prev_t = &turns[pos];
                let began = if prev_t.timestamp > 0.0 { prev_t.timestamp } else { parsed.began };
                let first = if first_ts_by_turn[pos] > 0.0 { first_ts_by_turn[pos] } else { parsed.first };
                first_ts_by_turn[pos] = first;
                let (ttft_ms, total_ms) = compute_times(began, first, parsed.ended);
                turns[pos] = TurnSample {
                    turn_index: prev_t.turn_index,
                    step_idx: sidx,
                    timestamp: began,
                    model: model_out,
                    model_display: disp_out,
                    input_tokens: parsed.input,
                    cache_read: parsed.cache_read,
                    cache_write: parsed.cache_write,
                    output_tokens: parsed.output,
                    thinking_tokens: parsed.thinking_tokens,
                    response_tokens: parsed.response_tokens,
                    ttft_ms,
                    total_ms,
                    response_id: parsed.response_id,
                    step_type: stype,
                    is_checkpoint: stype == 23,
                    after_turn_start: prev_t.after_turn_start,
                    after_checkpoint: prev_t.after_checkpoint,
                };
                if stype == 23 {
                    saw_ckpt_boundary = true;
                }
                continue;
            }
        }

        let (ttft_ms, total_ms) = compute_times(parsed.began, parsed.first, parsed.ended);
        let pos = turns.len();
        let resp_id = parsed.response_id;
        turns.push(TurnSample {
            turn_index: pos + 1,
            step_idx: sidx,
            timestamp: parsed.began,
            model: model_out,
            model_display: disp_out,
            input_tokens: parsed.input,
            cache_read: parsed.cache_read,
            cache_write: parsed.cache_write,
            output_tokens: parsed.output,
            thinking_tokens: parsed.thinking_tokens,
            response_tokens: parsed.response_tokens,
            ttft_ms,
            total_ms,
            response_id: resp_id.clone(),
            step_type: stype,
            is_checkpoint: stype == 23,
            after_turn_start: if has_step_type { Some(saw_turn_start) } else { None },
            after_checkpoint: saw_ckpt_boundary,
        });
        first_ts_by_turn.push(parsed.first);
        saw_ckpt_boundary = stype == 23;
        saw_turn_start = false;
        if !resp_id.is_empty() {
            by_resp.insert(resp_id, pos);
        }
    }

    if let Ok(Some(exact_max)) =
        conn.query_row("SELECT MAX(idx) FROM steps", [], |r| r.get::<_, Option<i64>>(0))
    {
        if exact_max > max_step_idx {
            max_step_idx = exact_max;
        }
    }

    let live_probe = probe_live_step_state(&conn, has_step_type, &turns, &models, max_gen_idx);

    Some(ParsedLedgerState {
        dev: stamp.dev,
        ino: stamp.ino,
        sig: stamp.sig.clone(),
        has_step_type,
        models,
        max_gen_idx,
        max_step_idx,
        last_known_model,
        saw_ckpt_boundary,
        saw_turn_start,
        by_resp,
        turns: Arc::new(turns),
        live_probe,
    })
}

/// O(log N) indexed live step probe on an already-open read-only conversation connection.
/// Gracefully ignores missing columns (`status`, `error_details`, `step_payload`) on minimal/legacy schemas.
fn probe_live_step_state(
    conn: &Connection,
    has_step_type: bool,
    turns: &[TurnSample],
    models: &HashMap<i64, (String, String)>,
    max_gen_idx: i64,
) -> LiveStepProbe {
    let mut probe = LiveStepProbe::default();
    if !has_step_type {
        return probe;
    }

    // 1. Active in-flight steps via `idx_steps_status`
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT idx, step_type, status, metadata
         FROM steps
         WHERE status IN (1, 2, 8, 9, 11)
         ORDER BY idx DESC
         LIMIT 4",
    ) {
        let mut active_rows: Vec<(i64, i32, i32, Option<Vec<u8>>)> = Vec::with_capacity(4);
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                let idx = row.get::<_, i64>(0).unwrap_or(-1);
                let stype = row.get::<_, Option<i32>>(1).unwrap_or(Some(0)).unwrap_or(0);
                let status = row.get::<_, Option<i32>>(2).unwrap_or(Some(0)).unwrap_or(0);
                let meta = row
                    .get_ref(3)
                    .ok()
                    .and_then(|v| v.as_bytes().ok())
                    .map(|b| b.to_vec());
                active_rows.push((idx, stype, status, meta));
            }
        }
        let chosen = if let Some(ckpt_row) = active_rows.iter().find(|r| r.1 == 23) {
            probe.live_phase = "compacting".to_string();
            Some(ckpt_row)
        } else if let Some(non_user) = active_rows.iter().find(|r| r.1 != 14) {
            probe.live_phase = if non_user.1 == 15 {
                "llm".to_string()
            } else {
                "tool".to_string()
            };
            Some(non_user)
        } else {
            None
        };
        if let Some((idx, _stype, status, meta)) = chosen {
            probe.live_active_step_idx = *idx;
            probe.live_step_waiting = matches!(*status, 1 | 8 | 9 | 11);
            let (start_ts, gen_idx_opt) = meta
                .as_deref()
                .map(extract_step_start_and_gen_idx)
                .unwrap_or((0.0, None));
            probe.live_started_ts = start_ts;
            let resolved_gen_idx = gen_idx_opt.or_else(|| {
                if probe.live_phase == "compacting" && max_gen_idx >= 0 {
                    Some(max_gen_idx)
                } else {
                    None
                }
            });
            if let Some(gidx) = resolved_gen_idx {
                if let Some((m_id, m_disp)) = models.get(&gidx) {
                    probe.live_model = if !m_id.is_empty() {
                        m_id.clone()
                    } else {
                        m_disp.clone()
                    };
                } else if let Ok(mut gq) = conn.prepare_cached(
                    "SELECT data FROM gen_metadata WHERE idx = ?1 AND data IS NOT NULL",
                ) {
                    if let Ok(Some(blob)) =
                        gq.query_row(params![gidx], |r| r.get::<_, Vec<u8>>(0)).optional()
                    {
                        let (m_id, m_disp) = extract_gen_model(&blob);
                        probe.live_model = if !m_id.is_empty() { m_id } else { m_disp };
                    }
                }
            }
        }
    }

    // 2. Latest terminal/active step across step_type IN (15, 17, 23) using zero-sort O(log N) index seeks
    let latest_completed_turn_step_idx = turns.iter().map(|t| t.step_idx).max().unwrap_or(-1);
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT idx, step_type, status, error_details
         FROM steps
         WHERE step_type = ?1
         ORDER BY idx DESC
         LIMIT 1",
    ) {
        let mut best: Option<(i64, i32, Option<Vec<u8>>)> = None;
        for st in [15i32, 17, 23] {
            if let Ok(Some((idx, status, err_blob))) = stmt
                .query_row(params![st], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<i32>>(2)?.unwrap_or(0),
                        r.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                })
                .optional()
            {
                if best.as_ref().map(|b| idx > b.0).unwrap_or(true) {
                    best = Some((idx, status, err_blob));
                }
            }
        }
        if let Some((err_idx, status, err_blob)) = best {
            if status == 7 && err_idx >= latest_completed_turn_step_idx {
                let summary = err_blob
                    .as_deref()
                    .map(extract_error_summary)
                    .unwrap_or_default();
                probe.live_error = if summary.is_empty() {
                    "Step error".to_string()
                } else {
                    summary
                };
                probe.live_error_step_idx = err_idx;
            }
        }
    }

    // 3. Latest completed checkpoint summary byte length via `idx_steps_step_type` (zero-alloc)
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT idx, step_payload
         FROM steps
         WHERE step_type = 23 AND status = 3
         ORDER BY idx DESC
         LIMIT 1",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            if let Ok(Some(row)) = rows.next() {
                if let Some(bytes) = row.get_ref(1).ok().and_then(|v| v.as_bytes().ok()) {
                    probe.compact_summary_bytes = extract_checkpoint_summary_len(bytes);
                }
            }
        }
    }

    probe
}
