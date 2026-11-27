---
name: context-radar
description: >-
  Monitor Antigravity session and subagent context budgets, attribute 3-way
  cold-cache misses, distill bloated history slices to scratch/ without
  invalidating KV prefixes, and execute gated /compact or official target:new
  relay handoffs using context-radar-mcp tools.
---

# Context Radar Workflow

## MCP Tools

1. `context_radar_overview({ action?, if_digest?, fresh?, conversation_id?, limit?, show_history?, include_archived?, workspace_filter? })` — Opens the live panel and returns 4-state status (`ready`/`running`/`waiting`/`error`), headroom, thinking share, Stream/E2E speed, loop rollups, cross-model `/compact` notice, cold-cache counts, and subagents. Pass `conversation_id` to bind direct `ui/message` actions to the owning session.
2. `context_radar_turns({ conversation_id?, limit? })` — Per-turn context tokens, thinking vs. response split, cache hit rate, 3-way cold-cache attribution (`turn_start_cold`, `mid_loop_cold`, `post_compact_cold`), TTFT, Stream/E2E speed, and cost.
3. `context_radar_hotspots({ conversation_id?, mode?: "hotspots" | "distill", start_step?, end_step?, save_note?, top_k? })` — `hotspots` locates top context-growth steps since the last checkpoint; `distill` compacts a step range into a `<600`-token digest (`save_note: true` writes to `scratch/` without breaking the live KV prefix).
4. `radar_prepare_handoff({ conversation_id?, estimated_remaining_turns?, upcoming_noisy_tokens? })` — Evaluates model budget tier, checkpoint cooldown, cold-rebuild protection, `running_subagent_count == 0`, and break-even turns.
5. `radar_commit_handoff({ preserve_points, resume_task?, scratch_note_path?, conversation_id?, estimated_remaining_turns?, auto_fire?, kind?: "handoff" | "compact" })` — Runs hard gates and issues a scoped single-use grant for `/compact` (`kind: "compact"`) or a fresh chat (`kind: "handoff"`, `target: "new"`). Pass `auto_fire: false` during interactive Q&A. In Antigravity App, dispatch requires a visible card/pane (`ui/message`, clipboard fallback); under `agy` CLI / ACP (`acp-proxy` / `acp-run` pure-protocol thin clients, optionally with `--native-auto-compact`), call `kind: "compact", auto_fire: true` and end the turn cleanly for unattended in-place compaction (blocked gates return `isError: true` under ACP).

## Best Practices

- **Protect KV prefixes**: Batch reads and tests in parallel; before compacting, distill old bloat with `context_radar_hotspots(mode: "distill", save_note: true)` or isolate noisy probes in a disposable helper and kill it after collecting a concise summary.
- **Calibrated budget & TTL defaults**: On a `512k` window, global defaults are `300k / 360k / 430k` (warn / compact / critical), and panel critical status aligns 1:1 with the resolved tier's `urgent_ctx` (`carbon_heavy`: `260k/320k/400k`, `high_reasoning`/`standard`: `300k/360k/430k`, `flagship`: `310k/370k/440k`, `claude_tier`: `300k/360k/440k`, `fast_reasoning`: `320k/380k/445k`, `fast`: `340k/400k/460k`). Prefill-anchored implicit KV cache TTLs are `180s` for Gemini 4 Carbon/Argon, `240s` for Gemini 4 Neon/Hydrogen, and `300s` for other models.
- **Respect hard gates & boundaries**: Call `radar_commit_handoff` only at a clean phase boundary once `radar_prepare_handoff` reports `gate_allowed: true`.
- **Subagent baton pass**: Subagents lack `/compact`; wrap up cleanly near their budget threshold and let the coordinator spawn a fresh worker if work remains.
- **Command waiting**: Pass `WaitMsBeforeAsync: 10000` for normal `run_command` calls (`IsDaemon: true` for long-running servers/watchers); the `PreToolUse` guard exempts daemons automatically.

## Cross-Client Setup

Any stdio MCP client (Antigravity, Claude Code, Codex, Cursor, OpenCode) can connect via `context-radar-mcp daemon --data-dir <dir>`. For unattended CLI compaction, wrap `agy acp` via `context-radar-mcp acp-proxy` or `acp-run`.
