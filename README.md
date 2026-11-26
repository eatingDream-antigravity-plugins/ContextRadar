<div align="center">
<img src="assets/logo.svg" alt="Context Radar MCP" width="80" height="80">

# Context Radar MCP

**See how full your Antigravity context really is, why a turn got slow, and when it's actually worth compacting or starting fresh.**

[![CI](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-555)

English | [简体中文](README.zh-CN.md)

<img src="docs/social-preview.png" alt="Context Radar MCP panel preview" width="860">
</div>

## What it does

Long Antigravity sessions slow down and get expensive when context bloats or the prompt cache breaks silently. Context Radar adds a live side panel and MCP tools so you and your agent can spot cache misses, trim bloated history safely, and compact or start fresh only when it pays off.

- **Live 4-state telemetry & headroom**: Tracks main sessions and running subagents (`ready`, `running` [LLM / Tool / Compacting], `waiting`, `error`), remaining context headroom, thinking vs. response token split, Stream/E2E speed, loop rollups, and estimated cost in real time.
- **Slow-turn & cross-model diagnostics**: Distinguishes normal new-turn misses, mid-loop cache breaks, and post-compact rebuilds, flags cross-model `/compact` cache resets, and shows checkpoint summary size (`KB`).
- **Hotspot analysis & prefix-safe distillation**: Pinpoints turns that caused context spikes and saves short digests to `scratch/` without breaking the live prompt KV cache.
- **Gated `/compact` & fresh-chat handoffs**: Evaluates model budgets and break-even economics before triggering `/compact` or a new-chat handoff via official protocols only.
- **Optional guardrails**: Prevents short shell commands from being backgrounded too early and optionally injects a workspace status note at session start.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar-mcp/main/scripts/install.sh | sh
```

Installs the SHA-256 verified prebuilt binary (macOS Apple Silicon, macOS Intel, Linux x86_64) into `~/.gemini/config/plugins/context-radar-mcp`. Reload Antigravity afterward. Set `CONTEXT_RADAR_VERSION=vX.Y.Z` or `ANTIGRAVITY_PLUGINS_DIR=...` to pin a release or custom folder.

## Usage

Open **Context Radar** from the Antigravity side pane, or ask your agent directly—e.g. *"Why did the last few turns get slow?"*, *"Find bloated turns and summarize them"*, or *"Is it worth running `/compact` now?"*.

| Tool | What it does |
| :--- | :--- |
| `context_radar_overview` | Opens the live panel and returns 4-state status, headroom, thinking share, Stream/E2E speed, loop rollup, and subagents. |
| `context_radar_turns` | Breaks down per-turn tokens, thinking/response split, cache hits, cold-cache causes, speed, and cost. |
| `context_radar_hotspots` | Finds the biggest context spikes and distills selected slices into `scratch/`. |
| `context_radar_prepare_handoff` | Checks whether `/compact` or a fresh session breaks even right now. |
| `context_radar_commit_handoff` | Runs hard gates and issues a scoped single-use `/compact` or new-chat handoff grant. |

### Compaction & Handoff (Official Protocols Only)

- **Antigravity App (SEP-1865 `ui/message`)**: Once hard gates pass (`running_subagent_count == 0`, budget & break-even satisfied), a visibly mounted inline handoff card (`ui://context-radar-mcp/handoff`) or matching-owner side pane (`!document.hidden`) claims the single-use grant to dispatch `/compact` (with automatic clipboard fallback) or open a fresh chat (`com.google.antigravity/message`, `target: "new"`). Because Antigravity App does not mount hidden headless iframes for collapsed tool calls and exposes no headless MCP/hook `/compact` API, unattended compaction without an open UI view requires ACP below.
- **`agy` CLI & ACP editors (ACP v1 `session/prompt`)**: `acp-proxy` and `acp-run` are pure-protocol ACP v1 thin clients (zero local SQLite or grant-file coupling, compatible with `--remote-agent`, SSH, and ACP editors like Zed, JetBrains, Android Studio, Xcode, and VS Code ACP). Point your custom ACP agent at `acp-proxy` (instead of `agy acp`) so completed `auto_fire` `context_radar_commit_handoff(kind: "compact")` calls inject native `/compact` in-place at `end_turn` (blocked gates return `isError: true` so the proxy never fires), optionally pairing `--native-auto-compact <val>` (`AGY_CLI_AUTO_COMPACT_OVERRIDE`, e.g. `300K` or `80%`), or run unattended tasks with `acp-run`:

```json
{
  "command": "~/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp",
  "args": ["acp-proxy"]
}
```

```sh
~/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp acp-run \
  --cwd /path/to/project --auto-approve --prompt "Implement and verify the migration"
```

## Configuration & Safety

No config file is needed out of the box—language (`en` / `zh-CN`), currency (`USD` / `CNY`), and Antigravity data dirs (`~/.gemini/antigravity{,-ide,-cli}`) are auto-detected. Copy [settings.example.json](settings.example.json) to `~/.config/context-radar-mcp/settings.json` for custom pricing, thresholds, or startup notes (or set `CONTEXT_RADAR_DATA_ROOT` / `CONTEXT_RADAR_CONFIG`).

- **100% local & read-only**: Zero network requests or telemetry; reads Antigravity SQLite ledgers with `SQLITE_OPEN_READ_ONLY` plus read-only WAL/JSONL tailing.
- **Prefix-safe `scratch/` distillation**: Writes summaries out-of-band instead of mutating history, keeping the live KV prefix hot while fresh chats or subagents read digests on demand.
- **Scoped grants & safe guards**: Single-use grants bind to `conversation_id` + `workspace` in `0600` local files; the optional `PreToolUse` wait guard exempts explicit daemons and async builds automatically.

## AI Agent Integration

- **Antigravity**: One-line installer above registers the MCP server, live panel, and `skills/context-radar/SKILL.md` automatically.
- **Claude Code / Codex / Cursor / OpenCode (any MCP stdio client)**: Point your MCP config to the installed binary and add the portable skill [`skills/context-radar/SKILL.md`](skills/context-radar/SKILL.md) (or `npx skills add eatingDream-antigravity-plugins/context-radar-mcp`):

```json
{
  "mcpServers": {
    "radar": {
      "command": "/path/to/context-radar-mcp",
      "args": ["daemon", "--data-dir", "/path/to/context-radar-data"]
    }
  }
}
```

- **LLM index & coding-agent guide**: [`llms.txt`](llms.txt) · [`AGENTS.md`](AGENTS.md)

## Build from source

Requires Rust (`cargo`) and Node.js 20+ (only to bundle and test the panel).

```sh
git clone https://github.com/eatingDream-antigravity-plugins/context-radar-mcp.git && cd context-radar-mcp
node panel/pack.mjs && node --test panel/panel.test.mjs
cargo test --locked && cargo build --release --locked
```

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md), and [SECURITY.md](SECURITY.md). Licensed under the [MIT License](LICENSE).
