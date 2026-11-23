use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::json;
use tempfile::TempDir;

use context_radar_mcp::engine::budget::{
    builtin_tiers, evaluate_handoff_gate, prepare_handoff_plan, resolve_budget_tier,
};
use context_radar_mcp::engine::grants::{build_handoff_prompt, RelayGrantStore};
use context_radar_mcp::engine::pruner::distill_slice;
use context_radar_mcp::engine::rates::RateTable;
use context_radar_mcp::engine::telemetry::{
    build_session_telemetry, format_turn_rows, ColdCacheBreakdown, TelemetryEngine,
};
use context_radar_mcp::ledger::journal_tail::JournalTailCache;
use context_radar_mcp::ledger::sqlite_ro::open_host_readonly;
use context_radar_mcp::ledger::TurnSample;
use context_radar_mcp::rpc::protocol::McpDaemon;
use context_radar_mcp::rpc::tools::{HANDOFF_URI, PANEL_URI};
use context_radar_mcp::settings::{SettingsWatcher, DEFAULT_CONTEXT_LIMIT, DEFAULT_FX_RATE};
use context_radar_mcp::wire::pb_cursor::{decode_step_metadata, decode_timestamp, extract_gen_model};

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

fn make_gen_blob(model_id: &str, model_display: &str, padding: usize) -> Vec<u8> {
    let mut inner = pb_bytes(19, model_id.as_bytes());
    inner.extend_from_slice(&pb_bytes(21, model_display.as_bytes()));
    if padding > 0 {
        inner.extend_from_slice(&pb_bytes(99, &vec![b'x'; padding]));
    }
    pb_bytes(1, &inner)
}

fn make_step_meta(
    ts_sec: u64,
    in_tok: u64,
    cr_tok: u64,
    out_tok: u64,
    resp_id: &str,
    gen_idx: u64,
    cw_tok: u64,
) -> Vec<u8> {
    let mut usage = pb_varint(2, in_tok);
    usage.extend_from_slice(&pb_varint(3, out_tok));
    usage.extend_from_slice(&pb_varint(4, cw_tok));
    usage.extend_from_slice(&pb_varint(5, cr_tok));
    usage.extend_from_slice(&pb_bytes(11, resp_id.as_bytes()));

    let mut out = pb_bytes(1, &pb_ts(ts_sec, 0));
    out.extend_from_slice(&pb_bytes(6, &pb_ts(ts_sec + 1, 0)));
    out.extend_from_slice(&pb_bytes(7, &pb_ts(ts_sec + 3, 0)));
    out.extend_from_slice(&pb_bytes(9, &usage));
    out.extend_from_slice(&pb_bytes(20, &pb_varint(3, gen_idx)));
    out
}

fn create_conv_db(
    path: &Path,
    steps: &[(i64, i32, Option<Vec<u8>>)],
    model_id: &str,
    model_display: &str,
) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER, metadata BLOB);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO gen_metadata (idx, data) VALUES (0, ?1)",
        params![make_gen_blob(model_id, model_display, 0)],
    )
    .unwrap();
    for (idx, stype, meta) in steps {
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (?1, ?2, ?3)",
            params![idx, stype, meta],
        )
        .unwrap();
    }
}

#[allow(clippy::type_complexity)]
fn create_summaries_db(
    path: &Path,
    rows: &[(
        &str,
        &str,
        &str,
        &str,
        &str,
        &str,
        &str,
        i64,
        i64,
        i64,
        String,
        i64,
    )],
) {
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
    for r in rows {
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10, r.11],
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
fn test_builtin_generic_defaults_when_config_disabled() {
    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
    assert_eq!(DEFAULT_CONTEXT_LIMIT, 512_000);

    let p = rates.resolve_rate("gemini-3-pro", "Gemini 3 Pro (High)");
    assert_eq!((p.input_per_m, p.cache_read_per_m, p.output_per_m), (2.0, 0.20, 12.0));
    assert_eq!(p.context_limit, 512_000);

    let cl = rates.resolve_rate("claude-sonnet-4", "Claude Sonnet 4");
    assert_eq!((cl.input_per_m, cl.cache_read_per_m, cl.output_per_m), (3.0, 0.30, 15.0));

    let (fx, src) = rates.get_fx_rate();
    assert!((fx - DEFAULT_FX_RATE).abs() < 1e-4);
    assert_eq!(src, "default");
}

#[test]
fn test_external_user_config_overrides_pricing_tiers_and_live_reloads() {
    let td = TempDir::new().unwrap();
    let ext_fx = td.path().join("fx.json");
    fs::write(&ext_fx, serde_json::to_string(&json!({"rate": 6.85})).unwrap()).unwrap();

    let ext_models = td.path().join("ext_models.json");
    fs::write(
        &ext_models,
        serde_json::to_string(&json!({
            "custom_prov": {
                "models": {
                    "fallback-catalog-model": {
                        "cost": { "input": 1.2, "output": 6.0, "cache_read": 0.12 },
                        "limit": { "context": 400000 }
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let cfg_path = td.path().join("settings.json");
    let mut cfg_data = json!({
        "version": 1,
        "context": { "default_limit": 512000, "warn_threshold": 210000 },
        "pricing": {
            "currency_code": "USD",
            "currency_symbol": "$",
            "fx_rate": 1.0,
            "external_fxrate_path": ext_fx.to_string_lossy(),
            "external_models_path": ext_models.to_string_lossy(),
            "overrides": [{
                "match_prefix": ["custom-alpha"],
                "match_contains": ["custom alpha"],
                "input_per_m": 4.0,
                "cache_read_per_m": 0.20,
                "cache_write_per_m": 4.0,
                "output_per_m": 20.0,
                "context_limit": 512000
            }],
            "short_labels": [{ "contains": "Custom Alpha (High)", "label": "Alpha" }]
        },
        "budgets": {
            "tiers": {
                "alpha_tier": {
                    "label": "Alpha Custom Tier",
                    "min_ctx": 240000,
                    "recommend_ctx": 280000,
                    "urgent_ctx": 400000,
                    "min_turns_since_ckpt": 14,
                    "min_remaining_turns": 6
                }
            },
            "rules": [{ "match_any": ["alpha"], "tier": "alpha_tier" }]
        }
    });
    fs::write(&cfg_path, serde_json::to_string(&cfg_data).unwrap()).unwrap();

    let mut rates = RateTable::new(
        SettingsWatcher::new(Some(cfg_path.to_string_lossy().into_owned())),
        None,
        None,
    );

    let (rate, src) = rates.get_fx_rate();
    assert!((rate - 6.85).abs() < 1e-3);
    assert_eq!(src, "external-fx");
    assert_eq!(rates.get_currency(), ("USD".into(), "$".into()));

    // Override hits BEFORE lazy external catalog load
    let p_alpha = rates.resolve_rate("custom-alpha-high", "Custom Alpha (High)");
    assert_eq!((p_alpha.input_per_m, p_alpha.cache_read_per_m, p_alpha.output_per_m), (4.0, 0.20, 20.0));
    assert!(!rates.external_models_loaded());
    assert_eq!(rates.short_model_label("Custom Alpha (High)", "custom-alpha-high"), "Alpha");

    // Miss triggers lazy external catalog load
    let p_ext = rates.resolve_rate("fallback-catalog-model", "");
    assert_eq!((p_ext.input_per_m, p_ext.output_per_m, p_ext.context_limit), (1.2, 6.0, 400_000));
    assert!(rates.external_models_loaded());

    // Custom budget tier matches
    let cfg = rates.current_settings();
    let tier = resolve_budget_tier("custom-alpha-high", "Custom Alpha (High)", &cfg);
    assert_eq!(tier.profile_id, "alpha_tier");
    assert_eq!(tier.min_ctx, 240_000);

    // Hot-reload on config edit (wait briefly for mtime/size update)
    thread::sleep(Duration::from_millis(20));
    cfg_data["pricing"]["overrides"][0]["input_per_m"] = json!(5.55);
    fs::write(&cfg_path, serde_json::to_string_pretty(&cfg_data).unwrap()).unwrap();
    let p_reloaded = rates.resolve_rate("custom-alpha-high", "Custom Alpha (High)");
    assert!((p_reloaded.input_per_m - 5.55).abs() < 1e-6);
}

#[test]
fn test_checkpoint_post_compaction_estimation() {
    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
    let turns = vec![
        TurnSample {
            turn_index: 1,
            step_idx: 100,
            timestamp: 100.0,
            model: "gemini-3-pro".into(),
            model_display: "Gemini 3 Pro (XHigh)".into(),
            input_tokens: 2748,
            cache_read: 462189,
            cache_write: 0,
            output_tokens: 553,
            ttft_ms: 4000,
            total_ms: 8000,
            response_id: "r1".into(),
            step_type: 15,
            is_checkpoint: false,
            after_turn_start: None,
            after_checkpoint: false,
        },
        TurnSample {
            turn_index: 2,
            step_idx: 102,
            timestamp: 110.0,
            model: "gemini-3-pro".into(),
            model_display: "Gemini 3 Pro (XHigh)".into(),
            input_tokens: 2120,
            cache_read: 464281,
            cache_write: 0,
            output_tokens: 5878,
            ttft_ms: 0,
            total_ms: 0,
            response_id: "r2".into(),
            step_type: 23,
            is_checkpoint: true,
            after_turn_start: None,
            after_checkpoint: false,
        },
    ];
    let m = build_session_telemetry(
        "10000000-0000-0000-0000-000000000001",
        None,
        &turns,
        110.0,
        7.2,
        &mut rates,
        120.0,
        None,
    );
    assert!(m.just_compacted);
    assert_eq!(m.pre_compact_tokens, 466_401);
    assert!(m.latest_context_tokens > 24_000 && m.latest_context_tokens < 35_000);
    assert!(!m.suggest_compact);
    assert_eq!(m.level, "ok");
}

#[test]
fn test_generic_tiers_and_hard_gates() {
    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
    let tiers = builtin_tiers();
    assert!(tiers.contains_key("high_reasoning"));
    assert!(tiers.contains_key("flagship"));
    assert!(tiers.contains_key("fast"));

    let cfg = rates.current_settings();
    assert_eq!(resolve_budget_tier("gemini-3-pro", "Gemini 3 Pro (XHigh)", &cfg).min_ctx, 250_000);
    assert_eq!(resolve_budget_tier("gemini-3-pro", "Gemini 3 Pro (High)", &cfg).min_ctx, 270_000);
    assert_eq!(resolve_budget_tier("gemini-3-flash", "Gemini 3 Flash", &cfg).min_ctx, 330_000);

    let mut base = build_session_telemetry(
        "10000000-0000-0000-0000-000000000099",
        None,
        &[],
        100.0,
        7.2,
        &mut rates,
        120.0,
        None,
    );
    base.model = "gemini-3-pro".into();
    base.model_display = "Gemini 3 Pro (High)".into();
    base.turns = 60;
    base.turns_since_compact = 60;

    // 1) Block below min_ctx
    let mut low = base.clone();
    low.latest_context_tokens = 180_000;
    assert!(!evaluate_handoff_gate(Some(&low), 20, &mut rates).allowed);

    // 2) Block subagent and advise baton-pass
    let mut sub = base.clone();
    sub.is_subagent = true;
    sub.agent_name = "worker-sample".into();
    sub.latest_context_tokens = 455_600;
    let sub_gate = evaluate_handoff_gate(Some(&sub), 20, &mut rates);
    assert!(!sub_gate.allowed);
    assert!(sub_gate.reasons.iter().any(|r| r.contains("切棒")));
    assert_eq!(prepare_handoff_plan(Some(&sub), 12, 0, &mut rates).action, "subagent_handoff");

    // 3) Block when running_subagent_count > 0 (even at urgent context!)
    let mut busy = base.clone();
    busy.latest_context_tokens = 450_000;
    busy.running_subagent_count = 2;
    let busy_gate = evaluate_handoff_gate(Some(&busy), 20, &mut rates);
    assert!(!busy_gate.allowed);
    assert!(busy_gate.reasons.iter().any(|r| r.contains("running_subagent_count == 0")));

    // 4) Allow healthy high-context main with 0 running subagents
    let mut ok = base.clone();
    ok.latest_context_tokens = 330_000;
    ok.checkpoint_count = 1;
    ok.turns_since_compact = 25;
    ok.running_subagent_count = 0;
    ok.recent_cold_rebuild = false;
    assert!(evaluate_handoff_gate(Some(&ok), 20, &mut rates).allowed);
}

#[test]
fn test_incremental_sqlite_and_disk_cache_match_full_parse_exactly() {
    let td = TempDir::new().unwrap();
    let cid = "30000000-0000-0000-0000-000000000003";
    let db_path = td.path().join("conversations").join(format!("{cid}.db"));
    let cache_path = td.path().join("index_cache.sqlite");
    let base_ts = 1_790_000_000u64;

    let initial_steps = vec![
        (0, 14, None),
        (1, 15, Some(make_step_meta(base_ts, 40_000, 0, 500, "r1", 0, 0))),
        (2, 132, Some(make_step_meta(base_ts + 5, 1, 1, 1, "ignored-tool", 0, 0))),
        (3, 15, None), // In-flight step with NULL metadata
    ];
    create_conv_db(&db_path, &initial_steps, "gemini-3-pro", "Gemini 3 Pro (High)");

    let mut eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(Some(cache_path.clone())),
    );
    let t1 = eng.ledger.read_turns(cid, None);
    assert_eq!(t1.len(), 1);
    assert_eq!(t1[0].step_idx, 1);

    // Update step 3 in place + add checkpoint + user turn + new model gen_metadata
    thread::sleep(Duration::from_millis(20));
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (1, ?1)",
            params![make_gen_blob("gemini-3-flash", "Gemini 3 Flash (High)", 4096)],
        )
        .unwrap();
        conn.execute(
            "UPDATE steps SET metadata = ?1 WHERE idx = 3",
            params![make_step_meta(base_ts + 15, 5_000, 42_000, 400, "r2", 0, 0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (4, 23, ?1)",
            params![make_step_meta(base_ts + 30, 2_000, 46_000, 3_000, "r3", 0, 0)],
        )
        .unwrap();
        conn.execute("INSERT INTO steps (idx, step_type, metadata) VALUES (5, 14, NULL)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (6, 15, ?1)",
            params![make_step_meta(base_ts + 60, 25_000, 0, 600, "r4", 1, 0)],
        )
        .unwrap();
    }

    let t_inc = eng.ledger.read_turns(cid, None);
    let mut eng_full = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(None),
    );
    let t_full = eng_full.ledger.read_turns(cid, None);
    assert_eq!(t_inc, t_full);
    assert_eq!(t_inc.len(), 4);
    assert_eq!(t_inc[3].model, "gemini-3-flash");
    assert!(t_inc[3].after_checkpoint);

    // Cold restart loading from persistent disk index cache
    let mut eng_cold = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(Some(cache_path)),
    );
    let t_disk = eng_cold.ledger.read_turns(cid, None);
    assert_eq!(t_disk, t_full);
}

#[test]
fn test_read_only_host_db_safety_and_live_wal_visibility() {
    let td = TempDir::new().unwrap();
    let cid = "31000000-0000-0000-0000-000000000031";
    let db_path = td.path().join("conversations").join(format!("{cid}.db"));
    let base_ts = 1_790_100_000u64;

    create_conv_db(
        &db_path,
        &[
            (0, 14, None),
            (1, 15, Some(make_step_meta(base_ts, 30_000, 0, 400, "w1", 0, 0))),
        ],
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );

    // Switch host DB into WAL mode and hold an active writer that appends to -wal without checkpoint
    let writer = Connection::open(&db_path).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;").unwrap();

    let mut eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(None),
    );
    assert_eq!(eng.ledger.read_turns(cid, None).len(), 1);

    // Verify open_host_readonly refuses any mutation
    let ro = open_host_readonly(&db_path).unwrap();
    assert!(ro.execute("CREATE TABLE forbidden (x INT)", []).is_err());
    assert!(ro.execute("INSERT INTO steps (idx, step_type, metadata) VALUES (99, 15, NULL)", []).is_err());
    drop(ro);

    // Commit live step into -wal file
    thread::sleep(Duration::from_millis(20));
    writer
        .execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (2, 15, ?1)",
            params![make_step_meta(base_ts + 20, 4_000, 32_000, 350, "w2", 0, 0)],
        )
        .unwrap();

    let wal_path = db_path.with_extension("db-wal");
    assert!(wal_path.is_file(), "WAL file should exist");
    let before_db_bytes = fs::read(&db_path).unwrap();

    // Incremental WAL delta must see the live WAL step cleanly!
    let turns = eng.ledger.read_turns(cid, None);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].cache_read, 32_000);
    assert_eq!(fs::read(&db_path).unwrap(), before_db_bytes, "Host .db must remain unmodified");
}

#[test]
fn test_journal_tail_backward_checkpoint_seek_and_incremental_append() {
    let td = TempDir::new().unwrap();
    let cid = "40000000-0000-0000-0000-000000000004";
    let tdir = td.path().join("brain").join(cid).join(".system_generated").join("logs");
    fs::create_dir_all(&tdir).unwrap();
    let tpath = tdir.join("transcript.jsonl");

    // Write > 300 KiB pre-checkpoint noise (exceeding 256 KiB backward chunk!) + CHECKPOINT + post steps
    let mut content = String::new();
    for i in 1..=65 {
        content.push_str(&serde_json::to_string(&json!({
            "step_index": i,
            "type": "PLANNER_RESPONSE",
            "content": format!("old step {i} {}", "A".repeat(5000))
        })).unwrap());
        content.push('\n');
    }
    content.push_str(&serde_json::to_string(&json!({"step_index": 66, "type": "CHECKPOINT", "content": "summary"})).unwrap());
    content.push('\n');
    content.push_str(&serde_json::to_string(&json!({"step_index": 67, "type": "USER_INPUT", "content": "continue task"})).unwrap());
    content.push('\n');
    content.push_str(&serde_json::to_string(&json!({
        "step_index": 68,
        "type": "PLANNER_RESPONSE",
        "content": "checked module A",
        "tool_calls": [{ "name": "read_file", "args": { "Path": "/repo/app.rs" } }]
    })).unwrap());
    content.push('\n');
    fs::write(&tpath, &content).unwrap();

    let mut jcache = JournalTailCache::default();
    let win1 = jcache.get_active_window(td.path(), cid);
    let idx1: Vec<i64> = win1.iter().map(|e| e.step_index).collect();
    assert_eq!(idx1, vec![67, 68]);

    // Append step 69 -> incremental tail read adds only step 69
    thread::sleep(Duration::from_millis(20));
    content.push_str(&serde_json::to_string(&json!({
        "step_index": 69,
        "type": "PLANNER_RESPONSE",
        "status": "ERROR",
        "content": "sample error msg"
    })).unwrap());
    content.push('\n');
    fs::write(&tpath, &content).unwrap();

    let win2 = jcache.get_active_window(td.path(), cid);
    let idx2: Vec<i64> = win2.iter().map(|e| e.step_index).collect();
    assert_eq!(idx2, vec![67, 68, 69]);

    let dist = distill_slice(td.path(), cid, &[], "distill", None, None, true, 8, &mut jcache);
    assert!(dist.found);
    assert_eq!(dist.slice_range, Some([67, 68]));
    let note_path = dist.saved_note_path.unwrap();
    assert!(Path::new(&note_path).is_file());

    let stypes = jcache.get_step_types(td.path(), cid);
    assert_eq!(stypes.get(&66).map(String::as_str), Some("CHECKPOINT"));
    assert_eq!(stypes.get(&67).map(String::as_str), Some("USER_INPUT"));
    assert_eq!(stypes.get(&69).map(String::as_str), Some("PLANNER_RESPONSE"));
}

#[test]
fn test_active_subagent_filtering_and_three_way_cold_cache() {
    let now = 1_795_342_200.0;
    let td = TempDir::new().unwrap();
    let conv_dir = td.path().join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    let main_active = "aaaa0000-0000-0000-0000-000000000001";
    let main_idle = "aaaa0000-0000-0000-0000-000000000002";
    let sub_running = "bbbb0000-0000-0000-0000-000000000001";
    let sub_parked = "bbbb0000-0000-0000-0000-000000000002";
    let sub_idle = "bbbb0000-0000-0000-0000-000000000003";
    let sub_stale = "bbbb0000-0000-0000-0000-000000000004";
    let sub_killed = "bbbb0000-0000-0000-0000-000000000005";

    let main_steps = vec![
        (0, 14, None),
        (1, 15, Some(make_step_meta((now - 500.0) as u64, 40_000, 0, 500, "r1", 0, 0))),
        (2, 132, None),
        (3, 15, Some(make_step_meta((now - 480.0) as u64, 65_000, 0, 500, "r2", 0, 0))),
        (4, 132, None),
        (5, 15, Some(make_step_meta((now - 460.0) as u64, 3_000, 68_000, 400, "r3", 0, 0))),
        (6, 101, None),
        (7, 15, Some(make_step_meta((now - 100.0) as u64, 75_000, 0, 400, "r4", 0, 0))),
        (8, 23, Some(make_step_meta((now - 80.0) as u64, 2_000, 75_000, 3_500, "r5", 0, 0))),
        (9, 14, None),
        (10, 15, Some(make_step_meta((now - 30.0) as u64, 28_000, 0, 300, "r6", 0, 0))),
    ];
    create_conv_db(
        &conv_dir.join(format!("{main_active}.db")),
        &main_steps,
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );

    let one_step = |ts: f64, ctx: u64| {
        vec![
            (0, 14, None),
            (1, 15, Some(make_step_meta(ts as u64, ctx, 0, 300, &format!("r-{}", ts as u64), 0, 0))),
        ]
    };
    for (cid, ts, ctx) in [
        (main_idle, now - 600.0, 120_000),
        (sub_running, now - 10.0, 420_000),
        (sub_parked, now - 300.0, 210_000),
        (sub_idle, now - 18_000.0, 45_000),
        (sub_stale, now - 7_200.0, 80_000),
        (sub_killed, now - 1_200.0, 150_000),
    ] {
        let p = conv_dir.join(format!("{cid}.db"));
        create_conv_db(&p, &one_step(ts, ctx), "gemini-3-pro", "Gemini 3 Pro (High)");
        // Set mtime to match ts so stale subagent exceeds silent_timeout_sec
        let ft = filetime_set(&p, ts as i64);
        assert!(ft);
    }
    assert!(filetime_set(&conv_dir.join(format!("{main_active}.db")), (now - 30.0) as i64));

    create_summaries_db(
        &td.path().join("conversation_summaries.db"),
        &[
            (main_active, "Active Session", "", r#"["file:///workspace/demo"]"#, "CASCADE_RUN_STATUS_RUNNING", "", "", 0, 1, 0, iso(now - 30.0), 0),
            (main_idle, "Idle Session", "", r#"["file:///workspace/demo"]"#, "CASCADE_RUN_STATUS_IDLE", "", "", 0, 0, 0, iso(now - 600.0), 0),
            (sub_running, "", "", r#"["file:///workspace/demo"]"#, "CASCADE_RUN_STATUS_RUNNING", "worker-run", main_active, 1, 1, 0, iso(now - 10.0), -1),
            (sub_parked, "", "", r#"["file:///workspace/demo"]"#, "CASCADE_RUN_STATUS_IDLE", "worker-park", main_active, 1, 1, 0, iso(now - 300.0), -1),
            (sub_idle, "", "", r#"["file:///workspace/demo"]"#, "CASCADE_RUN_STATUS_IDLE", "browser", main_active, 1, 0, 0, iso(now - 18_000.0), -1),
            (sub_stale, "", "", r#"["file:///workspace/demo"]"#, "CASCADE_RUN_STATUS_IDLE", "worker-stale", main_active, 1, 1, 0, iso(now - 7_200.0), -1),
            (sub_killed, "", "", r#"["file:///workspace/demo"]"#, "CASCADE_RUN_STATUS_IDLE", "worker-dead", main_active, 1, 0, 1, iso(now - 1_200.0), -1),
        ],
    );

    let mut eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(None),
    );
    let snap = eng.build_overview_snapshot(20, false, false, "", Some(now));
    assert_eq!(snap.sessions.len(), 2);
    let act = snap.sessions.iter().find(|s| s.id == main_active).unwrap();
    let sub_ids: Vec<&str> = act.subagents.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(sub_ids.len(), 2);
    assert!(sub_ids.contains(&sub_running));
    assert!(sub_ids.contains(&sub_parked));
    assert_eq!(snap.summary.history_subagents_hidden_count, 3);
    assert_eq!(
        act.cold_cache_breakdown,
        ColdCacheBreakdown {
            turn_start_cold: 2,
            mid_loop_cold: 1,
            post_compact_cold: 1,
        }
    );

    let turns = eng.ledger.read_turns(main_active, None);
    let rows = format_turn_rows(&turns, 1, 7.2, &mut eng.rates, None);
    assert_eq!(rows[0].cold_cache_kind, "post_compact_cold");

    let fast = eng.resolve_session_fast(&main_active[..10], Some(now)).unwrap();
    assert_eq!(fast.id, main_active);
    assert_eq!(fast.running_subagent_count, 2);
}

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}
extern "C" {
    fn utimensat(dirfd: i32, path: *const i8, times: *const Timespec, flag: i32) -> i32;
}
fn filetime_set(path: &Path, sec: i64) -> bool {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
    let ts = [Timespec { tv_sec: sec, tv_nsec: 0 }, Timespec { tv_sec: sec, tv_nsec: 0 }];
    unsafe { utimensat(-2 /* AT_FDCWD on macOS */, c.as_ptr(), ts.as_ptr(), 0) == 0 }
}

#[test]
fn test_relay_grant_lifecycle_and_anti_replay() {
    let td = TempDir::new().unwrap();
    let mut gs = RelayGrantStore::new(td.path().join("relay_grants.json"));
    assert!(gs.get_settings().allow_auto_handoff);

    let prompt = build_handoff_prompt(
        "50000000-0000-0000-0000-000000000005",
        "Keep finding A",
        "Run regression suite",
        "/tmp/slice_1_10.md",
        "Demo Session",
    );
    assert!(prompt.contains("target:new"));
    assert!(prompt.contains("/tmp/slice_1_10.md"));

    let grant = gs.create_grant(
        "Keep finding A",
        "Run regression suite",
        true,
        "50000000-0000-0000-0000-000000000005",
        40,
        0,
        "handoff",
        "",
        "Demo Session",
    );

    gs.update_settings(Some(false));
    assert_eq!(
        gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(0), true).reason.as_deref(),
        Some("auto_handoff_disabled")
    );

    gs.update_settings(Some(true));
    assert_eq!(
        gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(1), true).reason.as_deref(),
        Some("running_subagents_active")
    );

    let ok = gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(0), true);
    assert!(ok.claimed);

    // Anti-replay check
    assert_eq!(
        gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(0), false).reason.as_deref(),
        Some("already_claimed")
    );

    let rec = gs.record_relay(&grant.grant_id, "new-session-999");
    assert!(rec.ok);
    assert_eq!(rec.handoff_conversation_id.as_deref(), Some("new-session-999"));

    let dis = gs.dismiss_grant(&grant.grant_id);
    assert!(dis.ok);
    assert!(gs.get_pending_grant(None).is_none());
}

#[test]
fn test_zero_copy_protobuf_cursor_with_large_padding_and_edge_cases() {
    let gen_blob = make_gen_blob("gemini-3-pro", "Gemini 3 Pro (High)", 65_536);
    assert_eq!(
        extract_gen_model(&gen_blob),
        ("gemini-3-pro".into(), "Gemini 3 Pro (High)".into())
    );

    let step_blob = make_step_meta(1_760_000_000, 12_000, 240_000, 1_500, "resp-abc", 2, 3_000);
    let parsed = decode_step_metadata(&step_blob).unwrap();
    assert_eq!(parsed.input, 12_000);
    assert_eq!(parsed.cache_read, 240_000);
    assert_eq!(parsed.cache_write, 3_000);
    assert_eq!(parsed.output, 1_500);
    assert_eq!(parsed.gen_idx, 2);
    assert_eq!(parsed.response_id, "resp-abc");

    // Truncated / malformed inputs must not panic
    assert_eq!(decode_timestamp(&[0x08, 0x80, 0x80]), 0.0);
    assert!(decode_step_metadata(&[0x0a, 0xff, 0xff]).is_none());
    assert_eq!(extract_gen_model(&[0x0a, 0x05, 0x00]), (String::new(), String::new()));
}

#[test]
fn test_ssr_boot_data_injection_xss_escaping_and_digest_dedup() {
    let td = TempDir::new().unwrap();
    let cid = "70000000-0000-0000-0000-000000000007";
    let now = 1_795_342_200.0;
    let db_path = td.path().join("conversations").join(format!("{cid}.db"));
    create_conv_db(
        &db_path,
        &[
            (0, 14, None),
            (1, 15, Some(make_step_meta(now as u64, 50_000, 100_000, 400, "d1", 0, 0))),
        ],
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );
    // Include XSS characters in title to verify safe SSR escaping
    create_summaries_db(
        &td.path().join("conversation_summaries.db"),
        &[(
            cid,
            "</script><img src=x>&test",
            "",
            r#"["file:///workspace/demo"]"#,
            "CASCADE_RUN_STATUS_RUNNING",
            "",
            "",
            0,
            1,
            0,
            iso(now),
            0,
        )],
    );

    let panel_html = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("panel")
        .join("dist")
        .join("panel.html");
    assert!(panel_html.is_file(), "panel/dist/panel.html must exist");
    let raw_bytes = fs::metadata(&panel_html).unwrap().len();
    assert!(
        raw_bytes < 45 * 1024,
        "panel.html must be <45 KiB, got {raw_bytes} bytes"
    );

    let eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(None),
    );
    let grants = RelayGrantStore::new(td.path().join("relay_grants.json"));
    let mut daemon = McpDaemon::new(Some(eng), Some(grants), Some(panel_html));

    // 1) SSR injection for ui://context-radar-mcp/panel
    let r_panel = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": PANEL_URI }
        }))
        .unwrap();
    let html = r_panel["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(html.contains("window.__CONTEXT_RADAR_BOOT__="));
    assert!(html.contains(cid));
    assert!(!html.contains("</script><img"), "raw XSS sequence must be escaped");
    assert!(html.contains("\\u003c/script\\u003e"));

    // 2) SSR hint for ui://context-radar-mcp/handoff
    let r_handoff = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/read",
            "params": { "uri": HANDOFF_URI }
        }))
        .unwrap();
    let h_html = r_handoff["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(h_html.contains(r#"window.__CONTEXT_RADAR_BOOT__={"view":"handoff"};"#));

    // 3) Digest dedup: first call gets full snapshot + digest
    let s1 = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    let sc1 = &s1["result"]["structuredContent"];
    assert_eq!(sc1["unchanged"], json!(false));
    let digest1 = sc1["digest"].as_str().unwrap().to_string();
    assert_eq!(digest1.len(), 16);

    // Second call with matching if_digest returns minimal unchanged: true
    let s2 = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": { "if_digest": digest1 } }
        }))
        .unwrap();
    let sc2 = &s2["result"]["structuredContent"];
    assert_eq!(sc2["unchanged"], json!(true));
    assert_eq!(s2["result"]["content"][0]["text"], json!("unchanged"));

    // After DB update, same if_digest returns full snapshot with a new digest
    thread::sleep(Duration::from_millis(20));
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (2, 15, ?1)",
            params![make_step_meta((now + 10.0) as u64, 5_000, 155_000, 300, "d2", 0, 0)],
        )
        .unwrap();
    }
    let s3 = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": { "if_digest": digest1 } }
        }))
        .unwrap();
    let sc3 = &s3["result"]["structuredContent"];
    assert_eq!(sc3["unchanged"], json!(false));
    assert_ne!(sc3["digest"].as_str().unwrap(), digest1);
}

#[test]
fn test_startup_prewarm_and_swr_stale_while_revalidate() {
    let td = TempDir::new().unwrap();
    let cid = "80000000-0000-0000-0000-000000000008";
    let now = 1_795_342_200.0;
    let db_path = td.path().join("conversations").join(format!("{cid}.db"));
    create_conv_db(
        &db_path,
        &[
            (0, 14, None),
            (1, 15, Some(make_step_meta(now as u64, 45_000, 95_000, 400, "swr1", 0, 0))),
        ],
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );
    create_summaries_db(
        &td.path().join("conversation_summaries.db"),
        &[(
            cid,
            "SWR Warm Session",
            "",
            r#"["file:///workspace/demo"]"#,
            "CASCADE_RUN_STATUS_RUNNING",
            "",
            "",
            0,
            1,
            0,
            iso(now),
            0,
        )],
    );

    let panel_html = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("panel")
        .join("dist")
        .join("panel.html");
    let eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(SettingsWatcher::new(Some("none".into())), None, None)),
        Some(None),
    );
    let grants = RelayGrantStore::new(td.path().join("relay_grants.json"));
    let mut daemon = McpDaemon::new(Some(eng), Some(grants), Some(panel_html));
    daemon.start_prewarm();

    // Wait briefly for startup pre-warm thread to populate RAM cache
    for _ in 0..100 {
        if daemon.swr_is_warm() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(daemon.swr_is_warm(), "background pre-warm should warm snapshot in RAM");

    // 1) Warm resources/read and context_radar_overview({}) should return matching digest instantly
    let r1 = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": { "uri": PANEL_URI }
        }))
        .unwrap();
    let html1 = r1["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(html1.contains(cid));
    assert_eq!(r1["result"]["contents"][0]["_meta"]["ui"]["prefersBorder"], json!(false));

    let o1 = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    let digest1 = o1["result"]["structuredContent"]["digest"].as_str().unwrap().to_string();
    assert!(o1["result"]["structuredContent"].get("stale").is_none());

    // 2) Mutate DB + backdate cache past SWR_STALE_TTL:
    //    Next call MUST refresh synchronously (zero stale delay!) and immediately return the new digest.
    thread::sleep(Duration::from_millis(20));
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (2, 15, ?1)",
            params![make_step_meta((now + 10.0) as u64, 5_000, 160_000, 350, "swr2", 0, 0)],
        )
        .unwrap();
    }
    daemon.backdate_swr_cache(Duration::from_secs(60));

    let o_live = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    let refreshed_digest = o_live["result"]["structuredContent"]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(refreshed_digest, digest1, "expired cache must refresh live immediately without serving stale data");
    assert!(o_live["result"]["structuredContent"].get("stale").is_none());

    // 3) context_radar_ui_action updates prefs and immediately reflects in subsequent SWR reads
    let pref = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "allow_auto_handoff": false } }
        }))
        .unwrap();
    assert_eq!(pref["result"]["structuredContent"]["settings"]["allow_auto_handoff"], json!(false));

    let o_after_pref = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    assert_eq!(o_after_pref["result"]["structuredContent"]["settings"]["allow_auto_handoff"], json!(false));

    daemon.close();
}

#[test]
fn test_security_path_traversal_and_symlink_guards() {
    use context_radar_mcp::engine::grants::sanitize_scratch_note_path;
    use context_radar_mcp::ledger::is_safe_id;

    // 1) is_safe_id must reject traversal & dot-prefixed IDs
    for bad in ["", ".", "..", ".hidden", "a..b", "../secret", "abc/def", "abc\\def"] {
        assert!(!is_safe_id(bad), "expected unsafe id: {bad}");
    }
    assert!(is_safe_id("70000000-0000-0000-0000-000000000007"));

    // 2) sanitize_scratch_note_path rejects traversal, control chars, and symlinks
    assert!(sanitize_scratch_note_path("../../etc/passwd").is_err());
    assert!(sanitize_scratch_note_path("scratch/ok\nInjected").is_err());
    assert!(sanitize_scratch_note_path("").unwrap().is_empty());
    assert_eq!(
        sanitize_scratch_note_path("scratch/slice_1_5.md").unwrap(),
        "scratch/slice_1_5.md"
    );

    let td = TempDir::new().unwrap();
    let real_file = td.path().join("target.md");
    fs::write(&real_file, "secret").unwrap();
    let link_file = td.path().join("link.md");
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();
    assert!(sanitize_scratch_note_path(link_file.to_str().unwrap()).is_err());

    // 3) boot guard rejects relative path traversal and symlinks escaping workspace_root
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("secret.md");
    fs::write(&secret, "TOP_SECRET").unwrap();
    let ws = TempDir::new().unwrap();
    let escape_link = ws.path().join("STATUS.md");
    std::os::unix::fs::symlink(&secret, &escape_link).unwrap();

    let cfg = json!({
        "enabled": true,
        "state_files": [
            { "workspace_root": ws.path().to_string_lossy(), "path": "STATUS.md" },
            { "workspace_root": ws.path().to_string_lossy(), "path": "../outside.md" }
        ]
    });
    assert_eq!(
        context_radar_mcp::guard::boot::evaluate_boot_guard(&json!({ "cwd": ws.path().to_string_lossy() }), &cfg),
        json!({})
    );
}
