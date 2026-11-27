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
                "未找到目标会话，暂不能生成接续。".into()
            } else {
                "Target session not found; can't create handoff.".into()
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
        reasons.push(if s.is_side_question {
            if zh {
                format!("「{sub_name}」是顺便问会话，请回到主会话处理。")
            } else {
                format!("'{sub_name}' is an aside. Handle it from its main chat.")
            }
        } else if zh {
            format!("「{sub_name}」是子代理会话，请回到主会话处理。")
        } else {
            format!("'{sub_name}' is a subagent. Handle it from its main chat.")
        });
    }
    if s.just_compacted {
        reasons.push(if zh {
            format!("这个会话刚压缩过（约 {}k），不用重复压缩。", ctx / 1000)
        } else {
            format!(
                "This chat was just compacted (~{}k); no need to compact again.",
                ctx / 1000
            )
        });
    }
    if ctx < prof.min_ctx {
        reasons.push(if zh {
            format!(
                "当前上下文 {}k，还没到「{}」下限 {}k（上限 {}k）；现在压缩会丢掉已有缓存，换新会话或先把要点存进笔记文件更划算。",
                ctx / 1000,
                prof.label,
                prof.min_ctx / 1000,
                limit_k
            )
        } else {
            format!(
                "Current context {}k is below the {} floor {}k (limit {}k). Compacting now throws away the cache; save key points to a notes file instead.",
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
                "距上次压缩只过了 {} 轮（建议至少 {} 轮，或达到 {}k 再压缩）。",
                s.turns_since_compact,
                prof.min_turns_since_ckpt,
                prof.urgent_ctx / 1000
            )
        } else {
            format!(
                "Only {} turns since the last compact (wait {} turns, or until {}k).",
                s.turns_since_compact,
                prof.min_turns_since_ckpt,
                prof.urgent_ctx / 1000
            )
        });
    }
    if s.recent_cold_rebuild && !urgent {
        reasons.push(if zh {
            "最近 3 轮刚重建过缓存，先继续用，暂不压缩。".into()
        } else {
            "Cache was rebuilt within the last 3 turns; keep going before compacting.".into()
        });
    }
    if s.running_subagent_count > 0 {
        reasons.push(if zh {
            format!(
                "还有 {} 个子代理在运行，请等所有子代理结束后再操作。",
                s.running_subagent_count
            )
        } else {
            format!(
                "{} subagent(s) still running. Wait until all subagents finish.",
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
                    "预计还剩 {} 轮，大约 {} 轮后回本（压缩成本约 {}{:.2}），先不压缩更划算。",
                    rem_turns, req_rem, sym, disp_ovh
                )
            } else {
                format!(
                    "预计还剩 {} 轮，大约 {} 轮后回本，先不压缩更省更快。",
                    rem_turns, req_rem
                )
            }
        } else if econ.pricing_known {
            format!(
                "About {} turns left, but compacting pays off after about {} turns (~{}{:.2}). Keep going instead.",
                rem_turns, req_rem, sym, disp_ovh
            )
        } else {
            format!(
                "About {} turns left, and compacting pays off after about {} turns. Keep going instead.",
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
            "未找到匹配的会话，先继续当前对话。".into()
        } else {
            "No matching session found; keep going.".into()
        };
        return HandoffPlan {
            found: false,
            reason: Some(msg),
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
            headline: if zh {
                "未找到匹配的会话，先继续当前对话。".into()
            } else {
                "No matching session found; keep going.".into()
            },
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

    let (action, headline) = if s.is_side_question {
        (
            "keep_prefix",
            if zh {
                format!(
                    "这是顺便问会话（{}k / {}k），请回到主会话处理。",
                    ctx / 1000,
                    limit_k
                )
            } else {
                format!(
                    "This is an aside ({}k / {}k). Handle it from its main chat.",
                    ctx / 1000,
                    limit_k
                )
            },
        )
    } else if s.is_subagent {
        if ctx >= prof.recommend_ctx {
            (
                "subagent_handoff",
                if zh {
                    format!(
                        "子代理上下文已达 {}k（建议线 {}k）：收尾后带上接续说明开新子代理。",
                        ctx / 1000,
                        prof.recommend_ctx / 1000
                    )
                } else {
                    format!(
                        "Subagent is at {}k (suggested {}k): wrap up and continue in a fresh subagent.",
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
                        "子代理可继续运行（{}k / {}k，缓存命中 {:.1}%）。",
                        ctx / 1000,
                        limit_k,
                        cache_hit
                    )
                } else {
                    format!(
                        "Keep going in this subagent ({}k / {}k, {:.1}% cache hit).",
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
                    "可以压缩或换新会话：当前 {}k >= 建议线 {}k（上限 {}k），大约 {} 轮后回本。",
                    ctx / 1000,
                    prof.recommend_ctx / 1000,
                    limit_k,
                    gate.break_even_turns
                )
            } else {
                format!(
                    "Ready to compact or start a new chat: {}k >= {}k (limit {}k), pays off after about {} turns.",
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
                    "处于可选压缩区（{}k，下限 {}k / 建议 {}k）：先把要点存进笔记文件，或在阶段收尾时换新会话。",
                    ctx / 1000,
                    prof.min_ctx / 1000,
                    prof.recommend_ctx / 1000
                )
            } else {
                format!(
                    "Optional compact range ({}k; floor {}k, target {}k): save key points to a notes file, or hand off at a clean milestone.",
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
                    "未到压缩下限（{}k）：先把要点存进笔记文件，噪声大的探索交给子代理去做。",
                    prof.min_ctx / 1000
                )
            } else {
                format!(
                    "Below compact floor ({}k): save key points to a notes file first, and hand noisy exploration to a subagent.",
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
                    format!("（{r}）")
                } else {
                    format!(" ({r})")
                }
            })
            .unwrap_or_default();
        (
            "keep_prefix",
            if zh {
                format!(
                    "先不压缩（{}k / {}k，缓存命中 {:.1}%）{reason_tail}",
                    ctx / 1000,
                    limit_k,
                    cache_hit
                )
            } else {
                format!(
                    "Keep going ({}k / {}k, {:.1}% cache hit){reason_tail}",
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
            past_slice_distill: "先把要点存进笔记文件（context_radar_hotspots），不改动当前缓存，换新会话或新子代理时按需读取。".into(),
            future_branch_fold: if is_gemini_4_model(&s.model, &s.model_display) && ctx > 40_000 {
                format!(
                    "当前上下文 >40k（缓存有效期 {cache_ttl_sec} 秒）：派发新子代理时给简短摘要，不要直接 fork 完整历史。"
                )
            } else {
                "噪声大的探索交给子代理去做，回传简短结论后再继续。".into()
            },
            official_handoff_new: "所有子代理结束后，调用 radar_commit_handoff 生成 /compact 指令或换新会话。".into(),
        }
    } else {
        HandoffRecipes {
            past_slice_distill: "Save the key points to a notes file first (context_radar_hotspots) without breaking the current cache.".into(),
            future_branch_fold: if is_gemini_4_model(&s.model, &s.model_display) && ctx > 40_000 {
                format!(
                    "Context >40k (cache lifetime {cache_ttl_sec}s): start a fresh subagent with a short brief instead of forking."
                )
            } else {
                "Hand noisy exploration to a subagent so noisy output stays off the main chat.".into()
            },
            official_handoff_new: "Once all subagents finish, call radar_commit_handoff to create a /compact command or open a new chat.".into(),
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
