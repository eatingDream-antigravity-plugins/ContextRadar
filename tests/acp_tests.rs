use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

use context_radar_mcp::acp::{prompt_starts_with_compact, Outbound, ProxyCoordinator};
use context_radar_mcp::i18n::Lang;
use context_radar_mcp::settings::infer_data_root_from_plugin_data;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_context-radar-mcp")
}

#[test]
fn test_acp_compact_prompt_detection_and_data_root_inference() {
    let td = TempDir::new().unwrap();
    let cli_root = td.path().join("antigravity-cli");
    let pdd = cli_root.join("plugin_data").join("context-radar-mcp");
    fs::create_dir_all(&pdd).unwrap();
    assert_eq!(infer_data_root_from_plugin_data(&pdd), Some(cli_root));

    let unrelated = td.path().join("other").join("context-radar-mcp");
    fs::create_dir_all(&unrelated).unwrap();
    assert_eq!(infer_data_root_from_plugin_data(&unrelated), None);

    // Direct `/compact` and leading non-text attachments (aligned with agy 1.5.16)
    assert!(prompt_starts_with_compact(
        &json!({ "prompt": [{ "type": "text", "text": "/compact" }] })
    ));
    assert!(prompt_starts_with_compact(
        &json!({ "prompt": [{ "type": "text", "text": "  /COMPACT Keep findings" }] })
    ));
    assert!(prompt_starts_with_compact(&json!({
        "prompt": [
            { "type": "image", "data": "" },
            { "type": "resource", "uri": "file:///a.rs" },
            { "type": "text", "text": "   " },
            { "type": "text", "text": "/compact Preserve diff" }
        ]
    })));
    assert!(!prompt_starts_with_compact(
        &json!({ "prompt": [{ "type": "text", "text": "explain /compact" }] })
    ));
    assert!(!prompt_starts_with_compact(&json!({
        "prompt": [
            { "type": "file", "uri": "file:///a.rs" },
            { "type": "text", "text": "first non-empty" },
            { "type": "text", "text": "/compact too late" }
        ]
    })));
}

#[test]
fn test_acp_coordinator_pure_protocol_direct_and_lazy_wrapper_and_gates() {
    let mut coord = ProxyCoordinator::new(2, 0, "/compact".into(), false).with_lang(Lang::En);

    // Open session
    let out = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/new",
        "params": { "cwd": "/workspace/demo", "mcpServers": [] }
    }));
    assert_eq!(out.len(), 1);
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 1, "result": { "sessionId": "sess-1" }
    }));
    assert_eq!(
        coord.session_state("sess-1").unwrap().cwd,
        "/workspace/demo"
    );

    // (a) Direct MCP tool_call -> tool_call_update(status: "completed") without rawOutput triggers /compact
    //     including preserve_points + sanitized scratch_note_path + resume_task
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 2, "method": "session/prompt",
        "params": { "sessionId": "sess-1", "prompt": [{ "type": "text", "text": "Do phase 1" }] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tc-1",
                "name": "mcp_context-radar-mcp_context_radar_commit_handoff",
                "rawInput": {
                    "kind": "compact",
                    "preserve_points": "Keep API schema",
                    "scratch_note_path": "scratch/slice_1_8.md",
                    "resume_task": "Continue phase 2",
                    "auto_fire": true
                },
                "status": "in_progress"
            }
        }
    }));
    // agy 1.5.16 omits rawOutput/content on success and may omit rawInput on update
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-1",
                "status": "completed"
            }
        }
    }));

    let actions = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 2, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(actions.len(), 2, "expected notice + injected /compact");
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
            assert!(txt.starts_with("/compact "), "got {txt}");
            assert!(txt.contains("Keep API schema"));
            assert!(txt.contains("scratch/slice_1_8.md"));
            assert!(txt.contains("Continue phase 2"));
            v["id"].clone()
        }
        _ => panic!("expected ToAgent injected /compact prompt"),
    };

    // Injected /compact finishes -> id rewritten back to 2
    let final_out = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": internal_id,
        "result": { "stopReason": "end_turn", "usage": { "totalTokens": 24000 } }
    }));
    assert_eq!(final_out.len(), 1);
    assert!(matches!(&final_out[0], Outbound::ToClient(v) if v["id"] == json!(2)));
    assert_eq!(coord.total_auto_compacts(), 1);

    // (b) Lazy call_mcp_tool wrapper triggers /compact
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": "sess-1", "prompt": [{ "type": "text", "text": "Phase 2" }] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tc-lazy",
                "name": "call_mcp_tool",
                "rawInput": {
                    "ServerName": "context-radar-mcp",
                    "ToolName": "context_radar_commit_handoff",
                    "Arguments": {
                        "kind": "compact",
                        "preserve_points": "Keep lazy state",
                        "resume_task": "Run phase 3"
                    }
                },
                "status": "in_progress"
            }
        }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": { "sessionUpdate": "tool_call_update", "toolCallId": "tc-lazy", "status": "completed" }
        }
    }));
    let lazy_actions = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" }
    }));
    let lazy_iid = match &lazy_actions[1] {
        Outbound::ToAgent(v) => {
            let txt = v["params"]["prompt"][0]["text"].as_str().unwrap();
            assert!(txt.contains("Keep lazy state"));
            assert!(txt.contains("Run phase 3"));
            v["id"].clone()
        }
        _ => panic!("expected ToAgent /compact for lazy wrapper"),
    };
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": lazy_iid, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(coord.total_auto_compacts(), 2);

    // (c) Failed tool call (MCP gate blocked -> isError: true -> agy acp status: "failed") NEVER triggers
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 4, "method": "session/prompt",
        "params": { "sessionId": "sess-1", "prompt": [{ "type": "text", "text": "Phase 3" }] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tc-fail",
                "name": "context_radar_commit_handoff",
                "rawInput": { "kind": "compact", "preserve_points": "Blocked", "auto_fire": true },
                "status": "in_progress"
            }
        }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-fail",
                "status": "failed",
                "rawOutput": { "mode": "handoff_blocked", "blocked": true }
            }
        }
    }));
    let fail_out = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 4, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(fail_out.len(), 1);
    assert!(matches!(&fail_out[0], Outbound::ToClient(v) if v["id"] == json!(4)));
    assert_eq!(coord.total_auto_compacts(), 2);

    // (d) auto_fire: false NEVER triggers
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 5, "method": "session/prompt",
        "params": { "sessionId": "sess-1", "prompt": [{ "type": "text", "text": "Interactive" }] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-nofire",
                "name": "context_radar_commit_handoff",
                "rawInput": { "kind": "compact", "preserve_points": "Manual", "auto_fire": false },
                "status": "completed"
            }
        }
    }));
    let nofire_out = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 5, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(nofire_out.len(), 1);
    assert!(matches!(&nofire_out[0], Outbound::ToClient(v) if v["id"] == json!(5)));
    assert_eq!(coord.total_auto_compacts(), 2);

    // (e) Omitted `kind` defaults to "handoff" (MUST NOT fire `/compact`)
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 6, "method": "session/prompt",
        "params": { "sessionId": "sess-1", "prompt": [{ "type": "text", "text": "Handoff default" }] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-default-kind",
                "name": "context_radar_commit_handoff",
                "rawInput": { "preserve_points": "Default handoff", "auto_fire": true },
                "status": "completed"
            }
        }
    }));
    let def_kind_out = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 6, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(def_kind_out.len(), 1);
    assert!(matches!(&def_kind_out[0], Outbound::ToClient(v) if v["id"] == json!(6)));
    assert_eq!(coord.total_auto_compacts(), 2);

    // (f) Traversal scratch_note_path ("../../etc/passwd") is stripped before building `/compact`
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 7, "method": "session/prompt",
        "params": { "sessionId": "sess-1", "prompt": [{ "type": "text", "text": "Traversal guard" }] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-trav",
                "name": "context_radar_commit_handoff",
                "rawInput": {
                    "kind": "compact",
                    "preserve_points": "Keep safe state",
                    "scratch_note_path": "../../etc/passwd",
                    "resume_task": "Next safe step"
                },
                "status": "completed"
            }
        }
    }));
    let trav_actions = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 7, "result": { "stopReason": "end_turn" }
    }));
    let trav_iid = match &trav_actions[1] {
        Outbound::ToAgent(v) => {
            let txt = v["params"]["prompt"][0]["text"].as_str().unwrap();
            assert!(txt.contains("Keep safe state"));
            assert!(
                !txt.contains("etc/passwd"),
                "traversal path must be stripped: {txt}"
            );
            v["id"].clone()
        }
        _ => panic!("expected ToAgent /compact"),
    };
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": trav_iid, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(coord.total_auto_compacts(), 3);

    // (g) Historical commit_handoff(kind="compact") replayed during `session/load`
    //     followed by a normal `session/prompt` MUST NOT spuriously fire `/compact`
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 8, "method": "session/load",
        "params": { "sessionId": "sess-loaded", "cwd": "/workspace/demo", "mcpServers": [] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "sess-loaded",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-hist",
                "name": "context_radar_commit_handoff",
                "rawInput": {
                    "kind": "compact",
                    "preserve_points": "Stale historical compact",
                    "auto_fire": true
                },
                "status": "completed"
            }
        }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 8, "result": { "sessionId": "sess-loaded" }
    }));
    assert!(coord
        .session_state("sess-loaded")
        .unwrap()
        .pending_compact
        .is_none());

    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 9, "method": "session/prompt",
        "params": { "sessionId": "sess-loaded", "prompt": [{ "type": "text", "text": "Fresh question" }] }
    }));
    let loaded_out = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 9, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(loaded_out.len(), 1);
    assert!(matches!(&loaded_out[0], Outbound::ToClient(v) if v["id"] == json!(9)));
    assert_eq!(coord.total_auto_compacts(), 3);
}

#[test]
fn test_acp_coordinator_threshold_fallback_user_compact_and_cancel() {
    let mut coord =
        ProxyCoordinator::new(2, 10_000, "/compact Keep basics".into(), true).with_lang(Lang::En);

    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/new",
        "params": { "cwd": "/ws", "mcpServers": [] }
    }));
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 1, "result": { "sessionId": "s-thr" }
    }));
    // Feed high usage update
    let _ = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": { "sessionId": "s-thr", "update": { "sessionUpdate": "usage_update", "used": 25000, "size": 512000 } }
    }));

    // User already sent leading attachment + `/compact` -> MUST NOT double-compact!
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 2, "method": "session/prompt",
        "params": {
            "sessionId": "s-thr",
            "prompt": [
                { "type": "image", "data": "" },
                { "type": "text", "text": "/compact" }
            ]
        }
    }));
    let out_user = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 2, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(out_user.len(), 1);
    assert!(matches!(&out_user[0], Outbound::ToClient(v) if v["id"] == json!(2)));
    assert_eq!(coord.total_auto_compacts(), 0);

    // Normal prompt exceeding usage_update threshold -> triggers threshold fallback `/compact`
    let _ = coord.on_client_message(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": "s-thr", "prompt": [{ "type": "text", "text": "work" }] }
    }));
    let injected = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" }
    }));
    assert_eq!(injected.len(), 1);
    let internal_id = match &injected[0] {
        Outbound::ToAgent(v) => {
            assert_eq!(v["params"]["prompt"][0]["text"], "/compact Keep basics");
            v["id"].clone()
        }
        _ => panic!("expected ToAgent threshold /compact"),
    };

    // Client cancels while internal /compact is running
    let cancel_out = coord.on_client_message(json!({
        "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": "s-thr" }
    }));
    assert_eq!(cancel_out.len(), 1);
    assert!(matches!(&cancel_out[0], Outbound::ToAgent(v) if v["method"] == "session/cancel"));

    let fin = coord.on_agent_message(json!({
        "jsonrpc": "2.0", "id": internal_id, "result": { "stopReason": "cancelled" }
    }));
    assert_eq!(fin.len(), 1);
    assert!(matches!(
        &fin[0],
        Outbound::ToClient(v) if v["id"] == json!(3) && v["result"]["stopReason"] == "cancelled"
    ));
    assert_eq!(coord.total_auto_compacts(), 1);
}

fn write_mock_agy_script(dir: &Path) -> PathBuf {
    let script = dir.join("mock_agy.py");
    let body = r#"#!/usr/bin/env python3
import json, os, sys

assert len(sys.argv) >= 2 and sys.argv[1] == "acp", f"expected 'acp' subcmd, got {sys.argv}"
assert os.environ.get("CONTEXT_RADAR_ACP_PROXY") == "1", "missing CONTEXT_RADAR_ACP_PROXY=1"
assert "CONTEXT_RADAR_DATA_ROOT" not in os.environ, "thin client must not inject CONTEXT_RADAR_DATA_ROOT"
assert "CONTEXT_RADAR_GRANTS_PATH" not in os.environ, "thin client must not inject CONTEXT_RADAR_GRANTS_PATH"
native_ov = os.environ.get("AGY_CLI_AUTO_COMPACT_OVERRIDE", "")

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
        emit({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": 1, "agentInfo": {"name": "Mock ACP", "version": "1.5.16"}}})
    elif method == "session/new":
        emit({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "mock-sess-001"}})
    elif method == "session/prompt":
        sid = params["sessionId"]
        txt = params["prompt"][0]["text"]
        if txt == "Request permission":
            emit({"jsonrpc": "2.0", "id": 1001, "method": "session/request_permission", "params": {
                "sessionId": sid,
                "options": [{"optionId": "allow-only", "name": "Allow", "kind": "allow_once"}]
            }})
            reply = json.loads(next(sys.stdin))
            assert reply["id"] == 1001, reply
            assert reply["result"]["outcome"] == {"outcome": "cancelled"}, reply
            emit({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn"}})
        elif txt.startswith("/compact"):
            assert "mock state" in txt and "scratch/slice_1_4.md" in txt and "finish" in txt, f"unexpected compact cmd: {txt}"
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "usage_update", "used": 21000, "size": 512000}}})
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": f"COMPACTED_AND_RESUMED({native_ov})"}}}})
            emit({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn", "usage": {"totalTokens": 21000}}})
        else:
            # Real agy 1.5.16 shape: tool_call -> tool_call_update(completed) WITHOUT rawOutput/content
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "call_01",
                "name": "context_radar_commit_handoff",
                "args": {"kind": "compact", "preserve_points": "mock state", "scratch_note_path": "scratch/slice_1_4.md", "resume_task": "finish", "auto_fire": True},
                "rawInput": {"kind": "compact", "preserve_points": "mock state", "scratch_note_path": "scratch/slice_1_4.md", "resume_task": "finish", "auto_fire": True},
                "status": "in_progress"
            }}})
            emit({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_01",
                "name": "context_radar_commit_handoff",
                "status": "completed"
            }}})
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
fn test_acp_run_cancels_allow_only_permission_without_auto_approve() {
    let td = TempDir::new().unwrap();
    let mock_agy = write_mock_agy_script(td.path());
    let out = Command::new(bin_path())
        .arg("acp-run")
        .arg("--agy-bin")
        .arg(mock_agy)
        .arg("--cwd")
        .arg(td.path())
        .args(["--json", "--prompt", "Request permission"])
        .env_remove("CONTEXT_RADAR_DATA_ROOT")
        .env_remove("CONTEXT_RADAR_GRANTS_PATH")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["stop_reason"], "end_turn");
}

#[test]
fn test_acp_proxy_and_acp_run_pure_protocol_stdio_e2e_zero_disk_state() {
    let td = TempDir::new().unwrap();
    let mock_agy = write_mock_agy_script(td.path());

    // 1. Test `acp-run --json` end-to-end with zero SQLite DB and zero relay_grants.json,
    //    verifying backwards-compat no-op flags and `--native-auto-compact 300K` env pass-through.
    let out = Command::new(bin_path())
        .args([
            "acp-run",
            "--agy-bin",
            mock_agy.to_str().unwrap(),
            "--cwd",
            td.path().to_str().unwrap(),
            "--data-root",
            "/nonexistent/root",
            "--grants-path",
            "/nonexistent/relay_grants.json",
            "--native-auto-compact",
            "300K",
            "--json",
            "--prompt",
            "Run phase 1",
        ])
        .env_remove("CONTEXT_RADAR_DATA_ROOT")
        .env_remove("CONTEXT_RADAR_GRANTS_PATH")
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
        .contains("COMPACTED_AND_RESUMED(300K)"));

    // 2. Test `acp-proxy` transparent stdio proxy end-to-end with zero local DB/grants
    let mut child = Command::new(bin_path())
        .args([
            "acp-proxy",
            "--agy-bin",
            mock_agy.to_str().unwrap(),
            "--data-dir",
            "/nonexistent/plugin_data",
            "--native-auto-compact=80%",
        ])
        .env_remove("CONTEXT_RADAR_DATA_ROOT")
        .env_remove("CONTEXT_RADAR_GRANTS_PATH")
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
    let read_until =
        |r: &mut BufReader<std::process::ChildStdout>, want_id: u64| -> (Value, String) {
            let mut chunks = String::new();
            let mut line = String::new();
            loop {
                line.clear();
                let n = r.read_line(&mut line).unwrap();
                assert!(n > 0, "unexpected EOF waiting for id={want_id}");
                let v: Value = serde_json::from_str(line.trim()).unwrap();
                if v.get("id") == Some(&json!(want_id))
                    && (v.get("result").is_some() || v.get("error").is_some())
                {
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
    assert!(chunks.contains("COMPACTED_AND_RESUMED(80%)"));

    drop(stdin);
    let st = child.wait().unwrap();
    assert!(st.success());
}

#[cfg(unix)]
#[test]
fn test_proxy_log_uses_0600_and_refuses_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let td = TempDir::new().unwrap();
    let log_path = td.path().join("proxy.log");
    context_radar_mcp::acp::append_proxy_log_line(&log_path, "C->A hello").unwrap();
    let mode = fs::metadata(&log_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(fs::read_to_string(&log_path).unwrap(), "C->A hello\n");

    let target = td.path().join("outside.txt");
    fs::write(&target, "untouched").unwrap();
    let link_path = td.path().join("proxy_link.log");
    symlink(&target, &link_path).unwrap();
    assert!(context_radar_mcp::acp::append_proxy_log_line(&link_path, "pwn").is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
}
