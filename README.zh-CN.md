<div align="center">
<img src="assets/logo.svg" alt="Context Radar MCP" width="80" height="80">

# Context Radar MCP

**看清 Antigravity 的上下文到底占了多少、为什么突然变慢，以及什么时候才值得 `/compact` 或开新对话。**

[![CI](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-555)

[English](README.md) | 简体中文

<img src="docs/social-preview.png" alt="Context Radar MCP 面板预览" width="860">
</div>

## 它能帮你做什么

Antigravity 长对话一旦上下文膨胀或提示词缓存悄悄断掉，就会变慢且费用飙升。Context Radar 提供实时侧边栏面板和 MCP 工具，让你和 Agent 看清缓存与水位变化，只在真正划算时整理历史或压缩会话。

- **实时四态遥测与余量监控**：实时展示主会话与运行中子 Agent 的四态运行/压缩状态（`ready` / `running` [LLM·工具·压缩中] / `waiting` / `error`）、上下文余量、思考与正文输出占比、流式/端到端速度、本轮多步循环汇总及估算费用。
- **定位变慢原因与跨模型压缩提示**：自动区分新轮次正常未命中、对话中途意外断缓存与压缩后首次重建，识别跨模型 `/compact` 缓存重置并展示检查点摘要体积（`KB`）。
- **热点排查与不破缓存提炼**：定位撑大上下文的轮次，将早期冗长历史浓缩为简短摘要存入 `scratch/`，保持当前会话 KV 前缀完整复用。
- **带门槛的 `/compact` 与新会话交接**：结合模型预算与回本轮数判断是否值得操作，严格通过官方协议执行 `/compact` 或新对话交接。
- **可选护栏**：温和提醒避免把短命令过早转入后台，并支持在会话启动时自动注入自定义工作区状态文件。

## 安装

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar-mcp/main/scripts/install.sh | sh
```

脚本会下载经 SHA-256 校验的预编译程序（支持 macOS Apple Silicon、macOS Intel、Linux x86_64）并安装到 `~/.gemini/config/plugins/context-radar-mcp`，完成后重启或重载 Antigravity 即可。可通过 `CONTEXT_RADAR_VERSION=vX.Y.Z` 或 `ANTIGRAVITY_PLUGINS_DIR=...` 指定版本或安装目录。

## 怎么用

在侧边栏打开 **Context Radar** 面板，或直接对 Agent 提问，例如 *"刚才几轮怎么这么慢？"*、*"找出最占上下文的轮次并整理摘要"* 或 *"现在适合 `/compact` 吗？"*。

| 工具 | 作用 |
| :--- | :--- |
| `context_radar_overview` | 打开实时面板，返回四态状态、上下文余量、思考占比、流式/端到端速度、本轮循环汇总与子 Agent 概览。 |
| `context_radar_turns` | 按轮次拆分 Token 占用、思考/正文占比、缓存命中、冷启动原因、速度与估算费用。 |
| `context_radar_hotspots` | 定位上下文激增片段，并按需将选定区间提炼为摘要写入 `scratch/`。 |
| `context_radar_prepare_handoff` | 评估当前执行 `/compact` 或开新对话是否满足预算与回本条件。 |
| `context_radar_commit_handoff` | 校验硬门槛后签发作用域绑定的单次 `/compact` 或新对话交接票据。 |

### 自主压缩与交接（仅使用官方协议）

- **Antigravity App（SEP-1865 `ui/message`）**：通过全部硬门槛（`running_subagent_count == 0`、预算与回本达标）后，可见挂载的内联交接卡片（`ui://context-radar-mcp/handoff`）或归属匹配的侧边栏（`!document.hidden`）会核销单次票据并触发 `/compact`（失败自动复制兜底）或拉起新对话（`com.google.antigravity/message`，`target: "new"`）。由于 Antigravity App 不会为折叠工具调用静默挂载无头 iframe，也未提供无 UI 的 MCP/Hook `/compact` 接口，无人值守压缩需使用下方官方 ACP 方案。
- **`agy` CLI 与 ACP 编辑器（ACP v1 `session/prompt`）**：`acp-proxy` 与 `acp-run` 为纯协议薄客户端（零本地 SQLite / 票据文件耦合，原生兼容 `--remote-agent`、SSH 及 Zed、JetBrains、Android Studio、Xcode、VS Code ACP 等编辑器）。将自定义 ACP Agent 指向 `acp-proxy`（替代 `agy acp`），当 `context_radar_commit_handoff(kind: "compact", auto_fire: true)` 成功返回后，代理会在本轮 `end_turn` 原地注入原生 `/compact`（门禁拦截时服务端返回 `isError: true`，绝不误触发），也支持可选 `--native-auto-compact <val>`（透传 `AGY_CLI_AUTO_COMPACT_OVERRIDE`，如 `300K` 或 `80%`）或使用 `acp-run` 无头托管长任务：

```json
{
  "command": "~/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp",
  "args": ["acp-proxy"]
}
```

```sh
~/.gemini/config/plugins/context-radar-mcp/bin/context-radar-mcp acp-run \
  --cwd /path/to/project --auto-approve --prompt "完成迁移并运行完整验证"
```

## 自定义配置与安全边界

开箱即用，自动识别系统语言（`en` / `zh-CN`）、币种（`USD` / `CNY`）以及 Antigravity 数据目录（`~/.gemini/antigravity{,-ide,-cli}`）。如需自定义单价、预警阈值或启动提示，可将 [settings.example.json](settings.example.json) 复制到 `~/.config/context-radar-mcp/settings.json`（或通过 `CONTEXT_RADAR_DATA_ROOT` / `CONTEXT_RADAR_CONFIG` 覆盖）。

- **纯本地只读访问**：零网络请求、零遥测；严格使用 `SQLITE_OPEN_READ_ONLY` 只读打开会话数据库，并增量 tail WAL/JSONL 日志。
- **不破缓存的 `scratch/` 旁路提炼**：不删改线上历史消息，而是将旧片段提炼到 `scratch/`，保住当前 KV 前缀热缓存并供新会话或子 Agent 按需读取。
- **作用域票据与安全护栏**：单次交接票据绑定 `conversation_id` 与 `workspace` 并以 `0600` 权限存盘；可选 `PreToolUse` 等待守护自动放行后台守护进程与异步构建。

## 从源码构建

需要 Rust（`cargo`）和 Node.js 20+（仅用于打包和测试前端面板）。

```sh
git clone https://github.com/eatingDream-antigravity-plugins/context-radar-mcp.git && cd context-radar-mcp
node panel/pack.mjs && node --test panel/panel.test.mjs
cargo test --locked && cargo build --release --locked
```

欢迎提 PR 或反馈问题，详情见 [CONTRIBUTING.md](CONTRIBUTING.md)、[CHANGELOG.md](CHANGELOG.md) 和 [SECURITY.md](SECURITY.md)。本项目基于 [MIT License](LICENSE) 开源。
