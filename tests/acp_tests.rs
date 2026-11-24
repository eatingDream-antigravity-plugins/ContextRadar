use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use context_radar_mcp::acp::{
    prompt_starts_with_compact, resolve_cli_data_root_from_args, LiveSessionCheck, Outbound,
    ProxyCoordinator,
};
use context_radar_mcp::engine::grants::RelayGrantStore;
use context_radar_mcp::i18n::Lang;
use context_radar_mcp::settings::infer_data_root_from_plugin_data;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_context-radar-mcp")
}

#[test]
fn test_acp_data_root_inference_and_compact_prompt_detection() {
    let td = TempDir::new().unwrap();
    let cli_root = td.path().join("antigravity-cli");
    let pdd = cli_root.join("plugin_data").join("context-radar-mcp");
    fs::create_dir_all(&pdd).unwrap();
    assert_eq!(infer_data_root_from_plugin_data(&pdd), Some(cli_root.clone()));

    let unrelated = td.path().join("other").join("context-radar-mcp");
    fs::create_dir_all(&unrelated).unwrap();
    assert_eq!(infer_data_root_from_plugin_data(&unrelated), None);

    let resolved = resolve_cli_data_root_from_args(&[
        "--gemini-dir".to_string(),
        td.path().to_string_lossy().to_string(),
        "--app-data-dir".to_string(),
        "antigravity-cli".to_string(),
    ]);
    assert_eq!(resolved, cli_root);

    assert!(prompt_starts_with_compact(
        &json!({ "prompt": [{ "type": "text", "text": "/compact" }] })
    ));
    assert!(prompt_starts_with_compact(
        &json!({ "prompt": [{ "type": "text", "text": "  /COMPACT Keep findings" }] })
    ));
    assert!(!prompt_starts_with_compact(
        &json!({ "prompt": [{ "type": "text", "text": "explain /compact" }] })
    ));
    assert!(!prompt_starts_with_compact(
        &json!({ "prompt": [{ "type": "image", "data": "" }, { "type": "text", "text": "/compact" }] })
    ));
}

#[test]
fn test_acp_coordinator_claims_compact_grant_and_rewrites_client_id() {
    let td = TempDir::new().unwrap();
    let grants_path = td.path().join("relay_grants.json");
    let mut grants = RelayGrantStore::new(grants_path.clone());
    let mut coord = ProxyCoordinator::new(2, 0, "/compact".into(), false);

    // 1. Client opens session
    let out = coord.on_client_message(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/new",
        "params": { "cwd": "/workspace/demo", "mcpServers": [] }
    }));
    assert_eq!(out.len(), 1);
    let _ = coord.on_agent_message(
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "sessionId": "sess-1" }
        }),
        &mut grants,
        |_| LiveSessionCheck::default(),
    );
    assert_eq!(coord.session_state("sess-1").unwrap().cwd, "/workspace/demo");

    // 2. Client sends prompt id=2
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/prompt",
        "params": { "sessionId": "sess-1", "prompt": [{ "type": "text", "text": "Do phase 1" }] }
    }));

    // 3. During turn, Agent issues a scoped compact grant via Context Radar MCP
    let g = grants.create_grant_scoped(
        "Keep API schema",
        "Continue phase 2",
        true,
        "sess-1",
        "/workspace/demo",
        &["/workspace/demo".into()],
        10,
        0,
        "compact",
        "",
        "Demo",
        Lang::En,
    );
    assert_eq!(g.kind, "compact");

    // 4. Agent finishes Turn 1 with end_turn -> coordinator MUST hold id=2 and inject native /compact!
    let actions = coord.on_agent_message(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "stopReason": "end_turn" }
        }),
        &mut grants,
        |_| LiveSessionCheck {
            turns: Some(10),
            checkpoints: Some(0),
            running_subagents: Some(0),
        },
    );
    assert_eq!(actions.len(), 2, "expected notice to client + injected prompt to agent");
    match &actions[0] {
        Outbound::ToClient(v) => {
            assert_eq!(v["method"], "session/update");
            assert!(v["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap()
                .contains("/compact"));
        }
        _ => panic!("expected ToClient notice"),
    }
    let internal_id = match &actions[1] {
        Outbound::ToAgent(v) => {
            assert_eq!(v["method"], "session/prompt");
            assert_eq!(v["params"]["sessionId"], "sess-1");
            let txt = v["params"]["prompt"][0]["text"].as_str().unwrap();
            assert!(txt.starts_with("/compact "), "expected /compact cmd, got {txt}");
            assert!(txt.contains("Keep API schema"));
            assert!(txt.contains("Continue phase 2"));
            v["id"].clone()
        }
        _ => panic!("expected ToAgent injected /compact prompt"),
    };

    // 5. Agent finishes injected /compact -> coordinator records acp_compacted and resolves original id=2!
    let final_out = coord.on_agent_message(
        json!({
            "jsonrpc": "2.0",
            "id": internal_id,
            "result": { "stopReason": "end_turn", "usage": { "totalTokens": 24000 } }
        }),
        &mut grants,
        |_| LiveSessionCheck::default(),
    );
    assert_eq!(final_out.len(), 1);
    match &final_out[0] {
        Outbound::ToClient(v) => {
            assert_eq!(v["id"], json!(2));
            assert_eq!(v["result"]["stopReason"], "end_turn");
        }
        _ => panic!("expected ToClient rewritten final response"),
    }

    let updated = grants.find_grant(&g.grant_id).unwrap();
    assert!(updated.claimed);
    assert_eq!(updated.compact_status, "acp_compacted");
    assert_eq!(coord.total_auto_compacts(), 1);
}

#[test]
fn test_acp_coordinator_blocks_compaction_when_subagents_running_or_user_ran_compact() {
    let td = TempDir::new().unwrap();
    let mut grants = RelayGrantStore::new(td.path().join("relay_grants.json"));
    let mut coord = ProxyCoordinator::new(2, 10_000, "/compact".into(), true);

    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/new",
        "params": { "cwd": "/ws", "mcpServers": [] }
    }));
    let _ = coord.on_agent_message(
        json!({ "jsonrpc": "2.0", "id": 1, "result": { "sessionId": "s-sub" } }),
        &mut grants,
        |_| LiveSessionCheck::default(),
    );
    // Feed high usage update
    let _ = coord.on_agent_message(
        json!({
            "jsonrpc": "2.0", "method": "session/update",
            "params": { "sessionId": "s-sub", "update": { "sessionUpdate": "usage_update", "used": 25000, "size": 512000 } }
        }),
        &mut grants,
        |_| LiveSessionCheck::default(),
    );

    // Case 1: Active subagent -> threshold fallback MUST NOT fire!
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 2, "method": "session/prompt",
        "params": { "sessionId": "s-sub", "prompt": [{ "type": "text", "text": "hello" }] }
    }));
    let out1 = coord.on_agent_message(
        json!({ "jsonrpc": "2.0", "id": 2, "result": { "stopReason": "end_turn" } }),
        &mut grants,
        |_| LiveSessionCheck {
            turns: Some(5),
            checkpoints: Some(0),
            running_subagents: Some(1),
        },
    );
    assert_eq!(out1.len(), 1);
    assert!(matches!(&out1[0], Outbound::ToClient(v) if v["id"] == json!(2)));
    assert_eq!(coord.total_auto_compacts(), 0);

    // Case 2: User already sent `/compact` -> MUST NOT double-compact!
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": "s-sub", "prompt": [{ "type": "text", "text": "/compact" }] }
    }));
    let out2 = coord.on_agent_message(
        json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" } }),
        &mut grants,
        |_| LiveSessionCheck {
            turns: Some(6),
            checkpoints: Some(1),
            running_subagents: Some(0),
        },
    );
    assert_eq!(out2.len(), 1);
    assert!(matches!(&out2[0], Outbound::ToClient(v) if v["id"] == json!(3)));
    assert_eq!(coord.total_auto_compacts(), 0);
}

#[test]
fn test_acp_coordinator_handles_cancel_during_internal_compact() {
    let td = TempDir::new().unwrap();
    let mut grants = RelayGrantStore::new(td.path().join("relay_grants.json"));
    let mut coord = ProxyCoordinator::new(2, 0, "/compact".into(), true);

    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/new",
        "params": { "cwd": "/ws", "mcpServers": [] }
    }));
    let _ = coord.on_agent_message(
        json!({ "jsonrpc": "2.0", "id": 1, "result": { "sessionId": "s-cancel" } }),
        &mut grants,
        |_| LiveSessionCheck::default(),
    );
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 5, "method": "session/prompt",
        "params": { "sessionId": "s-cancel", "prompt": [{ "type": "text", "text": "work" }] }
    }));
    let g = grants.create_grant_scoped(
        "Keep x",
        "Next y",
        true,
        "s-cancel",
        "/ws",
        &[],
        12,
        0,
        "compact",
        "",
        "T",
        Lang::En,
    );
    let injected = coord.on_agent_message(
        json!({ "jsonrpc": "2.0", "id": 5, "result": { "stopReason": "end_turn" } }),
        &mut grants,
        |_| LiveSessionCheck::default(),
    );
    let internal_id = match &injected[0] {
        Outbound::ToAgent(v) => v["id"].clone(),
        _ => panic!("expected ToAgent"),
    };

    // Client cancels while internal /compact is running
    let cancel_out = coord.on_client_message(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": { "sessionId": "s-cancel" }
    }));
    assert_eq!(cancel_out.len(), 1);
    assert!(matches!(&cancel_out[0], Outbound::ToAgent(v) if v["method"] == "session/cancel"));

    // Agent replies cancelled -> coordinator maps id back to 5 and marks grant acp_cancelled
    let fin = coord.on_agent_message(
        json!({ "jsonrpc": "2.0", "id": internal_id, "result": { "stopReason": "cancelled" } }),
        &mut grants,
        |_| LiveSessionCheck::default(),
    );
    assert_eq!(fin.len(), 1);
    assert!(matches!(&fin[0], Outbound::ToClient(v) if v["id"] == json!(5) && v["result"]["stopReason"] == "cancelled"));
    assert_eq!(grants.find_grant(&g.grant_id).unwrap().compact_status, "acp_cancelled");
}

fn write_mock_agy_script(dir: &Path) -> PathBuf {
    let script = dir.join("mock_agy.py");
    let body = r#"#!/usr/bin/env python3
import json, os, sys

assert len(sys.argv) >= 2 and sys.argv[1] == "acp", f"expected 'acp' subcmd, got {sys.argv}"
grants_path = os.environ.get("CONTEXT_RADAR_GRANTS_PATH", "")
acp_flag = os.environ.get("CONTEXT_RADAR_ACP_PROXY", "")

def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    msg = json.loads(raw)
    mid = msg.get("id")
    method = msg.get("method")
    params = msg.get("params") or {}
    if method == "initialize":
        emit({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": 1, "agentInfo": {"name": "Mock ACP", "version": "1.0"}}})
    elif method == "session/new":
        emit({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "mock-sess-001"}})
    elif method == "session/prompt":
        sid = params["sessionId"]
        txt = params["prompt"][0]["text"]
        if txt.startswith("/compact"):
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "usage_update", "used": 21000, "size": 512000}}})
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "COMPACTED_AND_RESUMED"}}}})
            emit({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn", "usage": {"totalTokens": 21000}}})
        else:
            # Simulate Agent calling context_radar_commit_handoff(kind="compact") writing a grant
            if grants_path and acp_flag == "1":
                os.makedirs(os.path.dirname(grants_path), exist_ok=True)
                with open(grants_path, "w", encoding="utf-8") as f:
                    json.dump({
                        "settings": {"allow_auto_handoff": True, "lang": "en"},
                        "grants": {
                            "g_mock_01": {
                                "grant_id": "g_mock_01",
                                "kind": "compact",
                                "conversation_id": sid,
                                "workspace": "",
                                "title": "Mock",
                                "created_at": 1999999999.0,
                                "ttl_sec": 900.0,
                                "created_turns": 1,
                                "created_checkpoints": 0,
                                "auto_fire": True,
                                "claimed": False,
                                "claimed_at": 0.0,
                                "dismissed": False,
                                "preserve_points": "mock state",
                                "resume_task": "finish",
                                "scratch_note_path": "",
                                "compact_command": "/compact Preserve mock state. Continue with finish.",
                                "handoff_prompt": "",
                                "handoff_conversation_id": "",
                                "handoff_at": 0.0
                            }
                        }
                    }, f)
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "usage_update", "used": 300000, "size": 512000}}})
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "TURN1_DONE\n"}}}})
            emit({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn", "usage": {"totalTokens": 300000}}})
"#;
    fs::write(&script, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

#[test]
fn test_acp_proxy_and_acp_run_stdio_e2e_with_mock_acp_server() {
    let td = TempDir::new().unwrap();
    let mock_agy = write_mock_agy_script(td.path());
    let grants_path = td.path().join("plugin_data").join("relay_grants.json");
    let data_root = td.path().join("cli_root");
    fs::create_dir_all(&data_root).unwrap();

    // 1. Test `acp-run --json` end-to-end
    let out = Command::new(bin_path())
        .args([
            "acp-run",
            "--agy-bin",
            mock_agy.to_str().unwrap(),
            "--cwd",
            td.path().to_str().unwrap(),
            "--data-root",
            data_root.to_str().unwrap(),
            "--grants-path",
            grants_path.to_str().unwrap(),
            "--json",
            "--prompt",
            "Run phase 1",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "acp-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rep: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rep["session_id"], "mock-sess-001");
    assert_eq!(rep["stop_reason"], "end_turn");
    assert_eq!(rep["auto_compacts"], 1);
    assert_eq!(rep["final_used_tokens"], 21000);
    assert!(rep["response"].as_str().unwrap().contains("TURN1_DONE"));
    assert!(rep["response"]
        .as_str()
        .unwrap()
        .contains("COMPACTED_AND_RESUMED"));

    // Verify grant was claimed and marked acp_compacted
    let g_raw: Value = serde_json::from_str(&fs::read_to_string(&grants_path).unwrap()).unwrap();
    assert_eq!(g_raw["grants"]["g_mock_01"]["claimed"], json!(true));
    assert_eq!(
        g_raw["grants"]["g_mock_01"]["compact_status"],
        json!("acp_compacted")
    );

    // 2. Test `acp-proxy` transparent stdio proxy end-to-end
    let _ = fs::remove_file(&grants_path);
    let mut child = Command::new(bin_path())
        .args([
            "acp-proxy",
            "--agy-bin",
            mock_agy.to_str().unwrap(),
            "--data-root",
            data_root.to_str().unwrap(),
            "--grants-path",
            grants_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    let send = |w: &mut std::process::ChildStdin, v: Value| {
        writeln!(w, "{v}").unwrap();
        w.flush().unwrap();
    };
    let read_until = |r: &mut BufReader<std::process::ChildStdout>, want_id: u64| -> (Value, String) {
        let mut chunks = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = r.read_line(&mut line).unwrap();
            assert!(n > 0, "unexpected EOF waiting for id={want_id}");
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            if v.get("id") == Some(&json!(want_id)) && (v.get("result").is_some() || v.get("error").is_some()) {
                return (v, chunks);
            }
            if v.get("method") == Some(&json!("session/update")) {
                if let Some(txt) = v["params"]["update"]["content"]["text"].as_str() {
                    chunks.push_str(txt);
                }
            }
        }
    };

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}),
    );
    let (r1, _) = read_until(&mut reader, 1);
    assert_eq!(r1["result"]["protocolVersion"], 1);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":td.path().to_string_lossy(),"mcpServers":[]}}),
    );
    let (r2, _) = read_until(&mut reader, 2);
    assert_eq!(r2["result"]["sessionId"], "mock-sess-001");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"mock-sess-001","prompt":[{"type":"text","text":"work"}]}}),
    );
    let (r3, chunks) = read_until(&mut reader, 3);
    assert_eq!(r3["id"], 3);
    assert_eq!(r3["result"]["stopReason"], "end_turn");
    assert!(chunks.contains("TURN1_DONE"));
    assert!(chunks.contains("[Context Radar] Executing native ACP /compact"));
    assert!(chunks.contains("COMPACTED_AND_RESUMED"));

    drop(stdin);
    let st = child.wait().unwrap();
    assert!(st.success());
    let _ = Duration::from_millis(1);
}
