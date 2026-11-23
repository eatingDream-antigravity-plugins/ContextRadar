//! MCP tool/resource specifications and concise model-facing text formatters for Context Radar MCP.

use serde_json::{json, Value};

use crate::engine::telemetry::OverviewSnapshot;

pub const PANEL_URI: &str = "ui://context-radar-mcp/panel";
pub const HANDOFF_URI: &str = "ui://context-radar-mcp/handoff";
pub const APP_MIME: &str = "text/html;profile=mcp-app";

pub fn fmt_k(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.2}M", (tokens as f64) / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", (tokens as f64) / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub fn tools_spec() -> Value {
    json!([
        {
            "name": "context_radar_overview",
            "title": "Context Radar 上下文监控",
            "description": "打开 Context Radar 实时会话与活跃子代理上下文监控面板（支持增量摘要去重 if_digest、冷缓存三类归因、一键复制完整 conversation_id 与面板快照查询）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "snapshot",
                            "turns"
                        ],
                        "description": "snapshot（默认）：返回会话列表快照；turns：按 conversation_id 返回逐轮明细"
                    },
                    "if_digest": {
                        "type": "string",
                        "maxLength": 64,
                        "description": "上次快照的 digest；若状态未变化则返回极简 { unchanged: true } 节省传输"
                    },
                    "if_hash": {
                        "type": "string",
                        "maxLength": 64,
                        "description": "if_digest 的兼容别名"
                    },
                    "fresh": {
                        "type": "boolean",
                        "description": "是否强制同步刷新（跳过 SWR 缓存直查磁盘增量），默认 false"
                    },
                    "conversation_id": {
                        "type": "string",
                        "description": "目标会话 ID（action=turns 时使用）"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多展示的会话数或逐轮条数（默认 25，范围 1-200）",
                        "minimum": 1,
                        "maximum": 200
                    },
                    "show_history": {
                        "type": "boolean",
                        "description": "是否包含已结束/空闲/超时静默的历史子代理，默认 false（仅展示活跃运行中子代理）"
                    },
                    "include_archived": {
                        "type": "boolean",
                        "description": "是否展示已归档会话，默认 false"
                    },
                    "workspace_filter": {
                        "type": "string",
                        "description": "按工作区路径关键词筛选（可选）"
                    }
                },
                "additionalProperties": false
            },
            "_meta": {
                "ui": { "resourceUri": PANEL_URI },
                "com.google.antigravity/ui": {
                    "entrypoints": [
                        { "type": "aux_pane", "title": "Context Radar" },
                        { "type": "global", "title": "Context Radar" }
                    ]
                }
            }
        },
        {
            "name": "context_radar_ui_action",
            "title": "Context Radar 面板内部状态操作",
            "description": "仅供 Context Radar MCP App 面板内部调用的偏好持久化与单次接力票据状态操作（对模型不可见）。",
            "inputSchema": {
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "set_prefs",
                            "claim_grant",
                            "record_relay",
                            "dismiss_grant"
                        ],
                        "description": "set_prefs：持久化面板自主接力开关；claim_grant：单次核销接力票据；record_relay：记录官方 target:new 新会话 ID；dismiss_grant：关闭待处理提示"
                    },
                    "allow_auto_handoff": {
                        "type": "boolean",
                        "description": "action=set_prefs 时设置是否允许面板自动通过官方 target:new 拉起接力新会话"
                    },
                    "grant_id": {
                        "type": "string",
                        "description": "单次接力票据 ID"
                    },
                    "conversation_id": {
                        "type": "string",
                        "description": "源会话 ID（claim_grant 实时校验用）"
                    },
                    "claim_mode": {
                        "type": "string",
                        "enum": ["auto_handoff", "manual_handoff", "manual"],
                        "description": "claim_grant 核销模式"
                    },
                    "handoff_conversation_id": {
                        "type": "string",
                        "description": "action=record_relay 时记录官方 target:new 返回的新 conversationId"
                    }
                },
                "additionalProperties": false
            },
            "_meta": {
                "ui": { "visibility": ["app"] }
            }
        },
        {
            "name": "context_radar_turns",
            "title": "逐轮上下文与冷缓存明细",
            "description": "查询指定会话的逐轮上下文、缓存命中率、三类冷缓存归因（跨回合冷/途中破缓/压后重建）、TTFT 首字延迟、吐字速度与单轮费用。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversation_id": {
                        "type": "string",
                        "description": "目标会话 ID（省略时自动取当前活跃最新主会话）"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多返回最近轮数（默认 30，范围 1-200）",
                        "minimum": 1,
                        "maximum": 200
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "context_radar_hotspots",
            "title": "上下文膨胀热点定位与保前缀切片提炼",
            "description": "在不修改主会话前缀（100% 保留 KV 缓存命中）的前提下，基于 256 KiB 逆向 CHECKPOINT 扫描与增量偏移缓存旁路定位上下文暴涨步骤，或将指定历史步区间提炼为 <600 Token 结构化摘要保存到 scratch/。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversation_id": {
                        "type": "string",
                        "description": "目标会话 ID（省略时自动取当前进行中的最新主会话）"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["hotspots", "distill"],
                        "description": "hotspots（默认）：定位上次检查点以来增长最快的步骤；distill：提炼指定步区间为紧凑摘要"
                    },
                    "start_step": {
                        "type": "integer",
                        "description": "mode=distill 起始 step_index（省略时取当前检查点后首步）"
                    },
                    "end_step": {
                        "type": "integer",
                        "description": "mode=distill 结束 step_index（省略时取前半段历史）"
                    },
                    "save_note": {
                        "type": "boolean",
                        "description": "是否同步将提炼摘要写入会话 scratch/ 目录供分叉或接力精读，默认 false"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "mode=hotspots 最多返回的热点条数（默认 8，范围 1-15）",
                        "minimum": 1,
                        "maximum": 15
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "context_radar_prepare_handoff",
            "title": "评估模型档位与压缩接力预算",
            "description": "按模型×推理强度预算档位、冷却轮数、冷缓存重建保护、running_subagent_count == 0 校验与经济回本轮数，评估应继续复用热前缀、局部提炼/分叉折叠、子代理切棒，还是进入全量压缩/官方 target:new 接力。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversation_id": {
                        "type": "string",
                        "description": "目标会话 ID（省略时自动取当前进行中的最新主会话）"
                    },
                    "estimated_remaining_turns": {
                        "type": "integer",
                        "description": "预计剩余交互轮数（默认 12）",
                        "minimum": 1,
                        "maximum": 500
                    },
                    "upcoming_noisy_tokens": {
                        "type": "integer",
                        "description": "下一步预计新增高噪声临时 Token 数（默认 0）",
                        "minimum": 0,
                        "maximum": 2000000
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "context_radar_commit_handoff",
            "title": "签发压缩指令与官方 target:new 接力票据",
            "description": "先执行服务端硬拦截（模型档位硬下限、冷却、冷重建、经济回本、running_subagent_count == 0），通过后签发单次防重放接力票据、一键复制的 /compact 指令与官方 target:new 交接提示；面板自主接力开启且 auto_fire=true 时自动拉起接力新会话（实时问答传 auto_fire=false 保留当前窗口）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["handoff", "compact"],
                        "description": "票据类型（默认 handoff）"
                    },
                    "conversation_id": {
                        "type": "string",
                        "description": "源会话 ID（省略时自动取当前进行中的最新主会话）"
                    },
                    "preserve_points": {
                        "type": "string",
                        "description": "交接/压缩必须保留的关键结论、已改文件路径与约束要点"
                    },
                    "resume_task": {
                        "type": "string",
                        "description": "接力/压缩完成后应立即继续执行的下一步任务说明（留空则暂停待命）"
                    },
                    "scratch_note_path": {
                        "type": "string",
                        "description": "可选：通过 context_radar_hotspots(save_note=true) 保存的 scratch/ 摘要路径"
                    },
                    "estimated_remaining_turns": {
                        "type": "integer",
                        "description": "预计剩余交互轮数（默认 12，用于服务端经济回本硬校验）",
                        "minimum": 1,
                        "maximum": 500
                    },
                    "auto_fire": {
                        "type": "boolean",
                        "description": "是否在面板自主接力开启且零运行子代理时自动拉起 target:new 新会话（默认 true）"
                    }
                },
                "additionalProperties": false
            },
            "_meta": {
                "ui": { "resourceUri": HANDOFF_URI }
            }
        }
    ])
}

pub fn resources_spec() -> Value {
    json!([
        {
            "uri": PANEL_URI,
            "name": "Context Radar 上下文监控面板",
            "description": "Antigravity 会话与子代理上下文长度、冷缓存三类归因、一键复制会话 ID 与官方 target:new 接力监控面板",
            "mimeType": APP_MIME,
            "_meta": { "ui": { "prefersBorder": false } }
        },
        {
            "uri": HANDOFF_URI,
            "name": "Context Radar 压缩与官方接力卡片",
            "description": "提供一键复制完整 /compact 指令，并在零运行子代理且授权时通过官方 target:new 拉起接力新会话的内联卡片",
            "mimeType": APP_MIME,
            "_meta": { "ui": { "prefersBorder": false } }
        }
    ])
}

pub fn summarize_snapshot_text(snap: &OverviewSnapshot) -> String {
    let s = &snap.summary;
    let sym = if snap.currency_symbol.is_empty() {
        "¥"
    } else {
        snap.currency_symbol.as_str()
    };
    let mut lines = vec![format!(
        "Context Radar 快照（上限 {}）：未归档主会话 {} 个（进行中 {}，已隐藏归档 {} 个），活跃子代理 {} 个（已隐藏历史子代理 {} 个），达阈值 {} 个，合计约 {}{:.2}。",
        fmt_k(snap.thresholds.max),
        s.shown_main_count,
        s.running_main_count,
        s.archived_hidden_count,
        s.running_subagent_count,
        s.history_subagents_hidden_count,
        s.compact_needed_count,
        sym,
        s.total_cost_cny
    )];

    for item in snap.sessions.iter().take(8) {
        let flag = if item.just_compacted {
            format!(" [已压缩·原{}]", fmt_k(item.pre_compact_tokens))
        } else if item.suggest_compact {
            " [建议 /compact]".to_string()
        } else {
            String::new()
        };
        let run_tag = if item.is_active { " [进行中]" } else { "" };
        let cb = &item.cold_cache_breakdown;
        let cold_txt = if item.cold_cache_count > 0 {
            format!(
                " (冷{}:跨{}/途{}/压{})",
                item.cold_cache_count,
                cb.turn_start_cold,
                cb.mid_loop_cold,
                cb.post_compact_cold
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "- {} ({}){}: {}/{} | {} | 缓存 {:.1}%{} | {} 轮 | {}{:.2}{}",
            item.title,
            item.id,
            run_tag,
            fmt_k(item.latest_context_tokens),
            fmt_k(item.context_limit),
            item.model_display,
            item.cache_hit_rate,
            cold_txt,
            item.turns,
            sym,
            item.cost_cny,
            flag
        ));
        for sub in item.subagents.iter().take(4) {
            let sub_run = if sub.is_active { " [运行中]" } else { " [历史]" };
            let sub_flag = if sub.suggest_compact && !sub.just_compacted {
                " [超阈值·建议切棒]"
            } else {
                ""
            };
            lines.push(format!(
                "  · 子代理 {} ({}){}: {}/{} | {} | 缓存 {:.1}% | {}{:.2}{}",
                sub.title,
                sub.id,
                sub_run,
                fmt_k(sub.latest_context_tokens),
                fmt_k(sub.context_limit),
                sub.model_display,
                sub.cache_hit_rate,
                sym,
                sub.cost_cny,
                sub_flag
            ));
        }
    }

    lines.join("\n")
}
