//! Token-physics break-even economics, hard handoff gates, and partial/full handoff recipes.

use serde::Serialize;

use crate::engine::rates::RateTable;
use crate::engine::telemetry::SessionTelemetry;
pub use crate::engine::tiers::{
    builtin_tiers, builtin_tiers_for_lang, effective_critical_threshold, is_gemini_4_carbon,
    is_gemini_4_carbon_or_argon, is_gemini_4_model, model_cache_ttl_sec, resolve_budget_tier,
    BudgetTierSpec, DEFAULT_CACHE_TTL_SEC, GEMINI_4_CACHE_TTL_SEC, GEMINI_4_NEON_CACHE_TTL_SEC,
};
use crate::i18n::select_display_cost;

#[derive(Debug, Clone, Serialize)]
pub struct EconomicsReport {
    pub pricing_known: bool,
    pub post_compact_target_tokens: u64,
    pub compact_overhead_usd: f64,
    pub compact_overhead_cny: f64,
    pub per_turn_saving_usd: f64,
    pub per_turn_saving_cny: f64,
    pub break_even_turns: usize,
}

#[inline]
fn round_dp(v: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    (v * factor).round() / factor
}

/// Token-physics break-even estimate when dollar pricing is unavailable (`!pricing_known` or `cache_read_per_m == 0`).
/// Models KV cache physics with 1x warm cache read, 10x uncached prefill, and 50x summary generation weight.
fn token_physics_break_even_turns(ctx: u64, target_ctx: u64, saved_per_turn: u64) -> usize {
    if saved_per_turn < 40_000 {
        return 16;
    }
    let summary_eq = (ctx as f64) * 1.54 + 325_000.0;
    let cold_rebuild_eq = (target_ctx as f64) * 9.0;
    let turns = ((summary_eq + cold_rebuild_eq) / (saved_per_turn as f64)).ceil() as usize;
    turns.clamp(2, 12)
}

pub fn compute_economics(
    ctx: u64,
    model: &str,
    model_disp: &str,
    rates: &mut RateTable,
) -> EconomicsReport {
    let (fx_rate, _) = rates.get_fx_rate();
    let price = rates.resolve_rate(model, model_disp);
    let target_ctx = if ctx > 40_000 {
        ctx.min(((ctx as f64 * 0.08) as u64).max(26_000))
    } else {
        ctx
    };
    let saved_per_turn = ctx.saturating_sub(target_ctx);
    let pricing_known = price.pricing_known && price.cache_read_per_m > 0.0;

    let (compact_overhead_usd, per_turn_saving_usd, break_even_turns) = if pricing_known {
        let summary_usd = ((ctx as f64) * 0.94 / 1e6) * price.cache_read_per_m
            + ((ctx as f64) * 0.06 / 1e6) * price.input_per_m
            + (6_500.0 / 1e6) * price.output_per_m;
        let post_ckpt_cold_usd =
            ((target_ctx as f64) / 1e6) * (price.input_per_m - price.cache_read_per_m);
        let overhead_usd = summary_usd + post_ckpt_cold_usd.max(0.0);
        let saving_usd = ((saved_per_turn as f64) / 1e6) * price.cache_read_per_m;
        let turns = if saving_usd > 1e-6 && saved_per_turn >= 40_000 {
            ((overhead_usd / saving_usd).ceil() as usize).max(1)
        } else {
            token_physics_break_even_turns(ctx, target_ctx, saved_per_turn)
        };
        (overhead_usd, saving_usd, turns)
    } else {
        let turns = token_physics_break_even_turns(ctx, target_ctx, saved_per_turn);
        (0.0, 0.0, turns)
    };

    EconomicsReport {
        pricing_known,
        post_compact_target_tokens: target_ctx,
        compact_overhead_usd: round_dp(compact_overhead_usd, 4),
        compact_overhead_cny: round_dp(compact_overhead_usd * fx_rate, 3),
        per_turn_saving_usd: round_dp(per_turn_saving_usd, 4),
        per_turn_saving_cny: round_dp(per_turn_saving_usd * fx_rate, 3),
        break_even_turns,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GateDecision {
    pub allowed: bool,
    pub urgent_bypass: bool,
    pub pricing_known: bool,
    pub reasons: Vec<String>,
    pub profile: BudgetTierSpec,
    pub post_compact_target_tokens: u64,
    pub compact_overhead_usd: f64,
    pub compact_overhead_cny: f64,
    pub per_turn_saving_usd: f64,
    pub per_turn_saving_cny: f64,
    pub break_even_turns: usize,
}

pub fn evaluate_handoff_gate(
    session: Option<&SessionTelemetry>,
    estimated_remaining_turns: usize,
    rates: &mut RateTable,
) -> GateDecision {
    let cfg = rates.current_settings();
    let lang = rates.get_lang();
    let zh = lang.is_zh();
    let (ccode, sym) = rates.get_currency();
    let limit_k = (cfg.context.default_limit / 1000).max(1);

    let Some(s) = session else {
        let fallback_prof = resolve_budget_tier("", "", &cfg);
        return GateDecision {
            allowed: false,
            urgent_bypass: false,
            pricing_known: false,
            reasons: vec![if zh {
                "未找到目标活跃会话，已拒绝签发接力票据。".into()
            } else {
                "Target active session not found; handoff grant denied.".into()
            }],
            profile: fallback_prof,
            post_compact_target_tokens: 0,
            compact_overhead_usd: 0.0,
            compact_overhead_cny: 0.0,
            per_turn_saving_usd: 0.0,
            per_turn_saving_cny: 0.0,
            break_even_turns: 0,
        };
    };

    let ctx = s.latest_context_tokens;
    let model = if s.model.is_empty() {
        "unknown"
    } else {
        s.model.as_str()
    };
    let model_disp = if s.model_display.is_empty() {
        model
    } else {
        s.model_display.as_str()
    };
    let mut prof = resolve_budget_tier(model, model_disp, &cfg);
    prof.urgent_ctx = effective_critical_threshold(&prof, &cfg);
    let econ = compute_economics(ctx, model, model_disp, rates);
    let rem_turns = estimated_remaining_turns.max(1);
    let urgent = ctx >= prof.urgent_ctx;

    let mut reasons = Vec::new();
    if s.is_subagent {
        let sub_name = if !s.agent_name.is_empty() {
            s.agent_name.as_str()
        } else if !s.title.is_empty() {
            s.title.as_str()
        } else {
            s.id.as_str()
        };
        reasons.push(if zh {
            format!(
                "目标 {sub_name} 是子代理，无主输入框执行 /compact；请在当前步骤收尾后保存结论并切换新子代理接续（切棒）。"
            )
        } else {
            format!(
                "Target '{sub_name}' is a subagent without a /compact input box; finish the current atomic step and pass the baton to a fresh subagent."
            )
        });
    }
    if s.just_compacted {
        reasons.push(if zh {
            format!(
                "该会话最后一步已是压缩检查点（等效上下文约 {}k），禁止重复压缩。",
                ctx / 1000
            )
        } else {
            format!(
                "Latest step is already a compaction checkpoint (~{}k context); duplicate compaction blocked.",
                ctx / 1000
            )
        });
    }
    if ctx < prof.min_ctx {
        reasons.push(if zh {
            format!(
                "当前上下文 {}k 低于【{}】硬下限 {}k（满载 {}k），全量压缩会白白作废热前缀；若需整理早期长输出，请改用 context_radar_hotspots 旁路提炼到 scratch/。",
                ctx / 1000,
                prof.label,
                prof.min_ctx / 1000,
                limit_k
            )
        } else {
            format!(
                "Current context {}k is below {} floor {}k (limit {}k); full compaction would discard the warm KV prefix. Use context_radar_hotspots to distill slices to scratch/ instead.",
                ctx / 1000,
                prof.label,
                prof.min_ctx / 1000,
                limit_k
            )
        });
    }
    if s.checkpoint_count > 0 && s.turns_since_compact < prof.min_turns_since_ckpt && !urgent {
        reasons.push(if zh {
            format!(
                "距上次压缩仅间隔 {} 轮 < 冷却轮数 {} 轮（且未达紧急线 {}k）。",
                s.turns_since_compact,
                prof.min_turns_since_ckpt,
                prof.urgent_ctx / 1000
            )
        } else {
            format!(
                "Only {} turns elapsed since last compaction < {} cooldown turns (and below urgent threshold {}k).",
                s.turns_since_compact,
                prof.min_turns_since_ckpt,
                prof.urgent_ctx / 1000
            )
        });
    }
    if s.recent_cold_rebuild && !urgent {
        reasons.push(if zh {
            "最近 3 轮刚发生过冷缓存重建，尚未收回冷启动成本，暂缓全量压缩。".into()
        } else {
            "A full cold-cache rebuild occurred within the last 3 turns; deferring compaction until prefill cost amortizes.".into()
        });
    }
    if s.running_subagent_count > 0 {
        reasons.push(if zh {
            format!(
                "当前有 {} 个活跃子代理正在运行（不满足 running_subagent_count == 0 硬校验），禁止原地压缩或自动接力新会话；请等待子代理收尾后再执行。",
                s.running_subagent_count
            )
        } else {
            format!(
                "{} subagent(s) currently active (violates running_subagent_count == 0); wait for subagents to finish before compacting or handing off.",
                s.running_subagent_count
            )
        });
    }
    let req_rem = econ.break_even_turns.max(prof.min_remaining_turns);
    if rem_turns < req_rem && !urgent {
        let disp_ovh =
            select_display_cost(econ.compact_overhead_usd, econ.compact_overhead_cny, &ccode);
        reasons.push(if zh {
            if econ.pricing_known {
                format!(
                    "预计剩余 {} 轮 < 回本所需 {} 轮（冷缓存+摘要约 {}{:.2}），直接复用热前缀更划算。",
                    rem_turns, req_rem, sym, disp_ovh
                )
            } else {
                format!(
                    "预计剩余 {} 轮 < Token 物理回本所需 {} 轮，直接复用热前缀更省更快。",
                    rem_turns, req_rem
                )
            }
        } else if econ.pricing_known {
            format!(
                "Estimated remaining {} turns < {} turns required to break even (compaction + cold rebuild ~{}{:.2}); keeping warm prefix is cheaper.",
                rem_turns, req_rem, sym, disp_ovh
            )
        } else {
            format!(
                "Estimated remaining {} turns < {} token-physics break-even turns; keeping warm prefix is faster.",
                rem_turns, req_rem
            )
        });
    }

    GateDecision {
        allowed: reasons.is_empty(),
        urgent_bypass: urgent && !s.just_compacted,
        pricing_known: econ.pricing_known,
        reasons,
        profile: prof,
        post_compact_target_tokens: econ.post_compact_target_tokens,
        compact_overhead_usd: econ.compact_overhead_usd,
        compact_overhead_cny: econ.compact_overhead_cny,
        per_turn_saving_usd: econ.per_turn_saving_usd,
        per_turn_saving_cny: econ.per_turn_saving_cny,
        break_even_turns: econ.break_even_turns,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HandoffRecipes {
    pub past_slice_distill: String,
    pub future_branch_fold: String,
    pub official_handoff_new: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HandoffPlan {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub lang: String,
    pub conversation_id: String,
    pub title: String,
    pub model_display: String,
    pub pricing_known: bool,
    pub current_context_tokens: u64,
    pub context_limit: u64,
    pub currency_code: String,
    pub currency_symbol: String,
    pub cache_ttl_sec: u64,
    pub cache_hit_rate: f64,
    pub estimated_remaining_turns: usize,
    pub upcoming_noisy_tokens: u64,
    pub gate_allowed: bool,
    pub gate_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<BudgetTierSpec>,
    pub post_compact_target_tokens: u64,
    pub compact_overhead_usd: f64,
    pub compact_overhead_cny: f64,
    pub per_turn_saving_usd: f64,
    pub per_turn_saving_cny: f64,
    pub break_even_turns: usize,
    pub action: String,
    pub headline: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_compaction_recipes: Option<HandoffRecipes>,
}

pub fn prepare_handoff_plan(
    session: Option<&SessionTelemetry>,
    estimated_remaining_turns: usize,
    upcoming_noisy_tokens: u64,
    rates: &mut RateTable,
) -> HandoffPlan {
    let cfg = rates.current_settings();
    let lang = rates.get_lang();
    let zh = lang.is_zh();
    let (ccode, sym) = rates.get_currency();
    let rem_turns = estimated_remaining_turns.max(1);

    let Some(s) = session else {
        let msg = if zh {
            "未找到匹配的活跃会话，建议保持当前前缀继续执行。"
        } else {
            "No matching active session found; keep current prefix."
        };
        return HandoffPlan {
            found: false,
            reason: Some(msg.into()),
            lang: lang.as_str().into(),
            conversation_id: String::new(),
            title: String::new(),
            model_display: String::new(),
            pricing_known: false,
            current_context_tokens: 0,
            context_limit: cfg.context.default_limit,
            currency_code: ccode,
            currency_symbol: sym,
            cache_ttl_sec: DEFAULT_CACHE_TTL_SEC as u64,
            cache_hit_rate: 0.0,
            estimated_remaining_turns: rem_turns,
            upcoming_noisy_tokens,
            gate_allowed: false,
            gate_reasons: Vec::new(),
            profile: None,
            post_compact_target_tokens: 0,
            compact_overhead_usd: 0.0,
            compact_overhead_cny: 0.0,
            per_turn_saving_usd: 0.0,
            per_turn_saving_cny: 0.0,
            break_even_turns: 0,
            action: "keep_prefix".into(),
            headline: msg.into(),
            partial_compaction_recipes: None,
        };
    };

    let ctx = s.latest_context_tokens;
    let ctx_limit = if s.context_limit > 0 {
        s.context_limit
    } else {
        cfg.context.default_limit
    };
    let limit_k = (ctx_limit / 1000).max(1);
    let cache_hit = s.cache_hit_rate;
    let gate = evaluate_handoff_gate(Some(s), rem_turns, rates);
    let prof = &gate.profile;

    let (action, headline) = if s.is_subagent {
        if ctx >= prof.recommend_ctx {
            (
                "subagent_handoff",
                if zh {
                    format!(
                        "子代理超推荐线（{}k >= {}k）：子代理无 /compact 输入框，请在当前步骤收尾后保存结论并切换新子代理接续。",
                        ctx / 1000,
                        prof.recommend_ctx / 1000
                    )
                } else {
                    format!(
                        "Subagent above recommended threshold ({}k >= {}k): finish current step, report summary, and spawn a fresh subagent.",
                        ctx / 1000,
                        prof.recommend_ctx / 1000
                    )
                },
            )
        } else {
            (
                "keep_prefix",
                if zh {
                    format!(
                        "子代理保持热前缀继续执行（当前 {}k / {}k，缓存命中 {:.1}%）。",
                        ctx / 1000,
                        limit_k,
                        cache_hit
                    )
                } else {
                    format!(
                        "Keep subagent warm prefix ({}k / {}k, {:.1}% cache hit).",
                        ctx / 1000,
                        limit_k,
                        cache_hit
                    )
                },
            )
        }
    } else if gate.allowed && ctx >= prof.recommend_ctx {
        (
            "active_full_compact",
            if zh {
                format!(
                    "建议全量压缩或接力新会话：当前 {}k >= 推荐线 {}k（上限 {}k），已满足冷却、零运行子代理与回本校验（>= {} 轮）；可调 radar_commit_handoff 生成 /compact 指令或 target:new 接力票据。",
                    ctx / 1000,
                    prof.recommend_ctx / 1000,
                    limit_k,
                    gate.break_even_turns
                )
            } else {
                format!(
                    "Full /compact or new-chat handoff recommended: {}k >= {}k target (limit {}k), satisfying cooldown, 0 running subagents, and >={} turn break-even. Call radar_commit_handoff.",
                    ctx / 1000,
                    prof.recommend_ctx / 1000,
                    limit_k,
                    gate.break_even_turns
                )
            },
        )
    } else if gate.allowed && ctx >= prof.min_ctx {
        (
            "partial_branch_fold",
            if zh {
                format!(
                    "处于可选压缩区（{}k，下限 {}k / 推荐 {}k）：优先用 scratch/ 切片摘要或分支子代理保住热前缀，跨阶段边界时可调 radar_commit_handoff。",
                    ctx / 1000,
                    prof.min_ctx / 1000,
                    prof.recommend_ctx / 1000
                )
            } else {
                format!(
                    "Optional compaction window ({}k; floor {}k, target {}k): prefer scratch/ slice notes or branch workers to preserve warm cache, or commit handoff at a clean phase boundary.",
                    ctx / 1000,
                    prof.min_ctx / 1000,
                    prof.recommend_ctx / 1000
                )
            },
        )
    } else if upcoming_noisy_tokens >= 35_000 || (ctx >= 120_000 && ctx < prof.min_ctx) {
        (
            "partial_branch_fold",
            if zh {
                format!(
                    "未达全量压缩下限（{}k）：建议将早期冗长片段调 context_radar_hotspots 旁路写入 scratch/ 供后续接力读取，高噪声探索交给分支子代理消化。",
                    prof.min_ctx / 1000
                )
            } else {
                format!(
                    "Below full-compaction floor ({}k): keep live prefix intact—distill older bloated slices to scratch/ for future handoffs and isolate noisy exploration in a subagent.",
                    prof.min_ctx / 1000
                )
            },
        )
    } else {
        let reason_tail = gate
            .reasons
            .first()
            .map(|r| {
                if zh {
                    format!("（原因：{r}）")
                } else {
                    format!(" ({r})")
                }
            })
            .unwrap_or_default();
        (
            "keep_prefix",
            if zh {
                format!(
                    "保持现有热前缀（{}k / {}k，缓存命中 {:.1}%）{reason_tail}",
                    ctx / 1000,
                    limit_k,
                    cache_hit
                )
            } else {
                format!(
                    "Keep warm prefix ({}k / {}k, {:.1}% cache hit){reason_tail}",
                    ctx / 1000,
                    limit_k,
                    cache_hit
                )
            },
        )
    };

    let cache_ttl_sec = model_cache_ttl_sec(&s.model, &s.model_display) as u64;
    let recipes = if zh {
        HandoffRecipes {
            past_slice_distill: "调用 context_radar_hotspots(mode='distill', save_note=true)：在不改动当前会话前缀的前提下，将早期高膨胀步骤浓缩写入 scratch/，供全新子代理或后续 target:new 接力会话按需读取。".into(),
            future_branch_fold: if is_gemini_4_model(&s.model, &s.model_display) && ctx > 40_000 {
                format!(
                    "当前上下文 >40k：Gemini 4 上 fork 会触发全量冷 prefill 且缓存 TTL 为 {cache_ttl_sec}s（自 prefill 起算）；请用精简摘要派发全新子代理，勿使用 fork。"
                )
            } else {
                "需要继承当前上下文的高噪声探索可调用 swarm(action='add', fork='coordinator')：探索噪声留在分支，回传简短结论后结束子代理。".into()
            },
            official_handoff_new: "跨阶段且 running_subagent_count == 0 时调用 radar_commit_handoff：生成完整 /compact 指令，并在已打开的面板/卡片中通过官方 target:new 拉起同工作区新会话。".into(),
        }
    } else {
        HandoffRecipes {
            past_slice_distill: "Call context_radar_hotspots(mode='distill', save_note=true) to write an out-of-band <600-token summary to scratch/ for fresh subagents or a future target:new chat without touching the live KV prefix.".into(),
            future_branch_fold: if is_gemini_4_model(&s.model, &s.model_display) && ctx > 40_000 {
                format!(
                    "Context >40k: on Gemini 4, fork triggers a full cold Turn-1 prefill and cache TTL is {cache_ttl_sec}s from prefill start; spawn a fresh subagent with a concise brief instead of fork."
                )
            } else {
                "Delegate noisy exploration via swarm(action='add', fork='coordinator') so noisy tool output stays off the main thread.".into()
            },
            official_handoff_new: "At a phase boundary with running_subagent_count == 0, call radar_commit_handoff to prepare a copyable /compact command or launch a scoped target:new session from an open UI view.".into(),
        }
    };

    HandoffPlan {
        found: true,
        reason: None,
        lang: lang.as_str().into(),
        conversation_id: s.id.clone(),
        title: s.title.clone(),
        model_display: s.model_display.clone(),
        pricing_known: gate.pricing_known,
        current_context_tokens: ctx,
        context_limit: ctx_limit,
        currency_code: ccode,
        currency_symbol: sym,
        cache_ttl_sec,
        cache_hit_rate: round_dp(cache_hit, 1),
        estimated_remaining_turns: rem_turns,
        upcoming_noisy_tokens,
        gate_allowed: gate.allowed,
        gate_reasons: gate.reasons,
        profile: Some(gate.profile),
        post_compact_target_tokens: gate.post_compact_target_tokens,
        compact_overhead_usd: gate.compact_overhead_usd,
        compact_overhead_cny: gate.compact_overhead_cny,
        per_turn_saving_usd: gate.per_turn_saving_usd,
        per_turn_saving_cny: gate.per_turn_saving_cny,
        break_even_turns: gate.break_even_turns,
        action: action.into(),
        headline,
        partial_compaction_recipes: Some(recipes),
    }
}
