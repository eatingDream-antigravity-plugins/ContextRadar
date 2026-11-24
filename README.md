<div align="center">

<img src="assets/logo.svg" alt="Context Radar MCP" width="80" height="80">

# Context Radar MCP

**See how full your Antigravity context really is, why a turn got slow, and when it's actually worth compacting or starting fresh.**

[![CI](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/badge/release-v1.1.0-D97757)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/releases)<!-- x-release-please-version -->
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-555)

English | [简体中文](README.zh-CN.md)

<img src="docs/social-preview.png" alt="Context Radar MCP panel preview" width="860">

</div>

## Why this exists

Long coding sessions in Antigravity tend to slow down and get expensive without telling you why. Maybe one huge file dump ate half the window, maybe something broke the prompt cache mid-turn, or maybe you ran `/compact` too early and threw away a warm cache for nothing.

Context Radar puts a live status panel in Antigravity so you can see what is going on and let your agent make smarter decisions about trimming history or handing off to a new chat.

## What it does

- **Live context and cost meter**: Shows how much of your context window is used across the current chat and any running subagents, along with cache hit rate, response speed, and estimated cost. Stays live with adaptive `if_digest` polling (1s foreground / 2s background, 15s timeout) and refreshes immediately when a kept-alive side/global pane is revealed (`ResizeObserver`, `IntersectionObserver`, `onhostcontextchanged`, and passive `pointerenter`).
- **Explains slow turns**: Tells the difference between a normal new-turn cache miss, an unexpected mid-loop cache break, and a post-compact rebuild, so you know what actually slowed things down.
- **Finds and trims bloated history**: Points out the specific turns that caused context spikes and lets your agent save a short summary of old work into `scratch/` instead of dragging raw logs around.
- **Smarter `/compact` and fresh-chat handoffs**: Checks whether compacting or moving to a new conversation will actually save tokens over the next few turns, then dispatches via official protocols only—SEP-1865 `ui/message` (`Send /compact`, guarded auto-compact, or `target:new` handoff with automatic clipboard fallback) from a visibly mounted Antigravity App card/pane, or unattended in-place `/compact` on `agy acp` via `acp-proxy` and `acp-run`.
- **Helpful optional guardrails**: Can gently keep the agent from backgrounding short shell commands too early, and optionally inject a short workspace status file at session start if you configure one.

## Install

### macOS & Linux (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar-mcp/main/scripts/install.sh | sh
```

This downloads the prebuilt binary for your platform (macOS Apple Silicon, macOS Intel, or Linux x86_64), checks its SHA-256 hash, and installs the plugin into `~/.gemini/config/plugins/context-radar-mcp`. Restart or reload Antigravity afterward to pick it up.

To pin a specific release or custom plugin folder:

```sh
CONTEXT_RADAR_VERSION=v1.0.0 ANTIGRAVITY_PLUGINS_DIR=~/.gemini/config/plugins \
  curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar-mcp/main/scripts/install.sh | sh
```

## How to use it

### In Antigravity

Open **Context Radar** from the Antigravity side pane to watch your live context gauge, turn history, and cache health.

You can also just ask your agent in plain language. The plugin includes an on-demand skill so the agent knows how to read the numbers and act on them:

- *"Open Context Radar and show me how full this session is."*
- *"Why did the last few turns get slow? Did the cache break?"*
- *"Find which turns bloated the context and summarize them."*
- *"Is it worth running `/compact` now, or should we hand off to a new chat?"*

### Tools available to your agent

| Tool | What it does |
| :--- | :--- |
| `context_radar_overview` | Opens the live panel and returns the current session and subagent summary. |
| `context_radar_turns` | Breaks down per-turn tokens, cache hits, cache-miss causes, speed, and estimated cost. |
| `context_radar_hotspots` | Finds the biggest context spikes in the conversation and summarizes selected slices to `scratch/`. |
| `context_radar_prepare_handoff` | Checks whether `/compact` or a fresh session makes sense right now. |
| `context_radar_commit_handoff` | Creates the one-click `/compact` or new-conversation handoff card once checks pass. |

### Autonomous Compaction & Handoff Protocol Boundaries

Context Radar strictly uses official documented protocols (never undocumented host RPCs):

- **Antigravity App (SEP-1865 MCP Apps `ui/message`)**: When `context_radar_commit_handoff` passes all gates (`running_subagent_count == 0`, budget & break-even satisfied), a **visibly mounted** inline handoff card (`ui://context-radar-mcp/handoff`) or matching-owner `aux_pane` (`!document.hidden`) can atomically claim the single-use grant and dispatch `/compact` (`ui/message`) or open a fresh chat (`com.google.antigravity/message` `target: "new"`), automatically copying `/compact` to the clipboard if the host rejects the message. Because Antigravity App does not mount hidden headless iframes for collapsed tool calls and exposes no headless MCP/hook `/compact` API, fully unattended compaction without an open UI view requires ACP below.
- **`agy` CLI & ACP Editors (Official ACP v1 `session/prompt`)**: For **Antigravity CLI (`agy`)** and ACP-compatible editors (**Zed, JetBrains IDEs, Android Studio, Xcode, VS Code ACP Client**), Context Radar ships two official ACP v1 subcommands that execute unattended native `/compact` (and compact-and-continue) in-place on the same ACP session with zero concurrent session conflicts:

- **Editor transparent proxy (`acp-proxy`)** — point your editor's ACP custom agent to `context-radar-mcp acp-proxy` instead of `agy acp`:

```json
{
  "command": "~/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp",
  "args": ["acp-proxy"]
}
```

When the agent finishes a turn after calling `context_radar_commit_handoff(kind: "compact", auto_fire: true, ...)`, `acp-proxy` atomically claims the single-use grant and injects native ACP `session/prompt` `/compact ...` on the exact same connection before returning control to your editor.

- **Headless CLI / CI runner (`acp-run`)** — run autonomous long tasks directly from the terminal:

```sh
~/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp acp-run \
  --cwd /path/to/project --auto-approve --prompt "Implement and verify the migration"
```

## Configuration (optional)

No config file is needed out of the box. Language (`en` / `zh-CN`) and display currency (`USD` `$` / `CNY` `¥`) are auto-detected from your locale, and Antigravity's data folder is auto-discovered across `~/.gemini/antigravity`, `~/.gemini/antigravity-ide`, and `~/.gemini/antigravity-cli`.

If you want custom model pricing, display currency, budget thresholds, or workspace startup notes, copy [settings.example.json](settings.example.json) to `~/.config/context-radar-mcp/settings.json`. You can override the data directory with `CONTEXT_RADAR_DATA_ROOT` or point to a custom settings file with `CONTEXT_RADAR_CONFIG`.

## How It Works & Safety Principles

Everything runs locally on your machine with zero network requests and no telemetry:

- **Strictly read-only database access**: Context Radar opens Antigravity's SQLite ledger using `SQLITE_OPEN_READ_ONLY` (plus read-only WAL inspection) and tails transcript JSONL files without modifying or locking your conversation history.
- **Prefix-safe `scratch/` distillation**: Editing or deleting messages mid-conversation invalidates the model's prompt KV cache and triggers an expensive cold rebuild. Instead, `context_radar_hotspots` writes compact summaries out-of-band under `scratch/` so the live KV prefix stays untouched while fresh subagents or `target:new` handoff chats can read the digest on demand.
- **Scoped single-use relay grants**: Handoff grants are bound to both `conversation_id` and `workspace` and stored locally in `0600` files under plugin data, preventing cross-project side panes from triggering or hijacking another workspace's handoff.
- **Safe `PreToolUse` wait guard**: The optional hook only advises against prematurely backgrounding quick shell commands (and automatically allows explicit daemons and async builds), without altering system configuration or killing processes.

## Build from source

Requires Rust (`cargo`) and Node.js 20+ (only to bundle and test the panel).

```sh
git clone https://github.com/eatingDream-antigravity-plugins/context-radar-mcp.git
cd context-radar-mcp
node panel/pack.mjs && node --test panel/panel.test.mjs
cargo test --locked
cargo build --release --locked
```

## Contributing & License

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md), and [SECURITY.md](SECURITY.md).

Licensed under the [MIT License](LICENSE).
