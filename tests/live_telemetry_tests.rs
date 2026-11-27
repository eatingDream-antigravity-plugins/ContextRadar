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
    let mut out = encode_varint((field as u64) << 3);
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

type RichStep = (
    i64,
    i32,
    i32,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

fn create_rich_conv_db(path: &Path, gens: &[(i64, &str, &str)], steps: &[RichStep]) {
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
fn test_pb_rich_step_checkpoint_and_error_decoding() {
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

    // Checkpoint payload field 30 -> inner field 5
    let ckpt = make_checkpoint_payload(22_418);
    assert_eq!(extract_checkpoint_summary_len(&ckpt), 22_418);
    assert_eq!(extract_checkpoint_summary_len(&[]), 0);
    assert_eq!(extract_checkpoint_summary_len(&[0xFF, 0xFF, 0x01]), 0);

    // Flat protobuf error_details (field 2 & 3)
    let mut flat_err = pb_bytes(
        2,
        concat!(
            "HTTP 429\n\tMODEL_CAPACITY_EXHAUSTED for /Us",
            "ers/exampleuser/project/src/main.rs with Bearer eyJhbGciOiJIUzI1NiJ9.secret"
        )
        .as_bytes(),
    );
    flat_err.extend_from_slice(&pb_bytes(3, b"duplicate message"));
    let summary = extract_error_summary(&flat_err);
    assert!(
        summary.contains("HTTP 429 MODEL_CAPACITY_EXHAUSTED"),
        "got: {summary}"
    );
    assert!(
        summary.contains("~/project/src/main.rs"),
        "path should be redacted: {summary}"
    );
    assert!(
        !summary.contains("exampleuser"),
        "username leaked: {summary}"
    );
    assert!(
        !summary.contains("eyJhbGci"),
        "bearer token leaked: {summary}"
    );
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
    assert!(
        trunc.chars().count() <= 140,
        "len={}",
        trunc.chars().count()
    );
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
        &[(0, "gemini-4-neon-high", "Gemini 4 Standard (High)")],
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

    // 2. Session B: cross-model compacting in-flight (previous turn Flagship, compacting with Standard, elapsed only 10s -> "waiting"!)
    let cid_b = "bb000000-0000-0000-0000-000000000002";
    create_rich_conv_db(
        &conv_dir.join(format!("{cid_b}.db")),
        &[
            (0, "gemini-4-carbon-high", "Gemini 4 Flagship (High)"),
            (1, "gemini-4-neon-high", "Gemini 4 Standard (High)"),
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
        &[(0, "gemini-4-hydrogen-high", "Gemini 4 Fast (High)")],
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
            (0, "gemini-4-carbon-high", "Gemini 4 Flagship (High)"),
            (1, "gemini-4-neon-high", "Gemini 4 Standard (High)"),
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
            (
                cid_a,
                "Session A",
                "file:///ws/app",
                "CASCADE_RUN_STATUS_RUNNING",
                1,
                iso(now_ts),
            ),
            (
                cid_b,
                "Session B",
                "file:///ws/app",
                "CASCADE_RUN_STATUS_RUNNING",
                1,
                iso(now_ts - 5.0),
            ),
            (
                cid_c,
                "Session C",
                "file:///ws/app",
                "CASCADE_RUN_STATUS_IDLE",
                0,
                iso(now_ts - 10.0),
            ),
            (
                cid_d,
                "Session D",
                "file:///ws/app",
                "CASCADE_RUN_STATUS_IDLE",
                0,
                iso(now_ts - 15.0),
            ),
        ],
    );

    let mut engine = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
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
    // Latest turn (r3): thinking=600, response=200, total=4000ms, ttft=1000ms -> post-TTFT stream_tps = 200 / 3.0 = 66.7
    assert!((sa.latest_stream_tps - 66.7).abs() < 0.1);
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
    assert!(
        en_txt.contains("1 compacting") && en_txt.contains("1 error"),
        "en: {en_txt}"
    );
    assert!(
        en_txt.contains("[compacting 10s · new model]"),
        "en: {en_txt}"
    );
    assert!(
        en_txt.contains("[error: Rate limit exceeded (429)]"),
        "en: {en_txt}"
    );
    assert!(en_txt.contains("think 70%"), "en: {en_txt}");
    assert!(en_txt.contains("left)"), "en: {en_txt}");

    let mut zh_snap = snap.clone();
    zh_snap.lang = "zh-CN".to_string();
    let zh_txt = summarize_snapshot_text(&zh_snap);
    assert!(
        zh_txt.contains("压缩中 1 个") && zh_txt.contains("出错 1 个"),
        "zh: {zh_txt}"
    );
    assert!(zh_txt.contains("[压缩中 10 秒 · 换模型]"), "zh: {zh_txt}");
    assert!(
        zh_txt.contains("[出错：Rate limit exceeded (429)]"),
        "zh: {zh_txt}"
    );
    assert!(zh_txt.contains("思考 70%"), "zh: {zh_txt}");
    assert!(zh_txt.contains("余 "), "zh: {zh_txt}");

    // Verify tool phase & error suppression when newer active step runs
    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
    let sample_turns = vec![TurnSample {
        turn_index: 1,
        step_idx: 2,
        timestamp: 100.0,
        model: "gemini-4-neon-high".into(),
        model_display: "Gemini 4 Standard (High)".into(),
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
        is_side_question: false,
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
fn test_old_index_cache_versions_are_rebuilt_and_preserve_mcp_json_contract() {
    assert_eq!(INDEX_SCHEMA_VERSION, 4);

    let td = TempDir::new().unwrap();
    let conv_dir = td.path().join("conversations");
    let cid = "ff000000-0000-0000-0000-000000000009";
    let db_path = conv_dir.join(format!("{cid}.db"));
    let now_ts = 1_750_001_000.0;

    create_rich_conv_db(
        &db_path,
        &[(0, "gemini-4-neon-high", "Gemini 4 Standard (High)")],
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
        &[(
            cid,
            "Migrate Chat",
            "file:///ws/mig",
            "CASCADE_RUN_STATUS_IDLE",
            0,
            iso(now_ts),
        )],
    );

    // Seed an older cache entry with a matching stamp and a cursor beyond its cached rows.
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
            "models": [[0, "gemini-4-neon-high", "Gemini 4 Standard (High)"]],
            "max_gen_idx": 0,
            "max_step_idx": 2,
            "last_known_model": ["gemini-4-neon-high", "Gemini 4 Standard (High)"],
            "saw_ckpt_boundary": false,
            "saw_turn_start": false,
            "turns": []
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

    let mut cache_check = IndexDbCache::new(cache_path.clone());
    assert!(cache_check.load(cid, stamp.dev, stamp.ino).is_none());
    cache_check.close();
    {
        let conn = Connection::open(&cache_path).unwrap();
        conn.execute(
            "UPDATE ledger_index_cache SET schema_ver = 2 WHERE cid = ?1",
            params![cid],
        )
        .unwrap();
    }
    assert!(cache_check.load(cid, stamp.dev, stamp.ino).is_none());
    cache_check.close();
    {
        let conn = Connection::open(&cache_path).unwrap();
        conn.execute(
            "UPDATE ledger_index_cache SET schema_ver = 3 WHERE cid = ?1",
            params![cid],
        )
        .unwrap();
    }
    assert!(cache_check.load(cid, stamp.dev, stamp.ino).is_none());
    cache_check.close();

    // Rebuild even when the old row has a matching stamp and a decodable payload.
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
    assert_eq!(ver, INDEX_SCHEMA_VERSION);

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
    assert!(
        sess.get("live_error").is_none(),
        "empty live_error should be omitted"
    );

    let turns_resp = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "context_radar_turns", "arguments": { "conversation_id": cid, "limit": 5 } }
        }))
        .unwrap();
    let t_text = turns_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        t_text.contains("left") && t_text.contains("think 75%"),
        "got: {t_text}"
    );
    let t0 = &turns_resp["result"]["structuredContent"]["turns"][0];
    assert_eq!(t0["thinking_tokens"], 750);
    assert_eq!(t0["response_tokens"], 250);
    assert_eq!(t0["e2e_tps"], 200.0);
}

#[test]
fn test_stream_tps_accounts_for_thinking_time_inside_ttft() {
    use context_radar_mcp::ledger::TurnSample;

    // 1) Reasoning model where Antigravity ttft_ms spans prefill + thinking (109ms post-TTFT for 31 response tokens)
    let reasoning = TurnSample {
        step_idx: 1,
        turn_index: 1,
        timestamp: 1000.0,
        model: "gemini-4-neon".into(),
        model_display: "Gemini 4 Standard".into(),
        input_tokens: 3000,
        cache_read: 100_000,
        cache_write: 0,
        output_tokens: 226,
        thinking_tokens: 195,
        response_tokens: 31,
        ttft_ms: 6566,
        total_ms: 6675,
        response_id: "r1".into(),
        step_type: 15,
        is_checkpoint: false,
        after_turn_start: None,
        after_checkpoint: false,
    };
    // Must compute 31 / 0.109 = 284.4 t/s, NOT 226 / 0.109 = 2073.4 t/s
    assert!(
        (reasoning.stream_tps() - 284.4).abs() < 0.2,
        "got {}",
        reasoning.stream_tps()
    );
    assert!(
        (reasoning.e2e_tps() - 33.86).abs() < 0.1,
        "got {}",
        reasoning.e2e_tps()
    );

    // 2) Pure thinking turn (0 response tokens) falls back to E2E throughput
    let pure_think = TurnSample {
        output_tokens: 700,
        thinking_tokens: 700,
        response_tokens: 0,
        ttft_ms: 6900,
        total_ms: 7000,
        ..reasoning.clone()
    };
    assert!(
        (pure_think.stream_tps() - 100.0).abs() < 0.1,
        "got {}",
        pure_think.stream_tps()
    );

    // 3) Legacy non-thinking turn uses full output_tokens / (total_ms - ttft_ms)
    let legacy = TurnSample {
        output_tokens: 600,
        thinking_tokens: 0,
        response_tokens: 0,
        ttft_ms: 1000,
        total_ms: 4000,
        ..reasoning
    };
    assert!(
        (legacy.stream_tps() - 200.0).abs() < 0.1,
        "got {}",
        legacy.stream_tps()
    );
}

#[test]
fn test_btw_side_question_synthesized_nested_and_lifecycle() {
    let td = TempDir::new().unwrap();
    let conv_dir = td.path().join("conversations");
    let parent_cid = "11000000-0000-0000-0000-000000000001";
    let btw_idle_cid = "11000000-0000-0000-0000-000000000002";
    let btw_live_cid = "11000000-0000-0000-0000-000000000003";
    let orphan_btw_cid = "11000000-0000-0000-0000-000000000004";
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let now_sec = now_ts as u64;
    let idle_mod_ns: i64 = 1_750_000_900_123_456_000;
    let live_mod_ns: i64 = ((now_sec as i64) - 5) * 1_000_000_000;

    let add_traj_meta = |db_path: &Path, cid: &str, ttype: i32| {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE trajectory_meta (
                 trajectory_id TEXT PRIMARY KEY,
                 cascade_id TEXT,
                 trajectory_type INTEGER,
                 source INTEGER
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO trajectory_meta VALUES (?1, ?2, ?3, 1)",
            params![format!("traj-{cid}"), cid, ttype],
        )
        .unwrap();
    };

    // Parent main conversation
    let parent_db = conv_dir.join(format!("{parent_cid}.db"));
    create_rich_conv_db(
        &parent_db,
        &[(0, "gemini-4-neon-high", "Gemini 4 Standard (High)")],
        &[
            (1, 14, 3, None, None, None),
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    now_sec - 120,
                    1,
                    4,
                    8_000,
                    60_000,
                    0,
                    800,
                    Some(300),
                    Some(500),
                    "p-r1",
                    0,
                )),
                None,
                None,
            ),
        ],
    );
    add_traj_meta(&parent_db, parent_cid, 4);

    // Idle /btw side question (absent from conversation_summaries, present in parents + FTS).
    // Include an earlier cloned parent tool step stuck at status=2 (idx=1) before the finished
    // side-question answer step (idx=2, status=3) to mirror real mid-turn /btw snapshots.
    let btw_idle_db = conv_dir.join(format!("{btw_idle_cid}.db"));
    create_rich_conv_db(
        &btw_idle_db,
        &[(0, "gemini-4-neon-high", "Gemini 4 Standard (High)")],
        &[
            (0, 14, 3, None, None, None),
            (
                1,
                132,
                2,
                Some(make_inflight_meta(now_sec - 65, Some(0))),
                None,
                None,
            ),
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    now_sec - 60,
                    1,
                    3,
                    4_000,
                    50_000,
                    0,
                    400,
                    Some(150),
                    Some(250),
                    "btw-idle-r1",
                    0,
                )),
                None,
                None,
            ),
        ],
    );
    add_traj_meta(&btw_idle_db, btw_idle_cid, 22);

    // Live in-flight /btw side question (recent mtime + running step status=2)
    let btw_live_db = conv_dir.join(format!("{btw_live_cid}.db"));
    create_rich_conv_db(
        &btw_live_db,
        &[(0, "gemini-4-neon-high", "Gemini 4 Standard (High)")],
        &[
            (1, 14, 3, None, None, None),
            (
                2,
                15,
                2,
                Some(make_inflight_meta(now_sec - 3, Some(0))),
                None,
                None,
            ),
        ],
    );
    add_traj_meta(&btw_live_db, btw_live_cid, 22);

    // Defense-in-depth /btw DB without any conversation_parents or conversation_summaries row
    let orphan_btw_db = conv_dir.join(format!("{orphan_btw_cid}.db"));
    create_rich_conv_db(
        &orphan_btw_db,
        &[(0, "gemini-4-neon-high", "Gemini 4 Standard (High)")],
        &[
            (1, 14, 3, None, None, None),
            (
                2,
                15,
                3,
                Some(make_step_meta_rich(
                    now_sec - 30,
                    1,
                    2,
                    2_000,
                    20_000,
                    0,
                    200,
                    Some(100),
                    Some(100),
                    "btw-orp-r1",
                    0,
                )),
                None,
                None,
            ),
        ],
    );
    add_traj_meta(&orphan_btw_db, orphan_btw_cid, 22);

    // Build summaries DB: ONLY parent in conversation_summaries; /btw rows in parents + FTS
    let sum_db = td.path().join("conversation_summaries.db");
    create_summaries_db(
        &sum_db,
        &[(
            parent_cid,
            "Main Project Chat",
            "file:///ws/proj-a",
            "CASCADE_RUN_STATUS_IDLE",
            0,
            iso(now_ts - 120.0),
        )],
    );
    {
        let conn = Connection::open(&sum_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_parents (
                 conversation_id TEXT PRIMARY KEY,
                 parent_id TEXT NOT NULL
             ) WITHOUT ROWID;
             CREATE VIRTUAL TABLE conversations_fts USING fts5(
                 conversation_id UNINDEXED,
                 chunk_index UNINDEXED,
                 last_modified_time UNINDEXED,
                 archived UNINDEXED,
                 title,
                 workspace
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_parents VALUES (?1, ?2)",
            params![btw_idle_cid, parent_cid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_parents VALUES (?1, ?2)",
            params![btw_live_cid, parent_cid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations_fts
             (conversation_id, chunk_index, last_modified_time, archived, title, workspace)
             VALUES (?1, -1, ?2, 0, ?3, '')",
            params![btw_idle_cid, idle_mod_ns, "Check Current Progress"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations_fts
             (conversation_id, chunk_index, last_modified_time, archived, title, workspace)
             VALUES (?1, -1, ?2, 0, ?3, '')",
            params![btw_live_cid, live_mod_ns, "Live Side Question"],
        )
        .unwrap();
    }

    let cfg_path = td.path().join("settings.json");
    fs::write(&cfg_path, r#"{"lang":"en"}"#).unwrap();
    let rates = RateTable::new(
        SettingsWatcher::new(Some(cfg_path.to_string_lossy().into_owned())),
        None,
        None,
    );
    let mut engine = TelemetryEngine::new(Some(td.path().to_path_buf()), Some(rates), Some(None));
    engine.set_preferred_workspace(None, false);

    // 1. Default overview (show_history=false): NO /btw leaks into top-level sessions!
    let snap_live = engine.build_overview_snapshot(25, false, false, "proj-a", Some(now_ts));
    assert_eq!(snap_live.summary.shown_main_count, 1);
    assert_eq!(snap_live.sessions.len(), 1);
    assert_eq!(snap_live.sessions[0].id, parent_cid);
    // Only the actively streaming /btw is active; the idle /btw stays hidden in default tab
    assert_eq!(snap_live.sessions[0].running_subagent_count, 1);
    assert_eq!(snap_live.sessions[0].subagents.len(), 1);
    assert_eq!(snap_live.sessions[0].subagents[0].id, btw_live_cid);
    assert_eq!(
        snap_live.sessions[0].subagents[0].title,
        "Live Side Question"
    );
    assert!(snap_live.sessions[0].subagents[0].is_active);
    let live_json = serde_json::to_value(&snap_live.sessions[0].subagents[0]).unwrap();
    assert_eq!(live_json["is_side_question"], true);
    assert!(snap_live.orphan_subagents.is_empty());

    // 2. Finish the live /btw step -> is_active clears to false & running_subagent_count drops to 0
    {
        let conn = Connection::open(&btw_live_db).unwrap();
        conn.execute(
            "UPDATE steps SET status = 3, metadata = ?1 WHERE idx = 2",
            params![make_step_meta_rich(
                now_sec - 3,
                1,
                2,
                3_000,
                40_000,
                0,
                300,
                Some(120),
                Some(180),
                "btw-live-done",
                0,
            )],
        )
        .unwrap();
    }
    let snap_idle = engine.build_overview_snapshot(25, false, false, "", Some(now_ts + 2.0));
    assert_eq!(snap_idle.summary.shown_main_count, 1);
    assert_eq!(snap_idle.sessions.len(), 1);
    assert_eq!(snap_idle.sessions[0].id, parent_cid);
    assert_eq!(snap_idle.sessions[0].running_subagent_count, 0);
    assert!(snap_idle.sessions[0].subagents.is_empty());
    assert!(
        snap_idle.orphan_subagents.is_empty(),
        "idle trajectory_type=22 orphan must stay hidden when show_history=false"
    );
    assert_eq!(
        engine.get_running_subagent_count(parent_cid, Some(now_ts + 2.0)),
        0
    );

    // 3. History tab (show_history=true): /btw rows attach under parent with real titles,
    // inherited workspace, numeric nanosecond timestamp parsed cleanly, and trajectory_type=22
    // fallback lands in orphan_subagents instead of top-level sessions.
    let snap_hist = engine.build_overview_snapshot(25, true, false, "", Some(now_ts + 2.0));
    assert_eq!(snap_hist.summary.shown_main_count, 1);
    assert_eq!(snap_hist.sessions.len(), 1);
    let subs = &snap_hist.sessions[0].subagents;
    assert_eq!(subs.len(), 2);
    let idle_btw = subs.iter().find(|s| s.id == btw_idle_cid).unwrap();
    assert_eq!(idle_btw.title, "Check Current Progress");
    assert_eq!(idle_btw.workspace, "/ws/proj-a");
    assert!(idle_btw.is_subagent);
    assert!(!idle_btw.is_active);
    assert!(
        (idle_btw.summary_modified_ts - 1_750_000_900.123).abs() < 0.01,
        "nanosecond FTS timestamp should parse cleanly, got {}",
        idle_btw.summary_modified_ts
    );
    let idle_json = serde_json::to_value(idle_btw).unwrap();
    assert_eq!(idle_json["is_side_question"], true);

    assert_eq!(snap_hist.orphan_subagents.len(), 1);
    assert_eq!(snap_hist.orphan_subagents[0].id, orphan_btw_cid);
    assert!(snap_hist.orphan_subagents[0].is_subagent);
    let orphan_json = serde_json::to_value(&snap_hist.orphan_subagents[0]).unwrap();
    assert_eq!(orphan_json["is_side_question"], true);

    // Workspace-filtered history query retains /btw via inherited parent workspace
    let snap_ws = engine.build_overview_snapshot(25, true, false, "proj-a", Some(now_ts + 2.0));
    assert_eq!(snap_ws.sessions.len(), 1);
    assert_eq!(snap_ws.sessions[0].subagents.len(), 2);

    // 4. Default auto-resolution never targets a /btw session
    let auto_target = engine.resolve_session_fast("", Some(now_ts + 2.0)).unwrap();
    assert_eq!(auto_target.id, parent_cid);

    // 5. Text summary uses Aside label for /btw children
    let txt = summarize_snapshot_text(&snap_hist);
    assert!(txt.contains("Aside Check Current Progress"), "got: {txt}");
}

#[test]
fn test_tab_count_invariance_archived_priority_and_stale_waiting_suppression() {
    use context_radar_mcp::rpc::tools::format_duration_compact;

    // 1. Duration formatter covers seconds/minutes/hours/days and >i32::MAX cleanly
    assert_eq!(format_duration_compact(45, false), "45s");
    assert_eq!(format_duration_compact(420, false), "7m");
    assert_eq!(format_duration_compact(7500, false), "2h 5m");
    assert_eq!(format_duration_compact(55_390, false), "15h 23m");
    assert_eq!(format_duration_compact(273_600, false), "3d 4h");
    assert_eq!(format_duration_compact(3_000_000_000, false), "34722d 5h");
    assert_eq!(format_duration_compact(45, true), "45 秒");
    assert_eq!(format_duration_compact(420, true), "7 分钟");
    assert_eq!(format_duration_compact(7500, true), "2 小时 5 分");
    assert_eq!(format_duration_compact(55_390, true), "15 小时 23 分");
    assert_eq!(format_duration_compact(273_600, true), "3 天 4 小时");

    // 2. Fixture: 3 non-archived mains, 3 archived mains, 7 idle subagents (one with stale status=2 step)
    let td = TempDir::new().unwrap();
    let conv_dir = td.path().join("conversations");
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let now_sec = now_ts as u64;

    let main_ids = [
        "22000000-0000-0000-0000-000000000001",
        "22000000-0000-0000-0000-000000000002",
        "22000000-0000-0000-0000-000000000003",
    ];
    let arch_ids = [
        "22000000-0000-0000-0000-000000000101",
        "22000000-0000-0000-0000-000000000102",
        "22000000-0000-0000-0000-000000000103",
    ];
    let sub_ids: Vec<String> = (1..=7)
        .map(|i| format!("22000000-0000-0000-0000-00000000020{i}"))
        .collect();

    let mk_turn_step = |ts: u64| -> RichStep {
        let meta = make_step_meta_rich(
            ts,
            1,
            2,
            20_000,
            40_000,
            0,
            800,
            Some(300),
            Some(500),
            "resp-1",
            0,
        );
        (0, 24, 3, Some(meta), None, None)
    };
    let gens = [(0_i64, "gemini-4-neon-high", "Gemini 4 Standard (High)")];
    for (idx, cid) in main_ids.iter().enumerate() {
        create_rich_conv_db(
            &conv_dir.join(format!("{cid}.db")),
            &gens,
            &[mk_turn_step(now_sec - 10 - (idx as u64) * 5)],
        );
    }
    for (idx, cid) in arch_ids.iter().enumerate() {
        create_rich_conv_db(
            &conv_dir.join(format!("{cid}.db")),
            &gens,
            &[mk_turn_step(now_sec - 500 - (idx as u64) * 5)],
        );
    }
    for (idx, cid) in sub_ids.iter().enumerate() {
        if idx == 6 {
            // Parked subagent with lingering status=2 tool step from 55,390s ago
            let stale_meta = pb_bytes(1, &pb_varint(1, now_sec.saturating_sub(55_390)));
            create_rich_conv_db(
                &conv_dir.join(format!("{cid}.db")),
                &gens,
                &[
                    mk_turn_step(now_sec - 30),
                    (1, 36, 2, Some(stale_meta), None, None),
                ],
            );
        } else {
            create_rich_conv_db(
                &conv_dir.join(format!("{cid}.db")),
                &gens,
                &[mk_turn_step(now_sec - 100 - (idx as u64) * 5)],
            );
        }
    }

    let sum_db = td.path().join("conversation_summaries.db");
    let conn = Connection::open(&sum_db).unwrap();
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
             pinned INTEGER,
             last_modified_time TEXT,
             last_user_input_step_index INTEGER
         );
         CREATE TABLE conversations_fts (
             conversation_id TEXT,
             chunk_index INTEGER,
             title TEXT,
             workspace TEXT,
             archived INTEGER,
             last_modified_time INTEGER
         );",
    )
    .unwrap();
    let iso = |sec: u64| {
        DateTime::<Utc>::from_timestamp(sec as i64, 0)
            .unwrap()
            .to_rfc3339()
    };
    for (idx, cid) in main_ids.iter().enumerate() {
        let status = if idx == 0 {
            "CASCADE_RUN_STATUS_RUNNING"
        } else {
            "CASCADE_RUN_STATUS_IDLE"
        };
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2, '', '/ws/a', ?3, '', '', 0, ?4, 0, 0, ?5, 1)",
            params![cid, format!("Main {idx}"), status, i32::from(idx == 0), iso(now_sec - 10 - (idx as u64) * 5)],
        )
        .unwrap();
    }
    for (idx, cid) in arch_ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2, '', '/ws/a', 'CASCADE_RUN_STATUS_IDLE', '', '', 0, 0, 0, 0, ?3, 1)",
            params![cid, format!("Archived {idx}"), iso(now_sec - 500 - (idx as u64) * 5)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations_fts VALUES (?1, -1, ?2, '/ws/a', 1, ?3)",
            params![
                cid,
                format!("Archived {idx}"),
                ((now_sec - 500 - (idx as u64) * 5) as i64) * 1_000_000_000
            ],
        )
        .unwrap();
    }
    for (idx, cid) in sub_ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2, '', '/ws/a', 'CASCADE_RUN_STATUS_IDLE', ?3, ?4, 1, 0, 0, 0, ?5, 1)",
            params![
                cid,
                format!("Sub {idx}"),
                format!("worker-{idx}"),
                main_ids[0],
                iso(now_sec - 100 - (idx as u64) * 5),
            ],
        )
        .unwrap();
    }
    drop(conn);

    let cfg_path = td.path().join("settings.json");
    fs::write(&cfg_path, r#"{"lang":"en"}"#).unwrap();
    let rates = RateTable::new(
        SettingsWatcher::new(Some(cfg_path.to_string_lossy().into_owned())),
        None,
        None,
    );
    let mut engine = TelemetryEngine::new(Some(td.path().to_path_buf()), Some(rates), Some(None));

    // Limit=2 -> subagent cap is limit*3 = 6 < 7 total subagents
    let snap_active = engine.build_overview_snapshot(2, false, false, "", Some(now_ts));
    let snap_subs = engine.build_overview_snapshot(2, true, false, "", Some(now_ts));
    let snap_arch = engine.build_overview_snapshot(2, false, true, "", Some(now_ts));

    // Invariant totals across ALL 3 tabs (no jumping!)
    for snap in [&snap_active, &snap_subs, &snap_arch] {
        assert_eq!(snap.summary.total_main_count, 3);
        assert_eq!(snap.summary.total_subagent_count, 7);
        assert_eq!(snap.summary.total_archived_count, 3);
    }

    // Subagents tab caps at 6 and reports 1 hidden (NOT 0!)
    assert_eq!(snap_subs.summary.shown_subagent_count, 6);
    assert_eq!(snap_subs.summary.history_subagents_hidden_count, 1);

    // Parked summarized subagent with stale status=2 step MUST report ready (not waiting 55390s)
    let parked = snap_subs.sessions[0]
        .subagents
        .iter()
        .find(|s| s.id == sub_ids[6])
        .expect("parked subagent should appear in history");
    assert_eq!(parked.live_status, "ready");
    assert_eq!(parked.live_phase, "");
    assert_eq!(parked.live_elapsed_sec, 0);

    // Archived tab prioritizes archived mains so they are not crowded out by non-archived mains
    let shown_archived: Vec<&str> = snap_arch
        .sessions
        .iter()
        .filter(|s| s.archived)
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(shown_archived.len(), 2);
    assert_eq!(snap_arch.summary.archived_hidden_count, 1);
}

#[test]
fn test_unified_settings_atomic_merge_hot_reload_and_fail_closed() {
    use context_radar_mcp::settings::SettingsWatcher;
    use std::os::unix::fs::PermissionsExt;

    let td = TempDir::new().unwrap();
    let plugin_data = td.path().join("plugin_data");
    fs::create_dir_all(&plugin_data).unwrap();
    let settings_path = plugin_data.join("settings.json");

    // Seed settings.json with advanced pricing.overrides & budgets.tiers
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&json!({
            "lang": "en",
            "pricing": {
                "fx_rate": 7.1,
                "overrides": [
                    { "match_prefix": ["custom-model"], "input_per_m": 1.25, "output_per_m": 5.0 }
                ]
            },
            "budgets": {
                "tiers": [
                    { "name": "custom_tier", "min_ctx": 200000, "recommend_ctx": 250000, "urgent_ctx": 300000 }
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let engine = TelemetryEngine::new_with_plugin_data(
        Some(td.path().to_path_buf()),
        Some(plugin_data.clone()),
        None,
        Some(None),
    );
    let grants = context_radar_mcp::engine::grants::RelayGrantStore::new(
        plugin_data.join("relay_grants.json"),
    );
    let mut daemon = McpDaemon::new(Some(engine), Some(grants), None);

    // Initialize with SEP-1865 apps capability so ui_action is enabled
    let init = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {
                    "extensions": {
                        "io.modelcontextprotocol/ui": {
                            "mimeTypes": ["text/html;profile=mcp-app"]
                        }
                    }
                },
                "clientInfo": { "name": "test-host", "version": "1.0" }
            }
        }))
        .unwrap();
    assert!(init.get("result").is_some());

    // Update GUI preferences via context_radar_ui_action(set_prefs)
    let set_resp = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "context_radar_ui_action",
                "arguments": {
                    "action": "set_prefs",
                    "lang": "zh-CN",
                    "currency_code": "CNY",
                    "fx_rate": 7.35,
                    "poll_interval_sec": 5,
                    "bg_poll_interval_sec": 30,
                    "allow_auto_handoff": false,
                    "compact_threshold": 320000
                }
            }
        }))
        .unwrap();
    let sc = &set_resp["result"]["structuredContent"];
    assert_eq!(sc["mode"], "prefs", "unexpected response: {set_resp}");
    assert_eq!(sc["ui_settings"]["lang"], "zh-CN");
    assert_eq!(sc["ui_settings"]["currency_code"], "CNY");
    assert_eq!(sc["ui_settings"]["fx_rate"], 7.35);
    assert_eq!(sc["ui_settings"]["poll_interval_sec"], 5);
    assert_eq!(sc["ui_settings"]["bg_poll_interval_sec"], 30);
    assert_eq!(sc["ui_settings"]["allow_auto_handoff"], false);
    assert_eq!(sc["ui_settings"]["compact_threshold"], 320_000);
    assert!(sc["settings_error"].is_null());

    // Verify 0600 perms and preserved advanced pricing.overrides & budgets.tiers on disk
    let mode = fs::metadata(&settings_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "settings.json must be written with 0600 mode");
    let on_disk: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(
        on_disk["pricing"]["overrides"][0]["match_prefix"][0],
        "custom-model"
    );
    assert_eq!(on_disk["budgets"]["tiers"][0]["name"], "custom_tier");
    assert_eq!(on_disk["ui"]["poll_interval_sec"], 5);
    assert_eq!(on_disk["context"]["compact_threshold"], 320_000);

    // Hot-reload on external atomic rename (even with same byte length -> inode changes!)
    let mut watcher = SettingsWatcher::new_with_plugin_data(None, Some(plugin_data.clone()));
    let s1 = watcher.current();
    assert!((s1.pricing.fx_rate - 7.35).abs() < 1e-6);

    let tmp_swap = plugin_data.join("settings.json.swap");
    let updated_raw = fs::read_to_string(&settings_path)
        .unwrap()
        .replace("7.35", "7.88");
    fs::write(&tmp_swap, updated_raw).unwrap();
    fs::rename(&tmp_swap, &settings_path).unwrap();
    let s2 = watcher.current();
    assert!(
        (s2.pricing.fx_rate - 7.88).abs() < 1e-6,
        "watcher must hot-reload on inode change, got {}",
        s2.pricing.fx_rate
    );
    assert!(s2.settings_error.is_none());

    // Fail-closed on broken JSON: keeps last-known-good fx_rate (7.88) and surfaces settings_error
    fs::write(&settings_path, "{broken json").unwrap();
    let s3 = watcher.current();
    assert_eq!(s3.settings_error.as_deref(), Some("invalid_json"));
    assert!((s3.pricing.fx_rate - 7.88).abs() < 1e-6);

    // Refuses to overwrite broken settings.json via GUI save
    let err_save = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "context_radar_ui_action",
                "arguments": { "action": "set_prefs", "lang": "en" }
            }
        }))
        .unwrap();
    assert_eq!(err_save["result"]["isError"], true);
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), "{broken json");
}

#[test]
fn test_tool_names_within_64_chars_and_pruner_handoff_hint_uses_short_name() {
    use context_radar_mcp::engine::pruner::distill_slice;
    use context_radar_mcp::ledger::journal_tail::JournalTailCache;
    use context_radar_mcp::rpc::tools::tools_spec;

    for tool in tools_spec().as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let full = format!("mcp__context-radar-mcp__{name}");
        assert!(
            full.len() <= 64 && full.is_ascii(),
            "tool name exceeds 64-char namespace budget: {full} ({})",
            full.len()
        );
    }

    let td = TempDir::new().unwrap();
    let cid = "33000000-0000-0000-0000-000000000001";
    let logs_dir = td
        .path()
        .join("brain")
        .join(cid)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&logs_dir).unwrap();
    let transcript = [
        json!({"step_index": 1, "type": "CORTEX_STEP_TYPE_USER_INPUT", "user_input": "Check context budget"}).to_string(),
        json!({"step_index": 2, "type": "CORTEX_STEP_TYPE_VIEW_FILE", "path": "/ws/src/main.rs"}).to_string(),
    ]
    .join("\n");
    fs::write(logs_dir.join("transcript.jsonl"), transcript).unwrap();
    let mut cache = JournalTailCache::default();
    let report = distill_slice(
        td.path(),
        cid,
        &[],
        "distill",
        Some(1),
        Some(2),
        true,
        5,
        &mut cache,
    );
    assert!(report.found);
    let hint = report.handoff_hint.as_deref().unwrap_or("");
    assert!(
        hint.contains("radar_commit_handoff"),
        "expected radar_commit_handoff in hint, got: {hint}"
    );
    assert!(
        !hint.contains("context_radar_commit_handoff"),
        "stale tool name leaked in hint: {hint}"
    );
}
