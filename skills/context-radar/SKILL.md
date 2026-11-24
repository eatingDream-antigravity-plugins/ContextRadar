---
name: context-radar
description: >-
  Monitor Antigravity session and subagent context budgets, attribute 3-way
  cold-cache misses, distill bloated history slices to scratch/ without
  invalidating KV prefixes, and execute gated /compact or official target:new
  relay handoffs using context-radar-mcp tools.
---

# Context Radar Workflow

Use `context-radar-mcp` tools to inspect live context telemetry, keep KV cache prefixes hot, and perform safe compaction or relay handoffs only when economically justified.

## MCP Tools

1. `context_radar_overview({ action?: "snapshot" | "turns", if_digest?, fresh?, conversation_id?, limit?, show_history?, include_archived?, workspace_filter? })`
   - Opens the interactive Context Radar side/global panel (`ui://context-radar-mcp/panel`) and returns a summary of active main sessions, running subagents, cache hit rates, 3-way cold-cache counts, median TTFT/stream speed, and cost.
2. `context_radar_turns({ conversation_id?, limit? })`
   - Returns per-turn context length, cache hit rate, cold-cache attribution (`turn_start_cold`, `mid_loop_cold`, `post_compact_cold`), TTFT, throughput, and turn cost.
3. `context_radar_hotspots({ conversation_id?, mode?: "hotspots" | "distill", start_step?, end_step?, save_note?, top_k? })`
   - `mode: "hotspots"` locates the highest-growth steps since the last `CHECKPOINT` via 256 KiB reverse scanning with zero impact on the live prompt prefix.
   - `mode: "distill"` compacts a historical step range into a `<600` token digest (optionally persisted to the session `scratch/` directory via `save_note: true`).
4. `context_radar_prepare_handoff({ conversation_id?, estimated_remaining_turns?, upcoming_noisy_tokens? })`
   - Evaluates model x reasoning-effort budget tiers, cooldown turns since last checkpoint, recent cold-rebuild protection, `running_subagent_count == 0`, and economic break-even turns.
5. `context_radar_commit_handoff({ preserve_points, resume_task?, scratch_note_path?, conversation_id?, estimated_remaining_turns?, auto_fire?, kind?: "handoff" | "compact" })`
   - Runs server-side hard gates before issuing a single-use replay-protected grant, a 1-click `Send /compact` & copyable `/compact` command, and an official `target:new` handoff prompt (`ui://context-radar-mcp/handoff`).
   - On Antigravity App: use `kind: "handoff", auto_fire: true` for multi-phase background continuation via official `target:new`, or `auto_fire: false` during interactive Q&A to keep the current window.
   - On `agy` CLI / ACP editors wrapped by `context-radar-mcp acp-proxy` or `acp-run`: call `kind: "compact", auto_fire: true` with `preserve_points` and `resume_task`, then end your turn cleanly so the official ACP proxy executes native in-place `/compact` on the same session and resumes automatically.

## Best Practices

- **Protect KV hot prefixes**: Batch related file reads and tests in parallel. Before considering full compaction, distill older high-bloat slices with `context_radar_hotspots(mode: "distill", save_note: true)` or isolate noisy exploratory probes in a forked helper (`swarm(action: "add", fork: "coordinator")`) and `kill` it after collecting a concise summary.
- **Respect hard gates and phase boundaries**: Only call `context_radar_commit_handoff` at a clean phase boundary once `context_radar_prepare_handoff` reports `gate_allowed: true` (`running_subagent_count == 0`, above model tier `min_ctx`, cooldown satisfied, and remaining turns >= break-even turns).
- **Subagent baton pass**: Subagents have no interactive `/compact` input box. When a subagent approaches its model tier's recommended threshold, wrap up its current atomic step, report concise results, and let the coordinator spawn a fresh subagent if further work remains.
- **Command waiting and guards**: For normal synchronous `run_command` calls, pass `WaitMsBeforeAsync: 10000` (and set `IsDaemon: true` for long-running servers/watchers). The built-in `PreToolUse` guard exempts daemons and background patterns automatically.
