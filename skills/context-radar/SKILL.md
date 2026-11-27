---
name: context-radar
description: >-
  Monitor Antigravity session, subagent, and aside context budgets, diagnose
  cache misses, save key-point notes without breaking active caches, and run
  gated /compact or new-chat handoffs using context-radar-mcp tools.
---

# Context Radar Workflow

## MCP Tools

1. `context_radar_overview({ action?, if_digest?, fresh?, conversation_id?, limit?, show_history?, include_archived?, workspace_filter? })` — Opens the live panel and returns status (`ready`/`running`/`waiting`/`error`), headroom, thinking share, speed, cost, cache misses, subagents, and `/btw` asides. Pass `conversation_id` to bind direct `ui/message` actions to the owning session.
2. `context_radar_turns({ conversation_id?, limit? })` — Per-turn context size, thinking vs. response split, cache hit rate, cache-miss attribution (`turn_start_cold`, `mid_loop_cold`, `post_compact_cold`), latency, speed, and cost.
3. `context_radar_hotspots({ conversation_id?, mode?: "hotspots" | "distill", start_step?, end_step?, save_note?, top_k? })` — `hotspots` locates top context-growth steps since the last checkpoint; `distill` writes a `<600`-token summary (`save_note: true` saves to `scratch/` without touching the live cache).
4. `radar_prepare_handoff({ conversation_id?, estimated_remaining_turns?, upcoming_noisy_tokens? })` — Evaluates model budget tier, checkpoint cooldown, recent-rebuild protection, running subagents, and break-even turns.
5. `radar_commit_handoff({ preserve_points, resume_task?, scratch_note_path?, conversation_id?, estimated_remaining_turns?, auto_fire?, kind?: "handoff" | "compact" })` — Runs hard gates and issues a scoped single-use grant for `/compact` (`kind: "compact"`) or a fresh chat (`kind: "handoff"`, `target: "new"`). Pass `auto_fire: false` during interactive Q&A. Under `agy` CLI / ACP (`acp-proxy` / `acp-run`), call `kind: "compact", auto_fire: true` and end the turn cleanly for unattended in-place compaction.

## Configuration (`settings.json`)

- **Path**: `~/.gemini/antigravity/plugin_data/context-radar-mcp/settings.json` (with fallback to `~/.config/context-radar-mcp/settings.json` or `CONTEXT_RADAR_CONFIG`).
- **Live hot-reload**: Changes are auto-reloaded on file `(mtime, size, inode)` change on the next poll or tool call — no Antigravity restart needed. Syntax errors fail closed (last good settings stay active).
- **GUI-managed keys** (edited via the panel `⚙` drawer or directly in JSON):
  - `lang`: `"auto"` | `"en"` | `"zh-CN"`
  - `pricing.currency_code`: `"auto"` | `"USD"` | `"CNY"` and `pricing.fx_rate` (`0.01–1000`)
  - `ui.poll_interval_sec` (`0` = auto, or `2–300`) and `ui.bg_poll_interval_sec` (`0` = auto, `-1` = paused, or `1–3600`)
  - `ui.allow_auto_handoff` (`true` | `false`) and `context.compact_threshold` (default `360000`)
- For advanced model pricing, tier rules, and guard hooks, see `settings.schema.json` and `docs/ai-guide.md`.

## Best Practices

- **Protect warm caches**: Batch reads and tests in parallel; before compacting, save key points with `context_radar_hotspots(mode: "distill", save_note: true)` or isolate noisy exploration in a subagent.
- **Budget defaults**: On a `512k` window, global defaults are `300k / 360k / 430k` (warn / compact / critical), aligned with each model tier's `urgent_ctx`.
- **Respect hard gates & boundaries**: Call `radar_commit_handoff` only at a clean milestone once `radar_prepare_handoff` reports `gate_allowed: true`.
- **Subagents & asides**: Neither has a `/compact` input box — wrap up subagents and continue in a fresh worker; handle `/btw` asides from their parent chat.
- **Command waiting**: Pass `WaitMsBeforeAsync: 10000` for normal `run_command` calls (`IsDaemon: true` for long-running servers/watchers); the `PreToolUse` guard exempts daemons automatically.

## Cross-Client Setup

Any stdio MCP client (Antigravity, Claude Code, Codex, Cursor, OpenCode) can connect via `context-radar-mcp daemon --data-dir <dir>`. For unattended CLI compaction, wrap `agy acp` via `context-radar-mcp acp-proxy` or `acp-run`.
