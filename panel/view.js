import {
  McpAppBridge,
  applyDocumentTheme,
  applyHostFonts,
  applyHostStyleVariables,
} from "./bridge.js";

const D = document;
const DE = D.documentElement;
const bridge = new McpAppBridge({ name: "context-radar-mcp", version: "1.0.0" }, {});
const boot = typeof window !== "undefined" ? window.__CONTEXT_RADAR_BOOT__ : null;

const BRAND_SVG = `<svg class="mk" viewBox="0 0 24 24" fill="none" aria-hidden="true"><circle cx="12" cy="12" r="9" stroke="currentColor" stroke-opacity=".22" stroke-width="1.6"/><circle cx="12" cy="12" r="5" stroke="currentColor" stroke-opacity=".28" stroke-width="1.4"/><path d="M5.6 18.4A9 9 0 1 1 18.7 6" stroke="var(--ac)" stroke-width="2.1" stroke-linecap="round"/><line x1="12" y1="12" x2="18.2" y2="5.8" stroke="var(--ac)" stroke-width="1.7" stroke-linecap="round"/><circle cx="12" cy="12" r="1.8" fill="currentColor"/><circle cx="16.5" cy="9.6" r="1.6" fill="var(--ok)"/></svg>`;
const REFRESH_SVG = `<svg width="12" height="12" viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="M13.5 8a5.5 5.5 0 1 1-1.4-3.7M13.5 2.5v2.7h-2.7" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

let viewMode = boot?.view === "handoff" ? "handoff_card" : "dashboard";
let snapshot = boot?.mode === "snapshot" ? boot : null;
let lastDigest = boot?.digest || "";
let handoffCard = null;
let handoffStatus = "pending";
let loadingInitial = !(boot?.mode === "snapshot");
let refreshing = false;
let autoRefresh = true;
let filterTab = "active";
let allowAutoHandoff = boot?.settings?.allow_auto_handoff ?? true;
let busyGrantId = null;
let errorText = "";
let canMessage = false;
let copiedId = null;
let confirmSendId = null;
let microMsg = "";
let ewmaRttMs = null;
let serverMs = boot?.server_ms ?? null;
let pollIntervalMs = 3000;
let unchangedHits = 0;
let pausedHidden = false;
let pollErrors = 0;
let lastSyncTs = boot?.mode === "snapshot" ? Date.now() : 0;
let spinTimer = null;
let showSpinner = false;

let openRowId = null;
let loadingKey = "";
let subTrayRev = 0;
const openSubTrays = new Map();
const turnsCache = new Map();
const hotspotsCache = new Map();
const adviceCache = new Map();
const attemptedAutoGrants = new Set();

let canCallTools = true;
let inFlightPoll = false;
let queuedForcePoll = false;
let pollTimer = null;
let microTimer = null;
let copiedTimer = null;

// Persistent DOM mount state (mount ONCE, patch strictly in place — zero full-shell, hero, or row-tray wipes!)
let mountedMode = "";
let ui = null;
let lastPendingSig = "";
let lastHandoffSig = "";
const rowNodes = new Map();

const uiAct = (args) => bridge.callServerTool({ name: "context_radar_ui_action", arguments: args });
const num = (v) => Number(v) || 0;
const esc = (v) => String(v ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
const safeLvl = (l) => (l === "critical" || l === "warn" ? l : "ok");
const btn = (cls, act, txt, id = "", extra = "") =>
  `<button type="button" class="bt${cls ? ` ${cls}` : ""}" data-act="${act}"${id ? ` data-id="${esc(id)}"` : ""}${extra}>${txt}</button>`;
const bdg = (cls, txt) => `<span class="bd ${cls}">${txt}</span>`;
const sw = (lbl) =>
  `<button type="button" role="switch" aria-checked="${allowAutoHandoff}" class="cr-switch cr-cap-msg${allowAutoHandoff ? " on" : ""}" data-act="toggle-auto-handoff" title="零运行子代理时自动拉起 target:new"><span class="tk" aria-hidden="true"><span class="th"></span></span><span>${lbl}</span></button>`;

const fmtK = (n) => { const x = num(n); return x >= 1e6 ? `${(x / 1e6).toFixed(2)}M` : x >= 1e3 ? `${(x / 1e3).toFixed(1)}k` : String(x); };
const fmtId = (id) => { const s = String(id || "").trim(); return s.length > 12 ? `${s.slice(0, 6)}…${s.slice(-4)}` : s; };
const fmtMs = (ms) => { const v = num(ms); return !v ? "-" : v >= 1000 ? `${(v / 1000).toFixed(1)}s` : `${v}ms`; };
const fmtClock = (iso) => { if (!iso) return "-"; const d = new Date(iso); return Number.isNaN(d.getTime()) ? "-" : d.toLocaleTimeString("zh-CN", { hour12: false }); };
const fmtAge = (ts) => {
  if (!ts) return "空闲";
  const d = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  return d < 60 ? `${d}秒前` : d < 3600 ? `${Math.floor(d / 60)}分前` : d < 86400 ? `${Math.floor(d / 3600)}小时前` : `${Math.floor(d / 86400)}天前`;
};

function getCold(it) {
  const c = it.cold_cache_breakdown;
  return { t: num(c?.turn_start_cold ?? it.turn_start_cold_count), m: num(c?.mid_loop_cold ?? it.mid_loop_cold_count), p: num(c?.post_compact_cold ?? it.post_compact_cold_count) };
}

function getVerdict(it) {
  const lvl = safeLvl(it.level);
  if (it.just_compacted) return { cls: "ok", text: "已压缩" };
  if (it.suggest_compact || lvl === "critical") return { cls: lvl === "critical" ? "critical" : "warn", text: "建议切棒接力" };
  const ctx = num(it.latest_context_tokens);
  const minL = num(it.min_compact_tokens) || 270_000;
  if (ctx >= minL * 0.72 || (ctx >= 120_000 && getCold(it).m > 0)) return { cls: "warn", text: "建议局部提炼" };
  return { cls: "ok", text: "状态充裕" };
}

function renderIdPill(id) {
  if (!id) return "";
  const ok = copiedId === id;
  return `<button type="button" class="id${ok ? " ok" : ""}" data-act="copy-id" data-id="${esc(id)}" aria-label="复制会话 ID" title="复制完整 ID：${esc(id)}">${ok ? "✓ 已复制" : esc(fmtId(id))}</button>`;
}

function renderSubTray(it) {
  const cid = it.id;
  const tab = openSubTrays.get(cid);
  if (!tab) return "";
  if (loadingKey === `${tab}:${cid}`) return `<div class="bx"><span class="nt">正在读取…</span></div>`;
  if (tab === "turns") {
    const d = turnsCache.get(cid);
    if (!d?.turns?.length) return `<div class="bx"><span class="nt">暂无轮次记录</span></div>`;
    const chrono = [...d.turns].slice(0, 24).reverse();
    const mx = Math.max(1, ...chrono.map((t) => num(t.context_tokens)));
    const bars = chrono.map((t) => {
      const h = Math.max(12, Math.round((num(t.context_tokens) / mx) * 100));
      const cls = t.is_checkpoint ? "ckpt" : t.is_cold_cache ? "cold" : "";
      const tip = `#${num(t.turn)} · ${fmtK(t.context_tokens)} · 缓存 ${num(t.cache_hit_rate).toFixed(0)}%${t.is_cold_cache ? ` (${t.cold_cache_label || "冷"})` : ""}`;
      return `<span class="sr ${cls}" style="height:${h}%" title="${esc(tip)}"></span>`;
    }).join("");
    const sym = snapshot?.currency_symbol || "¥";
    const rows = d.turns.slice(0, 12).map((t) => {
      const cCell = !t.is_cold_cache
        ? `${num(t.cache_hit_rate).toFixed(0)}%`
        : `<span class="ch">0%</span> <span class="ct ${t.cold_cache_kind === "mid_loop_cold" ? "mid" : t.cold_cache_kind === "post_compact_cold" ? "post" : "turn"}">${esc(t.cold_cache_label || "冷")}</span>`;
      return `<tr><td>#${num(t.turn)}${t.is_checkpoint ? " 压" : ""}</td><td>${esc(fmtClock(t.time_iso))}</td><td><b>${esc(fmtK(t.context_tokens))}</b></td><td>${cCell}</td><td>${t.is_checkpoint ? "检查点" : `${esc(fmtMs(t.ttft_ms))}/${num(t.stream_tps) > 0 ? num(t.stream_tps).toFixed(0) : "-"}`}</td><td>${esc(t.currency_symbol || sym)}${num(t.cost_cny).toFixed(2)}</td></tr>`;
    }).join("");
    return `<div class="bx"><div class="nt">逐轮趋势（最新 ${chrono.length}/${num(d.total_turns)} 轮 · 红=冷 · 蓝=压）</div><div class="sk" aria-label="逐轮上下文趋势">${bars}</div><div class="ts"><table class="tb"><thead><tr><th>轮</th><th>时间</th><th>上下文</th><th>缓存</th><th>TTFT/tps</th><th>费用</th></tr></thead><tbody>${rows}</tbody></table></div></div>`;
  }
  if (tab === "hotspots") {
    const h = hotspotsCache.get(cid);
    if (!h?.found) return `<div class="bx"><span class="nt">${esc(h?.error || "暂无明显膨胀热点")}</span></div>`;
    const list = (h.hotspots || []).slice(0, 5);
    const rec = h.recommended_distill_range;
    const rows = list.length
      ? list.map((x) => `<div class="nt wr">· <b>#${num(x.turn)}</b> (step ${num(x.step_idx)}) <b class="ch">+${esc(fmtK(x.delta_tokens))}</b> → ${esc(fmtK(x.context_after))} · ${esc(x.cause)}</div>`).join("")
      : `<div class="nt">当前检查点后无明显单步膨胀。</div>`;
    return `<div class="bx"><div class="nt"><b>膨胀热点</b>${rec ? ` · 建议保前缀提炼步 ${rec[0]}–${rec[1]}` : ""}</div>${rows}</div>`;
  }
  if (tab === "advice") {
    const p = adviceCache.get(cid);
    if (!p?.found) return `<div class="bx ac"><span class="nt">${esc(p?.reason || "暂无法评估")}</span></div>`;
    const sym = p.currency_symbol || snapshot?.currency_symbol || "¥";
    const runSubs = num(it.running_subagent_count);
    const subBdg = runSubs > 0 ? bdg("warn", `${runSubs} 运行子代理`) : bdg("ok", "零运行子代理");
    const reasons = (p.gate_reasons || []).map((r) => `<div class="nt wr">· ${esc(r)}</div>`).join("");
    return `<div class="bx ac"><div class="hh"><div class="bds">${bdg(p.gate_allowed ? "ok" : "warn", p.gate_allowed ? "硬校验通过·可接力" : "保护热前缀·暂缓全量")}${subBdg}</div><span class="nt">回本>=${num(p.break_even_turns)}轮 · 冷启${esc(sym)}${num(p.compact_overhead_cny).toFixed(2)} · 每轮省${esc(sym)}${num(p.per_turn_saving_cny).toFixed(3)}</span></div><div class="nt wr" style="color:var(--t1)">${esc(p.headline)}</div>${reasons}<div class="ac">${btn("", "copy-compact", "复制 /compact")}${sw("自主接力新会话")}</div></div>`;
  }
  return "";
}

function renderExtraActions(it) {
  const cid = it.id;
  const lvl = safeLvl(it.level);
  const over = (it.suggest_compact || lvl === "critical") && !it.just_compacted;
  if (!over) return "";
  if (it.is_subagent) return btn(lvl === "critical" ? "c" : "w", "copy-subagent-relay", "复制切棒提示", cid);
  return btn(lvl === "critical" ? "c" : "w", "copy-compact", "复制 /compact") +
    (confirmSendId === cid
      ? btn("c cr-cap-msg", "confirm-send", "确认发送", cid) + btn("cr-cap-msg", "cancel-send", "取消")
      : btn("cr-cap-msg", "ask-send", "发提醒", cid));
}

function mountDetailBlock() {
  const el = D.createElement("div");
  el.className = "dtl";
  el.innerHTML = `<div class="gg"><div class="gt" role="progressbar" aria-label="上下文水位" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0"><div class="gf ok" style="width:0%"></div><span class="gk"></span><span class="gk"></span><span class="gk u"></span></div><div class="gl"><span></span><span></span></div></div><div class="pls"><div class="pl"><span class="pll">上下文水位</span><span class="pv"></span><span class="ps"></span></div><div class="pl"><span class="pll">KV 缓存命中</span><span class="pv"></span><span class="ps"></span></div><div class="pl"><span class="pll">延迟与费用</span><span class="pv"></span><span class="ps"></span></div></div><div class="ac"><button type="button" class="bt cr-cap-tools" data-act="tray-turns">逐轮趋势</button><button type="button" class="bt cr-cap-tools" data-act="tray-hotspots">定位热点</button><button type="button" class="bt p cr-cap-tools" data-act="tray-advice">评估与接力</button><span class="ac" hidden></span></div><div hidden></div>`;
  const [gg, pls, acts, tr] = el.children;
  const gt = gg.firstElementChild;
  const [gf, t1, t2, t3] = gt.children;
  const [g1, g2] = gg.lastElementChild.children;
  const [p1, p2, p3] = pls.children;
  const [bt, bh, ba, ex] = acts.children;
  return {
    el, gt, gf, t1, t2, t3, g1, g2,
    a1: p1.children[1], b1: p1.children[2],
    a2: p2.children[1], b2: p2.children[2],
    a3: p3.children[1], b3: p3.children[2],
    bt, bh, ba, ex, tr, es: "", ts: "",
  };
}

function patchDetailBlock(d, it) {
  const cid = it.id || "";
  const lvl = safeLvl(it.level);
  const runSubs = num(it.running_subagent_count);
  const lim = num(it.context_limit || snapshot?.thresholds?.max) || 512_000;
  const ctx = num(it.latest_context_tokens);
  const pct = Math.min(100, Math.max(0, (ctx / lim) * 100));
  const mL = num(it.min_compact_tokens) || 270_000;
  const rL = num(it.recommend_compact_tokens) || 310_000;
  const uL = num(it.urgent_compact_tokens) || 420_000;
  const pos = (v) => `${Math.min(100, (v / lim) * 100).toFixed(1)}%`;

  d.gt.setAttribute("aria-valuenow", String(Math.round(pct)));
  d.gf.className = `gf ${lvl}`;
  d.gf.style.width = `${pct.toFixed(1)}%`;
  d.t1.style.left = pos(mL); d.t1.title = `下限 ${Math.round(mL / 1000)}k`;
  d.t2.style.left = pos(rL); d.t2.title = `推荐 ${Math.round(rL / 1000)}k`;
  d.t3.style.left = pos(uL); d.t3.title = `紧急 ${Math.round(uL / 1000)}k`;
  d.g1.textContent = `${fmtK(ctx)} / ${fmtK(lim)} (${num(it.context_pct).toFixed(1)}%)`;
  d.g2.textContent = `下限 ${Math.round(mL / 1000)}k · 推荐 ${Math.round(rL / 1000)}k · 紧急 ${Math.round(uL / 1000)}k`;

  const c = getCold(it);
  const cN = num(it.cold_cache_count);
  const sym = it.currency_symbol || snapshot?.currency_symbol || "¥";
  const extra = it.just_compacted && it.pre_compact_tokens
    ? `压前 ${fmtK(it.pre_compact_tokens)}`
    : `峰 ${fmtK(it.peak_context_tokens || it.latest_context_tokens)} · ${num(it.turns)} 轮`;
  const tps = num(it.median_stream_tps) > 0 ? `${num(it.median_stream_tps).toFixed(0)}/s` : "-";

  d.a1.textContent = `${fmtK(ctx)} · ${num(it.context_pct).toFixed(1)}%`;
  d.b1.textContent = extra;
  d.a2.textContent = `${num(it.cache_hit_rate).toFixed(1)}%`;
  d.b2.className = `ps${cN > 0 && c.m > 0 ? " ch" : ""}`;
  d.b2.textContent = cN > 0 ? `冷 ${cN} (跨${c.t}/途${c.m}/压${c.p})` : "零冷缓存";
  d.a3.textContent = `${fmtMs(it.median_ttft_ms)} · ${tps}`;
  d.b3.textContent = `${sym}${num(it.cost_cny).toFixed(2)}${it.checkpoint_count ? ` · 压${num(it.checkpoint_count)}` : ""}`;

  const cur = openSubTrays.get(cid) || "";
  d.bt.dataset.id = cid; d.bt.classList.toggle("on", cur === "turns");
  d.bh.dataset.id = cid; d.bh.classList.toggle("on", cur === "hotspots");
  d.ba.dataset.id = cid; d.ba.classList.toggle("on", cur === "advice");

  const exSig = `${cid}|${lvl}|${Boolean(it.suggest_compact)}|${Boolean(it.just_compacted)}|${Boolean(it.is_subagent)}|${confirmSendId === cid}`;
  if (exSig !== d.es) {
    d.es = exSig;
    const html = renderExtraActions(it);
    d.ex.hidden = !html;
    d.ex.innerHTML = html;
  }

  const traySig = cur ? `${cid}|${cur}|${loadingKey}|${subTrayRev}|${runSubs}|${allowAutoHandoff}` : "";
  if (traySig !== d.ts) {
    d.ts = traySig;
    d.tr.hidden = !cur;
    d.tr.innerHTML = cur ? renderSubTray(it) : "";
  }
}

const setCls = (el, cls) => { if (el && el.className !== cls) el.className = cls; };
const setTxt = (el, v) => { const s = String(v ?? ""); if (el && el.textContent !== s) el.textContent = s; };

function ensureDashboardMount(root) {
  if (mountedMode === "dashboard" && ui) return;
  rowNodes.clear();
  lastPendingSig = "";
  mountedMode = "dashboard";
  root.innerHTML = `<div class="cr-shell" id="cr-shell"><header class="ms"><div class="bd0">${BRAND_SVG}<h1>Context Radar</h1></div><div class="mr"><span class="mi" aria-live="polite" hidden></span><span class="mt"></span><button type="button" class="ib cr-cap-tools" data-act="refresh" aria-label="刷新"><span style="display:inline-flex">${REFRESH_SVG}</span></button></div></header><div class="er" hidden></div><div hidden></div><section class="cr-hero" aria-label="主会话总览" hidden><div class="hh"><div class="hm"><span class="dt idle"></span><span class="tt"></span><button type="button" class="id" data-act="copy-id" aria-label="复制会话 ID"></button></div><div class="bds"><span class="bd muted"></span><span class="bd info" hidden></span><span class="bd ok"></span></div></div></section><div class="br" hidden><div class="sg cr-cap-tools" role="tablist" aria-label="会话筛选"><button type="button" role="tab" class="sb" data-act="set-tab" data-id="active"></button><button type="button" role="tab" class="sb" data-act="set-tab" data-id="subs">含子代理</button><button type="button" role="tab" class="sb" data-act="set-tab" data-id="archived"></button></div><div class="ac"><button type="button" class="bt cr-cap-tools" data-act="toggle-auto"></button><button type="button" role="switch" class="cr-switch cr-cap-msg" data-act="toggle-auto-handoff" title="零运行子代理时自动拉起 target:new"><span class="tk" aria-hidden="true"><span class="th"></span></span><span>自主接力</span></button></div></div><div class="ls" hidden></div><div class="em" hidden></div></div>`;
  const sh = root.firstElementChild;
  const [ms, er, pg, he, br, ls, em] = sh.children;
  const [mi, mt, rb] = ms.lastElementChild.children;
  const hHead = he.firstElementChild;
  const [hd, ht, hi] = hHead.firstElementChild.children;
  const [hm, hs, hv] = hHead.lastElementChild.children;
  const hD = mountDetailBlock();
  he.appendChild(hD.el);
  const [ta, ts, tr] = br.firstElementChild.children;
  const [ab, swEl] = br.lastElementChild.children;
  ui = { sh, mi, mt, rb, ri: rb.firstElementChild, er, pg, he, hd, ht, hi, hm, hs, hv, hD, br, ta, ts, tr, ab, sw: swEl, ls, em };
}

function ensureHandoffMount(root) {
  if (mountedMode === "handoff_card" && ui) return;
  rowNodes.clear();
  lastHandoffSig = "";
  mountedMode = "handoff_card";
  root.innerHTML = `<div class="cr-shell inl" id="cr-shell"><div></div></div>`;
  const sh = root.firstElementChild;
  ui = { sh, hl: sh.firstElementChild };
}

function patchPendingSlot() {
  const g = snapshot?.pending_grant;
  if (!g) {
    if (!ui.pg.hidden) { ui.pg.hidden = true; ui.pg.innerHTML = ""; lastPendingSig = ""; }
    return;
  }
  const busy = busyGrantId === g.grant_id;
  const runSubs = num(g.running_subagent_count);
  const newCid = g.handoff_conversation_id || "";
  const sig = `${g.grant_id}|${busy}|${runSubs}|${newCid}|${copiedId}|${g.preserve_points}|${g.resume_task}`;
  if (sig === lastPendingSig) return;
  lastPendingSig = sig;
  const canLaunch = !newCid && runSubs === 0 && !busy;
  ui.pg.hidden = false;
  ui.pg.innerHTML = `<div class="bn${newCid ? " dn" : ""}"><div class="hh"><div class="hm"><span class="dt ${newCid ? "run" : "idle"}"></span><span class="tt">${newCid ? "已通过 target:new 拉起新会话" : "接力票据与 /compact 已就绪"}</span>${g.conversation_id ? renderIdPill(g.conversation_id) : ""}</div><div class="bds">${newCid ? bdg("ok", "已完成") : runSubs > 0 ? bdg("warn", `${runSubs}运行子`) : bdg("info", busy ? "拉起中…" : "待触发")}${btn("", "dismiss-grant", "×", g.grant_id, ' aria-label="关闭"')}</div></div>${newCid ? `<div class="nt">新会话 ID：${renderIdPill(newCid)}</div>` : ""}${g.preserve_points ? `<div class="nt wr">保留：${esc(g.preserve_points)}</div>` : ""}${g.resume_task ? `<div class="nt wr">续跑：${esc(g.resume_task)}</div>` : ""}<div class="ac">${g.compact_command ? btn("", "copy-pending-compact", "复制 /compact") : ""}${canLaunch ? btn("p cr-cap-msg", "launch-pending-handoff", "拉起新会话 (target:new)", g.grant_id) : ""}</div></div>`;
}

function patchHeroSlot(hero) {
  if (!hero) {
    if (!ui.he.hidden) ui.he.hidden = true;
    return;
  }
  const cid = hero.id || "";
  const lvl = safeLvl(hero.level);
  const v = getVerdict(hero);
  const runSubs = num(hero.running_subagent_count);
  ui.he.hidden = false;
  setCls(ui.he, `cr-hero lvl-${lvl}`);
  setCls(ui.hd, `dt ${hero.is_active ? "run" : "idle"}`);
  if (ui.ht.textContent !== hero.title) { ui.ht.textContent = hero.title; ui.ht.title = hero.title; }
  if (cid) {
    const ok = copiedId === cid;
    ui.hi.hidden = false;
    ui.hi.dataset.id = cid;
    setCls(ui.hi, `id${ok ? " ok" : ""}`);
    setTxt(ui.hi, ok ? "✓ 已复制" : fmtId(cid));
    ui.hi.title = `复制完整 ID：${cid}`;
  } else {
    ui.hi.hidden = true;
  }
  setTxt(ui.hm, hero.model_short || hero.model_display || "");
  ui.hs.hidden = !(runSubs > 0);
  if (runSubs > 0) setTxt(ui.hs, `${runSubs}运行子`);
  setCls(ui.hv, `bd ${v.cls}`);
  setTxt(ui.hv, v.text);
  patchDetailBlock(ui.hD, hero);
}

function createRowEntry(cid) {
  const el = D.createElement("div");
  el.className = "it";
  el.dataset.id = cid;
  el.innerHTML = `<div class="rw" role="button" tabindex="0" data-act="toggle-row" data-id="${esc(cid)}"><span class="cv">▸</span><span class="dt idle"></span><span class="bd ac" hidden>置顶</span><span class="bd muted" hidden>子</span><span class="rt"></span><button type="button" class="id" data-act="copy-id" data-id="${esc(cid)}" aria-label="复制会话 ID"></button><span class="bd" hidden></span><span class="cx"></span><span class="ag"></span></div><div class="ty" hidden></div>`;
  const r = el.firstElementChild;
  const ch = r.children;
  return { el, r, c: ch[0], d: ch[1], p: ch[2], sb: ch[3], t: ch[4], id: ch[5], v: ch[6], x: ch[7], a: ch[8], tr: el.lastElementChild, dt: null };
}

function patchSessionList(items) {
  if (!items.length) {
    if (!ui.ls.hidden) ui.ls.hidden = true;
    return;
  }
  ui.ls.hidden = false;
  const seen = new Set();
  for (let i = 0; i < items.length; i++) {
    const [it, isSub] = items[i];
    const cid = it.id;
    seen.add(cid);
    let rec = rowNodes.get(cid);
    if (!rec) { rec = createRowEntry(cid); rowNodes.set(cid, rec); }
    const lvl = safeLvl(it.level);
    const isOpen = openRowId === cid;
    const ts = it.sidebar_sort_ts || it.effective_last_active_ts || it.last_active_ts;
    const v = getVerdict(it);

    setCls(rec.r, `rw${isSub ? " sub" : ""} lvl-${lvl}`);
    rec.r.setAttribute("aria-expanded", String(isOpen));
    setTxt(rec.c, isOpen ? "▾" : "▸");
    setCls(rec.d, `dt ${it.is_active ? "run" : "idle"}`);
    rec.p.hidden = isSub || !it.pinned;
    rec.sb.hidden = !isSub;
    if (rec.t.textContent !== it.title) { rec.t.textContent = it.title; rec.t.title = it.title; }
    const ok = copiedId === cid;
    setCls(rec.id, `id${ok ? " ok" : ""}`);
    setTxt(rec.id, ok ? "✓ 已复制" : fmtId(cid));
    rec.id.title = `复制完整 ID：${cid}`;
    if (v.cls !== "ok") {
      rec.v.hidden = false;
      setCls(rec.v, `bd ${v.cls}`);
      setTxt(rec.v, v.text);
    } else {
      rec.v.hidden = true;
    }
    setCls(rec.x, `cx ${lvl}`);
    setTxt(rec.x, fmtK(it.latest_context_tokens));
    setTxt(rec.a, fmtAge(ts));

    if (isOpen) {
      if (!rec.dt) { rec.dt = mountDetailBlock(); rec.tr.appendChild(rec.dt.el); }
      rec.tr.hidden = false;
      patchDetailBlock(rec.dt, it);
    } else if (!rec.tr.hidden) {
      rec.tr.hidden = true;
    }

    if (ui.ls.children[i] !== rec.el) ui.ls.insertBefore(rec.el, ui.ls.children[i] || null);
  }
  for (const [cid, rec] of rowNodes) {
    if (!seen.has(cid)) { rec.el.remove(); rowNodes.delete(cid); }
  }
}

function patchHandoffView() {
  const c = handoffCard;
  const sig = c ? `${c.mode}|${c.blocked}|${c.grant_id}|${handoffStatus}|${c.handoff_conversation_id}|${copiedId}|${microMsg}` : "empty";
  if (sig === lastHandoffSig) return;
  lastHandoffSig = sig;
  if (!c) { ui.hl.innerHTML = `<div class="em">接力卡片加载中…</div>`; return; }
  if (c.mode === "handoff_blocked" || c.blocked) {
    const reasons = (c.reasons || []).map((r) => `<div class="nt wr">· ${esc(r)}</div>`).join("");
    ui.hl.innerHTML = `<div class="cd"><div class="hh"><div class="hm"><span class="dt idle"></span><span class="tt">全量压缩/接力已被拦截</span>${c.conversation_id ? renderIdPill(c.conversation_id) : ""}</div>${bdg("info", "保护热前缀")}</div>${reasons}</div>`;
    return;
  }
  const runSubs = num(c.running_subagent_count);
  const newCid = c.handoff_conversation_id || "";
  const badges = {
    pending: ["info", c.allow_auto_handoff ? "正在拉起 target:new…" : "就绪"],
    sending: ["warn", "正在拉起新会话…"],
    sent: ["ok", "已拉起新会话"],
    manual: ["info", runSubs > 0 ? `${runSubs}运行子` : "就绪 · 可复制"],
    claimed_old: ["muted", "票据已核销"],
    blocked: ["info", "已拦截"],
  };
  const [bCls, bTxt] = badges[handoffStatus] || badges.manual;
  const canLaunch = !newCid && runSubs === 0 && !["sent", "sending", "claimed_old"].includes(handoffStatus);
  ui.hl.innerHTML = `<div class="cd"><div class="hh"><div class="hm"><span class="dt ${handoffStatus === "sent" ? "run" : "idle"}"></span><span class="tt">${c.kind === "compact" ? "压缩与接力卡片" : "官方接力新会话卡片"}</span>${c.conversation_id ? renderIdPill(c.conversation_id) : ""}</div>${bdg(bCls, esc(bTxt))}</div>${newCid ? `<div class="nt">新会话 ID：${renderIdPill(newCid)}</div>` : ""}<div class="nt wr">保留：${esc(c.preserve_points || "")}</div>${c.resume_task ? `<div class="nt wr">续跑：${esc(c.resume_task)}</div>` : ""}${c.scratch_note_path ? `<div class="nt wr">摘要：${esc(c.scratch_note_path)}</div>` : ""}<div class="ac">${btn("", "copy-card-compact", "复制完整 /compact")}${canLaunch ? btn("p cr-cap-msg", "fire-handoff-now", "拉起新会话 (target:new)") : ""}${microMsg ? `<span class="mi" aria-live="polite">${esc(microMsg)}</span>` : ""}</div></div>`;
}

function render() {
  const root = D.getElementById("root");
  if (!root) return;
  if (viewMode === "handoff_card") {
    ensureHandoffMount(root);
    patchHandoffView();
    return;
  }

  ensureDashboardMount(root);
  const snap = snapshot;
  const pollSec = Math.round(pollIntervalMs / 1000);
  const netTip = pausedHidden ? "标签页不可见，已暂停" : `自适应 ${pollSec}s · 服务端 ${serverMs ?? "-"}ms · RTT ${ewmaRttMs ?? "-"}ms · 跳过 ${unchangedHits} 次`;

  if (microMsg) {
    setTxt(ui.mi, microMsg);
    ui.mi.hidden = false;
  } else {
    ui.mi.hidden = true;
  }
  ui.mt.hidden = false;
  setTxt(ui.mt, pausedHidden ? "已暂停" : snap ? fmtClock(snap.generated_at) : loadingInitial ? "加载中…" : "-");
  ui.mt.title = netTip;
  ui.rb.title = netTip;
  ui.ri.classList.toggle("spn", showSpinner);

  if (errorText) { ui.er.textContent = errorText; ui.er.hidden = false; }
  else ui.er.hidden = true;

  if (!snap) {
    ui.pg.hidden = ui.he.hidden = ui.br.hidden = ui.ls.hidden = true;
    ui.em.hidden = false;
    ui.em.textContent = loadingInitial ? "正在读取会话数据…" : "暂无活跃会话";
    return;
  }

  const s = snap.summary;
  const mains = snap.sessions || [];
  const hero = mains.find((m) => m.is_active) || mains[0] || null;
  const showSubs = filterTab === "subs";
  const items = [];
  for (const m of mains) {
    if (!hero || m.id !== hero.id) items.push([m, false]);
    for (const sub of (m.subagents || []).filter((x) => showSubs || x.is_active)) items.push([sub, true]);
  }
  for (const o of (snap.orphan_subagents || []).filter((x) => showSubs || x.is_active)) items.push([o, true]);

  patchPendingSlot();
  patchHeroSlot(hero);

  ui.br.hidden = false;
  const archN = num(s.archived_hidden_count);
  ui.ta.textContent = `活跃 (${num(s.running_main_count)}/${num(s.running_subagent_count)})`;
  ui.tr.textContent = `含归档${archN ? ` (${archN})` : ""}`;
  for (const [el, id] of [[ui.ta, "active"], [ui.ts, "subs"], [ui.tr, "archived"]]) {
    const on = filterTab === id;
    el.classList.toggle("on", on);
    el.setAttribute("aria-selected", String(on));
  }
  ui.ab.textContent = `自动 ${pollSec}s`;
  ui.ab.title = netTip;
  ui.ab.classList.toggle("on", autoRefresh);
  ui.sw.classList.toggle("on", allowAutoHandoff);
  ui.sw.setAttribute("aria-checked", String(allowAutoHandoff));

  patchSessionList(items);
  ui.em.hidden = Boolean(hero || items.length);
  if (!ui.em.hidden) ui.em.textContent = "暂无符合条件的会话";
}

function flashMicro(msg) {
  microMsg = msg;
  render();
  clearTimeout(microTimer);
  microTimer = setTimeout(() => { microMsg = ""; render(); }, 2000);
}

async function copyText(text, okMsg) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      if (okMsg) flashMicro(okMsg);
      return true;
    }
  } catch {}
  const ta = D.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;top:-9999px;left:-9999px;opacity:0";
  D.body.appendChild(ta);
  ta.focus(); ta.select(); ta.setSelectionRange(0, text.length);
  let ok = false;
  try { ok = D.execCommand("copy"); } catch {} finally { ta.remove(); }
  if (ok) { if (okMsg) flashMicro(okMsg); return true; }
  flashMicro("剪贴板受限");
  return false;
}

async function copyCid(cid) {
  if (!(await copyText(cid, "✓ 已复制 ID"))) return;
  copiedId = cid;
  render();
  clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => { if (copiedId === cid) { copiedId = null; render(); } }, 1500);
}

function findSession(cid) {
  if (!snapshot) return undefined;
  for (const s of snapshot.sessions) {
    if (s.id === cid) return s;
    for (const sub of s.subagents || []) if (sub.id === cid) return sub;
  }
  return (snapshot.orphan_subagents || []).find((x) => x.id === cid);
}

const extractRelayCid = (res) => { const c = res?._meta?.["com.google.antigravity/message"]?.conversationId; return typeof c === "string" ? c.trim() : ""; };

async function executeRelay(payload, claimMode, isInline) {
  const gid = payload.grant_id || "";
  const prompt = payload.handoff_prompt || "";
  if (!prompt) return;
  if (!canMessage) {
    if (isInline) handoffStatus = "manual";
    if (claimMode === "manual_handoff") await copyText(payload.compact_command || "/compact", "✓ 已复制 /compact");
    render(); return;
  }
  if ((payload.running_subagent_count ?? 0) > 0) {
    if (isInline) handoffStatus = "manual";
    if (claimMode === "manual_handoff") flashMicro("存在活跃子代理");
    render(); return;
  }
  if (canCallTools && gid) {
    try {
      const r = await uiAct({ action: "claim_grant", grant_id: gid, conversation_id: payload.conversation_id || "", claim_mode: claimMode });
      const sc = r?.structuredContent;
      if (!sc?.claimed) {
        const done = new Set(["already_claimed", "already_compacted", "session_advanced"]);
        if (isInline) {
          if (sc?.handoff_conversation_id && handoffCard) {
            handoffCard.handoff_conversation_id = sc.handoff_conversation_id;
            handoffStatus = "sent";
          } else handoffStatus = sc?.reason && done.has(sc.reason) ? "claimed_old" : "manual";
        } else if (claimMode === "manual_handoff") {
          flashMicro(sc?.reason === "running_subagents_active" ? "存在活跃子代理" : "票据已核销");
        }
        render(); return;
      }
    } catch {
      if (isInline) handoffStatus = "manual";
      if (claimMode === "manual_handoff") flashMicro("核销失败");
      render(); return;
    }
  }
  if (isInline) handoffStatus = "sending"; else busyGrantId = gid || "pending";
  render();
  try {
    const res = await bridge.sendMessage({
      role: "user",
      content: [{ type: "text", text: prompt }],
      _meta: { "com.google.antigravity/message": { target: "new" } },
    });
    if (res?.isError) {
      if (isInline) handoffStatus = "manual";
      flashMicro("已保留 /compact");
    } else {
      const newCid = extractRelayCid(res);
      if (canCallTools && gid) {
        try { await uiAct({ action: "record_relay", grant_id: gid, handoff_conversation_id: newCid }); } catch {}
      }
      if (isInline && handoffCard) { handoffCard.handoff_conversation_id = newCid; handoffStatus = "sent"; }
      else if (snapshot?.pending_grant?.grant_id === gid) { snapshot.pending_grant.claimed = true; snapshot.pending_grant.handoff_conversation_id = newCid; }
      flashMicro("✓ 已拉起新会话");
    }
  } catch {
    if (isInline) handoffStatus = "manual";
    flashMicro("拉起失败");
  } finally {
    busyGrantId = null;
    render();
  }
}

function maybeAutoRelay(payload, isInline) {
  if (!payload?.grant_id || !payload?.handoff_prompt || !canMessage) return;
  const allow = isInline ? Boolean(payload.allow_auto_handoff) : Boolean(allowAutoHandoff && payload.allow_auto_handoff !== false);
  if (!allow || payload.auto_fire === false || payload.claimed || payload.handoff_conversation_id) return;
  if ((payload.running_subagent_count ?? 0) > 0 || attemptedAutoGrants.has(payload.grant_id)) return;
  attemptedAutoGrants.add(payload.grant_id);
  void executeRelay(payload, "auto_handoff", isInline);
}

function applySnapshot(snap) {
  if (typeof snap.settings?.allow_auto_handoff === "boolean") allowAutoHandoff = snap.settings.allow_auto_handoff;
  if (snap.digest) lastDigest = snap.digest;
  snapshot = snap;
  maybeAutoRelay(snap.pending_grant, false);
}

function computeNextPollMs() {
  const rtt = ewmaRttMs ?? 0;
  const base = rtt > 800 ? 12000 : rtt > 350 ? 8000 : rtt > 150 ? 5000 : 3000;
  return pollErrors > 0 ? Math.min(45000, base * Math.pow(2, Math.min(pollErrors, 3))) : base;
}

function cancelPoll() { if (pollTimer !== null) { clearTimeout(pollTimer); pollTimer = null; } }

function scheduleNextPoll() {
  cancelPoll();
  pausedHidden = Boolean(D.hidden);
  if (viewMode !== "dashboard" || !autoRefresh || !canCallTools || pausedHidden) return;
  pollIntervalMs = computeNextPollMs();
  pollTimer = setTimeout(() => { pollTimer = null; void refreshSnapshot(false); }, pollIntervalMs);
}

async function refreshSnapshot(force = false, showSynced = false) {
  if (viewMode !== "dashboard" || !canCallTools) return;
  if (!force && D.hidden) { pausedHidden = true; return; }
  if (inFlightPoll) { if (force) queuedForcePoll = true; return; }
  cancelPoll();
  inFlightPoll = true;
  clearTimeout(spinTimer);
  if (force || !snapshot) {
    refreshing = true;
    spinTimer = setTimeout(() => { if (refreshing) { showSpinner = true; render(); } }, 150);
  }
  const t0 = performance.now();
  try {
    const args = { action: "snapshot", limit: 25, show_history: filterTab === "subs", include_archived: filterTab === "archived" };
    if (force) args.fresh = true; else if (lastDigest) args.if_digest = lastDigest;
    const res = await bridge.callServerTool({ name: "context_radar_overview", arguments: args });
    const totalMs = Math.max(0, Math.round(performance.now() - t0));
    const sc = res?.structuredContent;
    const srvMs = Math.max(0, Math.round(num(sc?.server_ms)));
    const netRtt = Math.max(0, totalMs - srvMs);
    serverMs = srvMs;
    ewmaRttMs = ewmaRttMs === null ? netRtt : Math.round(ewmaRttMs * 0.65 + netRtt * 0.35);
    pollErrors = 0;
    lastSyncTs = Date.now();
    clearTimeout(spinTimer);
    showSpinner = false;

    if (res?.isError) { errorText = "刷新失败：服务端错误"; refreshing = false; render(); }
    else if (sc?.unchanged || (sc?.digest && sc.digest === lastDigest && snapshot)) {
      unchangedHits += 1;
      errorText = "";
      if (sc?.generated_at && snapshot) snapshot.generated_at = sc.generated_at;
      if (refreshing) { refreshing = false; if (showSynced) flashMicro("✓ 已同步"); else render(); }
    } else if (sc) {
      applySnapshot(sc);
      loadingInitial = false;
      errorText = "";
      refreshing = false;
      if (showSynced) flashMicro("✓ 已同步"); else render();
    }
  } catch (err) {
    pollErrors += 1;
    errorText = `刷新提示：${err instanceof Error ? err.message : String(err)}`;
    clearTimeout(spinTimer);
    showSpinner = false;
    refreshing = false;
    render();
  } finally {
    inFlightPoll = false;
    if (queuedForcePoll) { queuedForcePoll = false; void refreshSnapshot(true, false); }
    else scheduleNextPoll();
  }
}

async function openTray(cid, tab) {
  if (openSubTrays.get(cid) === tab) { openSubTrays.delete(cid); render(); return; }
  openSubTrays.set(cid, tab);
  if (!canCallTools) { render(); return; }
  loadingKey = `${tab}:${cid}`;
  render();
  try {
    const call = tab === "turns"
      ? { name: "context_radar_turns", arguments: { conversation_id: cid, limit: 24 } }
      : tab === "hotspots"
        ? { name: "context_radar_hotspots", arguments: { conversation_id: cid, mode: "hotspots", top_k: 5 } }
        : { name: "context_radar_prepare_handoff", arguments: { conversation_id: cid, estimated_remaining_turns: 12 } };
    const res = await bridge.callServerTool(call);
    if (!res?.isError && res?.structuredContent) {
      (tab === "turns" ? turnsCache : tab === "hotspots" ? hotspotsCache : adviceCache).set(cid, res.structuredContent);
      subTrayRev += 1;
    }
  } catch { flashMicro("加载失败"); }
  finally { loadingKey = ""; render(); }
}

async function toggleAutoHandoff() {
  const next = !allowAutoHandoff;
  allowAutoHandoff = next;
  render();
  if (!canCallTools) return;
  try {
    const res = await uiAct({ action: "set_prefs", allow_auto_handoff: next });
    const val = res?.structuredContent?.settings?.allow_auto_handoff;
    if (typeof val === "boolean") allowAutoHandoff = val;
    flashMicro(allowAutoHandoff ? "✓ 已开启接力" : "✓ 已暂停接力");
    if (allowAutoHandoff && snapshot?.pending_grant) {
      snapshot.pending_grant.allow_auto_handoff = true;
      maybeAutoRelay(snapshot.pending_grant, false);
    }
  } catch { allowAutoHandoff = !next; flashMicro("保存失败，已回滚"); render(); }
}

async function sendReminder(it) {
  if (!canMessage || it.is_subagent) return;
  confirmSendId = null;
  const lim = num(it.context_limit) || 512_000;
  const rec = Math.round((num(it.recommend_compact_tokens) || 310_000) / 1000);
  const msg = `当前会话上下文已达 ${fmtK(it.latest_context_tokens)} / ${fmtK(lim)}（超推荐线 ${rec}k），请在模块边界整理进度或准备 /compact 交接。`;
  try {
    const res = await bridge.sendMessage({ role: "user", content: [{ type: "text", text: msg }] });
    if (res?.isError) await copyText("/compact", "✓ 已复制 /compact");
    else flashMicro("✓ 已发提醒");
  } catch { await copyText("/compact", "✓ 已复制 /compact"); }
  render();
}

function setFilterTab(nextTab, focusTab = false) {
  if (filterTab === nextTab) return;
  filterTab = nextTab;
  lastDigest = "";
  render();
  if (focusTab) D.querySelector(`[data-act="set-tab"][data-id="${nextTab}"]`)?.focus();
  void refreshSnapshot(true, false);
}

D.addEventListener("click", (ev) => {
  const el = ev.target?.closest?.("[data-act]");
  if (!el) return;
  ev.stopPropagation();
  const act = el.dataset.act;
  const id = el.dataset.id || "";
  if (act === "copy-id" && id) { ev.preventDefault(); void copyCid(id); }
  else if (act === "refresh") void refreshSnapshot(true, true);
  else if (act === "toggle-auto") { autoRefresh = !autoRefresh; if (autoRefresh) scheduleNextPoll(); else cancelPoll(); render(); }
  else if (act === "set-tab" && id) setFilterTab(id);
  else if (act === "toggle-auto-handoff") void toggleAutoHandoff();
  else if (act === "toggle-row" && id) { openRowId = openRowId === id ? null : id; render(); }
  else if (act === "tray-turns" && id) void openTray(id, "turns");
  else if (act === "tray-hotspots" && id) void openTray(id, "hotspots");
  else if (act === "tray-advice" && id) void openTray(id, "advice");
  else if (act === "copy-compact") void copyText("/compact", "✓ 已复制 /compact");
  else if (act === "copy-subagent-relay" && id) {
    const it = findSession(id);
    if (it) void copyText(`子代理 ${it.agent_name || it.title || it.short_id} (${it.id}) 上下文已达 ${fmtK(it.latest_context_tokens)}，请在当前步骤收尾后保存结论并切换新子代理接续`, "✓ 已复制切棒提示");
  } else if (act === "copy-card-compact" && handoffCard?.compact_command) void copyText(handoffCard.compact_command, "✓ 已复制 /compact");
  else if (act === "copy-pending-compact" && snapshot?.pending_grant?.compact_command) void copyText(snapshot.pending_grant.compact_command, "✓ 已复制 /compact");
  else if (act === "fire-handoff-now" && handoffCard) void executeRelay(handoffCard, "manual_handoff", true);
  else if (act === "launch-pending-handoff" && snapshot?.pending_grant) void executeRelay(snapshot.pending_grant, "manual_handoff", false);
  else if (act === "dismiss-grant" && id) {
    if (snapshot?.pending_grant?.grant_id === id) { delete snapshot.pending_grant; render(); }
    if (canCallTools) void uiAct({ action: "dismiss_grant", grant_id: id }).catch(() => {});
  } else if (act === "ask-send" && id) { confirmSendId = id; render(); }
  else if (act === "cancel-send") { confirmSendId = null; render(); }
  else if (act === "confirm-send" && id) { const it = findSession(id); if (it) void sendReminder(it); }
});

D.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    let ch = false;
    if (confirmSendId) { confirmSendId = null; ch = true; }
    if (openSubTrays.size) { openSubTrays.clear(); ch = true; }
    if (openRowId) { openRowId = null; ch = true; }
    if (ch) render();
    return;
  }
  const el = ev.target?.closest?.("[data-act]");
  if (!el) return;
  if ((ev.key === "Enter" || ev.key === " ") && el.dataset.act === "toggle-row") {
    ev.preventDefault();
    const id = el.dataset.id || "";
    openRowId = openRowId === id ? null : id;
    render();
  } else if ((ev.key === "ArrowLeft" || ev.key === "ArrowRight") && el.dataset.act === "set-tab") {
    ev.preventDefault();
    const order = ["active", "subs", "archived"];
    setFilterTab(order[(order.indexOf(filterTab) + (ev.key === "ArrowRight" ? 1 : 2)) % 3], true);
  }
});

D.addEventListener("visibilitychange", () => {
  if (D.hidden) { pausedHidden = true; cancelPoll(); render(); }
  else {
    pausedHidden = false;
    if (viewMode === "dashboard" && autoRefresh && canCallTools) {
      if (Date.now() - lastSyncTs > 2_000) void refreshSnapshot(false);
      else scheduleNextPoll();
    }
    render();
  }
});

bridge.ontoolinput = () => { if (!snapshot && viewMode === "dashboard") { loadingInitial = true; render(); } };

bridge.ontoolresult = (result) => {
  if (result?.isError) { errorText = "工具返回错误，请刷新重试"; loadingInitial = false; render(); return; }
  const sc = result?.structuredContent;
  if (sc && (sc.mode === "handoff_card" || sc.mode === "handoff_blocked")) {
    viewMode = "handoff_card";
    handoffCard = sc;
    if (typeof sc.allow_auto_handoff === "boolean") allowAutoHandoff = sc.allow_auto_handoff;
    handoffStatus = sc.mode === "handoff_blocked" ? "blocked"
      : sc.allow_auto_handoff && sc.auto_fire !== false && (sc.running_subagent_count ?? 0) === 0 ? "pending" : "manual";
    loadingInitial = false;
    cancelPoll();
    render();
    if (sc.mode === "handoff_card") maybeAutoRelay(sc, true);
    return;
  }
  if (sc?.mode === "snapshot") {
    viewMode = "dashboard";
    loadingInitial = false;
    errorText = "";
    lastSyncTs = Date.now();
    if (sc.server_ms !== undefined) serverMs = Math.max(0, Math.round(num(sc.server_ms)));
    scheduleNextPoll();
    // Strictly skip DOM mutation when digest matches SSR boot or previous snapshot!
    if (sc.unchanged || (sc.digest && sc.digest === lastDigest && snapshot)) return;
    applySnapshot(sc);
    render();
  }
};

function parseColorLum(raw) {
  const s = String(raw || "").trim();
  if (!s || /^transparent$/i.test(s)) return null;
  const h = s.match(/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i)?.[1];
  if (h) {
    const k = h.length;
    if ((k === 4 && parseInt(h[3] + h[3], 16) < 128) || (k === 8 && parseInt(h.slice(6), 16) < 128)) return null;
    const [r, g, b] = k <= 4
      ? [parseInt(h[0] + h[0], 16), parseInt(h[1] + h[1], 16), parseInt(h[2] + h[2], 16)]
      : [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
    return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
  }
  const m = s.match(/^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:\s*[,/]\s*((?:\d+\.?\d*|\.\d+)%?))?\s*\)/i);
  if (!m) return null;
  if (m[4]) {
    const a = m[4].endsWith("%") ? parseFloat(m[4]) / 100 : parseFloat(m[4]);
    if (Number.isNaN(a) || a < 0.5) return null;
  }
  return (0.2126 * num(m[1]) + 0.7152 * num(m[2]) + 0.0722 * num(m[3])) / 255;
}

function syncTheme(ctx) {
  if (ctx?.styles?.variables) applyHostStyleVariables(ctx.styles.variables);
  if (ctx?.styles?.css?.fonts) applyHostFonts(ctx.styles.css.fonts);
  let resolved = null;
  const raw = String(ctx?.theme || "").trim().toLowerCase();
  if (raw.includes("dark") || raw.includes("night") || raw.includes("black")) resolved = "dark";
  else if (raw.includes("light") || raw.includes("day") || raw.includes("white")) resolved = "light";
  if (!resolved) {
    const lum = parseColorLum(ctx?.styles?.variables?.["--color-background-primary"]) ??
      parseColorLum(getComputedStyle(DE).getPropertyValue("--color-background-primary"));
    if (lum !== null) resolved = lum < 0.45 ? "dark" : "light";
  }
  if (resolved) applyDocumentTheme(resolved);
}

bridge.onhostcontextchanged = (ctx) => syncTheme(ctx);
bridge.onteardown = () => { cancelPoll(); return {}; };

async function bootPanel() {
  if (snapshot) applySnapshot(snapshot);
  render();
  try {
    await bridge.connect();
    syncTheme(bridge.getHostContext());
    const caps = bridge.getHostCapabilities();
    canMessage = Boolean(caps?.message);
    canCallTools = caps?.serverTools !== undefined ? Boolean(caps.serverTools) : true;
    DE.dataset.canMsg = canMessage ? "1" : "0";
    DE.dataset.canTools = canCallTools ? "1" : "0";
    scheduleNextPoll();
    if (viewMode === "handoff_card" && handoffCard && !handoffCard.blocked) maybeAutoRelay(handoffCard, true);
    else if (snapshot?.pending_grant) maybeAutoRelay(snapshot.pending_grant, false);
    if (viewMode === "dashboard" && canCallTools) {
      if (!snapshot) setTimeout(() => { if (!snapshot && !inFlightPoll) void refreshSnapshot(true); }, 250);
      else if (boot?.stale) setTimeout(() => { if (!inFlightPoll && !D.hidden) void refreshSnapshot(false); }, 380);
    }
  } catch (err) {
    if (!snapshot) {
      errorText = `无法连接宿主：${err instanceof Error ? err.message : String(err)}`;
      loadingInitial = false;
      render();
    }
  }
}

void bootPanel();
