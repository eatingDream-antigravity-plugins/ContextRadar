use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::json;
use tempfile::TempDir;

use context_radar_mcp::engine::budget::{
    builtin_tiers, compute_economics, evaluate_handoff_gate, is_gemini_4_carbon, is_gemini_4_model,
    model_cache_ttl_sec, prepare_handoff_plan, resolve_budget_tier,
};
use context_radar_mcp::engine::grants::{build_handoff_prompt, RelayGrantStore};
use context_radar_mcp::engine::pruner::{analyze_hotspots_with_lang, distill_slice};
use context_radar_mcp::engine::rates::RateTable;
use context_radar_mcp::engine::telemetry::{
    build_session_telemetry, classify_cold_cache_turns_detailed, format_turn_rows,
    ColdCacheBreakdown, ColdCacheKind, MidLoopBreakdown, MidLoopSubReason, TelemetryEngine,
};
use context_radar_mcp::i18n::{resolve_lang_from_sources, Lang};
use context_radar_mcp::ledger::journal_tail::JournalTailCache;
use context_radar_mcp::ledger::sqlite_ro::open_host_readonly;
use context_radar_mcp::ledger::TurnSample;
use context_radar_mcp::rpc::protocol::McpDaemon;
use context_radar_mcp::rpc::tools::{summarize_snapshot_text, HANDOFF_URI, PANEL_URI};
use context_radar_mcp::settings::{
    SettingsWatcher, DEFAULT_COMPACT_THRESHOLD, DEFAULT_CONTEXT_LIMIT, DEFAULT_CRITICAL_THRESHOLD,
    DEFAULT_FX_RATE, DEFAULT_WARN_THRESHOLD,
};
use context_radar_mcp::wire::pb_cursor::{
    decode_step_metadata, decode_timestamp, extract_gen_model,
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
fn test_builtin_defaults_and_external_user_config_live_reload() {
    {
        let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
        assert_eq!(DEFAULT_CONTEXT_LIMIT, 512_000);
        assert_eq!(DEFAULT_WARN_THRESHOLD, 300_000);
        assert_eq!(DEFAULT_COMPACT_THRESHOLD, 360_000);
        assert_eq!(DEFAULT_CRITICAL_THRESHOLD, 430_000);

        let p = rates.resolve_rate("gemini-3-pro", "Gemini 3 Pro (High)");
        assert_eq!(
            (p.input_per_m, p.cache_read_per_m, p.output_per_m),
            (2.0, 0.20, 12.0)
        );
        assert_eq!(p.context_limit, 512_000);

        let cl = rates.resolve_rate("claude-sonnet-4", "Claude Sonnet 4");
        assert_eq!(
            (cl.input_per_m, cl.cache_read_per_m, cl.output_per_m),
            (3.0, 0.30, 15.0)
        );

        let (fx, src) = rates.get_fx_rate();
        assert!((fx - DEFAULT_FX_RATE).abs() < 1e-4);
        assert_eq!(src, "default");
    }

    let td = TempDir::new().unwrap();
    let ext_fx = td.path().join("fx.json");
    fs::write(
        &ext_fx,
        serde_json::to_string(&json!({"rate": 6.85})).unwrap(),
    )
    .unwrap();

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
    assert_eq!(
        (
            p_alpha.input_per_m,
            p_alpha.cache_read_per_m,
            p_alpha.output_per_m
        ),
        (4.0, 0.20, 20.0)
    );
    assert!(!rates.external_models_loaded());
    assert_eq!(
        rates.short_model_label("Custom Alpha (High)", "custom-alpha-high"),
        "Alpha"
    );

    // Miss triggers lazy external catalog load
    let p_ext = rates.resolve_rate("fallback-catalog-model", "");
    assert_eq!(
        (p_ext.input_per_m, p_ext.output_per_m, p_ext.context_limit),
        (1.2, 6.0, 400_000)
    );
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
            thinking_tokens: 0,
            response_tokens: 553,
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
            thinking_tokens: 0,
            response_tokens: 5878,
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
    assert!(tiers.contains_key("carbon_heavy"));
    assert!(tiers.contains_key("high_reasoning"));
    assert!(tiers.contains_key("flagship"));
    assert!(tiers.contains_key("standard"));
    assert!(tiers.contains_key("fast_reasoning"));
    assert!(tiers.contains_key("fast"));
    assert!(tiers.contains_key("claude_tier"));

    let cfg = rates.current_settings();
    let carbon_t = resolve_budget_tier("gemini-4-carbon-high", "Gemini 4 Carbon (High)", &cfg);
    let neon_xh = resolve_budget_tier("gemini-4-neon-xhigh", "Gemini 4 Neon (XHigh)", &cfg);
    let neon_h = resolve_budget_tier("gemini-4-neon-high", "Gemini 4 Neon (High)", &cfg);
    let hydro_xh =
        resolve_budget_tier("gemini-4-hydrogen-xhigh", "Gemini 4 Hydrogen (XHigh)", &cfg);
    let hydro_m = resolve_budget_tier(
        "gemini-4-hydrogen-medium",
        "Gemini 4 Hydrogen (Medium)",
        &cfg,
    );
    let claude_t = resolve_budget_tier("claude-opus-5-5-high", "Claude Opus 5.5", &cfg);
    let std_t = resolve_budget_tier("gpt-oss-120b-medium", "GPT OSS 120B (Medium)", &cfg);

    assert_eq!(
        (
            carbon_t.min_ctx,
            carbon_t.recommend_ctx,
            carbon_t.urgent_ctx
        ),
        (260_000, 320_000, 400_000)
    );
    assert_eq!(
        (neon_xh.min_ctx, neon_xh.recommend_ctx, neon_xh.urgent_ctx),
        (300_000, 360_000, 430_000)
    );
    assert_eq!(
        (neon_h.min_ctx, neon_h.recommend_ctx, neon_h.urgent_ctx),
        (310_000, 370_000, 440_000)
    );
    assert_eq!(
        (std_t.min_ctx, std_t.recommend_ctx, std_t.urgent_ctx),
        (300_000, 360_000, 430_000)
    );
    assert_eq!(
        (
            hydro_xh.min_ctx,
            hydro_xh.recommend_ctx,
            hydro_xh.urgent_ctx
        ),
        (320_000, 380_000, 445_000)
    );
    assert_eq!(
        (hydro_m.min_ctx, hydro_m.recommend_ctx, hydro_m.urgent_ctx),
        (340_000, 400_000, 460_000)
    );
    assert_eq!(
        (
            claude_t.min_ctx,
            claude_t.recommend_ctx,
            claude_t.urgent_ctx
        ),
        (300_000, 360_000, 440_000)
    );
    // Strict tier ordering: carbon_heavy < high_reasoning < flagship < fast_reasoning < fast
    assert!(carbon_t.urgent_ctx < neon_xh.urgent_ctx);
    assert!(neon_xh.urgent_ctx < neon_h.urgent_ctx);
    assert!(neon_h.urgent_ctx < hydro_xh.urgent_ctx);
    assert!(hydro_xh.urgent_ctx < hydro_m.urgent_ctx);

    assert_eq!(
        resolve_budget_tier("gemini-3-pro", "Gemini 3 Pro (XHigh)", &cfg).min_ctx,
        300_000
    );
    assert_eq!(
        resolve_budget_tier("gemini-3-pro", "Gemini 3 Pro (High)", &cfg).min_ctx,
        310_000
    );
    assert_eq!(
        resolve_budget_tier("gemini-3-flash", "Gemini 3 Flash", &cfg).min_ctx,
        340_000
    );

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
    assert!(sub_gate
        .reasons
        .iter()
        .any(|r| r.contains("切棒") || r.contains("baton")));
    assert_eq!(
        prepare_handoff_plan(Some(&sub), 12, 0, &mut rates).action,
        "subagent_handoff"
    );

    // 3) Block when running_subagent_count > 0 (even at urgent context!)
    let mut busy = base.clone();
    busy.latest_context_tokens = 450_000;
    busy.running_subagent_count = 2;
    let busy_gate = evaluate_handoff_gate(Some(&busy), 20, &mut rates);
    assert!(!busy_gate.allowed);
    assert!(busy_gate
        .reasons
        .iter()
        .any(|r| r.contains("running_subagent_count == 0")));

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
        (
            1,
            15,
            Some(make_step_meta(base_ts, 40_000, 0, 500, "r1", 0, 0)),
        ),
        (
            2,
            132,
            Some(make_step_meta(base_ts + 5, 1, 1, 1, "ignored-tool", 0, 0)),
        ),
        (3, 15, None), // In-flight step with NULL metadata
    ];
    create_conv_db(
        &db_path,
        &initial_steps,
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );

    let mut eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
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
            params![make_gen_blob(
                "gemini-3-flash",
                "Gemini 3 Flash (High)",
                4096
            )],
        )
        .unwrap();
        conn.execute(
            "UPDATE steps SET metadata = ?1 WHERE idx = 3",
            params![make_step_meta(base_ts + 15, 5_000, 42_000, 400, "r2", 0, 0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (4, 23, ?1)",
            params![make_step_meta(
                base_ts + 30,
                2_000,
                46_000,
                3_000,
                "r3",
                0,
                0
            )],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (5, 14, NULL)",
            [],
        )
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
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
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
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
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
            (
                1,
                15,
                Some(make_step_meta(base_ts, 30_000, 0, 400, "w1", 0, 0)),
            ),
        ],
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );

    // Switch host DB into WAL mode and hold an active writer that appends to -wal without checkpoint
    let writer = Connection::open(&db_path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();

    let mut eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
        Some(None),
    );
    assert_eq!(eng.ledger.read_turns(cid, None).len(), 1);

    // Verify open_host_readonly refuses any mutation
    let ro = open_host_readonly(&db_path).unwrap();
    assert!(ro.execute("CREATE TABLE forbidden (x INT)", []).is_err());
    assert!(ro
        .execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (99, 15, NULL)",
            []
        )
        .is_err());
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
    assert_eq!(
        fs::read(&db_path).unwrap(),
        before_db_bytes,
        "Host .db must remain unmodified"
    );
}

#[test]
fn test_sqlite_row_read_error_preserves_incremental_cursor_and_caches() {
    use context_radar_mcp::ledger::index_db::IndexDbCache;
    use context_radar_mcp::ledger::sqlite_ro::{stat_db_stamp, LedgerReader};

    let td = TempDir::new().unwrap();
    let root = td.path().join("host");
    let cid = "31000000-0000-0000-0000-000000000032";
    let index_path = td.path().join("plugin").join("ledger.db");
    let mut ledger = LedgerReader::new(root.clone(), Some(index_path.clone()));
    fs::create_dir_all(&ledger.conv_dir).unwrap();
    let db_path = ledger.db_path(cid);
    let writer = Connection::open(&db_path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
         CREATE TABLE raw_steps (idx INTEGER PRIMARY KEY, metadata BLOB, bad INTEGER);
         CREATE VIEW steps AS SELECT idx, 15 AS step_type,
             CASE WHEN bad = 1 THEN json_extract('malformed', '$') ELSE metadata END AS metadata
             FROM raw_steps;",
        )
        .unwrap();

    let append = |idx: i64, bad: i32| {
        writer
            .execute(
                "INSERT INTO raw_steps VALUES (?1, ?2, ?3)",
                params![
                    idx,
                    make_step_meta(
                        1_790_100_000 + idx as u64,
                        30_000,
                        0,
                        400,
                        &format!("step-{idx}"),
                        0,
                        0
                    ),
                    bad
                ],
            )
            .unwrap();
    };
    append(1, 0);
    let (before, _) = ledger.try_read_turns_and_probe(cid, None).unwrap();
    let initial_stamp = stat_db_stamp(&db_path).unwrap();
    assert_eq!(
        before.iter().map(|turn| turn.step_idx).collect::<Vec<_>>(),
        [1]
    );

    append(2, 0);
    append(3, 1);
    append(20, 0);
    let failed_stamp = stat_db_stamp(&db_path).unwrap();
    assert_ne!(initial_stamp.sig, failed_stamp.sig);

    // SQLite emits the new row at idx=2 before failing when it evaluates idx=3.
    let ro = open_host_readonly(&db_path).unwrap();
    let mut stmt = ro
        .prepare("SELECT idx, metadata FROM steps ORDER BY idx")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    assert_eq!(rows.next().unwrap().unwrap().get::<_, i64>(0).unwrap(), 1);
    assert_eq!(rows.next().unwrap().unwrap().get::<_, i64>(0).unwrap(), 2);
    assert!(rows.next().is_err());
    drop(rows);
    drop(stmt);
    drop(ro);

    let err = ledger
        .try_read_turns_and_probe(cid, Some(&failed_stamp))
        .unwrap_err();
    assert!(err.to_string().contains("malformed JSON"), "{err}");
    assert!(std::sync::Arc::ptr_eq(
        &before,
        &ledger.read_turns(cid, None)
    ));
    assert!(LedgerReader::new(root, None)
        .try_read_turns_and_probe(cid, None)
        .is_err());

    let mut index = IndexDbCache::new(index_path);
    let persisted = index
        .load(cid, initial_stamp.dev, initial_stamp.ino)
        .unwrap();
    assert_eq!(persisted.max_step_idx, 1);
    assert_eq!(persisted.sig, initial_stamp.sig);
    assert_eq!(persisted.turns.as_ref(), before.as_ref());

    writer
        .execute("UPDATE raw_steps SET bad = 0 WHERE idx = 3", [])
        .unwrap();
    let (recovered, _) = ledger.try_read_turns_and_probe(cid, None).unwrap();
    assert_eq!(
        recovered
            .iter()
            .map(|turn| turn.step_idx)
            .collect::<Vec<_>>(),
        [1, 2, 3, 20]
    );
    assert_eq!(
        index
            .load(cid, initial_stamp.dev, initial_stamp.ino)
            .unwrap()
            .max_step_idx,
        20
    );
}

#[test]
fn test_journal_tail_backward_checkpoint_seek_and_incremental_append() {
    let td = TempDir::new().unwrap();
    let cid = "40000000-0000-0000-0000-000000000004";
    let tdir = td
        .path()
        .join("brain")
        .join(cid)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&tdir).unwrap();
    let tpath = tdir.join("transcript.jsonl");

    // Write > 300 KiB pre-checkpoint noise (exceeding 256 KiB backward chunk!) + CHECKPOINT + post steps
    let mut content = String::new();
    for i in 1..=65 {
        content.push_str(
            &serde_json::to_string(&json!({
                "step_index": i,
                "type": "PLANNER_RESPONSE",
                "content": format!("old step {i} {}", "A".repeat(5000))
            }))
            .unwrap(),
        );
        content.push('\n');
    }
    content.push_str(
        &serde_json::to_string(
            &json!({"step_index": 66, "type": "CHECKPOINT", "content": "summary"}),
        )
        .unwrap(),
    );
    content.push('\n');
    content.push_str(
        &serde_json::to_string(
            &json!({"step_index": 67, "type": "USER_INPUT", "content": "continue task"}),
        )
        .unwrap(),
    );
    content.push('\n');
    content.push_str(
        &serde_json::to_string(&json!({
            "step_index": 68,
            "type": "PLANNER_RESPONSE",
            "content": "checked module A",
            "tool_calls": [{ "name": "read_file", "args": { "Path": "/repo/app.rs" } }]
        }))
        .unwrap(),
    );
    content.push('\n');
    fs::write(&tpath, &content).unwrap();

    let mut jcache = JournalTailCache::default();
    let win1 = jcache.get_active_window(td.path(), cid);
    let idx1: Vec<i64> = win1.iter().map(|e| e.step_index).collect();
    assert_eq!(idx1, vec![67, 68]);

    // Append step 69 -> incremental tail read adds only step 69
    thread::sleep(Duration::from_millis(20));
    content.push_str(
        &serde_json::to_string(&json!({
            "step_index": 69,
            "type": "PLANNER_RESPONSE",
            "status": "ERROR",
            "content": "sample error msg"
        }))
        .unwrap(),
    );
    content.push('\n');
    fs::write(&tpath, &content).unwrap();

    let win2 = jcache.get_active_window(td.path(), cid);
    let idx2: Vec<i64> = win2.iter().map(|e| e.step_index).collect();
    assert_eq!(idx2, vec![67, 68, 69]);

    let dist = distill_slice(
        td.path(),
        cid,
        &[],
        "distill",
        None,
        None,
        true,
        8,
        &mut jcache,
    );
    assert!(dist.found);
    assert_eq!(dist.slice_range, Some([67, 68]));
    let note_path = dist.saved_note_path.unwrap();
    assert!(Path::new(&note_path).is_file());

    let stypes = jcache.get_step_types(td.path(), cid);
    assert_eq!(stypes.get(&66).map(String::as_str), Some("CHECKPOINT"));
    assert_eq!(stypes.get(&67).map(String::as_str), Some("USER_INPUT"));
    assert_eq!(
        stypes.get(&69).map(String::as_str),
        Some("PLANNER_RESPONSE")
    );
}

#[test]
fn test_distill_note_rejects_symlinks_and_atomically_replaces_file() {
    use std::io::Read;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    let td = TempDir::new().unwrap();
    let root = td.path().canonicalize().unwrap();
    let cid = "32000000-0000-0000-0000-000000000032";
    let brain = root.join("brain");
    let conversation = brain.join(cid);
    let scratch = conversation.join("scratch");
    let transcript = conversation.join(".system_generated/logs/transcript.jsonl");
    let note = scratch.join("slice_1_1.md");
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    let outside_note = outside.join("slice_1_1.md");
    fs::write(&outside_note, "untouched").unwrap();
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, "{\"step_index\":1,\"type\":\"USER_INPUT\",\"content\":\"Keep this\",\"status\":\"DONE\"}\n").unwrap();
    let distill = || {
        distill_slice(
            &root,
            cid,
            &[],
            "distill",
            Some(1),
            Some(1),
            true,
            8,
            &mut JournalTailCache::default(),
        )
    };

    for dir in [&brain, &conversation] {
        let moved = dir.with_extension("moved");
        fs::rename(dir, &moved).unwrap();
        symlink(&moved, dir).unwrap();
        let report = distill();
        assert!(report.found);
        assert!(report.digest.is_some());
        assert!(report.error.is_some());
        assert_eq!(report.saved_note_path.as_deref(), Some(""));
        fs::remove_file(dir).unwrap();
        fs::rename(moved, dir).unwrap();
    }

    symlink(&outside, &scratch).unwrap();
    let scratch_err = distill();
    assert!(scratch_err.found);
    assert!(scratch_err.digest.is_some());
    assert!(scratch_err.error.is_some());
    assert_eq!(scratch_err.saved_note_path.as_deref(), Some(""));
    fs::remove_file(&scratch).unwrap();
    fs::create_dir(&scratch).unwrap();

    symlink(&outside_note, &note).unwrap();
    let note_err = distill();
    assert!(note_err.found);
    assert!(note_err.digest.is_some());
    assert!(note_err.error.is_some());
    assert_eq!(note_err.saved_note_path.as_deref(), Some(""));
    assert!(fs::symlink_metadata(&note).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(&outside_note).unwrap(), "untouched");
    fs::remove_file(&note).unwrap();

    fs::write(&note, "old note").unwrap();
    fs::set_permissions(&note, fs::Permissions::from_mode(0o644)).unwrap();
    let mut old = fs::File::open(&note).unwrap();
    let report = distill();
    assert!(report.error.is_none());
    assert_eq!(report.saved_note_path.as_deref(), note.to_str());
    let meta = fs::metadata(&note).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o600);
    assert_ne!(meta.ino(), old.metadata().unwrap().ino());
    let mut old_body = String::new();
    old.read_to_string(&mut old_body).unwrap();
    assert_eq!(old_body, "old note");
    let body = fs::read_to_string(&note).unwrap();
    assert!(body.ends_with(&format!("{}\n", report.digest.unwrap())));
    assert_eq!(fs::read_dir(scratch).unwrap().count(), 1);
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
        (
            1,
            15,
            Some(make_step_meta(
                (now - 500.0) as u64,
                40_000,
                0,
                500,
                "r1",
                0,
                0,
            )),
        ),
        (2, 132, None),
        (
            3,
            15,
            Some(make_step_meta(
                (now - 480.0) as u64,
                65_000,
                0,
                500,
                "r2",
                0,
                0,
            )),
        ),
        (4, 132, None),
        (
            5,
            15,
            Some(make_step_meta(
                (now - 460.0) as u64,
                3_000,
                68_000,
                400,
                "r3",
                0,
                0,
            )),
        ),
        (6, 101, None),
        (
            7,
            15,
            Some(make_step_meta(
                (now - 100.0) as u64,
                75_000,
                0,
                400,
                "r4",
                0,
                0,
            )),
        ),
        (
            8,
            23,
            Some(make_step_meta(
                (now - 80.0) as u64,
                2_000,
                75_000,
                3_500,
                "r5",
                0,
                0,
            )),
        ),
        (9, 14, None),
        (
            10,
            15,
            Some(make_step_meta(
                (now - 30.0) as u64,
                28_000,
                0,
                300,
                "r6",
                0,
                0,
            )),
        ),
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
            (
                1,
                15,
                Some(make_step_meta(
                    ts as u64,
                    ctx,
                    0,
                    300,
                    &format!("r-{}", ts as u64),
                    0,
                    0,
                )),
            ),
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
        create_conv_db(
            &p,
            &one_step(ts, ctx),
            "gemini-3-pro",
            "Gemini 3 Pro (High)",
        );
        // Set mtime to match ts so stale subagent exceeds silent_timeout_sec
        let ft = filetime_set(&p, ts as i64);
        assert!(ft);
    }
    assert!(filetime_set(
        &conv_dir.join(format!("{main_active}.db")),
        (now - 30.0) as i64
    ));

    create_summaries_db(
        &td.path().join("conversation_summaries.db"),
        &[
            (
                main_active,
                "Active Session",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_RUNNING",
                "",
                "",
                0,
                1,
                0,
                iso(now - 30.0),
                0,
            ),
            (
                main_idle,
                "Idle Session",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_IDLE",
                "",
                "",
                0,
                0,
                0,
                iso(now - 600.0),
                0,
            ),
            (
                sub_running,
                "",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_RUNNING",
                "worker-run",
                main_active,
                1,
                1,
                0,
                iso(now - 10.0),
                -1,
            ),
            (
                sub_parked,
                "",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_IDLE",
                "worker-park",
                main_active,
                1,
                1,
                0,
                iso(now - 300.0),
                -1,
            ),
            (
                sub_idle,
                "",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_IDLE",
                "browser",
                main_active,
                1,
                0,
                0,
                iso(now - 18_000.0),
                -1,
            ),
            (
                sub_stale,
                "",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_IDLE",
                "worker-stale",
                main_active,
                1,
                1,
                0,
                iso(now - 7_200.0),
                -1,
            ),
            (
                sub_killed,
                "",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_IDLE",
                "worker-dead",
                main_active,
                1,
                0,
                1,
                iso(now - 1_200.0),
                -1,
            ),
        ],
    );

    let mut eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
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

    let fast = eng
        .resolve_session_fast(&main_active[..10], Some(now))
        .unwrap();
    assert_eq!(fast.id, main_active);
    assert_eq!(fast.running_subagent_count, 2);
}

#[repr(C)]
struct Timespec {
    tv_sec: core::ffi::c_long,
    tv_nsec: core::ffi::c_long,
}
#[cfg(target_os = "macos")]
const AT_FDCWD_PLAT: i32 = -2;
#[cfg(not(target_os = "macos"))]
const AT_FDCWD_PLAT: i32 = -100;

extern "C" {
    fn utimensat(
        dirfd: i32,
        path: *const core::ffi::c_char,
        times: *const Timespec,
        flag: i32,
    ) -> i32;
}
fn filetime_set(path: &Path, sec: i64) -> bool {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
    let ts = [
        Timespec {
            tv_sec: sec as core::ffi::c_long,
            tv_nsec: 0,
        },
        Timespec {
            tv_sec: sec as core::ffi::c_long,
            tv_nsec: 0,
        },
    ];
    unsafe { utimensat(AT_FDCWD_PLAT, c.as_ptr(), ts.as_ptr(), 0) == 0 }
}

#[test]
fn test_relay_grant_lifecycle_anti_replay_and_workspace_scoping() {
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

    let grant = gs
        .create_grant(
            "Keep finding A",
            "Run regression suite",
            true,
            "50000000-0000-0000-0000-000000000005",
            40,
            0,
            "handoff",
            "",
            "Demo Session",
        )
        .unwrap();

    gs.update_settings(Some(false)).unwrap();
    assert_eq!(
        gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(0), true)
            .reason
            .as_deref(),
        Some("auto_handoff_disabled")
    );

    gs.update_settings(Some(true)).unwrap();
    assert_eq!(
        gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(1), true)
            .reason
            .as_deref(),
        Some("running_subagents_active")
    );

    let ok = gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(0), true);
    assert!(ok.claimed);

    // Anti-replay check
    assert_eq!(
        gs.claim_grant(&grant.grant_id, Some(40), Some(0), Some(0), false)
            .reason
            .as_deref(),
        Some("already_claimed")
    );

    let rec = gs.record_relay(&grant.grant_id, "new-session-999");
    assert!(rec.ok);
    assert_eq!(
        rec.handoff_conversation_id.as_deref(),
        Some("new-session-999")
    );

    let dis = gs.dismiss_grant(&grant.grant_id);
    assert!(dis.ok);
    assert!(gs.get_pending_grant(None).is_none());

    // Scoped grants reject cross-workspace and cross-conversation claims
    let cid_a = "92000000-0000-0000-0000-000000000011";
    let scoped = gs
        .create_grant_scoped(
            "Keep A",
            "Finish A",
            true,
            cid_a,
            "/workspace/project-a",
            &["/workspace/project-a".to_string()],
            30,
            0,
            "handoff",
            "",
            "Project A",
            Lang::En,
        )
        .unwrap();

    assert!(gs
        .get_pending_grant_scoped(None, Some("/workspace/project-b"), None)
        .is_none());
    assert!(gs
        .get_pending_grant_scoped(None, Some("/workspace/project-a"), None)
        .is_some());

    let wrong_cid = gs.claim_grant_scoped(
        &scoped.grant_id,
        Some("92000000-0000-0000-0000-000000000099"),
        Some("/workspace/project-a"),
        Some(30),
        Some(0),
        Some(0),
        true,
    );
    assert!(!wrong_cid.claimed);
    assert_eq!(wrong_cid.reason.as_deref(), Some("conversation_mismatch"));

    let wrong_ws = gs.claim_grant_scoped(
        &scoped.grant_id,
        Some(cid_a),
        Some("/workspace/project-b"),
        Some(30),
        Some(0),
        Some(0),
        true,
    );
    assert!(!wrong_ws.claimed);
    assert_eq!(wrong_ws.reason.as_deref(), Some("workspace_mismatch"));

    let scoped_ok = gs.claim_grant_scoped(
        &scoped.grant_id,
        Some(cid_a),
        Some("/workspace/project-a"),
        Some(30),
        Some(0),
        Some(0),
        true,
    );
    assert!(scoped_ok.claimed);
}

#[test]
fn test_grant_lock_failure_withholds_instructions_and_allows_one_retry() {
    use std::os::unix::fs::symlink;

    let td = TempDir::new().unwrap();
    let store_path = td.path().join("relay_grants.json");
    let lock_path = td.path().join("relay_grants.json.lock");
    let mut store = RelayGrantStore::new(store_path.clone());
    let create = |store: &mut RelayGrantStore| {
        store.create_grant("Keep state", "Resume", true, "", 0, 0, "handoff", "", "")
    };

    let grant = create(&mut store).unwrap();
    let before = fs::read(&store_path).unwrap();
    fs::remove_file(&lock_path).unwrap();
    symlink(&store_path, &lock_path).unwrap();
    assert!(create(&mut store).is_err());
    let failed = store.claim_grant(&grant.grant_id, None, None, None, false);
    assert!(!failed.claimed);
    assert_eq!(failed.reason.as_deref(), Some("lock_error"));
    assert!(failed.handoff_prompt.is_none());
    assert!(failed.compact_command.is_none());
    assert_eq!(fs::read(&store_path).unwrap(), before);

    fs::remove_file(&lock_path).unwrap();
    let mut retry = RelayGrantStore::new(store_path.clone());
    assert!(
        retry
            .claim_grant(&grant.grant_id, None, None, None, false)
            .claimed
    );
    let replay =
        RelayGrantStore::new(store_path).claim_grant(&grant.grant_id, None, None, None, false);
    assert!(!replay.claimed);
    assert_eq!(replay.reason.as_deref(), Some("already_claimed"));
    assert!(replay.handoff_prompt.is_none());
    assert!(replay.compact_command.is_none());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn test_grant_save_failure_withholds_instructions_and_preserves_replay_protection() {
    #[cfg(target_os = "macos")]
    const PATH_LIMIT: usize = 1024;
    #[cfg(target_os = "linux")]
    const PATH_LIMIT: usize = 4096;

    let td = TempDir::new().unwrap();
    let root = td.path().canonicalize().unwrap();
    let path = root.join("plugin/g");
    let mut store = RelayGrantStore::new(path.clone());
    let grant = store
        .create_grant("Keep state", "Resume", true, "", 0, 0, "handoff", "", "")
        .unwrap();
    let before = fs::read(&path).unwrap();

    // The padded path reaches the same store and lock. The longer temporary
    // filename exceeds the OS path limit, so saving fails before replacement.
    let mut padded = root;
    while padded.as_os_str().len() < PATH_LIMIT - 22 {
        padded.push(".");
    }
    let mut fault = RelayGrantStore::new(padded.join("plugin/g"));
    assert!(fault.find_grant(&grant.grant_id).is_some());
    let failed = fault.claim_grant(&grant.grant_id, None, None, None, false);
    assert!(!failed.claimed);
    assert_eq!(failed.reason.as_deref(), Some("storage_error"));
    assert!(failed.compact_command.is_none());
    assert!(failed.handoff_prompt.is_none());
    assert_eq!(fs::read(&path).unwrap(), before);

    let retry = store.claim_grant(&grant.grant_id, None, None, None, false);
    assert!(retry.claimed);
    assert!(retry.handoff_prompt.is_some());
    let replay = RelayGrantStore::new(path).claim_grant(&grant.grant_id, None, None, None, false);
    assert!(!replay.claimed);
    assert_eq!(replay.reason.as_deref(), Some("already_claimed"));
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
    assert_eq!(
        extract_gen_model(&[0x0a, 0x05, 0x00]),
        (String::new(), String::new())
    );
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
            (
                1,
                15,
                Some(make_step_meta(now as u64, 50_000, 100_000, 400, "d1", 0, 0)),
            ),
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
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
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
    assert!(
        !html.contains("</script><img"),
        "raw XSS sequence must be escaped"
    );
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
    assert!(h_html.contains("window.__CONTEXT_RADAR_BOOT__={"));
    assert!(h_html.contains(r#""view":"handoff""#));
    assert!(h_html.contains(r#""lang":"#));

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
            params![make_step_meta(
                (now + 10.0) as u64,
                5_000,
                155_000,
                300,
                "d2",
                0,
                0
            )],
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
            (
                1,
                15,
                Some(make_step_meta(
                    now as u64, 45_000, 95_000, 400, "swr1", 0, 0,
                )),
            ),
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
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
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
    assert!(
        daemon.swr_is_warm(),
        "background pre-warm should warm snapshot in RAM"
    );

    // 1) Warm resources/read and context_radar_overview({}) should return matching digest instantly
    let r1 = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": { "uri": PANEL_URI }
        }))
        .unwrap();
    let html1 = r1["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(html1.contains(cid));
    assert_eq!(
        r1["result"]["contents"][0]["_meta"]["ui"]["prefersBorder"],
        json!(false)
    );

    let o1 = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    let digest1 = o1["result"]["structuredContent"]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(o1["result"]["structuredContent"].get("stale").is_none());

    // 2) Mutate DB + backdate cache past SWR_STALE_TTL:
    //    Next call MUST refresh synchronously (zero stale delay!) and immediately return the new digest.
    thread::sleep(Duration::from_millis(20));
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (2, 15, ?1)",
            params![make_step_meta(
                (now + 10.0) as u64,
                5_000,
                160_000,
                350,
                "swr2",
                0,
                0
            )],
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
    assert_ne!(
        refreshed_digest, digest1,
        "expired cache must refresh live immediately without serving stale data"
    );
    assert!(o_live["result"]["structuredContent"].get("stale").is_none());

    // 3) context_radar_ui_action updates prefs and immediately reflects in subsequent SWR reads
    daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 500, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "extensions": {
                    "io.modelcontextprotocol/ui": { "mimeTypes": ["text/html;profile=mcp-app"] }
                } }
            }
        }))
        .unwrap();
    let pref = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "allow_auto_handoff": false } }
        }))
        .unwrap();
    assert_eq!(
        pref["result"]["structuredContent"]["settings"]["allow_auto_handoff"],
        json!(false)
    );

    let o_after_pref = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    assert_eq!(
        o_after_pref["result"]["structuredContent"]["settings"]["allow_auto_handoff"],
        json!(false)
    );

    // 3) Mutate DB WITHOUT backdating SWR cache: live per-conversation disk activity stamp
    //    MUST invalidate burst cache immediately and resources/read MUST always serve fresh SSR data!
    thread::sleep(Duration::from_millis(20));
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO steps (idx, step_type, metadata) VALUES (3, 15, ?1)",
            params![make_step_meta(
                (now + 20.0) as u64,
                4_000,
                175_000,
                320,
                "swr3",
                0,
                0
            )],
        )
        .unwrap();
    }
    let o_instant = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": {} }
        }))
        .unwrap();
    let instant_digest = o_instant["result"]["structuredContent"]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(
        instant_digest, refreshed_digest,
        "new DB/WAL write must immediately invalidate burst cache without waiting for SWR_STALE_TTL"
    );
    let r_open = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "resources/read",
            "params": { "uri": PANEL_URI }
        }))
        .unwrap();
    let html_open = r_open["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(
        html_open.contains(&instant_digest),
        "opening panel via resources/read must immediately embed the live latest digest"
    );

    daemon.close();
}

#[test]
fn test_security_path_traversal_and_symlink_guards() {
    use context_radar_mcp::engine::grants::sanitize_scratch_note_path;
    use context_radar_mcp::ledger::is_safe_id;

    // 1) is_safe_id must reject traversal & dot-prefixed IDs
    for bad in [
        "",
        ".",
        "..",
        ".hidden",
        "a..b",
        "../secret",
        "abc/def",
        "abc\\def",
    ] {
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
        context_radar_mcp::guard::boot::evaluate_boot_guard(
            &json!({ "cwd": ws.path().to_string_lossy() }),
            &cfg
        ),
        json!({})
    );
}

#[test]
fn test_live_antigravity_models_unpriced_break_even_and_i18n() {
    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
    let cfg = rates.current_settings();

    for (id, disp, exp_tier) in [
        ("gemini-4-neon-high", "Gemini 4 Neon (High)", "flagship"),
        (
            "gemini-4-neon-xhigh",
            "Gemini 4 Neon (XHigh)",
            "high_reasoning",
        ),
        (
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            "carbon_heavy",
        ),
        (
            "gemini-4-carbon-xhigh",
            "Gemini 4 Carbon (XHigh)",
            "carbon_heavy",
        ),
        (
            "gemini-4-argon-medium",
            "Gemini 4 Argon (Medium)",
            "carbon_heavy",
        ),
        (
            "gemini-4-hydrogen-medium",
            "Gemini 4 Hydrogen (Medium)",
            "fast",
        ),
        (
            "gemini-4-hydrogen-xhigh",
            "Gemini 4 Hydrogen (XHigh)",
            "fast_reasoning",
        ),
        ("gemini-3.8-flash-high", "Gemini 3.8 Flash (High)", "fast"),
        (
            "gemini-pro-orchestrator-neon",
            "Gemini Pro Orchestrator",
            "flagship",
        ),
        ("claude-opus-5-5-high", "Claude Opus 5.5", "claude_tier"),
        (
            "claude-sonnet-5-5-medium",
            "Claude Sonnet 5.5",
            "claude_tier",
        ),
        ("gpt-oss-120b-medium", "GPT OSS 120B (Medium)", "standard"),
    ] {
        let card = rates.resolve_rate(id, disp);
        assert!(card.pricing_known, "expected known pricing for {id}");
        assert_eq!(
            resolve_budget_tier(id, disp, &cfg).profile_id,
            exp_tier,
            "tier mismatch for {id}"
        );
    }

    // Unknown brand-new model must NEVER deadlock at break_even_turns = 999
    let unknown_card = rates.resolve_rate("future-brand-new-model-9", "Future Brand New");
    assert!(!unknown_card.pricing_known);
    let econ = compute_economics(
        320_000,
        "future-brand-new-model-9",
        "Future Brand New",
        &mut rates,
    );
    assert!(!econ.pricing_known);
    assert!(
        (2..=12).contains(&econ.break_even_turns),
        "expected token-physics break-even in 2..=12, got {}",
        econ.break_even_turns
    );

    let mut s = build_session_telemetry(
        "90000000-0000-0000-0000-000000000009",
        None,
        &[],
        100.0,
        7.2,
        &mut rates,
        120.0,
        None,
    );
    s.model = "future-brand-new-model-9".into();
    s.model_display = "Future Brand New".into();
    s.latest_context_tokens = 340_000;
    s.turns = 30;
    s.turns_since_compact = 30;
    let gate = evaluate_handoff_gate(Some(&s), 12, &mut rates);
    assert!(
        gate.allowed,
        "unpriced model at 340k should pass handoff gate: {:?}",
        gate.reasons
    );

    // i18n locale & currency resolution
    assert_eq!(
        resolve_lang_from_sources("", None, Some("zh_CN.UTF-8"), None, None, None),
        Lang::ZhCn
    );
    assert_eq!(
        resolve_lang_from_sources("", None, Some("en_US.UTF-8"), None, None, None),
        Lang::En
    );
    assert_eq!(Lang::ZhCn.default_currency(), ("CNY", "¥"));
    assert_eq!(Lang::En.default_currency(), ("USD", "$"));
}

#[test]
fn test_hotspot_attribution_excludes_outgoing_step_and_detects_partial_cold() {
    let td = TempDir::new().unwrap();
    let cid = "91000000-0000-0000-0000-000000000010";
    let log_dir = td
        .path()
        .join("brain")
        .join(cid)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&log_dir).unwrap();
    let transcript = [
        json!({
            "step_index": 1,
            "type": "PLANNER_RESPONSE",
            "tool_calls": [{"name": "run_command", "arguments": {"CommandLine": "cargo test --workspace"}}]
        }),
        json!({
            "step_index": 2,
            "type": "PLANNER_RESPONSE",
            "tool_calls": [{"name": "read_file", "arguments": {"Path": "/tmp/unexecuted_outgoing.rs"}}]
        }),
    ]
    .iter()
    .map(|v| serde_json::to_string(v).unwrap())
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(log_dir.join("transcript.jsonl"), transcript).unwrap();

    let turns = vec![
        TurnSample {
            turn_index: 1,
            step_idx: 1,
            timestamp: 100.0,
            model: "gemini-4-carbon-high".into(),
            model_display: "Gemini 4 Carbon (High)".into(),
            input_tokens: 10_000,
            cache_read: 90_000,
            cache_write: 0,
            output_tokens: 200,
            thinking_tokens: 0,
            response_tokens: 200,
            ttft_ms: 1000,
            total_ms: 2000,
            response_id: "r1".into(),
            step_type: 15,
            is_checkpoint: false,
            after_turn_start: Some(false),
            after_checkpoint: false,
        },
        TurnSample {
            turn_index: 2,
            step_idx: 2,
            timestamp: 105.0,
            model: "gemini-4-carbon-high".into(),
            model_display: "Gemini 4 Carbon (High)".into(),
            input_tokens: 5_000,
            cache_read: 30_000,
            cache_write: 95_000,
            output_tokens: 300,
            thinking_tokens: 0,
            response_tokens: 300,
            ttft_ms: 4000,
            total_ms: 5000,
            response_id: "r2".into(),
            step_type: 15,
            is_checkpoint: false,
            after_turn_start: Some(false),
            after_checkpoint: false,
        },
    ];

    let mut jcache = JournalTailCache::new(16);
    let rep = analyze_hotspots_with_lang(td.path(), cid, &turns, 5, &mut jcache, Lang::En);
    assert!(rep.found);
    let hotspots = rep.hotspots.unwrap();
    assert_eq!(hotspots.len(), 1);
    assert!(
        hotspots[0].cause.contains("run_command"),
        "got: {}",
        hotspots[0].cause
    );
    assert!(
        !hotspots[0].cause.contains("unexecuted_outgoing.rs"),
        "must not attribute turn 2's own unexecuted outgoing tool call: {}",
        hotspots[0].cause
    );

    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);
    let rows = format_turn_rows(&turns, 10, 7.2, &mut rates, None);
    assert!(rows[0].is_cold_cache);
    assert!(rows[0].is_partial_cold);
    assert_eq!(rows[0].cold_cache_kind, "mid_loop_cold");
}

#[allow(clippy::too_many_arguments)]
fn sample_turn(
    turn_index: usize,
    began: f64,
    total_ms: u64,
    model: &str,
    model_display: &str,
    input_tokens: u64,
    cache_read: u64,
    cache_write: u64,
    after_turn_start: Option<bool>,
) -> TurnSample {
    TurnSample {
        turn_index,
        step_idx: (turn_index as i64) * 2,
        timestamp: began,
        model: model.into(),
        model_display: model_display.into(),
        input_tokens,
        cache_read,
        cache_write,
        output_tokens: 400,
        thinking_tokens: 0,
        response_tokens: 400,
        ttft_ms: 1_200,
        total_ms,
        response_id: format!("resp-{turn_index}"),
        step_type: 15,
        is_checkpoint: false,
        after_turn_start,
        after_checkpoint: false,
    }
}

#[test]
fn test_gemini4_180s_ttl_and_mid_loop_sub_reasons() {
    assert!(is_gemini_4_model(
        "gemini-4-carbon-high",
        "Gemini 4 Carbon (High)"
    ));
    assert!(is_gemini_4_model(
        "gemini-4-neon-xhigh",
        "Gemini 4 Neon (XHigh)"
    ));
    assert!(!is_gemini_4_model("claude-sonnet-4-5", "Claude Sonnet 4.5"));
    assert!(!is_gemini_4_model("gpt-5", "GPT-5"));

    assert_eq!(
        model_cache_ttl_sec("gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
        180.0
    );
    assert_eq!(
        model_cache_ttl_sec("gemini-4-argon-medium", "Gemini 4 Argon (Medium)"),
        180.0
    );
    assert_eq!(
        model_cache_ttl_sec("gemini-4-neon-xhigh", "Gemini 4 Neon (XHigh)"),
        240.0
    );
    assert_eq!(
        model_cache_ttl_sec("gemini-4-hydrogen-medium", "Gemini 4 Hydrogen (Medium)"),
        240.0
    );
    assert_eq!(
        model_cache_ttl_sec("claude-sonnet-4-5", "Claude Sonnet 4.5"),
        300.0
    );

    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);

    // Gap of 190s (>=180s, <300s): Gemini 4 treats it as expired TTL (TurnStart), Claude treats it as MidLoop (<300s)
    let g4_turns = vec![
        sample_turn(
            1,
            1000.0,
            5_000,
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            25_000,
            0,
            0,
            Some(true),
        ),
        sample_turn(
            2,
            1190.0,
            4_000,
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            28_000,
            0,
            0,
            None,
        ),
    ];
    let g4_tel = build_session_telemetry(
        "g4-ttl-test",
        None,
        &g4_turns,
        1194.0,
        7.2,
        &mut rates,
        1200.0,
        None,
    );
    assert_eq!(g4_tel.cache_ttl_sec, 180);
    assert_eq!(
        g4_tel.cold_cache_breakdown,
        ColdCacheBreakdown {
            turn_start_cold: 2,
            mid_loop_cold: 0,
            post_compact_cold: 0,
        }
    );
    let g4_rows = format_turn_rows(&g4_turns, 5, 7.2, &mut rates, None);
    assert!(
        g4_rows[0].cold_cache_reason.contains("3m")
            || g4_rows[0].cold_cache_reason.contains("180s")
    );

    // Claude at 190s (<300s) is NOT TTL-expired -> MidLoop; at 310s (>=300s) -> TurnStart
    let cl_turns = vec![
        sample_turn(
            1,
            1000.0,
            5_000,
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            25_000,
            0,
            0,
            Some(true),
        ),
        sample_turn(
            2,
            1190.0,
            4_000,
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            28_000,
            0,
            0,
            None,
        ),
        sample_turn(
            3,
            1500.0,
            4_000,
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            30_000,
            0,
            0,
            None,
        ),
    ];
    let cl_tel = build_session_telemetry(
        "cl-ttl-test",
        None,
        &cl_turns,
        1504.0,
        7.2,
        &mut rates,
        1510.0,
        None,
    );
    assert_eq!(cl_tel.cache_ttl_sec, 300);
    assert_eq!(
        cl_tel.cold_cache_breakdown,
        ColdCacheBreakdown {
            turn_start_cold: 2,
            mid_loop_cold: 1,
            post_compact_cold: 0,
        }
    );

    let g4_plan = prepare_handoff_plan(Some(&g4_tel), 12, 0, &mut rates);
    assert_eq!(g4_plan.cache_ttl_sec, 180);
    let cl_plan = prepare_handoff_plan(Some(&cl_tel), 12, 0, &mut rates);
    assert_eq!(cl_plan.cache_ttl_sec, 300);

    // Gemini 4 Neon/Hydrogen 240s TTL: 210s gap (<240s) is NOT TTL-expired (MidLoop), 250s (>=240s) IS TurnStart with 4m/240s copy
    let neon_turns = vec![
        sample_turn(
            1,
            1000.0,
            5_000,
            "gemini-4-neon-xhigh",
            "Gemini 4 Neon (XHigh)",
            45_000,
            0,
            0,
            Some(true),
        ),
        sample_turn(
            2,
            1210.0,
            4_000,
            "gemini-4-neon-xhigh",
            "Gemini 4 Neon (XHigh)",
            48_000,
            0,
            0,
            None,
        ),
        sample_turn(
            3,
            1460.0,
            4_000,
            "gemini-4-neon-xhigh",
            "Gemini 4 Neon (XHigh)",
            52_000,
            0,
            0,
            None,
        ),
    ];
    let neon_tel = build_session_telemetry(
        "g4-neon-ttl-test",
        None,
        &neon_turns,
        1464.0,
        7.2,
        &mut rates,
        1470.0,
        None,
    );
    assert_eq!(neon_tel.cache_ttl_sec, 240);
    assert_eq!(
        neon_tel.cold_cache_breakdown,
        ColdCacheBreakdown {
            turn_start_cold: 2,
            mid_loop_cold: 1,
            post_compact_cold: 0,
        }
    );
    let neon_rows = format_turn_rows(&neon_turns, 5, 7.2, &mut rates, None);
    assert!(
        neon_rows[0].cold_cache_reason.contains("4m")
            || neon_rows[0].cold_cache_reason.contains("4 分钟")
            || neon_rows[0].cold_cache_reason.contains("240s"),
        "expected 240s/4m reason copy for Neon, got: {}",
        neon_rows[0].cold_cache_reason
    );
    let neon_plan = prepare_handoff_plan(Some(&neon_tel), 12, 0, &mut rates);
    assert_eq!(neon_plan.cache_ttl_sec, 240);
    assert!(
        neon_plan
            .partial_compaction_recipes
            .as_ref()
            .unwrap()
            .future_branch_fold
            .contains("240s"),
        "Neon recipe should mention 240s TTL"
    );

    // 1) slow_turn_self_expiry: idle_gap = 7s (<60s) AND curr.began - prev.began = 192s (>=180s)
    // 2) replica_spillover: partial rollback to turn 1 prompt (22_847)
    // 3) model_switch_cold: Carbon -> Hydrogen AND Neon XHigh -> Neon High
    let turns = vec![
        sample_turn(
            1,
            1000.0,
            4_000,
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            22_847,
            0,
            0,
            Some(true),
        ),
        sample_turn(
            2,
            1010.0,
            185_000,
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            5_000,
            60_000,
            0,
            Some(false),
        ),
        // Turn 3 begins at 1202.0: start_dt = 192s >= 180s, prev.ended = 1195.0 -> idle_gap = 7s < 60s
        sample_turn(
            3,
            1202.0,
            5_000,
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            72_000,
            0,
            0,
            Some(false),
        ),
        // Turn 4 warm
        sample_turn(
            4,
            1212.0,
            4_000,
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            4_000,
            72_000,
            0,
            Some(false),
        ),
        // Turn 5 partial rollback matching turn 1 static prompt (22_847)
        sample_turn(
            5,
            1220.0,
            4_000,
            "gemini-4-carbon-high",
            "Gemini 4 Carbon (High)",
            58_000,
            22_847,
            58_000,
            Some(false),
        ),
        // Turn 6 model switch Carbon -> Hydrogen
        sample_turn(
            6,
            1228.0,
            3_000,
            "gemini-4-hydrogen-high",
            "Gemini 4 Hydrogen (High)",
            82_000,
            0,
            0,
            Some(false),
        ),
        // Turn 7 warm on Neon XHigh baseline
        sample_turn(
            7,
            1235.0,
            3_000,
            "gemini-4-neon",
            "Gemini 4 Neon (XHigh)",
            3_000,
            82_000,
            0,
            Some(false),
        ),
        // Turn 8 reasoning-effort switch Neon XHigh -> Neon High
        sample_turn(
            8,
            1242.0,
            3_000,
            "gemini-4-neon",
            "Gemini 4 Neon (High)",
            87_000,
            0,
            0,
            Some(false),
        ),
    ];

    let detailed = classify_cold_cache_turns_detailed(&turns, None, None);
    assert_eq!(detailed[2].kind, ColdCacheKind::MidLoop);
    assert_eq!(detailed[2].sub_reason, MidLoopSubReason::SlowTurnSelfExpiry);
    assert_eq!(detailed[4].kind, ColdCacheKind::MidLoop);
    assert_eq!(detailed[4].sub_reason, MidLoopSubReason::ReplicaSpillover);
    assert_eq!(detailed[5].kind, ColdCacheKind::MidLoop);
    assert_eq!(detailed[5].sub_reason, MidLoopSubReason::ModelSwitchCold);
    assert_eq!(detailed[7].kind, ColdCacheKind::MidLoop);
    assert_eq!(detailed[7].sub_reason, MidLoopSubReason::ModelSwitchCold);

    let tel = build_session_telemetry(
        "midloop-sub-test",
        None,
        &turns,
        1245.0,
        7.2,
        &mut rates,
        1250.0,
        None,
    );
    assert_eq!(tel.cold_cache_breakdown.mid_loop_cold, 4);
    assert_eq!(
        tel.mid_loop_breakdown,
        MidLoopBreakdown {
            slow_turn_self_expiry: 1,
            replica_spillover: 1,
            model_switch_cold: 2,
            other: 0,
        }
    );
}

#[test]
fn test_carbon_concurrency_warning_and_overlap_replica_spillover() {
    assert!(is_gemini_4_carbon(
        "gemini-4-carbon-high",
        "Gemini 4 Carbon (High)"
    ));
    assert!(!is_gemini_4_carbon(
        "gemini-4-hydrogen-high",
        "Gemini 4 Hydrogen (High)"
    ));
    let now = 1_795_400_000.0;
    let td = TempDir::new().unwrap();
    let conv_dir = td.path().join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    let c1 = "c4000000-0000-0000-0000-000000000001";
    let c2 = "c4000000-0000-0000-0000-000000000002";
    let c3 = "c4000000-0000-0000-0000-000000000003";
    let h1 = "c4000000-0000-0000-0000-000000000004";
    let idle_c = "c4000000-0000-0000-0000-000000000005";

    // c1 has a short-gap (<60s) mid-loop cold miss while 3 active Carbon sessions overlap
    create_conv_db(
        &conv_dir.join(format!("{c1}.db")),
        &[
            (0, 14, None),
            (
                1,
                15,
                Some(make_step_meta(
                    (now - 40.0) as u64,
                    30_000,
                    0,
                    400,
                    "r1",
                    0,
                    0,
                )),
            ),
            (2, 132, None),
            (
                3,
                15,
                Some(make_step_meta(
                    (now - 10.0) as u64,
                    36_000,
                    0,
                    400,
                    "r2",
                    0,
                    0,
                )),
            ),
        ],
        "gemini-4-carbon-high",
        "Gemini 4 Carbon (High)",
    );
    for (cid, model, disp) in [
        (c2, "gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
        (c3, "gemini-4-carbon-medium", "Gemini 4 Carbon (Medium)"),
        (h1, "gemini-4-hydrogen-high", "Gemini 4 Hydrogen (High)"),
        (idle_c, "gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
    ] {
        create_conv_db(
            &conv_dir.join(format!("{cid}.db")),
            &[
                (0, 14, None),
                (
                    1,
                    15,
                    Some(make_step_meta(
                        (now - 20.0) as u64,
                        25_000,
                        0,
                        300,
                        "r1",
                        0,
                        0,
                    )),
                ),
            ],
            model,
            disp,
        );
    }

    // Phase 1: only 2 active Carbon sessions (c1, c2) + 1 active Hydrogen (h1) + 1 idle Carbon (idle_c) -> NO warning
    create_summaries_db(
        &td.path().join("conversation_summaries.db"),
        &[
            (
                c1,
                "Carbon 1",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_RUNNING",
                "",
                "",
                0,
                1,
                0,
                iso(now - 10.0),
                0,
            ),
            (
                c2,
                "Carbon 2",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_RUNNING",
                "w-c2",
                c1,
                1,
                1,
                0,
                iso(now - 15.0),
                -1,
            ),
            (
                c3,
                "Carbon 3 (idle)",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_IDLE",
                "w-c3",
                c1,
                1,
                0,
                0,
                iso(now - 20.0),
                -1,
            ),
            (
                h1,
                "Hydrogen 1",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_RUNNING",
                "w-h1",
                c1,
                1,
                1,
                0,
                iso(now - 12.0),
                -1,
            ),
            (
                idle_c,
                "Idle Carbon",
                "",
                r#"["file:///workspace/demo"]"#,
                "CASCADE_RUN_STATUS_IDLE",
                "",
                "",
                0,
                0,
                0,
                iso(now - 20.0),
                0,
            ),
        ],
    );

    let mut eng = TelemetryEngine::new(
        Some(td.path().to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
        Some(None),
    );
    let snap2 = eng.build_overview_snapshot(20, false, false, "", Some(now));
    assert_eq!(snap2.summary.active_carbon_count, 2);
    assert!(!snap2.summary.carbon_concurrency_warning);
    assert!(!snap2.carbon_concurrency_warning);
    assert!(snap2.carbon_concurrency_note.is_none());

    // Phase 2: activate c3 -> 3 simultaneous active Gemini 4 Carbon conversations -> warning triggers AND c1's short-gap cold miss attributes to replica_spillover!
    {
        let conn = Connection::open(td.path().join("conversation_summaries.db")).unwrap();
        conn.execute(
            "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_RUNNING', not_fully_idle=1 WHERE conversation_id=?1",
            params![c3],
        )
        .unwrap();
    }
    let snap3 = eng.build_overview_snapshot(20, false, false, "", Some(now));
    assert_eq!(snap3.summary.active_carbon_count, 3);
    assert!(snap3.summary.carbon_concurrency_warning);
    assert!(snap3.carbon_concurrency_warning);
    assert!(snap3
        .carbon_concurrency_note
        .as_deref()
        .unwrap()
        .contains("<= 2"));
    assert!(summarize_snapshot_text(&snap3).contains("<= 2"));

    let c1_tel = snap3.sessions.iter().find(|s| s.id == c1).unwrap();
    assert_eq!(c1_tel.mid_loop_breakdown.replica_spillover, 1);

    let turns_payload = eng.get_session_turns(c1, 10);
    assert_eq!(
        turns_payload.mid_loop_breakdown.unwrap().replica_spillover,
        1
    );
    assert_eq!(
        turns_payload.turns[0].cold_cache_sub_reason,
        "replica_spillover"
    );
}

#[test]
fn test_axiomatic_decoupling_tiers_flock_acp_transcript_cache_uri_and_lang() {
    use context_radar_mcp::engine::grants::workspaces_overlap;
    use context_radar_mcp::engine::tiers;
    use context_radar_mcp::guard::boot::extract_workspace_candidates;
    use context_radar_mcp::ledger::sqlite_ro::extract_workspaces;
    use context_radar_mcp::settings::{normalize_workspace_uri, ACP_PROXY_ENV};
    use std::env;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::{Arc, Barrier};

    // 1. tiers.rs isolation & budget.rs re-export parity
    let cfg = SettingsWatcher::new(Some("none".into())).current();
    let t_direct = tiers::resolve_budget_tier("gemini-4-neon-high", "Gemini 4 Neon (High)", &cfg);
    let t_reexp = resolve_budget_tier("gemini-4-neon-high", "Gemini 4 Neon (High)", &cfg);
    assert_eq!(t_direct.profile_id, t_reexp.profile_id);
    assert_eq!(
        tiers::model_cache_ttl_sec("gemini-4-carbon-high", ""),
        180.0
    );
    assert!(tiers::is_gemini_4_carbon("gemini-4-carbon-high", ""));
    assert!(tiers::is_gemini_4_model("gemini-4-hydrogen-high", ""));

    // 2. Unified percent-decoded workspace URI normalization across ledger, grants, and boot guard
    let uri = "file:///Users/demo/My%20Project%20Alpha/";
    assert_eq!(normalize_workspace_uri(uri), "/Users/demo/My Project Alpha");
    let (primary_ws, all_ws) = extract_workspaces(&format!(r#"["{uri}"]"#));
    assert_eq!(primary_ws, "/Users/demo/My Project Alpha");
    assert_eq!(all_ws, vec!["/Users/demo/My Project Alpha"]);
    let boot_cands = extract_workspace_candidates(&json!({ "workspaceRoots": [uri] }));
    assert_eq!(boot_cands, vec!["/Users/demo/My Project Alpha"]);
    assert!(workspaces_overlap(
        uri,
        &[],
        "/Users/demo/My Project Alpha/sub"
    ));
    assert!(workspaces_overlap("/Users/demo/My Project Alpha", &[], uri));

    // 3. Concurrent RelayGrantStore advisory flock: zero lost updates & single-winner atomic claim
    let td = TempDir::new().unwrap();
    let grants_path = td.path().join("relay_grants.json");
    let threads = 6usize;
    let barrier = Arc::new(Barrier::new(threads));
    let mut handles = Vec::new();
    for i in 0..threads {
        let gp = grants_path.clone();
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let mut store = RelayGrantStore::new(gp);
            b.wait();
            store
                .create_grant_scoped(
                    &format!("p-{i}"),
                    &format!("r-{i}"),
                    true,
                    &format!("cid-{i}"),
                    "file:///Users/demo/My%20Project%20Alpha/",
                    &[],
                    10,
                    0,
                    "compact",
                    "",
                    &format!("T-{i}"),
                    Lang::En,
                )
                .unwrap()
                .grant_id
        }));
    }
    let created_ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let mut verifier = RelayGrantStore::new(grants_path.clone());
    for gid in &created_ids {
        let g = verifier
            .find_grant(gid)
            .expect("locked concurrent create must not lose any grant");
        assert_eq!(g.workspace, "/Users/demo/My Project Alpha");
    }
    // Lock file must exist with 0600 permissions
    let lock_path = td.path().join("relay_grants.json.lock");
    let lock_meta = fs::symlink_metadata(&lock_path).unwrap();
    assert_eq!(lock_meta.permissions().mode() & 0o777, 0o600);

    // Concurrent claim on a single grant: strictly 1 winner
    let target_gid = created_ids[0].clone();
    let claim_barrier = Arc::new(Barrier::new(threads));
    let mut claim_handles = Vec::new();
    for _ in 0..threads {
        let gp = grants_path.clone();
        let gid = target_gid.clone();
        let b = Arc::clone(&claim_barrier);
        claim_handles.push(thread::spawn(move || {
            let mut store = RelayGrantStore::new(gp);
            b.wait();
            store
                .claim_grant_scoped(
                    &gid,
                    Some("cid-0"),
                    Some("file:///Users/demo/My%20Project%20Alpha/sub"),
                    Some(10),
                    Some(0),
                    Some(0),
                    true,
                )
                .claimed
        }));
    }
    let winners = claim_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|&ok| ok)
        .count();
    assert_eq!(
        winners, 1,
        "flock must guarantee strictly one winner under concurrent claims"
    );

    // Symlinked .lock file must fail closed (O_NOFOLLOW)
    let sym_dir = TempDir::new().unwrap();
    let sym_grants = sym_dir.path().join("relay_grants.json");
    let decoy = sym_dir.path().join("decoy");
    fs::write(&decoy, "decoy").unwrap();
    symlink(&decoy, sym_dir.path().join("relay_grants.json.lock")).unwrap();
    let mut sym_store = RelayGrantStore::new(sym_grants.clone());
    let claim_err = sym_store.claim_grant(&target_gid, None, None, None, false);
    assert!(!claim_err.claimed);
    assert_eq!(claim_err.reason.as_deref(), Some("lock_error"));

    // 4. Persisted acp_proxy_active on RelayGrant surfaces on side-pane pending_grant,
    //    unified PanelPrefs.lang syncs to RateTable::get_lang(), and legacy overview ui_action alias is removed
    let td_mcp = TempDir::new().unwrap();
    let conv_dir = td_mcp.path().join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    let cid = "d5000000-0000-0000-0000-000000000001";
    let now = 1_795_500_000.0;
    let mut rows: Vec<(i64, i32, Option<Vec<u8>>)> = Vec::new();
    for i in 0..20 {
        rows.push((
            i * 2,
            15,
            Some(make_step_meta(
                (now - 300.0 + (i as f64) * 5.0) as u64,
                10_000,
                320_000,
                400,
                &format!("acpr{i}"),
                0,
                0,
            )),
        ));
    }
    create_conv_db(
        &conv_dir.join(format!("{cid}.db")),
        &rows,
        "gemini-4-neon-high",
        "Gemini 4 Neon (High)",
    );
    create_summaries_db(
        &td_mcp.path().join("conversation_summaries.db"),
        &[(
            cid,
            "ACP & Lang Test",
            "",
            r#"["file:///Users/demo/My%20Project%20Alpha"]"#,
            "CASCADE_RUN_STATUS_RUNNING",
            "",
            "",
            0,
            1,
            0,
            iso(now - 5.0),
            0,
        )],
    );
    let eng = TelemetryEngine::new(
        Some(td_mcp.path().to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
        Some(None),
    );
    let gstore = RelayGrantStore::new(td_mcp.path().join("relay_grants.json"));
    let mut daemon = McpDaemon::new(Some(eng), Some(gstore), None);
    daemon
        .engine
        .set_preferred_workspace(Some("/Users/demo/My Project Alpha".into()), true);
    daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 9, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "extensions": {
                    "io.modelcontextprotocol/ui": { "mimeTypes": ["text/html;profile=mcp-app"] }
                } }
            }
        }))
        .unwrap();

    // Unified PanelPrefs.lang via context_radar_ui_action updates RateTable::get_lang()
    let set_zh = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "lang": "zh-CN" } }
        }))
        .unwrap();
    assert_eq!(
        set_zh["result"]["structuredContent"]["settings"]["lang"],
        json!("zh-CN")
    );
    assert_eq!(daemon.engine.rates.get_lang(), Lang::ZhCn);

    // Legacy hidden alias on context_radar_overview(action="set_prefs") is gone (returns snapshot instead of mutating prefs)
    let legacy = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 11, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": { "action": "set_prefs", "allow_auto_handoff": false } }
        }))
        .unwrap();
    assert_eq!(
        legacy["result"]["structuredContent"]["mode"],
        json!("snapshot")
    );
    assert!(daemon.grants.get_settings().allow_auto_handoff);

    // Create compact grant with CONTEXT_RADAR_ACP_PROXY=1 -> persisted acp_proxy_active=true on pending_grant,
    // scratch_note_path included in compact_command, and ACP gate blocks return isError=true (while App UI stays isError=false)
    let prev_acp = env::var(ACP_PROXY_ENV).ok();
    env::set_var(ACP_PROXY_ENV, "1");
    let commit = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 12, "method": "tools/call",
            "params": {
                "name": "context_radar_commit_handoff",
                "arguments": {
                    "kind": "compact",
                    "conversation_id": cid,
                    "preserve_points": "Keep locks",
                    "scratch_note_path": "scratch/slice_1_6.md",
                    "estimated_remaining_turns": 20
                }
            }
        }))
        .unwrap();
    assert_eq!(commit["result"]["isError"], json!(false));
    assert_eq!(
        commit["result"]["structuredContent"]["acp_proxy_active"],
        json!(true)
    );
    assert_eq!(
        commit["result"]["structuredContent"]["lang"],
        json!("zh-CN")
    );
    assert!(commit["result"]["structuredContent"]["compact_command"]
        .as_str()
        .unwrap()
        .contains("scratch/slice_1_6.md"));

    // Gate block under CONTEXT_RADAR_ACP_PROXY=1 (estimated_remaining_turns=1 fails break-even) -> isError=true
    let acp_blocked_gate = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 120, "method": "tools/call",
            "params": {
                "name": "context_radar_commit_handoff",
                "arguments": {
                    "kind": "compact",
                    "conversation_id": cid,
                    "preserve_points": "Keep locks",
                    "estimated_remaining_turns": 1
                }
            }
        }))
        .unwrap();
    assert_eq!(acp_blocked_gate["result"]["isError"], json!(true));
    assert_eq!(
        acp_blocked_gate["result"]["structuredContent"]["mode"],
        json!("handoff_blocked")
    );

    // Disabling allow_auto_handoff under CONTEXT_RADAR_ACP_PROXY=1 -> isError=true on auto_fire compact
    let _ = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1201, "method": "tools/call",
            "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "allow_auto_handoff": false } }
        }))
        .unwrap();
    let acp_blocked_pref = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 121, "method": "tools/call",
            "params": {
                "name": "context_radar_commit_handoff",
                "arguments": {
                    "kind": "compact",
                    "conversation_id": cid,
                    "preserve_points": "Keep locks",
                    "estimated_remaining_turns": 20
                }
            }
        }))
        .unwrap();
    assert_eq!(acp_blocked_pref["result"]["isError"], json!(true));
    assert_eq!(
        acp_blocked_pref["result"]["structuredContent"]["mode"],
        json!("handoff_blocked")
    );
    let _ = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1211, "method": "tools/call",
            "params": { "name": "context_radar_ui_action", "arguments": { "action": "set_prefs", "allow_auto_handoff": true } }
        }))
        .unwrap();

    // Unset ACP proxy env -> App UI gate block keeps isError=false so inline card renders normally
    env::remove_var(ACP_PROXY_ENV);
    let app_blocked_gate = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 122, "method": "tools/call",
            "params": {
                "name": "context_radar_commit_handoff",
                "arguments": {
                    "kind": "compact",
                    "conversation_id": cid,
                    "preserve_points": "Keep locks",
                    "estimated_remaining_turns": 1
                }
            }
        }))
        .unwrap();
    assert_eq!(app_blocked_gate["result"]["isError"], json!(false));
    assert_eq!(
        app_blocked_gate["result"]["structuredContent"]["mode"],
        json!("handoff_blocked")
    );

    match prev_acp {
        Some(v) => env::set_var(ACP_PROXY_ENV, v),
        None => env::remove_var(ACP_PROXY_ENV),
    }

    let ov = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 13, "method": "tools/call",
            "params": { "name": "context_radar_overview", "arguments": { "fresh": true } }
        }))
        .unwrap();
    assert_eq!(ov["result"]["structuredContent"]["lang"], json!("zh-CN"));
    assert_eq!(
        ov["result"]["structuredContent"]["pending_grant"]["acp_proxy_active"],
        json!(true),
        "side-pane pending_grant must carry persisted acp_proxy_active=true"
    );

    // 5. MetricsCacheKey invalidates when transcript.jsonl changes for legacy DBs (after_turn_start == None)
    let td_tr = TempDir::new().unwrap();
    let tr_conv = td_tr.path().join("conversations");
    fs::create_dir_all(&tr_conv).unwrap();
    let lcid = "d6000000-0000-0000-0000-000000000001";
    let ldb = tr_conv.join(format!("{lcid}.db"));
    {
        let conn = Connection::open(&ldb).unwrap();
        conn.execute(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, metadata BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, metadata) VALUES (0, ?1)",
            params![make_gen_blob(
                "gemini-4-neon-high",
                "Gemini 4 Neon (High)",
                0
            )],
        )
        .unwrap();
        // Legacy steps table WITHOUT step_type column -> after_turn_start is None
        conn.execute(
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, metadata BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO steps (idx, metadata) VALUES (1, ?1)",
            params![make_step_meta(
                (now - 100.0) as u64,
                30_000,
                0,
                300,
                "lr1",
                0,
                0
            )],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO steps (idx, metadata) VALUES (3, ?1)",
            params![make_step_meta(
                (now - 80.0) as u64,
                35_000,
                0,
                300,
                "lr2",
                0,
                0
            )],
        )
        .unwrap();
    }
    create_summaries_db(
        &td_tr.path().join("conversation_summaries.db"),
        &[(
            lcid,
            "Legacy Transcript Cache",
            "",
            r#"["file:///ws/legacy"]"#,
            "CASCADE_RUN_STATUS_IDLE",
            "",
            "",
            0,
            0,
            0,
            iso(now - 10.0),
            0,
        )],
    );
    let mut eng_tr = TelemetryEngine::new(
        Some(td_tr.path().to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
        Some(None),
    );
    // Before transcript.jsonl exists: step 1 is initial turn_start_cold, step 3 cold miss is mid_loop_cold
    let snap_before = eng_tr.build_overview_snapshot(10, false, false, "", Some(now));
    let s_before = snap_before.sessions.iter().find(|s| s.id == lcid).unwrap();
    assert_eq!(s_before.cold_cache_breakdown.mid_loop_cold, 1);
    assert_eq!(s_before.cold_cache_breakdown.turn_start_cold, 1);

    // Now write transcript.jsonl with USER_INPUT at step 2 WITHOUT modifying ldb
    let tdir = td_tr
        .path()
        .join("brain")
        .join(lcid)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&tdir).unwrap();
    fs::write(
        tdir.join("transcript.jsonl"),
        "{\"step_index\":0,\"type\":\"USER_INPUT\"}\n{\"step_index\":2,\"type\":\"USER_INPUT\"}\n",
    )
    .unwrap();

    // MetricsCacheKey must invalidate on transcript_stamp change and reclassify step 3 as turn_start_cold!
    let snap_after = eng_tr.build_overview_snapshot(10, false, false, "", Some(now));
    let s_after = snap_after.sessions.iter().find(|s| s.id == lcid).unwrap();
    assert_eq!(
        s_after.cold_cache_breakdown.turn_start_cold, 2,
        "MetricsCacheKey must invalidate when transcript.jsonl changes for legacy DBs"
    );
    assert_eq!(s_after.cold_cache_breakdown.mid_loop_cold, 0);
}

#[test]
fn test_summary_and_turn_read_errors_fail_closed_and_manual_claim_checks_subagents() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    let main_cid = "e1000000-0000-0000-0000-000000000001";
    let sub_cid = "e1000000-0000-0000-0000-000000000002";
    let now = 1_795_600_000.0;

    // 1. Set up main conversation + active subagent in conversation_summaries.db
    let main_db = root.join("conversations").join(format!("{main_cid}.db"));
    let sub_db = root.join("conversations").join(format!("{sub_cid}.db"));
    create_conv_db(
        &main_db,
        &[(
            1,
            15,
            Some(make_step_meta(
                now as u64 - 10,
                10_000,
                300_000,
                400,
                "m1",
                0,
                0,
            )),
        )],
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );
    create_conv_db(
        &sub_db,
        &[(
            1,
            15,
            Some(make_step_meta(
                now as u64 - 5,
                5_000,
                20_000,
                200,
                "s1",
                0,
                0,
            )),
        )],
        "gemini-3-pro",
        "Gemini 3 Pro (High)",
    );

    let sum_db = root.join("conversation_summaries.db");
    {
        let conn = Connection::open(&sum_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE raw_summaries (
                conversation_id TEXT PRIMARY KEY, title TEXT, preview TEXT, workspace_uris TEXT,
                status TEXT, agent_name TEXT, parent_conversation_id TEXT, nesting_depth INTEGER,
                not_fully_idle INTEGER, killed INTEGER, last_modified_time TEXT, last_user_input_step_index INTEGER,
                bad INTEGER DEFAULT 0
            );
            CREATE VIEW conversation_summaries AS
            SELECT conversation_id,
                CASE WHEN bad = 1 THEN json_extract('malformed', '$') ELSE title END AS title,
                preview, workspace_uris, status, agent_name, parent_conversation_id,
                nesting_depth, not_fully_idle, killed, last_modified_time, last_user_input_step_index
            FROM raw_summaries;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO raw_summaries VALUES (?1,'Main','','[\"file:///ws/a\"]','CASCADE_RUN_STATUS_RUNNING','','',0,1,0,?2,0,0)",
            params![main_cid, iso(now - 5.0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO raw_summaries VALUES (?1,'Sub','','[\"file:///ws/a\"]','CASCADE_RUN_STATUS_RUNNING','worker-1',?2,1,1,0,?3,0,0)",
            params![sub_cid, main_cid, iso(now - 2.0)],
        )
        .unwrap();
    }

    let eng = TelemetryEngine::new(
        Some(root.to_path_buf()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
        Some(None),
    );
    let gstore = RelayGrantStore::new(root.join("relay_grants.json"));
    let mut daemon = McpDaemon::new(Some(eng), Some(gstore), None);
    daemon
        .engine
        .set_preferred_workspace(Some("/ws/a".into()), true);
    daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "extensions": {
                    "io.modelcontextprotocol/ui": { "mimeTypes": ["text/html;profile=mcp-app"] }
                } }
            }
        }))
        .unwrap();

    let grant = daemon
        .grants
        .create_grant_scoped(
            "Keep state",
            "Resume",
            true,
            main_cid,
            "/ws/a",
            &["/ws/a".into()],
            1,
            0,
            "handoff",
            "",
            "Main",
            Lang::En,
        )
        .unwrap();

    // 2. Default "manual" claim_mode MUST enforce running_subagent_count check!
    let manual_blocked = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {
                "name": "context_radar_ui_action",
                "arguments": {
                    "action": "claim_grant",
                    "grant_id": grant.grant_id,
                    "conversation_id": main_cid,
                    "claim_mode": "manual"
                }
            }
        }))
        .unwrap();
    assert_eq!(
        manual_blocked["result"]["structuredContent"]["claimed"],
        json!(false)
    );
    assert_eq!(
        manual_blocked["result"]["structuredContent"]["reason"],
        json!("running_subagents_active")
    );

    // 3. Inject summary query error: try_read_summaries fails, does NOT advance DB stamp,
    //    preserves prior display cache, AND claim_grant fails closed with telemetry_read_error!
    thread::sleep(Duration::from_millis(20));
    {
        let conn = Connection::open(&sum_db).unwrap();
        conn.execute(
            "UPDATE raw_summaries SET bad = 1 WHERE conversation_id = ?1",
            params![sub_cid],
        )
        .unwrap();
    }
    assert!(daemon.engine.ledger.try_read_summaries().is_err());
    assert!(
        daemon.engine.ledger.read_summaries().contains_key(sub_cid),
        "display fallback must retain prior cached summaries without advancing stamp"
    );
    let claim_on_sum_err = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "context_radar_ui_action",
                "arguments": {
                    "action": "claim_grant",
                    "grant_id": grant.grant_id,
                    "conversation_id": main_cid,
                    "claim_mode": "manual_handoff"
                }
            }
        }))
        .unwrap();
    assert_eq!(
        claim_on_sum_err["result"]["structuredContent"]["claimed"],
        json!(false)
    );
    assert_eq!(
        claim_on_sum_err["result"]["structuredContent"]["reason"],
        json!("telemetry_read_error")
    );

    // Recover summaries DB (and mark subagent idle): cached stamp was NOT advanced, so recovery is seen immediately!
    thread::sleep(Duration::from_millis(20));
    {
        let conn = Connection::open(&sum_db).unwrap();
        conn.execute(
            "UPDATE raw_summaries SET bad = 0, status = 'CASCADE_RUN_STATUS_IDLE', not_fully_idle = 0 WHERE conversation_id = ?1",
            params![sub_cid],
        )
        .unwrap();
    }
    assert_eq!(
        daemon
            .engine
            .try_get_running_subagent_count(main_cid, Some(now))
            .unwrap(),
        0
    );

    // 4. Corrupt main conversation DB: claim_grant must NOT fall back to (0, 0) turns/ckpts -> fails closed!
    thread::sleep(Duration::from_millis(20));
    fs::write(&main_db, b"not a valid sqlite database").unwrap();
    let claim_on_turn_err = daemon
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {
                "name": "context_radar_ui_action",
                "arguments": {
                    "action": "claim_grant",
                    "grant_id": grant.grant_id,
                    "conversation_id": main_cid,
                    "claim_mode": "manual_handoff"
                }
            }
        }))
        .unwrap();
    assert_eq!(
        claim_on_turn_err["result"]["structuredContent"]["claimed"],
        json!(false)
    );
    assert_eq!(
        claim_on_turn_err["result"]["structuredContent"]["reason"],
        json!("telemetry_read_error")
    );
}

#[test]
fn test_crit_line_matches_urgent_ctx_and_respects_explicit_override() {
    let mut rates = RateTable::new(SettingsWatcher::new(Some("none".into())), None, None);

    let make_single = |model: &str, disp: &str, ctx_tokens: u64, rates: &mut RateTable| {
        let turns = vec![sample_turn(
            1,
            1000.0,
            4_000,
            model,
            disp,
            2_000,
            ctx_tokens.saturating_sub(2_000),
            0,
            Some(true),
        )];
        build_session_telemetry(
            "crit-parity-test",
            None,
            &turns,
            1004.0,
            7.2,
            rates,
            1010.0,
            None,
        )
    };

    // Default runs: panel critical line aligns 1:1 with prof.urgent_ctx across all tiers (never pinned to 350k)
    for (model, disp, urgent_ctx) in [
        ("gemini-4-carbon-high", "Gemini 4 Carbon (High)", 400_000),
        ("gemini-4-neon-xhigh", "Gemini 4 Neon (XHigh)", 430_000),
        ("gemini-4-neon-high", "Gemini 4 Neon (High)", 440_000),
        (
            "gemini-4-hydrogen-medium",
            "Gemini 4 Hydrogen (Medium)",
            460_000,
        ),
    ] {
        let below = make_single(model, disp, urgent_ctx - 1_000, &mut rates);
        assert_eq!(
            below.level,
            "warn",
            "{model} at {} should stay warn below urgent_ctx {}",
            urgent_ctx - 1_000,
            urgent_ctx
        );
        assert_eq!(below.urgent_compact_tokens, urgent_ctx);
        assert!(!evaluate_handoff_gate(Some(&below), 1, &mut rates).urgent_bypass);

        let at_line = make_single(model, disp, urgent_ctx, &mut rates);
        assert_eq!(
            at_line.level, "critical",
            "{model} at {} should turn critical at urgent_ctx",
            urgent_ctx
        );
        assert!(at_line.suggest_compact);
        assert_eq!(at_line.urgent_compact_tokens, urgent_ctx);
        assert!(evaluate_handoff_gate(Some(&at_line), 1, &mut rates).urgent_bypass);
    }

    // Explicit non-default critical_threshold override aligns level, urgent_compact_tokens, and urgent gate 1:1
    let td = TempDir::new().unwrap();
    let cfg_path = td.path().join("settings.json");
    fs::write(
        &cfg_path,
        serde_json::to_string(&json!({
            "version": 1,
            "context": { "critical_threshold": 380000 }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut custom_rates = RateTable::new(
        SettingsWatcher::new(Some(cfg_path.to_string_lossy().into_owned())),
        None,
        None,
    );
    let below_override = make_single(
        "gemini-4-neon-high",
        "Gemini 4 Neon (High)",
        379_000,
        &mut custom_rates,
    );
    assert_eq!(below_override.level, "warn");
    assert_eq!(below_override.urgent_compact_tokens, 380_000);
    assert!(!evaluate_handoff_gate(Some(&below_override), 1, &mut custom_rates).urgent_bypass);

    let at_override = make_single(
        "gemini-4-neon-high",
        "Gemini 4 Neon (High)",
        380_000,
        &mut custom_rates,
    );
    assert_eq!(at_override.level, "critical");
    assert_eq!(at_override.urgent_compact_tokens, 380_000);
    let gate = evaluate_handoff_gate(Some(&at_override), 1, &mut custom_rates);
    assert!(gate.urgent_bypass);
    assert_eq!(gate.profile.urgent_ctx, 380_000);
}
