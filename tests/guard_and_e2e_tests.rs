use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::TempDir;

use context_radar_mcp::guard::boot::evaluate_boot_guard;
use context_radar_mcp::guard::exec_gate::{evaluate_exec_gate, is_explicit_background_command};
use context_radar_mcp::guard::stdin_json::{truncate_utf8, JsonBalanceScanner};

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_context-radar-mcp")
}

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

fn make_gen_blob(model_id: &str, model_display: &str) -> Vec<u8> {
    let mut inner = pb_bytes(19, model_id.as_bytes());
    inner.extend_from_slice(&pb_bytes(21, model_display.as_bytes()));
    pb_bytes(1, &inner)
}

fn make_step_meta(ts_sec: u64, in_tok: u64, cr_tok: u64, out_tok: u64, resp_id: &str) -> Vec<u8> {
    let mut usage = pb_varint(2, in_tok);
    usage.extend_from_slice(&pb_varint(3, out_tok));
    usage.extend_from_slice(&pb_varint(5, cr_tok));
    usage.extend_from_slice(&pb_bytes(11, resp_id.as_bytes()));

    let mut out = pb_bytes(1, &pb_varint(1, ts_sec));
    out.extend_from_slice(&pb_bytes(6, &pb_varint(1, ts_sec + 1)));
    out.extend_from_slice(&pb_bytes(7, &pb_varint(1, ts_sec + 3)));
    out.extend_from_slice(&pb_bytes(9, &usage));
    out.extend_from_slice(&pb_bytes(20, &pb_varint(3, 0)));
    out
}

#[test]
fn test_exec_gate_wait_thresholds_and_decision_ask() {
    for wait_ms in [5000, 8000, 10000] {
        let res = evaluate_exec_gate(
            &json!({
                "hookEventName": "PreToolUse",
                "toolCall": {
                    "name": "run_command",
                    "args": { "CommandLine": "cargo test -q", "WaitMsBeforeAsync": wait_ms, "IsDaemon": false }
                }
            }),
            None,
        );
        assert_eq!(res, json!({ "decision": "ask" }));
    }

    for wait_val in [json!(500), json!(1000), json!(2000), json!("2000")] {
        let res = evaluate_exec_gate(
            &json!({
                "hookEventName": "PreToolUse",
                "toolCall": {
                    "name": "run_command",
                    "args": { "CommandLine": "git status", "WaitMsBeforeAsync": wait_val }
                }
            }),
            None,
        );
        assert_eq!(
            res,
            json!({ "decision": "ask", "overwrite": { "WaitMsBeforeAsync": 10000 } })
        );
    }
}

#[test]
fn test_exec_gate_is_daemon_background_patterns_and_custom_config() {
    assert_eq!(
        evaluate_exec_gate(
            &json!({
                "toolCall": {
                    "name": "run_command",
                    "args": { "CommandLine": "npm run dev", "WaitMsBeforeAsync": 500, "IsDaemon": true }
                }
            }),
            None
        ),
        json!({ "decision": "ask" })
    );

    for cmd in [
        "nohup python3 worker.py > /tmp/w.log 2>&1 &",
        "python3 server.py &",
        "docker compose up -d",
        "sleep 30",
        "npm run dev",
        "tail -f /tmp/app.log",
    ] {
        assert!(is_explicit_background_command(cmd, None), "expected bg: {cmd}");
    }
    assert!(!is_explicit_background_command("cargo test 2>&1", None));

    let custom_cfg = json!({
        "min_sync_wait_ms": 8000,
        "target_sync_wait_ms": 9000,
        "exempt_patterns": [r"(?:^|\s)ssh\s+.*-f\b"]
    });
    assert_eq!(
        evaluate_exec_gate(
            &json!({
                "toolCall": {
                    "name": "run_command",
                    "args": { "CommandLine": "cargo test", "WaitMsBeforeAsync": 6000 }
                }
            }),
            Some(&custom_cfg)
        ),
        json!({ "decision": "ask", "overwrite": { "WaitMsBeforeAsync": 9000 } })
    );
    assert_eq!(
        evaluate_exec_gate(
            &json!({
                "toolCall": {
                    "name": "run_command",
                    "args": { "CommandLine": "ssh host -f sleep 60", "WaitMsBeforeAsync": 1000 }
                }
            }),
            Some(&custom_cfg)
        ),
        json!({ "decision": "ask" })
    );
    assert_eq!(
        evaluate_exec_gate(
            &json!({
                "toolCall": {
                    "name": "run_command",
                    "args": { "CommandLine": "git status", "WaitMsBeforeAsync": 500 }
                }
            }),
            Some(&json!({ "enabled": false }))
        ),
        json!({ "decision": "ask" })
    );
}

#[test]
fn test_boot_guard_zero_config_silence_state_table_bullets_and_extra_rules() {
    assert_eq!(evaluate_boot_guard(&json!({ "cwd": "/tmp" }), &json!({})), json!({}));

    let td = TempDir::new().unwrap();
    let tmp_dir = td.path().to_string_lossy().into_owned();
    let state_md = [
        "# Workspace Status",
        "| Repo | Link | Open | Hold | Active |",
        "| --- | --- | --- | --- | --- |",
        "| repo-a | [S](a) | 2 | 0 | 1 |",
        "| repo-b | [S](b) | 0 | 0 | 0 |",
        "## Themes",
        "| Theme | Items | Running | Repos |",
        "| --- | --- | --- | --- |",
        "| core-refactor · Core cleanup | 3 | 1 | repo-a |",
    ]
    .join("\n");
    fs::write(td.path().join("TRACKER.md"), state_md).unwrap();
    fs::write(
        td.path().join("TODO.md"),
        "# Sprint\n- Finish cache layer\n- Verify latency\n",
    )
    .unwrap();

    let hook_cfg = json!({
        "enabled": true,
        "max_total_bytes": 1600,
        "state_files": [
            {
                "workspace_root": tmp_dir,
                "path": "TRACKER.md",
                "format": "markdown_state_table",
                "prefix": "[demo TRACKER.md]",
                "max_bytes": 600,
                "section_heading": "## Themes",
                "table_columns": {
                    "name_col": 0,
                    "min_cols": 5,
                    "ignore_headers": ["Repo"],
                    "counters": [
                        { "col": 2, "label": "open" },
                        { "col": 3, "label": "hold" },
                        { "col": 4, "label": "active" }
                    ]
                },
                "theme_columns": {
                    "name_col": 0,
                    "min_cols": 4,
                    "ignore_headers": ["Theme"],
                    "split_sep": "·"
                }
            },
            {
                "workspace_root": tmp_dir,
                "path": "TODO.md",
                "format": "markdown_bullets",
                "prefix": "[TODO]",
                "max_bytes": 300
            }
        ],
        "extra_rules": [{
            "workspace_root": tmp_dir,
            "tag": "hints",
            "lines": ["Prefer parallel conflict-free workers without nesting swarm(add)."],
            "max_bytes": 400
        }]
    });

    assert_eq!(evaluate_boot_guard(&json!({ "cwd": "/tmp" }), &hook_cfg), json!({}));

    let inside = evaluate_boot_guard(&json!({ "cwd": tmp_dir }), &hook_cfg);
    let msg = inside["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap();
    assert!(msg.contains("[demo TRACKER.md] non-zero: repo-a[open=2,active=1] | themes: core-refactor(3/1,repo-a)"));
    assert!(msg.contains("[TODO] Sprint | • Finish cache layer | • Verify latency"));
    assert!(msg.contains("[hints] Prefer parallel conflict-free workers"));
    assert!(msg.len() <= 1600);
}

#[test]
fn test_truncate_utf8_and_balanced_json_scanner() {
    let s = "中文测试字符".repeat(50);
    let out = truncate_utf8(&s, 40);
    assert!(out.len() <= 40);
    assert!(out.ends_with("..."));
    assert_eq!(truncate_utf8("abcdef", 2), "..");

    let mut scanner = JsonBalanceScanner::new();
    assert!(!scanner.push(br#"  {"text":"brace } inside \"quote\"" "#));
    assert!(!scanner.is_complete());
    assert!(scanner.push(br#", "nested":{"arr":[1,2,3]}} "#));
    assert!(scanner.is_complete());
}

#[test]
fn test_hooks_cli_e2e_env_override_and_open_pipe_fast_completion() {
    let td = TempDir::new().unwrap();
    let cfg_path = td.path().join("settings.json");
    fs::write(td.path().join("STATUS.md"), "# Active\n- Task 1\n").unwrap();
    fs::write(
        &cfg_path,
        serde_json::to_string(&json!({
            "version": 1,
            "guards": {
                "exec_gate": { "min_sync_wait_ms": 4000, "target_sync_wait_ms": 9500 },
                "boot": {
                    "state_files": [{
                        "workspace_root": td.path().to_string_lossy(),
                        "path": "STATUS.md",
                        "format": "markdown_bullets",
                        "prefix": "[STATUS]"
                    }]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // Warm up binary once to exclude first-exec OS code-sign scan jitter from latency assertion
    let warm = Command::new(bin_path()).arg("--version").output().unwrap();
    assert!(warm.status.success());

    // 1) Open-pipe pre-run exec guard must finish fast WITHOUT closing stdin
    let t0 = Instant::now();
    let mut child = Command::new(bin_path())
        .args(["guard", "exec"])
        .env("CONTEXT_RADAR_CONFIG", &cfg_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin_hold = child.stdin.take().unwrap();
    stdin_hold
        .write_all(
            serde_json::to_string(&json!({
                "toolCall": { "name": "run_command", "args": { "CommandLine": "git status", "WaitMsBeforeAsync": 1000 } }
            }))
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    stdin_hold.flush().unwrap();
    // Intentionally keep stdin_hold open while waiting for child exit!
    let status = child.wait().unwrap();
    let elapsed_ms = t0.elapsed().as_millis();
    drop(stdin_hold);
    assert!(status.success());
    assert!(elapsed_ms < 150, "open-pipe guard exec took {elapsed_ms}ms");
    let mut stdout_str = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout_str).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stdout_str).unwrap(),
        json!({ "decision": "ask", "overwrite": { "WaitMsBeforeAsync": 9500 } })
    );

    // 2) Open-pipe boot guard must finish fast WITHOUT closing stdin
    let t1 = Instant::now();
    let mut child2 = Command::new(bin_path())
        .args(["guard", "boot"])
        .env("CONTEXT_RADAR_CONFIG", &cfg_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin2 = child2.stdin.take().unwrap();
    stdin2
        .write_all(serde_json::to_string(&json!({ "cwd": td.path().to_string_lossy() })).unwrap().as_bytes())
        .unwrap();
    stdin2.flush().unwrap();
    let status2 = child2.wait().unwrap();
    let elapsed2 = t1.elapsed().as_millis();
    drop(stdin2);
    assert!(status2.success());
    assert!(elapsed2 < 150, "open-pipe guard boot took {elapsed2}ms");
    let mut stdout2 = String::new();
    std::io::Read::read_to_string(&mut child2.stdout.take().unwrap(), &mut stdout2).unwrap();
    let parsed2: Value = serde_json::from_str(&stdout2).unwrap();
    assert!(parsed2["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap()
        .contains("[STATUS] Active | • Task 1"));

    // 3) Malformed stdin fails open safely
    let mut child3 = Command::new(bin_path())
        .args(["guard", "exec"])
        .env("CONTEXT_RADAR_CONFIG", "none")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child3.stdin.take().unwrap().write_all(b"{bad-json").unwrap();
    let out3 = child3.wait_with_output().unwrap();
    assert!(out3.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out3.stdout).unwrap(),
        json!({ "decision": "ask" })
    );
}

#[test]
fn test_full_stdio_mcp_daemon_e2e_with_synthetic_fixture() {
    let td = TempDir::new().unwrap();
    let cid = "60000000-0000-0000-0000-000000000006";
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    let conv_dir = td.path().join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    {
        let conn = Connection::open(conv_dir.join(format!("{cid}.db"))).unwrap();
        conn.execute_batch(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
             CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER, metadata BLOB);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (0, ?1)",
            params![make_gen_blob("gemini-3-pro", "Gemini 3 Pro (High)")],
        )
        .unwrap();
        conn.execute("INSERT INTO steps VALUES (0, 14, NULL)", []).unwrap();
        for i in 1..=25u64 {
            let cr = if i == 25 { 340_000 } else { 200_000 };
            conn.execute(
                "INSERT INTO steps VALUES (?1, 15, ?2)",
                params![i as i64, make_step_meta(now - 300 + i * 5, 10_000, cr, 500, &format!("r{i}"))],
            )
            .unwrap();
        }
    }
    {
        let conn = Connection::open(td.path().join("conversation_summaries.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_summaries (
                conversation_id TEXT PRIMARY KEY, title TEXT, preview TEXT, workspace_uris TEXT,
                status TEXT, agent_name TEXT, parent_conversation_id TEXT, nesting_depth INTEGER,
                not_fully_idle INTEGER, killed INTEGER, last_modified_time TEXT, last_user_input_step_index INTEGER
            );",
        )
        .unwrap();
        let iso_now = DateTime::<Utc>::from_timestamp(now as i64, 0).unwrap().to_rfc3339();
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1,'Synthetic HighCtx Session','','[\"file:///workspace/sample\"]','CASCADE_RUN_STATUS_RUNNING','','',0,1,0,?2,0)",
            params![cid, iso_now],
        )
        .unwrap();
    }
    let tdir = td.path().join("brain").join(cid).join(".system_generated").join("logs");
    fs::create_dir_all(&tdir).unwrap();
    fs::write(
        tdir.join("transcript.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"step_index": 0, "type": "USER_INPUT", "content": "build parser"}),
            json!({
                "step_index": 25,
                "type": "PLANNER_RESPONSE",
                "content": "finished parser",
                "tool_calls": [{ "name": "edit_file", "args": { "Path": "/workspace/sample/parser.rs" } }]
            })
        ),
    )
    .unwrap();

    let panel_html = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("panel")
        .join("dist")
        .join("panel.html");

    let plugin_data = td.path().join("plugin_data_isolated");
    let mut proc = Command::new(bin_path())
        .args([
            "daemon",
            "--data-dir",
            plugin_data.to_str().unwrap(),
            "--panel-html",
            panel_html.to_str().unwrap(),
        ])
        .env("CONTEXT_RADAR_DATA_ROOT", td.path())
        .env("CONTEXT_RADAR_CONFIG", "none")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = proc.stdin.take().unwrap();
    let mut reader = BufReader::new(proc.stdout.take().unwrap());

    let mut rpc = |payload: Value| -> Value {
        writeln!(stdin, "{}", serde_json::to_string(&payload).unwrap()).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap();
        assert!(n > 0, "daemon closed stdout unexpectedly");
        serde_json::from_str(&line).unwrap()
    };

    let init = rpc(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-03-26", "capabilities": {} }
    }));
    assert_eq!(init["result"]["serverInfo"]["name"], "context-radar-mcp");
    assert_eq!(
        init["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );

    let tools = rpc(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tool_arr = tools["result"]["tools"].as_array().unwrap();
    let names: std::collections::HashSet<&str> = tool_arr
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "context_radar_overview",
            "context_radar_ui_action",
            "context_radar_turns",
            "context_radar_hotspots",
            "context_radar_prepare_handoff",
            "context_radar_commit_handoff",
        ]
        .into_iter()
        .collect()
    );
    let ui_act = tool_arr
        .iter()
        .find(|t| t["name"] == "context_radar_ui_action")
        .unwrap();
    assert_eq!(ui_act["_meta"]["ui"]["visibility"], json!(["app"]));

    let resources = rpc(json!({ "jsonrpc": "2.0", "id": 21, "method": "resources/list" }));
    for r in resources["result"]["resources"].as_array().unwrap() {
        assert_eq!(r["mimeType"], "text/html;profile=mcp-app");
        assert_eq!(r["_meta"]["ui"]["prefersBorder"], json!(false));
    }

    let snap = rpc(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "context_radar_overview", "arguments": {} }
    }));
    assert_eq!(snap["result"]["structuredContent"]["sessions"][0]["id"], cid);

    let turns = rpc(json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "context_radar_turns", "arguments": { "conversation_id": cid, "limit": 5 } }
    }));
    assert_eq!(turns["result"]["structuredContent"]["turns"].as_array().unwrap().len(), 5);

    let plan = rpc(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "context_radar_prepare_handoff", "arguments": { "conversation_id": cid, "estimated_remaining_turns": 20 } }
    }));
    assert_eq!(plan["result"]["structuredContent"]["gate_allowed"], true);

    let commit = rpc(json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {
            "name": "context_radar_commit_handoff",
            "arguments": {
                "conversation_id": cid,
                "preserve_points": "Parser implemented",
                "resume_task": "Run tests",
                "estimated_remaining_turns": 20
            }
        }
    }));
    assert_eq!(commit["result"]["structuredContent"]["mode"], "handoff_card");
    assert_eq!(commit["result"]["structuredContent"]["blocked"], false);
    assert!(plugin_data.join("relay_grants.json").is_file(), "grants must persist strictly under --data-dir");

    let distill = rpc(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "context_radar_hotspots", "arguments": { "conversation_id": cid, "mode": "distill" } }
    }));
    assert_eq!(distill["result"]["structuredContent"]["found"], true);

    let res = rpc(json!({
        "jsonrpc": "2.0", "id": 8, "method": "resources/read",
        "params": { "uri": "ui://context-radar-mcp/panel" }
    }));
    let html = res["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(html.contains("com.google.antigravity/message"));
    assert!(html.contains("window.__CONTEXT_RADAR_BOOT__="));
    assert!(html.contains("csw") && html.contains("role=\"switch\""));
    assert!(html.contains("data-a=\"ci\""));
    assert!(html.contains("Copy baton"));

    drop(stdin);
    assert!(proc.wait().unwrap().success());
}
