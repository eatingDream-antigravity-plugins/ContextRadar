<div align="center">
<img src="assets/logo.svg" alt="ContextRadar" width="76" height="76">

# ContextRadar (context-radar-mcp)

**看清 Antigravity 上下文到底占了多少、为什么突然变慢，以及什么时候才真正值得 `/compact` 或开新对话。**

[主页](https://contextradar.eatingdream.dev) · [English](README.md) · 简体中文 · [Releases](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/releases) · [AI 指南](docs/ai-guide.md) · [llms.txt](llms.txt)

[![CI](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/releases)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

<img src="docs/social-preview.png" alt="ContextRadar 面板预览" width="840">
</div>

## 它是什么

- **是什么**：面向 Antigravity 的本地原生 Rust MCP 服务、实时侧边栏面板与纯协议 ACP 薄客户端，用于监控上下文水位、诊断缓存断层、安全提炼历史切片，并在真正划算时触发 `/compact` 或新会话交接。
- **不是什么**：不是云端代理、不篡改线上对话记录、不采集任何遥测数据，全部读取与计算均在本地完成。

## 核心特性

- **实时四态遥测与余量**：同步监控主会话与子 Agent 的运行状态（`ready` / `running` / `waiting` / `error`）、剩余上下文空间、KV 缓存命中率、思考占比、流式/端到端速度与估算费用。
- **暖色极简实时面板**：暗色优先零闪屏界面，单一焦点主卡配横向子代理停靠条、热点迷你条形图，并清晰区分 **置顶 vs 近期会话** 卡片分组。
- **三路冷缓存归因**：自动区分新轮次未命中、中途断缓（慢轮过期、副本溢出、切模型）与压缩后重建，并预警跨模型 `/compact` 缓存重置。
- **不破缓存的 `scratch/` 提炼**：将早期冗长片段浓缩为简短摘要旁路落盘，保住当前会话的热 KV 前缀。
- **带门槛的 `/compact` 与新对话交接**：综合预算档位、子代理状态与回本轮数，仅通过官方 SEP-1865 `ui/message` 或 ACP v1（`acp-proxy` / `acp-run`）执行。
- **可选防误操作护栏**：避免短命令过早转入后台，并支持会话启动时自动注入自定义工作区状态说明。

## 快速开始

一键安装经 SHA-256 校验的预编译二进制（支持 macOS Apple Silicon、macOS Intel、Linux x86_64）：

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar-mcp/main/scripts/install.sh | sh
```

重载 Antigravity 后在侧边栏打开 **ContextRadar**，或直接问 Agent：*"刚才几轮怎么突然变慢了？"*、*"现在适合 `/compact` 吗？"*。

## 面向 AI Agent

项目内置完整的机器可读文档与通用 Skill（兼容 `agentskills.io`），支持 Antigravity、Claude Code、Codex、Cursor、OpenCode 等任意 MCP 客户端：

```sh
npx skills add eatingDream-antigravity-plugins/context-radar-mcp
```

> **给 Agent 的提示**：完整的 MCP 工具参数表、ACP `acp-proxy`/`acp-run` 用法、环境变量与源码构建步骤请直接阅读 [`llms.txt`](llms.txt)、[`skills/context-radar/SKILL.md`](skills/context-radar/SKILL.md) 和 [`docs/ai-guide.md`](docs/ai-guide.md)。

## 安全与隐私

- **100% 本地只读**：零外网请求、零遥测；严格以 `SQLITE_OPEN_READ_ONLY` 只读打开会话账本并增量读取 WAL/JSONL。
- **防符号链接原子落盘**：`scratch/` 摘要写入逐级使用 `O_NOFOLLOW` 目录句柄校验，并通过 `0600` 临时文件原子替换。
- **失败即拒绝的票据与无头鉴权**：单次交接票据绑定 `conversation_id` + `workspace` 并以 `0600` 存盘，落盘失败绝不放行；无头 `acp-run` 未开启 `--auto-approve` 时对纯放行请求一律取消。
- **严格面板 CSP**：单文件自包含 HTML（`default-src 'none'`），零外链资源，零 `innerHTML` 写入。

## 文档索引

- [AI Agent 与配置指南](docs/ai-guide.md) — MCP 工具全表、ACP 代理/无头运行、配置项与环境变量、源码构建
- [Agent Skill（`SKILL.md`）](skills/context-radar/SKILL.md) — 面向 Agent 的标准工作流说明
- [LLM 索引（`llms.txt`）](llms.txt) · [协作指南](AGENTS.md) · [配置示例](settings.example.json)
- [更新日志](CHANGELOG.md) · [安全策略](SECURITY.md) · [贡献指南](CONTRIBUTING.md)

## Antigravity 插件套件

隶属于 [eatingDream Antigravity 插件套件](https://eatingdream.dev)：**ContextRadar**（`context-radar-mcp`）· [ClamGuard](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp)（`mac-clamshell-mcp`）· [RemoteCast](https://github.com/eatingDream-antigravity-plugins/chrome-stream-mcp)（`chrome-stream-mcp`）· [ColabDeck](https://github.com/eatingDream-antigravity-plugins/colab-mcp)（`colab-mcp`），由 [PanelKit](https://github.com/eatingDream-antigravity-plugins/agy-mcp-kit)（`agy-mcp-kit`）统一驱动。

## 许可证

本项目基于 [MIT License](LICENSE) 开源。
