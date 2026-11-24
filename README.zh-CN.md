<div align="center">

<img src="assets/logo.svg" alt="Context Radar MCP" width="80" height="80">

# Context Radar MCP

**看清 Antigravity 的上下文到底占了多少、为什么突然变慢，以及什么时候才值得 `/compact` 或开新对话。**

[![CI](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/eatingDream-antigravity-plugins/context-radar-mcp?color=D97757&label=release)](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-555)

[English](README.md) | 简体中文

<img src="docs/social-preview.png" alt="Context Radar MCP 面板预览" width="860">

</div>

## 为什么做这个插件

用 Antigravity 写代码聊久了，经常会遇到回复突然变慢、消耗涨得莫名其妙，但界面上很难看清到底卡在哪里。有时候是一次读入太多内容把窗口撑大了，有时候是中途不小心打断了缓存，还有时候过早点了 `/compact`，反而白白丢掉了热缓存。

Context Radar 在 Antigravity 里提供了一个实时面板，把上下文占用、缓存命中情况和每一轮的变化摊开给你看，也能帮 Agent 判断什么时候该整理历史、什么时候该换新对话。

## 它能帮你做什么

- **实时看清上下文和消耗**：在侧边栏直观查看当前会话和子 Agent 的上下文占比、缓存命中率、输出速度和估算费用。
- **定位变慢的原因**：自动区分"新的一轮正常未命中""对话中途意外断缓存"和"压缩后的首次重建"，一眼知道慢在哪。
- **找出撑大上下文的罪魁祸首**：快速定位哪几轮历史最占空间，让 Agent 把早期冗长记录浓缩成简短摘要存到 `scratch/`，不用带着一堆旧日志继续跑。
- **判断 `/compact` 或换新对话划不划算**：结合当前模型预算和回本轮数给出建议，确认合适后再弹出 `/compact` 或一键交接新对话的卡片，避免盲目压缩。
- **顺手的小保护**：防止 Agent 把几秒钟就能跑完的命令过早丢到后台；如果你在配置里指定了工作区状态文件，也能在开新会话时自动带入。

## 安装

### macOS 与 Linux（推荐）

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar-mcp/main/scripts/install.sh | sh
```

脚本会自动下载对应平台的预编译程序（支持 macOS Apple Silicon、macOS Intel 和 Linux x86_64），校验 SHA-256 后安装到 `~/.gemini/config/plugins/context-radar-mcp`。装完后重启或重载 Antigravity 即可启用。

如需指定版本或自定义插件目录：

```sh
CONTEXT_RADAR_VERSION=v1.0.0 ANTIGRAVITY_PLUGINS_DIR=~/.gemini/config/plugins \
  curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar-mcp/main/scripts/install.sh | sh
```

## 怎么用

### 在 Antigravity 里直接用

在侧边栏打开 **Context Radar** 面板，就能实时看到上下文水位、最近几轮的变化趋势和缓存状态。

平时也可以直接跟 Agent 说话，插件自带按需加载的 Skill，Agent 会自己调用对应工具来排查：

- *"打开 Context Radar，看看现在上下文还剩多少。"*
- *"刚才几轮怎么这么慢？是不是缓存断了？"*
- *"查一下哪几轮最占上下文，帮我整理一份摘要。"*
- *"现在适合 `/compact` 还是直接交接到新对话？"*

### 提供给 Agent 的工具

| 工具 | 作用 |
| :--- | :--- |
| `context_radar_overview` | 打开实时面板，返回当前会话和子 Agent 的概览数据。 |
| `context_radar_turns` | 按轮次查看 Token 增长、缓存命中、冷启动原因、速度和估算费用。 |
| `context_radar_hotspots` | 找出历史里最占空间的片段，并按需整理成摘要写入 `scratch/`。 |
| `context_radar_prepare_handoff` | 评估现在执行 `/compact` 或开新对话是否划算。 |
| `context_radar_commit_handoff` | 条件满足后生成 `/compact` 或新对话交接卡片。 |

## 自定义配置（可选）

装好就能直接用，不需要任何配置文件。插件会根据系统语言自动切换中英文界面与对应币种（`USD` `$` / `CNY` `¥`），并自动发现 `~/.gemini/antigravity`、`~/.gemini/antigravity-ide` 和 `~/.gemini/antigravity-cli` 数据目录。

如果你想修改模型单价、显示币种、预警阈值或工作区启动提示，可以把 [settings.example.json](settings.example.json) 复制到 `~/.config/context-radar-mcp/settings.json` 按需修改；也可通过 `CONTEXT_RADAR_DATA_ROOT` 指定数据目录，或用 `CONTEXT_RADAR_CONFIG` 指定配置文件路径。

## 实现原理与安全边界

插件完全在本地运行，不发任何网络请求，也没有遥测统计：

- **全程只读数据库（Read-only SQLite）**：读取 Antigravity 会话数据库时严格使用 `SQLITE_OPEN_READ_ONLY` 只读模式并增量 tail 对话日志，绝不修改或锁住原始聊天记录。
- **为什么用 `scratch/` 旁路提炼来保护 KV 前缀**：在对话中途直接删改历史消息会打断模型提示词的 KV 缓存，导致下一轮高延迟冷重建。`context_radar_hotspots` 改为把早期冗长片段旁路浓缩写入 `scratch/` 笔记，保持当前会话 KV 前缀完整复用，同时让新子代理或 `target:new` 接力会话按需精读。
- **工作区与会话双绑定的单次交接票据**：所有接力票据均绑定 `conversation_id` 与 `workspace` 并以 `0600` 权限保存在本地插件数据目录，防止多窗口或跨项目侧边栏误触发其他项目的交接。
- **安全的 `PreToolUse` 等待守护**：内置 Hook 仅在短命令过早转入后台时温和提醒，对明确标注后台守护或异步构建的命令自动放行，不修改系统配置也不干涉进程状态。

## 从源码构建

需要 Rust（`cargo`）和 Node.js 20+（仅用于打包和测试前端面板）。

```sh
git clone https://github.com/eatingDream-antigravity-plugins/context-radar-mcp.git
cd context-radar-mcp
node panel/pack.mjs && node --test panel/panel.test.mjs
cargo test --locked
cargo build --release --locked
```

## 参与贡献与许可证

欢迎提 PR 或反馈问题，详情见 [CONTRIBUTING.md](CONTRIBUTING.md)、[CHANGELOG.md](CHANGELOG.md) 和 [SECURITY.md](SECURITY.md)。

本项目基于 [MIT License](LICENSE) 开源。
