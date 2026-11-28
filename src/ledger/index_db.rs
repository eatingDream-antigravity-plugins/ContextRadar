//! Optional persistent SQLite parse index cache stored strictly under plugin_data/context-radar-mcp/.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::ledger::{LiveStepProbe, TurnSample};
use crate::settings::{current_euid, UNIX_O_NOFOLLOW, UNIX_O_NONBLOCK};

// Older versions can contain cursors saved after incomplete host reads or lack trajectory_type metadata.
pub const INDEX_SCHEMA_VERSION: i64 = 4;
pub const MAX_CACHE_BLOB_BYTES: u64 = 16 * 1024 * 1024;

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

fn sidecar_path(base: &Path, suffix: &str) -> PathBuf {
    let mut os = base.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn harden_sidecar(path: &Path, euid: u32) -> bool {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            if m.file_type().is_symlink() || !m.file_type().is_file() || m.uid() != euid {
                return false;
            }
            if (m.permissions().mode() & 0o777) != 0o600 {
                return fs::set_permissions(path, fs::Permissions::from_mode(0o600)).is_ok();
            }
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn harden_wal_sidecars(db_path: &Path, euid: u32) -> bool {
    harden_sidecar(&sidecar_path(db_path, "-wal"), euid)
        && harden_sidecar(&sidecar_path(db_path, "-shm"), euid)
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
        let euid = current_euid();
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Ok(pmeta) = fs::symlink_metadata(parent) {
                    if pmeta.file_type().is_symlink() || !pmeta.file_type().is_dir() {
                        self.disabled = true;
                        return None;
                    }
                }
                if fs::create_dir_all(parent).is_err() {
                    self.disabled = true;
                    return None;
                }
                let Ok(pmeta) = fs::symlink_metadata(parent) else {
                    self.disabled = true;
                    return None;
                };
                if pmeta.file_type().is_symlink()
                    || !pmeta.file_type().is_dir()
                    || pmeta.uid() != euid
                {
                    self.disabled = true;
                    return None;
                }
                if fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).is_err() {
                    self.disabled = true;
                    return None;
                }
            }
        }
        // Pre-create or open atomically with O_NOFOLLOW + 0600 before SQLite touches the path.
        let pre_file = match OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(UNIX_O_NOFOLLOW | UNIX_O_NONBLOCK)
            .open(&self.path)
        {
            Ok(f) => f,
            Err(_) => {
                self.disabled = true;
                return None;
            }
        };
        let Ok(fmeta) = pre_file.metadata() else {
            self.disabled = true;
            return None;
        };
        if !fmeta.file_type().is_file() || fmeta.uid() != euid {
            self.disabled = true;
            return None;
        }
        if (fmeta.permissions().mode() & 0o777) != 0o600
            && pre_file
                .set_permissions(fs::Permissions::from_mode(0o600))
                .is_err()
        {
            self.disabled = true;
            return None;
        }
        drop(pre_file);

        if !harden_wal_sidecars(&self.path, euid) {
            self.disabled = true;
            return None;
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
        if setup_ok.is_err() || !harden_wal_sidecars(&self.path, euid) {
            self.disabled = true;
            return None;
        }
        if fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).is_err() {
            self.disabled = true;
            return None;
        }
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

        let decoder = ZlibDecoder::new(blob.as_slice());
        let mut json_bytes = Vec::new();
        decoder
            .take(MAX_CACHE_BLOB_BYTES + 1)
            .read_to_end(&mut json_bytes)
            .ok()?;
        if (json_bytes.len() as u64) > MAX_CACHE_BLOB_BYTES {
            return None;
        }
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
        if (raw_json.len() as u64) > MAX_CACHE_BLOB_BYTES {
            return;
        }
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
        let _ = harden_wal_sidecars(&self.path, current_euid());
    }

    pub fn close(&mut self) {
        self.conn = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn index_db_rejects_zlib_bomb_and_enforces_0600_and_symlink_guards() {
        let td = tempfile::tempdir().unwrap();
        let sub = td.path().join("cache_dir");
        let db_path = sub.join("index_cache.sqlite");
        let mut cache = IndexDbCache::new(db_path.clone());

        let state = ParsedLedgerState {
            dev: 1,
            ino: 2,
            sig: DbVersionSig {
                db_mtime_ns: 10,
                db_size: 20,
                wal_mtime_ns: 30,
                wal_size: 40,
            },
            has_step_type: true,
            models: HashMap::new(),
            max_gen_idx: 1,
            max_step_idx: 2,
            last_known_model: ("m".into(), "M".into()),
            saw_ckpt_boundary: false,
            saw_turn_start: true,
            by_resp: HashMap::new(),
            turns: Arc::new(Vec::new()),
            live_probe: LiveStepProbe::default(),
        };
        cache.save("cid-ok", &state);
        assert!(cache.load("cid-ok", 1, 2).is_some());
        cache.close();

        // Directory is 0700 and sqlite + wal/shm sidecars are 0600
        assert_eq!(
            fs::metadata(&sub).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&db_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for suffix in ["-wal", "-shm"] {
            let p = sidecar_path(&db_path, suffix);
            if p.exists() {
                assert_eq!(
                    fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }

        // Inject a 17 MiB Zlib bomb into the SQLite cache row and ensure load() rejects without OOM
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
        let zeros = vec![b' '; (MAX_CACHE_BLOB_BYTES as usize) + 1024 * 1024];
        enc.write_all(&zeros).unwrap();
        let bomb_blob = enc.finish().unwrap();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO ledger_index_cache
                 (cid, schema_ver, dev, ino, db_mtime_ns, db_size, wal_mtime_ns, wal_size, payload)
                 VALUES (?1, ?2, 1, 2, 10, 20, 30, 40, ?3)",
                params!["cid-bomb", INDEX_SCHEMA_VERSION, bomb_blob],
            )
            .unwrap();
        }
        let mut reader = IndexDbCache::new(db_path.clone());
        assert!(reader.load("cid-bomb", 1, 2).is_none());
        reader.close();

        // Symlink parent is rejected
        let real_dir = td.path().join("real_dir");
        fs::create_dir_all(&real_dir).unwrap();
        let sym_dir = td.path().join("sym_dir");
        symlink(&real_dir, &sym_dir).unwrap();
        let mut sym_parent_cache = IndexDbCache::new(sym_dir.join("index_cache.sqlite"));
        assert!(sym_parent_cache.ensure_conn().is_none());

        // Symlink db file is rejected
        let real_file = td.path().join("target.sqlite");
        fs::write(&real_file, b"").unwrap();
        let sym_file = td.path().join("link.sqlite");
        symlink(&real_file, &sym_file).unwrap();
        let mut sym_file_cache = IndexDbCache::new(sym_file);
        assert!(sym_file_cache.ensure_conn().is_none());
    }
}
