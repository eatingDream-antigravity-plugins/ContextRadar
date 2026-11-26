use std::env;
use std::path::PathBuf;
use std::process;
use std::time::Instant;

use serde_json::json;

use context_radar_mcp::acp::{parse_and_run_acp_headless, parse_and_run_acp_proxy};
use context_radar_mcp::engine::rates::RateTable;
use context_radar_mcp::engine::telemetry::TelemetryEngine;
use context_radar_mcp::guard::boot::{evaluate_boot_guard, run_boot_guard_cli};
use context_radar_mcp::guard::exec_gate::{evaluate_exec_gate, run_exec_gate_cli};
use context_radar_mcp::rpc::protocol::{run_stdio_daemon, SERVER_NAME, SERVER_VERSION};
use context_radar_mcp::settings::{default_data_root, expand_tilde, SettingsWatcher};
use context_radar_mcp::wire::pb_cursor::{decode_step_metadata, extract_gen_model};

fn print_usage() {
    eprintln!(
        "{SERVER_NAME} {SERVER_VERSION}\n\
Usage:\n  \
context-radar-mcp daemon [--data-dir <path>] [--panel-html <path>]\n  \
context-radar-mcp acp-proxy [options] [-- <agy acp flags...>]\n  \
context-radar-mcp acp-run --prompt <text> [options] [-- <agy acp flags...>]\n  \
context-radar-mcp guard boot\n  \
context-radar-mcp guard exec\n  \
context-radar-mcp bench [--iterations <n>] [--data-root <path>]\n  \
context-radar-mcp --version"
    );
}

fn run_bench(args: &[String]) {
    let mut iterations: usize = 20;
    let mut data_root: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--iterations" if i + 1 < args.len() => {
                iterations = args[i + 1].parse().unwrap_or(20).max(1);
                i += 2;
            }
            "--data-root" if i + 1 < args.len() => {
                data_root = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            _ => i += 1,
        }
    }

    let root = data_root.unwrap_or_else(default_data_root);
    let mut cold_engine = TelemetryEngine::new(
        Some(root.clone()),
        Some(RateTable::new(
            SettingsWatcher::new(Some("none".into())),
            None,
            None,
        )),
        Some(None),
    );
    let t_cold = Instant::now();
    let cold_snap = cold_engine.build_overview_snapshot(25, false, false, "", None);
    let cold_ms = t_cold.elapsed().as_secs_f64() * 1000.0;

    let mut warm_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let _ = cold_engine.build_overview_snapshot(25, false, false, "", None);
        warm_times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    warm_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let warm_med_ms = warm_times[warm_times.len() / 2];
    let warm_min_ms = warm_times[0];

    // Protobuf decode micro-benchmark (64 KiB padded gen_metadata + step_metadata)
    let mut padded_gen = vec![0x0a];
    let mut inner = Vec::new();
    inner.extend_from_slice(&[0x9a, 0x01, 12]);
    inner.extend_from_slice(b"gemini-3-pro");
    inner.extend_from_slice(&[0xaa, 0x01, 19]);
    inner.extend_from_slice(b"Gemini 3 Pro (High)");
    inner.extend_from_slice(&[0x9a, 0x06, 0x80, 0x80, 0x04]);
    inner.resize(inner.len() + 65536, b'x');
    // encode length varint
    let mut l = inner.len();
    loop {
        let b = (l & 0x7f) as u8;
        l >>= 7;
        if l > 0 {
            padded_gen.push(b | 0x80);
        } else {
            padded_gen.push(b);
            break;
        }
    }
    padded_gen.extend_from_slice(&inner);

    let proto_iters = 50_000usize;
    let t_proto = Instant::now();
    let mut hits = 0usize;
    for _ in 0..proto_iters {
        let (c, d) = extract_gen_model(&padded_gen);
        if !c.is_empty() && !d.is_empty() {
            hits += 1;
        }
    }
    let proto_ns_per_op = (t_proto.elapsed().as_nanos() as f64) / (proto_iters as f64);

    // Sample in-process guard evaluation micro-bench
    let exec_payload = json!({
        "toolCall": {
            "name": "run_command",
            "args": { "CommandLine": "cargo test", "WaitMsBeforeAsync": 1000 }
        }
    });
    let boot_payload = json!({ "cwd": "/tmp" });
    let empty_cfg = json!({});
    let t_guard = Instant::now();
    for _ in 0..10_000 {
        let _ = evaluate_exec_gate(&exec_payload, Some(&empty_cfg));
        let _ = evaluate_boot_guard(&boot_payload, &empty_cfg);
    }
    let guard_us_per_pair = (t_guard.elapsed().as_micros() as f64) / 10_000.0;

    // Ensure decode_step_metadata symbol is exercised
    let _ = decode_step_metadata(&[]);

    let report = json!({
        "data_root": root.to_string_lossy(),
        "total_db_files": cold_snap.summary.total_db_files,
        "shown_main_count": cold_snap.summary.shown_main_count,
        "shown_subagent_count": cold_snap.summary.shown_subagent_count,
        "cold_snapshot_ms": (cold_ms * 100.0).round() / 100.0,
        "warm_snapshot_median_ms": (warm_med_ms * 100.0).round() / 100.0,
        "warm_snapshot_min_ms": (warm_min_ms * 100.0).round() / 100.0,
        "proto_64k_early_exit_ns": proto_ns_per_op.round(),
        "proto_hits": hits,
        "guard_eval_pair_us": (guard_us_per_pair * 100.0).round() / 100.0,
        "snapshot_digest": cold_snap.digest
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    cold_engine.close();
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        process::exit(2);
    }
    match args[0].as_str() {
        "--version" | "-V" | "version" => {
            println!("{SERVER_NAME} {SERVER_VERSION}");
        }
        "daemon" => {
            let mut panel_override = None;
            let mut plugin_data_dir = None;
            let mut i = 1;
            while i < args.len() {
                let arg = args[i].as_str();
                if arg == "--panel-html" && i + 1 < args.len() {
                    let val = args[i + 1].trim();
                    if !val.is_empty() {
                        panel_override = Some(PathBuf::from(expand_tilde(val)));
                    }
                    i += 2;
                } else if let Some(val) = arg.strip_prefix("--panel-html=") {
                    if !val.trim().is_empty() {
                        panel_override = Some(PathBuf::from(expand_tilde(val.trim())));
                    }
                    i += 1;
                } else if (arg == "--data-dir" || arg == "--data_dir") && i + 1 < args.len() {
                    let val = args[i + 1].trim();
                    if !val.is_empty() {
                        plugin_data_dir = Some(PathBuf::from(expand_tilde(val)));
                    }
                    i += 2;
                } else if let Some(val) = arg
                    .strip_prefix("--data-dir=")
                    .or_else(|| arg.strip_prefix("--data_dir="))
                {
                    if !val.trim().is_empty() {
                        plugin_data_dir = Some(PathBuf::from(expand_tilde(val.trim())));
                    }
                    i += 1;
                } else {
                    i += 1;
                }
            }
            if let Err(err) = run_stdio_daemon(panel_override, plugin_data_dir) {
                eprintln!("[{SERVER_NAME}] daemon error: {err}");
                process::exit(1);
            }
        }
        "acp-proxy" => match parse_and_run_acp_proxy(&args[1..]) {
            Ok(code) => process::exit(code),
            Err(err) => {
                eprintln!("[{SERVER_NAME}] acp-proxy error: {err}");
                process::exit(1);
            }
        },
        "acp-run" => match parse_and_run_acp_headless(&args[1..]) {
            Ok(code) => process::exit(code),
            Err(err) => {
                eprintln!("[{SERVER_NAME}] acp-run error: {err}");
                process::exit(1);
            }
        },
        "guard" => match args.get(1).map(String::as_str) {
            Some("boot") => run_boot_guard_cli(),
            Some("exec") => run_exec_gate_cli(),
            _ => {
                print_usage();
                process::exit(2);
            }
        },
        "bench" => run_bench(&args[1..]),
        _ => {
            print_usage();
            process::exit(2);
        }
    }
}
