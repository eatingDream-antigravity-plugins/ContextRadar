use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use chrono::{DateTime, Utc};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::TempDir;

use context_radar_mcp::engine::rates::RateTable;
use context_radar_mcp::engine::telemetry::{
    bucket_live_elapsed_sec, build_session_telemetry_with_probe, TelemetryEngine,
};
use context_radar_mcp::ledger::index_db::{IndexDbCache, INDEX_SCHEMA_VERSION};
use context_radar_mcp::ledger::sqlite_ro::{stat_db_stamp, LedgerReader};
use context_radar_mcp::ledger::{LiveStepProbe, TurnSample};
use context_radar_mcp::rpc::protocol::McpDaemon;
use context_radar_mcp::rpc::tools::summarize_snapshot_text;
use context_radar_mcp::settings::SettingsWatcher;
use context_radar_mcp::wire::pb_cursor::{
    decode_step_metadata, extract_checkpoint_summary_len, extract_error_summary,
    extract_step_start_ts,
};

fn encode_varint(mut val: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (val & 0x7F) as u8;
        val >>= 7;
        if val > 0 {
            out.push(b | 0x80);
        } else {
            out.push(b);
            return out;
        }
    }
}

fn pb_varint(field: u32, val: u64) -> Vec<u8> {
    let mut out = encode_varint(((field as u64) << 3) | 0);
    out.extend_from_slice(&encode_varint(val));
    out
}

fn pb_bytes(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = encode_varint(((field as u64) << 3) | 2);
    out.extend_from_slice(&encode_varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

fn pb_ts(sec: u64, nanos: u64) -> Vec<u8> {
    let mut out = pb_varint(1, sec);
    if nanos > 0 {
        out.extend_from_slice(&pb_varint(2, nanos));
    }
    out
}

fn make_gen_blob(model_id: &str, model_display: &str) -> Vec<u8> {
    let mut inner = pb_bytes(19, model_id.as_bytes());
    inner.extend_from_slice(&pb_bytes(21, model_display.as_bytes()));
    pb_bytes(1, &inner)
}

#[allow(clippy::too_many_arguments)]
fn make_step_meta_rich(
    ts_sec: u64,
    ttft_sec: u64,
    total_sec: u64,
    in_tok: u64,
    cr_tok: u64,
    cw_tok: u64,
    out_tok: u64,
    resp_tok: Option<u64>,
    think_tok: Option<u64>,
    resp_id: &str,
    gen_idx: u64,
) -> Vec<u8> {
    let mut usage = pb_varint(2, in_tok);
    usage.extend_from_slice(&pb_varint(3, out_tok));
    if cw_tok > 0 {
        usage.extend_from_slice(&pb_varint(4, cw_tok));
    }
    if cr_tok > 0 {
        usage.extend_from_slice(&pb_varint(5, cr_tok));
    }
    if let Some(r) = resp_tok {
        usage.extend_from_slice(&pb_varint(9, r));
    }
    if let Some(th) = think_tok {
        usage.extend_from_slice(&pb_varint(10, th));
    }
    usage.extend_from_slice(&pb_bytes(11, resp_id.as_bytes()));

    let mut out = pb_bytes(1, &pb_ts(ts_sec, 0));
    out.extend_from_slice(&pb_bytes(6, &pb_ts(ts_sec + ttft_sec, 0)));
    out.extend_from_slice(&pb_bytes(7, &pb_ts(ts_sec + total_sec, 0)));
    out.extend_from_slice(&pb_bytes(9, &usage));
    out.extend_from_slice(&pb_bytes(20, &pb_varint(3, gen_idx)));
    out
}

fn make_inflight_meta(ts_sec: u64, gen_idx: Option<u64>) -> Vec<u8> {
    let mut out = pb_bytes(1, &pb_ts(ts_sec, 0));
    if let Some(g) = gen_idx {
        out.extend_from_slice(&pb_bytes(20, &pb_varint(3, g)));
    }
    out
}

fn make_checkpoint_payload(summary_bytes: usize) -> Vec<u8> {
    let mut f30 = pb_varint(1, 1);
    f30.extend_from_slice(&pb_bytes(5, &vec![b'S'; summary_bytes]));
    f30.extend_from_slice(&pb_varint(11, 100));
    pb_bytes(30, &f30)
}

fn create_rich_conv_db(
    path: &Path,
    gens: &[(i64, &str, &str)],
    steps: &[(i64, i32, i32, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)],
) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE steps (
             idx INTEGER PRIMARY KEY,
             step_type INTEGER NOT NULL DEFAULT 0,
             status INTEGER NOT NULL DEFAULT 0,
             metadata BLOB,
             error_details BLOB,
             step_payload BLOB
         );
         CREATE INDEX idx_steps_status ON steps(status);
         CREATE INDEX idx_steps_step_type ON steps(step_type);",
    )
    .unwrap();
    for (gidx, mid, mdisp) in gens {
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![gidx, make_gen_blob(mid, mdisp)],
        )
        .unwrap();
    }
    for (idx, stype, status, meta, err, payload) in steps {
        conn.execute(
            "INSERT INTO steps (idx, step_type, status, metadata, error_details, step_payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![idx, stype, status, meta, err, payload],
        )
        .unwrap();
    }
}

fn create_summaries_db(path: &Path, rows: &[(&str, &str, &str, &str, i64, String)]) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE conversation_summaries (
            conversation_id TEXT PRIMARY KEY,
            title TEXT,
            preview TEXT,
            workspace_uris TEXT,
            status TEXT,
            agent_name TEXT,
            parent_conversation_id TEXT,
            nesting_depth INTEGER,
            not_fully_idle INTEGER,
            killed INTEGER,
            last_modified_time TEXT,
            last_user_input_step_index INTEGER
        );",
    )
    .unwrap();
    for (cid, title, ws, status, not_idle, mod_time) in rows {
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1,?2,'',?3,?4,'','',0,?5,0,?6,1)",
            params![cid, title, ws, status, not_idle, mod_time],
        )
        .unwrap();
    }
}

fn iso(ts: f64) -> String {
    DateTime::<Utc>::from_timestamp(ts as i64, 0)
        .unwrap()
        .to_rfc3339()
}

#[test]
fn test_pb_thinking_response_split_legacy_fallback_and_start_ts() {
    // Rich usage with field 9 (response) and field 10 (thinking)
    let rich_meta = make_step_meta_rich(
        1_700_000_100,
        1,
        5,
        10_000,
        90_000,
        0,
        2_168,
        Some(140),
        Some(2_028),
        "resp-rich",
        0,
    );
    let dec = decode_step_metadata(&rich_meta).expect("decoded rich step metadata");
    assert_eq!(dec.output, 2_168);
    assert_eq!(dec.response_tokens, 140);
    assert_eq!(dec.thinking_tokens, 2_028);

    // Legacy usage without fields 9 & 10 -> fallback response_tokens = output, thinking_tokens = 0
    let legacy_meta = make_step_meta_rich(
        1_700_000_200,
        1,
        4,
        8_000,
        50_000,
        0,
        640,
        None,
        None,
        "resp-legacy",
        0,
    );
    let dec_leg = decode_step_metadata(&legacy_meta).expect("decoded legacy step metadata");
    assert_eq!(dec_leg.output, 640);
    assert_eq!(dec_leg.response_tokens, 640);
    assert_eq!(dec_leg.thinking_tokens, 0);

    // In-flight metadata without usage field -> decode_step_metadata returns None, extract_step_start_ts succeeds
    let inflight = make_inflight_meta(1_700_000_333, Some(2));
    assert!(decode_step_metadata(&inflight).is_none());
    assert!((extract_step_start_ts(&inflight) - 1_700_000_333.0).abs() < 1e-6);
    assert_eq!(extract_step_start_ts(&[]), 0.0);
}

#[test]
fn test_pb_checkpoint_summary_len_and_error_summary_sanitization() {
    // Checkpoint payload field 30 -> inner field 5
    let ckpt = make_checkpoint_payload(22_418);
    assert_eq!(extract_checkpoint_summary_len(&ckpt), 22_418);
    assert_eq!(extract_checkpoint_summary_len(&[]), 0);
    assert_eq!(extract_checkpoint_summary_len(&[0xFF, 0xFF, 0x01]), 0);

    // Flat protobuf error_details (field 2 & 3)
    let mut flat_err = pb_bytes(
        2,
        b"HTTP 429\n\tMODEL_CAPACITY_EXHAUSTED for /Users/exampleuser/project/src/main.rs with Bearer eyJhbGciOiJIUzI1NiJ9.secret",
    );
    flat_err.extend_from_slice(&pb_bytes(3, b"duplicate message"));
    let summary = extract_error_summary(&flat_err);
    assert!(summary.contains("HTTP 429 MODEL_CAPACITY_EXHAUSTED"), "got: {summary}");
    assert!(summary.contains("~/project/src/main.rs"), "path should be redacted: {summary}");
    assert!(!summary.contains("exampleuser"), "username leaked: {summary}");
    assert!(!summary.contains("eyJhbGci"), "bearer token leaked: {summary}");
    assert!(summary.contains("[REDACTED]"), "got: {summary}");

    // Nested protobuf error_details (field 1 -> field 2 -> UTF-8 string)
    let nested_inner = pb_bytes(2, b"Upstream deadline exceeded after 60s");
    let nested_outer = pb_bytes(1, &nested_inner);
    assert_eq!(
        extract_error_summary(&nested_outer),
        "Upstream deadline exceeded after 60s"
    );

    // Fallback plain UTF-8 multiline + truncation to <= 140 Unicode chars
    let long_utf8 = format!("\n  {}  \nsecond line", "错误详情".repeat(50));
    let trunc = extract_error_summary(long_utf8.as_bytes());
    assert!(trunc.chars().count() <= 140, "len={}", trunc.chars().count());
    assert!(trunc.ends_with('…'));

    // Pure binary garbage returns empty string
    assert_eq!(extract_error_summary(&[0x00, 0x01, 0x02, 0xFF, 0xFE]), "");
}

#[test]
fn test_four_state_live_classifier_cross_model_compact_and_last_loop_rollup() {
    assert_eq!(bucket_live_elapsed_sec(0), 0);
    assert_eq!(bucket_live_elapsed_sec(4), 0);
    assert_eq!(bucket_live_elapsed_sec(5), 5);
    assert_eq!(bucket_live_elapsed_sec(44), 40);
    assert_eq!(bucket_live_elapsed_sec(45), 45);
    assert_eq!(bucket_live_elapsed_sec(59), 55);
    assert_eq!(bucket_live_elapsed_sec(60), 60);
    assert_eq!(bucket_live_elapsed_sec(74), 70);

    let td = TempDir::new().unwrap();
    let conv_dir = td.path().join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    let now_ts = 1_750_000_500.0;

    // 1. Session A: active LLM step, plus multi-turn history with thinking/response split & loop boundary
    let cid_a = "aa000000-0000-0000-0000-000000000001";
    create_rich_conv_db(
        &conv_dir.join(format!("{cid_a}.db")),
        &[(0, "gemini-4-neon-high", "Gemini 4 Neon (High)")],
        &[
            (1, 14, 3, None, None, None), // user input 1
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    1_750_000_100,
                    1,
                    5,
                    10_000,
                    40_000,
                    0,
                    1_000,
                    Some(300),
                    Some(700),
                    "r1",
                    0,
                )),
                None,
                None,
            ),
            (3, 14, 3, None, None, None), // user input 2 -> starts current loop!
            (
                4,
                15,
                3,
                Some(make_step_meta_rich(
                    1_750_000_200,
                    2,
                    6,
                    12_000,
                    88_000,
                    0,
                    1_200,
                    Some(400),
                    Some(800),
                    "r2",
                    0,
                )),
                None,
                None,
            ),
            (
                5,
                15,
                3,
                Some(make_step_meta_rich(
                    1_750_000_300,
                    1,
                    4,
                    8_000,
                    102_000,
                    0,
                    800,
                    Some(200),
                    Some(600),
                    "r3",
                    0,
                )),
                None,
                None,
            ),
            // In-flight LLM step started at now - 22s (should be "running", elapsed=20s)
            (
                6,
                15,
                2,
                Some(make_inflight_meta(now_ts as u64 - 22, Some(0))),
                None,
                None,
            ),
        ],
    );

    // 2. Session B: cross-model compacting in-flight (previous turn Carbon, compacting with Neon, elapsed only 10s -> "waiting"!)
    let cid_b = "bb000000-0000-0000-0000-000000000002";
    create_rich_conv_db(
        &conv_dir.join(format!("{cid_b}.db")),
        &[
            (0, "gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
            (1, "gemini-4-neon-high", "Gemini 4 Neon (High)"),
        ],
        &[
            (1, 14, 3, None, None, None),
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    1_750_000_100,
                    2,
                    6,
                    20_000,
                    200_000,
                    0,
                    1_000,
                    Some(400),
                    Some(600),
                    "rb1",
                    0,
                )),
                None,
                None,
            ),
            (
                3,
                23,
                2,
                Some(make_inflight_meta(now_ts as u64 - 12, Some(1))),
                None,
                None,
            ),
        ],
    );

    // 3. Session C: terminal error step (status = 7) with error_details
    let cid_c = "cc000000-0000-0000-0000-000000000003";
    create_rich_conv_db(
        &conv_dir.join(format!("{cid_c}.db")),
        &[(0, "gemini-4-hydrogen-high", "Gemini 4 Hydrogen (High)")],
        &[
            (1, 14, 3, None, None, None),
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    1_750_000_100,
                    1,
                    3,
                    5_000,
                    15_000,
                    0,
                    500,
                    Some(500),
                    Some(0),
                    "rc1",
                    0,
                )),
                None,
                None,
            ),
            (
                3,
                15,
                7,
                None,
                Some(pb_bytes(2, b"Rate limit exceeded (429)")),
                None,
            ),
        ],
    );

    // 4. Session D: just_compacted (completed checkpoint with 22418B summary + cross-model) -> "ready"
    let cid_d = "dd000000-0000-0000-0000-000000000004";
    create_rich_conv_db(
        &conv_dir.join(format!("{cid_d}.db")),
        &[
            (0, "gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
            (1, "gemini-4-neon-high", "Gemini 4 Neon (High)"),
        ],
        &[
            (1, 14, 3, None, None, None),
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    1_750_000_100,
                    1,
                    5,
                    15_000,
                    225_000,
                    0,
                    500,
                    Some(200),
                    Some(300),
                    "rd1",
                    0,
                )),
                None,
                None,
            ),
            (
                3,
                23,
                3,
                Some(make_step_meta_rich(
                    1_750_000_200,
                    2,
                    10,
                    20_000,
                    220_000,
                    0,
                    5_000,
                    Some(5_000),
                    Some(0),
                    "rd2",
                    1,
                )),
                None,
                Some(make_checkpoint_payload(22_418)),
            ),
        ],
    );

    create_summaries_db(
        &td.path().join("conversation_summaries.db"),
        &[
            (cid_a, "Session A", "file:///ws/app", "CASCADE_RUN_STATUS_RUNNING", 1, iso(now_ts)),
            (cid_b, "Session B", "file:///ws/app", "CASCADE_RUN_STATUS_RUNNING", 1, iso(now_ts - 5.0)),
            (cid_c, "Session C", "file:///ws/app", "CASCADE_RUN_STATUS_IDLE", 0, iso(now_ts - 10.0)),
            (cid_d, "Session D", "file:///ws/app", "CASCADE_RUN_STATUS_IDLE", 0, iso(now_ts - 15.0)),
        ],
    );

    let mut engine = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(None),
    );
    let snap = engine.build_overview_snapshot(25, false, false, "", Some(now_ts));
    assert_eq!(snap.summary.compacting_count, 1);
    assert_eq!(snap.summary.error_session_count, 1);

    let by_id = |id: &str| snap.sessions.iter().find(|s| s.id == id).unwrap().clone();
    let sa = by_id(cid_a);
    assert_eq!(sa.live_status, "running");
    assert_eq!(sa.live_phase, "llm");
    assert_eq!(sa.live_elapsed_sec, 20);
    assert!(sa.live_error.is_empty());
    assert_eq!(sa.latest_context_tokens, 110_000);
    assert_eq!(sa.remaining_context_tokens, sa.context_limit - 110_000);
    assert_eq!(sa.total_output_tokens, 3_000);
    assert_eq!(sa.total_thinking_tokens, 2_100);
    assert_eq!(sa.total_response_tokens, 900);
    assert!((sa.thinking_share_pct - 70.0).abs() < 0.01);
    assert_eq!(sa.latest_ttft_ms, 1_000);
    // Latest turn (r3): out=800, total=4000ms, ttft=1000ms -> stream_tps = 800 / 3.0 = 266.7
    assert!((sa.latest_stream_tps - 266.7).abs() < 0.1);
    // e2e_tps across r1 (1000/5=200), r2 (1200/6=200), r3 (800/4=200) -> median 200.0
    assert!((sa.e2e_tps - 200.0).abs() < 0.1);
    // Last loop = r2 + r3 (2 steps, user input 3 triggered after_turn_start=Some(true) on r2)
    assert_eq!(sa.last_loop_steps, 2);
    assert_eq!(sa.last_loop_output_tokens, 2_000);
    assert_eq!(sa.last_loop_thinking_tokens, 1_400);
    assert!(sa.last_loop_cost_usd > 0.0 && sa.last_loop_cost_usd < sa.cost_usd);
    assert!(sa.last_loop_cost_cny > 0.0 && sa.last_loop_cost_cny < sa.cost_cny);

    // Advance time to +45s WITHOUT touching DB -> metrics_cache MUST transition Session A to "waiting"!
    let snap_later = engine.build_overview_snapshot(25, false, false, "", Some(now_ts + 26.0));
    let sa_later = snap_later.sessions.iter().find(|s| s.id == cid_a).unwrap();
    assert_eq!(sa_later.live_elapsed_sec, 45);
    assert_eq!(sa_later.live_status, "waiting");

    let sb = by_id(cid_b);
    assert_eq!(sb.live_phase, "compacting");
    assert!(sb.cross_model_compact);
    assert_eq!(sb.live_status, "waiting");
    assert_eq!(sb.live_elapsed_sec, 10);

    let sc = by_id(cid_c);
    assert_eq!(sc.live_status, "error");
    assert_eq!(sc.live_error, "Rate limit exceeded (429)");

    let sd = by_id(cid_d);
    assert_eq!(sd.live_status, "ready");
    assert!(sd.just_compacted);
    assert!(sd.cross_model_compact);
    assert!((sd.compact_summary_kb - 21.9).abs() < 0.01);

    // Check TurnViewRow fields on context_radar_turns
    let turns_payload = engine.get_session_turns(cid_a, 10);
    assert_eq!(turns_payload.turns.len(), 3);
    assert_eq!(turns_payload.turns[0].thinking_tokens, 600);
    assert_eq!(turns_payload.turns[0].response_tokens, 200);
    assert!((turns_payload.turns[0].e2e_tps - 200.0).abs() < 0.1);

    // Check bilingual summarize_snapshot_text (EN & zh-CN)
    let en_txt = summarize_snapshot_text(&snap);
    assert!(en_txt.contains("1 compacting") && en_txt.contains("1 error"), "en: {en_txt}");
    assert!(en_txt.contains("[compacting 10s·cross-model]"), "en: {en_txt}");
    assert!(en_txt.contains("[error: Rate limit exceeded (429)]"), "en: {en_txt}");
    assert!(en_txt.contains("think 70%"), "en: {en_txt}");
    assert!(en_txt.contains("left)"), "en: {en_txt}");

    let mut zh_snap = snap.clone();
    zh_snap.lang = "zh-CN".to_string();
    let zh_txt = summarize_snapshot_text(&zh_snap);
    assert!(zh_txt.contains("压缩中 1 个") && zh_txt.contains("异常 1 个"), "zh: {zh_txt}");
    assert!(zh_txt.contains("[压缩中 10s·跨模型]"), "zh: {zh_txt}");
    assert!(zh_txt.contains("[异常: Rate limit exceeded (429)]"), "zh: {zh_txt}");
    assert!(zh_txt.contains("思考 70%"), "zh: {zh_txt}");
    assert!(zh_txt.contains("余 "), "zh: {zh_txt}");

    // Verify tool phase & error suppression when newer active step runs
    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
    let sample_turns = vec![TurnSample {
        turn_index: 1,
        step_idx: 2,
        timestamp: 100.0,
        model: "gemini-4-neon-high".into(),
        model_display: "Gemini 4 Neon (High)".into(),
        input_tokens: 1_000,
        cache_read: 9_000,
        cache_write: 0,
        output_tokens: 200,
        thinking_tokens: 100,
        response_tokens: 100,
        ttft_ms: 500,
        total_ms: 2_000,
        response_id: "t1".into(),
        step_type: 15,
        is_checkpoint: false,
        after_turn_start: Some(true),
        after_checkpoint: false,
    }];
    let superseded_probe = LiveStepProbe {
        live_phase: "tool".into(),
        live_started_ts: 190.0,
        live_active_step_idx: 5,
        live_step_waiting: false,
        live_model: String::new(),
        live_error: "Old transient error".into(),
        live_error_step_idx: 4,
        compact_summary_bytes: 0,
    };
    let tel_sup = build_session_telemetry_with_probe(
        "ee000000-0000-0000-0000-000000000005",
        None,
        &sample_turns,
        200.0,
        7.2,
        &mut rates,
        202.0,
        None,
        None,
        Some(&superseded_probe),
    );
    assert_eq!(tel_sup.live_status, "running");
    assert_eq!(tel_sup.live_phase, "tool");
    assert_eq!(tel_sup.live_elapsed_sec, 10);
    assert!(tel_sup.live_error.is_empty());
}

#[test]
fn test_schema_v1_to_v2_index_cache_migration_and_mcp_json_contract() {
    assert_eq!(INDEX_SCHEMA_VERSION, 2);

    let td = TempDir::new().unwrap();
    let conv_dir = td.path().join("conversations");
    let cid = "ff000000-0000-0000-0000-000000000009";
    let db_path = conv_dir.join(format!("{cid}.db"));
    let now_ts = 1_750_001_000.0;

    create_rich_conv_db(
        &db_path,
        &[(0, "gemini-4-neon-high", "Gemini 4 Neon (High)")],
        &[
            (1, 14, 3, None, None, None),
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    1_750_000_900,
                    1,
                    5,
                    10_000,
                    50_000,
                    0,
                    1_000,
                    Some(250),
                    Some(750),
                    "rmig",
                    0,
                )),
                None,
                None,
            ),
        ],
    );
    create_summaries_db(
        &td.path().join("conversation_summaries.db"),
        &[(cid, "Migrate Chat", "file:///ws/mig", "CASCADE_RUN_STATUS_IDLE", 0, iso(now_ts))],
    );

    // Seed a schema v1 cache entry with matching dev/ino/sig but zero thinking_tokens
    let cache_path = td.path().join("plugin_data").join("index_cache.sqlite");
    fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    let stamp = stat_db_stamp(&db_path).unwrap();
    let meta = fs::metadata(&db_path).unwrap();
    {
        let conn = Connection::open(&cache_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE ledger_index_cache (
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
        )
        .unwrap();
        let stale_payload = json!({
            "has_step_type": true,
            "models": [[0, "gemini-4-neon-high", "Gemini 4 Neon (High)"]],
            "max_gen_idx": 0,
            "max_step_idx": 2,
            "last_known_model": ["gemini-4-neon-high", "Gemini 4 Neon (High)"],
            "saw_ckpt_boundary": false,
            "saw_turn_start": false,
            "turns": [{
                "turn_index": 1,
                "step_idx": 2,
                "timestamp": 1750000900.0,
                "model": "gemini-4-neon-high",
                "model_display": "Gemini 4 Neon (High)",
                "input_tokens": 10000,
                "cache_read": 50000,
                "cache_write": 0,
                "output_tokens": 1000,
                "ttft_ms": 1000,
                "total_ms": 5000,
                "response_id": "rmig",
                "step_type": 15,
                "is_checkpoint": false,
                "after_turn_start": true,
                "after_checkpoint": false
            }]
        });
        let raw = serde_json::to_vec(&stale_payload).unwrap();
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&raw).unwrap();
        let blob = enc.finish().unwrap();
        conn.execute(
            "INSERT INTO ledger_index_cache VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                cid,
                meta.dev() as i64,
                meta.ino() as i64,
                stamp.sig.db_mtime_ns as i64,
                stamp.sig.db_size as i64,
                stamp.sig.wal_mtime_ns as i64,
                stamp.sig.wal_size as i64,
                blob
            ],
        )
        .unwrap();
    }

    // Direct v1 load must miss cleanly
    let mut cache_check = IndexDbCache::new(cache_path.clone());
    assert!(cache_check.load(cid, stamp.dev, stamp.ino).is_none());
    cache_check.close();

    // LedgerReader re-parses and upgrades cache row to v2 with thinking/response split!
    let mut reader = LedgerReader::new(td.path().to_path_buf(), Some(cache_path.clone()));
    let (turns, _) = reader.read_turns_and_probe(cid, Some(&stamp));
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].thinking_tokens, 750);
    assert_eq!(turns[0].response_tokens, 250);
    reader.close();

    let conn = Connection::open(&cache_path).unwrap();
    let ver: i64 = conn
        .query_row(
            "SELECT schema_ver FROM ledger_index_cache WHERE cid = ?1",
            params![cid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ver, 2);

    // Verify full MCP daemon JSON contract for both overview and turns tools
    let cfg_path = td.path().join("settings.json");
    fs::write(&cfg_path, r#"{"lang":"en"}"#).unwrap();
    let rates = RateTable::new(
        SettingsWatcher::new(Some(cfg_path.to_string_lossy().into_owned())),
        None,
        None,
    );
    let engine = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(rates),
        Some(Some(cache_path)),
    );
    let mut daemon = McpDaemon::new(Some(engine), None, None);

    let ov_resp = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    let sc: &Value = &ov_resp["result"]["structuredContent"];
    assert_eq!(sc["summary"]["compacting_count"], 0);
    assert_eq!(sc["summary"]["error_session_count"], 0);
    let sess = &sc["sessions"][0];
    for key in [
        "live_status",
        "live_phase",
        "live_elapsed_sec",
        "cross_model_compact",
        "compact_summary_kb",
        "remaining_context_tokens",
        "total_thinking_tokens",
        "total_response_tokens",
        "thinking_share_pct",
        "latest_ttft_ms",
        "latest_stream_tps",
        "e2e_tps",
        "last_loop_steps",
        "last_loop_output_tokens",
        "last_loop_thinking_tokens",
        "last_loop_cost_usd",
        "last_loop_cost_cny",
    ] {
        assert!(sess.get(key).is_some(), "missing session JSON field: {key}");
    }
    assert!(sess.get("live_error").is_none(), "empty live_error should be omitted");

    let turns_resp = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "context_radar_turns", "arguments": { "conversation_id": cid, "limit": 5 } }
        }))
        .unwrap();
    let t_text = turns_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(t_text.contains("left") && t_text.contains("think 75%"), "got: {t_text}");
    let t0 = &turns_resp["result"]["structuredContent"]["turns"][0];
    assert_eq!(t0["thinking_tokens"], 750);
    assert_eq!(t0["response_tokens"], 250);
    assert_eq!(t0["e2e_tps"], 200.0);
}
