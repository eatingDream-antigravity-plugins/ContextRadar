# ContextRadar (context-radar-mcp) — AI Agent & Operator Guide

Complete reference for AI agents, MCP clients, ACP editors, and power users configuring **ContextRadar** (`context-radar-mcp`). Homepage: <https://contextradar.eatingdream.dev>.

## MCP Tools

| Tool | Visibility | Purpose & Key Arguments |
| :--- | :--- | :--- |
| `context_radar_overview` | model + app | Opens the live panel and returns 4-state status (`ready`, `running`, `waiting`, `error`), headroom, thinking share, Stream/E2E speed, loop rollups, cold-cache counts, and subagents. Args: `action?`, `if_digest?`, `fresh?`, `conversation_id?`, `limit?`, `show_history?`, `include_archived?`, `workspace_filter?`. Pass `conversation_id` to bind direct `ui/message` actions to the owning session. |
| `context_radar_turns` | model + app | Per-turn token breakdown, thinking vs. response share, KV cache hit rate, 3-way cold-cache attribution (`turn_start_cold`, `mid_loop_cold` [`slow_turn_self_expiry` / `replica_spillover` / `model_switch_cold`], `post_compact_cold`), TTFT, Stream/E2E speed, and estimated cost. Args: `conversation_id?`, `limit?`. |
| `context_radar_hotspots` | model + app | Locates largest context growth steps since the last checkpoint (`mode: "hotspots"`, `top_k?`), or distills a step range into a `<600`-token digest (`mode: "distill"`, `start_step?`, `end_step?`, `save_note: true` writes atomically to `scratch/` with `O_NOFOLLOW` symlink guards without breaking the live KV prefix). |
| `radar_prepare_handoff` | model + app | Evaluates model budget tier, checkpoint cooldown, cold-rebuild protection, `running_subagent_count == 0`, and break-even turns. Args: `conversation_id?`, `estimated_remaining_turns?`, `upcoming_noisy_tokens?`. |
| `radar_commit_handoff` | model + app | Runs hard gates and issues a scoped single-use grant (`0600` local file) for `/compact` (`kind: "compact"`) or a fresh chat (`kind: "handoff"`). Args: `preserve_points`, `resume_task?`, `scratch_note_path?`, `conversation_id?`, `estimated_remaining_turns?`, `auto_fire?`, `kind?`. |
| `context_radar_ui_action` | app-only (SEP-1865) | Internal panel action dispatcher (`claim_grant`, `record_compact`, `record_relay`, `dismiss_grant`, `distill_slice`, `create_handoff`, `set_prefs`). Exposed in `tools/list` only when the client advertises MCP Apps (SEP-1865) capability. |

## Compaction & Handoff Protocols (Official Only)

### Antigravity App (SEP-1865 `ui/message`)
Once all hard gates pass (`running_subagent_count == 0`, budget & break-even satisfied), a visibly mounted inline handoff card (`ui://context-radar-mcp/handoff`) or matching-owner side pane (`!document.hidden`) claims the single-use grant and dispatches `/compact` (with automatic clipboard fallback) or launches a fresh chat (`com.google.antigravity/message`, `target: "new"`). Unattended compaction without an open UI view requires ACP below.

### `agy` CLI & ACP Editors (ACP v1 `session/prompt`)
`acp-proxy` and `acp-run` are pure-protocol ACP v1 thin clients (zero local SQLite or grant-file coupling, compatible with `--remote-agent`, SSH, and ACP editors such as Zed, JetBrains, Android Studio, Xcode, and VS Code ACP).

Point your custom ACP agent at `acp-proxy` (instead of `agy acp`) so completed `auto_fire` `radar_commit_handoff(kind: "compact")` calls inject native `/compact` in-place at `end_turn` (blocked gates return `isError: true` so the proxy never fires):

```json
{
  "command": "/path/to/home/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp",
  "args": ["acp-proxy"]
}
```

Optionally pass `--native-auto-compact <val>` (`AGY_CLI_AUTO_COMPACT_OVERRIDE`, e.g. `300K` or `80%`), or run unattended tasks with `acp-run` (headless authorization cancels allow-only permission prompts unless `--auto-approve` is explicitly passed):

```sh
~/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp acp-run \
  --cwd /path/to/project --auto-approve --prompt "Implement and verify the migration"
```

## Generic MCP Client Configuration (Claude Code, Codex, Cursor, OpenCode)

```json
{
  "mcpServers": {
    "context-radar-mcp": {
      "command": "/path/to/home/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp",
      "args": ["daemon", "--data-dir", "/path/to/home/.gemini/antigravity/plugin_data/context-radar-mcp"]
    }
  }
}
```

Add the portable skill (`agentskills.io` compatible):
```sh
npx skills add eatingDream-antigravity-plugins/ContextRadar
```

## CLI Subcommands & Environment Variables

| Command | Description |
| :--- | :--- |
| `context-radar-mcp daemon [--data-dir <path>] [--panel-html <path>]` | Run the stdio MCP server and SEP-1865 UI host. |
| `context-radar-mcp acp-proxy [options] [-- <agy acp flags...>]` | Wrap `agy acp` to auto-inject `/compact` after gated `commit_handoff`. |
| `context-radar-mcp acp-run --prompt <text> [options] [-- <agy acp flags...>]` | Run an unattended ACP session with gated in-place compaction. |
| `context-radar-mcp guard boot \| exec` | Optional session-start note injection (`boot`) and `PreToolUse` wait guard (`exec`). |
| `context-radar-mcp bench [--iterations <n>] [--data-root <path>]` | Local read-only telemetry benchmark. |

| Variable | Default / Purpose |
| :--- | :--- |
| `CONTEXT_RADAR_DATA_ROOT` | Override auto-detected Antigravity data directory (`~/.gemini/antigravity{,-ide,-cli}`). |
| `CONTEXT_RADAR_CONFIG` | Override settings file path (default `~/.config/context-radar-mcp/settings.json`; see [`settings.example.json`](../settings.example.json) and [`settings.schema.json`](../settings.schema.json)). |
| `CONTEXT_RADAR_VERSION` | Pin a specific release tag (`vX.Y.Z`) during `scripts/install.sh`. |
| `ANTIGRAVITY_PLUGINS_DIR` | Override plugin install directory during `scripts/install.sh`. |

## Build & Verify From Source

Requires Rust (`cargo`) and Node.js 20+ (panel pack & tests):

```sh
git clone https://github.com/eatingDream-antigravity-plugins/ContextRadar.git && cd ContextRadar
node panel/pack.mjs && node --test panel/panel.test.mjs
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --locked
cargo build --release --locked
```
