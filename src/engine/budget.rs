//! Model x reasoning-effort budget tiers, break-even economics, and hard handoff gates.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use crate::engine::rates::RateTable;
use crate::engine::telemetry::SessionTelemetry;
use crate::settings::{BudgetTierSpec, Settings, TierMatchRule};

fn fast_model_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s_\-/])(?:flash|mini|haiku|lite)(?:$|[\s_\-/])").unwrap())
}

fn xhigh_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s_\-/(])(?:xhigh|o3)(?:$|[\s_\-/)])").unwrap())
}

fn pro_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s_\-/])pro(?:$|[\s_\-/])").unwrap())
}

pub fn builtin_tiers() -> HashMap<String, BudgetTierSpec> {
    let mut m = HashMap::new();
    let specs = [
        BudgetTierSpec {
            profile_id: "high_reasoning".into(),
            label: "高推理档位（高思考输出）".into(),
            min_ctx: 250_000,
            recommend_ctx: 290_000,
            urgent_ctx: 410_000,
            min_turns_since_ckpt: 15,
            min_remaining_turns: 7,
        },
        BudgetTierSpec {
            profile_id: "flagship".into(),
            label: "旗舰主力档位".into(),
            min_ctx: 270_000,
            recommend_ctx: 310_000,
            urgent_ctx: 420_000,
            min_turns_since_ckpt: 18,
            min_remaining_turns: 8,
        },
        BudgetTierSpec {
            profile_id: "standard".into(),
            label: "标准均衡档位".into(),
            min_ctx: 280_000,
            recommend_ctx: 320_000,
            urgent_ctx: 430_000,
            min_turns_since_ckpt: 18,
            min_remaining_turns: 8,
        },
        BudgetTierSpec {
            profile_id: "claude_tier".into(),
            label: "Claude 档位（缓存写 1.25x 溢价）".into(),
            min_ctx: 300_000,
            recommend_ctx: 350_000,
            urgent_ctx: 440_000,
            min_turns_since_ckpt: 22,
            min_remaining_turns: 10,
        },
        BudgetTierSpec {
            profile_id: "fast_reasoning".into(),
            label: "轻量高推理档位".into(),
            min_ctx: 300_000,
            recommend_ctx: 340_000,
            urgent_ctx: 430_000,
            min_turns_since_ckpt: 20,
            min_remaining_turns: 10,
        },
        BudgetTierSpec {
            profile_id: "fast".into(),
            label: "轻量高性价比档位".into(),
            min_ctx: 330_000,
            recommend_ctx: 370_000,
            urgent_ctx: 445_000,
            min_turns_since_ckpt: 25,
            min_remaining_turns: 12,
        },
    ];
    for s in specs {
        m.insert(s.profile_id.clone(), s);
    }
    m
}

fn rule_matches(rule: &TierMatchRule, raw: &str) -> bool {
    if !rule.exclude_any.is_empty() && rule.exclude_any.iter().any(|tok| raw.contains(tok.as_str())) {
        return false;
    }
    if !rule.require_all.is_empty() && !rule.require_all.iter().all(|tok| raw.contains(tok.as_str())) {
        return false;
    }
    if !rule.match_any.is_empty() && !rule.match_any.iter().any(|tok| raw.contains(tok.as_str())) {
        return false;
    }
    !rule.match_any.is_empty() || !rule.require_all.is_empty()
}

pub fn resolve_budget_tier(
    model: &str,
    model_display: &str,
    settings: &Settings,
) -> BudgetTierSpec {
    let raw = format!("{} {}", model.trim(), model_display.trim())
        .trim()
        .to_ascii_lowercase();
    let mut merged = builtin_tiers();
    for (k, v) in &settings.budgets.profiles {
        merged.insert(k.clone(), v.clone());
    }

    for rule in &settings.budgets.profile_rules {
        if let Some(spec) = merged.get(&rule.profile) {
            if rule_matches(rule, &raw) {
                return spec.clone();
            }
        }
    }

    let is_xhigh = xhigh_re().is_match(&raw);
    if raw.contains("claude") || raw.contains("opus") || raw.contains("sonnet") {
        return merged.remove("claude_tier").unwrap();
    }
    if fast_model_re().is_match(&raw) {
        let key = if is_xhigh { "fast_reasoning" } else { "fast" };
        return merged.remove(key).unwrap();
    }
    if is_xhigh {
        return merged.remove("high_reasoning").unwrap();
    }
    if pro_re().is_match(&raw) && !(raw.contains("medium") || raw.contains("low")) {
        return merged.remove("flagship").unwrap();
    }

    merged
        .remove(&settings.budgets.default_profile)
        .or_else(|| merged.remove("standard"))
        .unwrap()
}

#[derive(Debug, Clone, Serialize)]
pub struct EconomicsReport {
    pub post_compact_target_tokens: u64,
    pub compact_overhead_cny: f64,
    pub per_turn_saving_cny: f64,
    pub break_even_turns: usize,
}

#[inline]
fn round_dp(v: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    (v * factor).round() / factor
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

    let summary_usd = ((ctx as f64) * 0.94 / 1e6) * price.cache_read_per_m
        + ((ctx as f64) * 0.06 / 1e6) * price.input_per_m
        + (6_500.0 / 1e6) * price.output_per_m;
    let post_ckpt_cold_usd =
        ((target_ctx as f64) / 1e6) * (price.input_per_m - price.cache_read_per_m);
    let compact_overhead_usd = summary_usd + post_ckpt_cold_usd.max(0.0);

    let per_turn_saving_usd = ((saved_per_turn as f64) / 1e6) * price.cache_read_per_m;
    let break_even_turns = if per_turn_saving_usd > 1e-6 && saved_per_turn >= 40_000 {
        ((compact_overhead_usd / per_turn_saving_usd).ceil() as usize).max(1)
    } else {
        999
    };

    EconomicsReport {
        post_compact_target_tokens: target_ctx,
        compact_overhead_cny: round_dp(compact_overhead_usd * fx_rate, 3),
        per_turn_saving_cny: round_dp(per_turn_saving_usd * fx_rate, 3),
        break_even_turns,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GateDecision {
    pub allowed: bool,
    pub urgent_bypass: bool,
    pub reasons: Vec<String>,
    pub profile: BudgetTierSpec,
    pub post_compact_target_tokens: u64,
    pub compact_overhead_cny: f64,
    pub per_turn_saving_cny: f64,
    pub break_even_turns: usize,
}

pub fn evaluate_handoff_gate(
    session: Option<&SessionTelemetry>,
    estimated_remaining_turns: usize,
    rates: &mut RateTable,
) -> GateDecision {
    let cfg = rates.current_settings();
    let (_, sym) = rates.get_currency();
    let limit_k = (cfg.context.default_limit / 1000).max(1);

    let Some(s) = session else {
        let fallback_prof = resolve_budget_tier("", "", &cfg);
        return GateDecision {
            allowed: false,
            urgent_bypass: false,
            reasons: vec!["未找到目标活跃会话，已拒绝签发接力票据。".into()],
            profile: fallback_prof,
            post_compact_target_tokens: 0,
            compact_overhead_cny: 0.0,
            per_turn_saving_cny: 0.0,
            break_even_turns: 999,
        };
    };

    let ctx = s.latest_context_tokens;
    let model = if s.model.is_empty() { "unknown" } else { s.model.as_str() };
    let model_disp = if s.model_display.is_empty() { model } else { s.model_display.as_str() };
    let prof = resolve_budget_tier(model, model_disp, &cfg);
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
        reasons.push(format!(
            "目标 {sub_name} 是子代理，无主输入框执行 /compact；请在当前步骤收尾后保存结论并切换新子代理接续（切棒）。"
        ));
    }
    if s.just_compacted {
        reasons.push(format!(
            "该会话最后一步已是压缩检查点（等效上下文约 {}k），禁止重复压缩。",
            ctx / 1000
        ));
    }
    if ctx < prof.min_ctx {
        reasons.push(format!(
            "当前上下文 {}k 低于【{}】硬下限 {}k（满载 {}k），全量压缩会白白作废热前缀；若需清理历史长输出，请改用 context_radar_hotspots 局部提炼。",
            ctx / 1000,
            prof.label,
            prof.min_ctx / 1000,
            limit_k
        ));
    }
    if s.checkpoint_count > 0 && s.turns_since_compact < prof.min_turns_since_ckpt && !urgent {
        reasons.push(format!(
            "距上次压缩仅间隔 {} 轮 < 该模型硬性冷却轮数 {} 轮（且未达紧急线 {}k）。",
            s.turns_since_compact,
            prof.min_turns_since_ckpt,
            prof.urgent_ctx / 1000
        ));
    }
    if s.recent_cold_rebuild && !urgent {
        reasons.push("最近 3 轮刚发生过全价冷缓存重建，尚未收回冷启动成本，暂缓全量压缩。".into());
    }
    if s.running_subagent_count > 0 {
        reasons.push(format!(
            "当前有 {} 个活跃子代理正在运行（不满足 running_subagent_count == 0 硬校验），禁止原地压缩或自动接力新会话；请等待子代理收尾后再执行。",
            s.running_subagent_count
        ));
    }
    let req_rem = econ.break_even_turns.max(prof.min_remaining_turns);
    if rem_turns < req_rem && !urgent {
        reasons.push(format!(
            "预计剩余 {} 轮 < 经济回本所需 {} 轮（冷缓存+摘要需约 {}{:.2}），直接复用热前缀更省更快。",
            rem_turns, req_rem, sym, econ.compact_overhead_cny
        ));
    }

    GateDecision {
        allowed: reasons.is_empty(),
        urgent_bypass: urgent && !s.just_compacted,
        reasons,
        profile: prof,
        post_compact_target_tokens: econ.post_compact_target_tokens,
        compact_overhead_cny: econ.compact_overhead_cny,
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
    pub conversation_id: String,
    pub title: String,
    pub model_display: String,
    pub current_context_tokens: u64,
    pub context_limit: u64,
    pub currency_symbol: String,
    pub cache_hit_rate: f64,
    pub estimated_remaining_turns: usize,
    pub upcoming_noisy_tokens: u64,
    pub gate_allowed: bool,
    pub gate_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<BudgetTierSpec>,
    pub post_compact_target_tokens: u64,
    pub compact_overhead_cny: f64,
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
    let (_, sym) = rates.get_currency();
    let rem_turns = estimated_remaining_turns.max(1);

    let Some(s) = session else {
        return HandoffPlan {
            found: false,
            reason: Some("未找到匹配的活跃会话，建议保持当前前缀继续执行。".into()),
            conversation_id: String::new(),
            title: String::new(),
            model_display: String::new(),
            current_context_tokens: 0,
            context_limit: cfg.context.default_limit,
            currency_symbol: sym,
            cache_hit_rate: 0.0,
            estimated_remaining_turns: rem_turns,
            upcoming_noisy_tokens,
            gate_allowed: false,
            gate_reasons: Vec::new(),
            profile: None,
            post_compact_target_tokens: 0,
            compact_overhead_cny: 0.0,
            per_turn_saving_cny: 0.0,
            break_even_turns: 999,
            action: "keep_prefix".into(),
            headline: "未找到匹配的活跃会话，建议保持当前前缀继续执行。".into(),
            partial_compaction_recipes: None,
        };
    };

    let ctx = s.latest_context_tokens;
    let ctx_limit = if s.context_limit > 0 { s.context_limit } else { cfg.context.default_limit };
    let limit_k = (ctx_limit / 1000).max(1);
    let cache_hit = s.cache_hit_rate;
    let gate = evaluate_handoff_gate(Some(s), rem_turns, rates);
    let prof = &gate.profile;

    let (action, headline) = if s.is_subagent {
        if ctx >= prof.recommend_ctx {
            (
                "subagent_handoff",
                format!(
                    "子代理超阈值·建议切棒：当前 {}k >= 推荐线 {}k，子代理无 /compact 输入框，请在当前步骤收尾后保存结论并由主会话切换新子代理接续。",
                    ctx / 1000,
                    prof.recommend_ctx / 1000
                ),
            )
        } else {
            (
                "keep_prefix",
                format!(
                    "子代理保持热前缀继续执行（当前 {}k / {}k，缓存命中 {:.1}%）。",
                    ctx / 1000, limit_k, cache_hit
                ),
            )
        }
    } else if gate.allowed && ctx >= prof.recommend_ctx {
        (
            "active_full_compact",
            format!(
                "允许并建议全量压缩或接力新会话：当前 {}k >= 推荐线 {}k（上限 {}k），且满足冷却、零运行子代理与回本硬约束（需 >= {} 轮）；可调 context_radar_commit_handoff 准备 /compact 复制指令或官方 target:new 自主接力。",
                ctx / 1000,
                prof.recommend_ctx / 1000,
                limit_k,
                gate.break_even_turns
            ),
        )
    } else if gate.allowed && ctx >= prof.min_ctx {
        (
            "partial_branch_fold",
            format!(
                "处于可选压缩区（{}k，硬下限 {}k / 推荐线 {}k）：优先用局部切片提炼或分叉折叠保住热前缀，跨大阶段边界时可调 context_radar_commit_handoff。",
                ctx / 1000,
                prof.min_ctx / 1000,
                prof.recommend_ctx / 1000
            ),
        )
    } else if upcoming_noisy_tokens >= 35_000 || (ctx >= 120_000 && ctx < prof.min_ctx) {
        (
            "partial_branch_fold",
            format!(
                "禁止全量压缩（硬下限 {}k），建议执行【保前缀局部压缩】：历史长切片调 context_radar_hotspots 旁路提炼到 scratch/，后续高噪声探索调分叉子代理消化。",
                prof.min_ctx / 1000
            ),
        )
    } else {
        let reason_tail = gate
            .reasons
            .first()
            .map(|r| format!("（拦截原因：{r}）"))
            .unwrap_or_default();
        (
            "keep_prefix",
            format!(
                "保持现有热前缀（当前 {}k / {}k，缓存命中 {:.1}%）{reason_tail}",
                ctx / 1000, limit_k, cache_hit
            ),
        )
    };

    HandoffPlan {
        found: true,
        reason: None,
        conversation_id: s.id.clone(),
        title: s.title.clone(),
        model_display: s.model_display.clone(),
        current_context_tokens: ctx,
        context_limit: ctx_limit,
        currency_symbol: sym,
        cache_hit_rate: round_dp(cache_hit, 1),
        estimated_remaining_turns: rem_turns,
        upcoming_noisy_tokens,
        gate_allowed: gate.allowed,
        gate_reasons: gate.reasons,
        profile: Some(gate.profile),
        post_compact_target_tokens: gate.post_compact_target_tokens,
        compact_overhead_cny: gate.compact_overhead_cny,
        per_turn_saving_cny: gate.per_turn_saving_cny,
        break_even_turns: gate.break_even_turns,
        action: action.into(),
        headline,
        partial_compaction_recipes: Some(HandoffRecipes {
            past_slice_distill: "调用 context_radar_hotspots(mode='hotspots'|'distill', save_note=true)：在服务端旁路增量提炼历史高膨胀步区间到 scratch/ 供分叉或后续步精读，主会话前缀零改动。".into(),
            future_branch_fold: "需要主会话背景的高噪声任务调用 swarm(action='add', fork='coordinator')：首轮 100% 命中热前缀，噪声留在分支，仅回传精简结论后 kill。".into(),
            official_handoff_new: "需清空主会话历史且 running_subagent_count == 0 时，调用 context_radar_commit_handoff：准备好一键复制的 /compact 指令，并在面板「自主接力新会话」开关启用时通过官方 MCP App target:new 拉起接力新会话。".into(),
        }),
    }
}
