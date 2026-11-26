//! Optional persistent SQLite parse index cache stored strictly under plugin_data/context-radar-mcp/.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::ledger::{LiveStepProbe, TurnSample};

// Older versions can contain cursors saved after incomplete host reads.
pub const INDEX_SCHEMA_VERSION: i64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbVersionSig {
    pub db_mtime_ns: i128,
    pub db_size: u64,
    pub wal_mtime_ns: i128,
    pub wal_size: u64,
}

#[derive(Debug, Clone)]
pub struct ParsedLedgerState {
    pub dev: u64,
    pub ino: u64,
    pub sig: DbVersionSig,
    pub has_step_type: bool,
    pub models: HashMap<i64, (String, String)>,
    pub max_gen_idx: i64,
    pub max_step_idx: i64,
    pub last_known_model: (String, String),
    pub saw_ckpt_boundary: bool,
    pub saw_turn_start: bool,
    pub by_resp: HashMap<String, usize>,
    pub turns: Arc<Vec<TurnSample>>,
    pub live_probe: LiveStepProbe,
}

#[derive(Serialize, Deserialize)]
struct PersistedPayload {
    has_step_type: bool,
    models: Vec<(i64, String, String)>,
    max_gen_idx: i64,
    max_step_idx: i64,
    last_known_model: (String, String),
    saw_ckpt_boundary: bool,
    saw_turn_start: bool,
    turns: Vec<TurnSample>,
    #[serde(default)]
    live_probe: LiveStepProbe,
}

pub struct IndexDbCache {
    path: PathBuf,
    conn: Option<Connection>,
    disabled: bool,
}

impl IndexDbCache {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            conn: None,
            disabled: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_conn(&mut self) -> Option<&Connection> {
        if self.disabled || self.path.as_os_str().is_empty() {
            return None;
        }
        if self.conn.is_some() {
            return self.conn.as_ref();
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                if fs::create_dir_all(parent).is_err() {
                    self.disabled = true;
                    return None;
                }
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        if let Ok(smeta) = fs::symlink_metadata(&self.path) {
            if smeta.file_type().is_symlink() {
                self.disabled = true;
                return None;
            }
        }
        let Ok(conn) = Connection::open(&self.path) else {
            self.disabled = true;
            return None;
        };
        let _ = conn.busy_timeout(Duration::from_millis(1000));
        let setup_ok = conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS ledger_index_cache (
                 cid TEXT PRIMARY KEY,
                 schema_ver INTEGER NOT NULL,
                 dev INTEGER NOT NULL,
                 ino INTEGER NOT NULL,
                 db_mtime_ns INTEGER NOT NULL,
                 db_size INTEGER NOT NULL,
                 wal_mtime_ns INTEGER NOT NULL,
                 wal_size INTEGER NOT NULL,
                 payload BLOB NOT NULL
             );",
        );
        if setup_ok.is_err() {
            self.disabled = true;
            return None;
        }
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
        self.conn = Some(conn);
        self.conn.as_ref()
    }

    pub fn load(&mut self, cid: &str, dev: u64, ino: u64) -> Option<ParsedLedgerState> {
        let conn = self.ensure_conn()?;
        let row = conn
            .prepare_cached(
                "SELECT schema_ver, dev, ino, db_mtime_ns, db_size, wal_mtime_ns, wal_size, payload
                 FROM ledger_index_cache WHERE cid = ?1",
            )
            .ok()?
            .query_row(params![cid], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                    r.get::<_, i64>(3)? as i128,
                    r.get::<_, i64>(4)? as u64,
                    r.get::<_, i64>(5)? as i128,
                    r.get::<_, i64>(6)? as u64,
                    r.get::<_, Vec<u8>>(7)?,
                ))
            })
            .optional()
            .ok()??;

        let (schema_ver, r_dev, r_ino, dm, ds, wm, ws, blob) = row;
        if schema_ver != INDEX_SCHEMA_VERSION || r_dev != dev || r_ino != ino {
            return None;
        }

        let mut decoder = ZlibDecoder::new(blob.as_slice());
        let mut json_bytes = Vec::new();
        decoder.read_to_end(&mut json_bytes).ok()?;
        let payload: PersistedPayload = serde_json::from_slice(&json_bytes).ok()?;

        let mut models = HashMap::with_capacity(payload.models.len());
        for (k, m_id, m_disp) in payload.models {
            models.insert(k, (m_id, m_disp));
        }
        let mut by_resp = HashMap::with_capacity(payload.turns.len());
        for (idx, t) in payload.turns.iter().enumerate() {
            if !t.response_id.is_empty() {
                by_resp.insert(t.response_id.clone(), idx);
            }
        }

        Some(ParsedLedgerState {
            dev: r_dev,
            ino: r_ino,
            sig: DbVersionSig {
                db_mtime_ns: dm,
                db_size: ds,
                wal_mtime_ns: wm,
                wal_size: ws,
            },
            has_step_type: payload.has_step_type,
            models,
            max_gen_idx: payload.max_gen_idx,
            max_step_idx: payload.max_step_idx,
            last_known_model: payload.last_known_model,
            saw_ckpt_boundary: payload.saw_ckpt_boundary,
            saw_turn_start: payload.saw_turn_start,
            by_resp,
            turns: Arc::new(payload.turns),
            live_probe: payload.live_probe,
        })
    }

    pub fn save(&mut self, cid: &str, state: &ParsedLedgerState) {
        let Some(conn) = self.ensure_conn() else {
            return;
        };

        let mut sorted_models: Vec<(i64, String, String)> = state
            .models
            .iter()
            .map(|(&k, (a, b))| (k, a.clone(), b.clone()))
            .collect();
        sorted_models.sort_unstable_by_key(|x| x.0);
        if sorted_models.len() > 32 {
            let keep_from = sorted_models.len() - 32;
            sorted_models.drain(..keep_from);
        }

        let payload = PersistedPayload {
            has_step_type: state.has_step_type,
            models: sorted_models,
            max_gen_idx: state.max_gen_idx,
            max_step_idx: state.max_step_idx,
            last_known_model: state.last_known_model.clone(),
            saw_ckpt_boundary: state.saw_ckpt_boundary,
            saw_turn_start: state.saw_turn_start,
            turns: (*state.turns).clone(),
            live_probe: state.live_probe.clone(),
        };

        let Ok(raw_json) = serde_json::to_vec(&payload) else {
            return;
        };
        let mut encoder =
            ZlibEncoder::new(Vec::with_capacity(raw_json.len() / 2), Compression::fast());
        if encoder.write_all(&raw_json).is_err() {
            return;
        }
        let Ok(blob) = encoder.finish() else { return };

        if let Ok(mut stmt) = conn.prepare_cached(
            "INSERT OR REPLACE INTO ledger_index_cache
             (cid, schema_ver, dev, ino, db_mtime_ns, db_size, wal_mtime_ns, wal_size, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        ) {
            let _ = stmt.execute(params![
                cid,
                INDEX_SCHEMA_VERSION,
                state.dev as i64,
                state.ino as i64,
                state.sig.db_mtime_ns as i64,
                state.sig.db_size as i64,
                state.sig.wal_mtime_ns as i64,
                state.sig.wal_size as i64,
                blob
            ]);
        }
    }

    pub fn close(&mut self) {
        self.conn = None;
    }
}
