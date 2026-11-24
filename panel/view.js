import { McpAppBridge, applyDocumentTheme, applyHostFonts, applyHostStyleVariables } from "./bridge.js";

const D = document, DE = D.documentElement, bridge = new McpAppBridge({ name: "context-radar-mcp", version: "1.0.0" }, {});
const boot = typeof window !== "undefined" ? window.__CONTEXT_RADAR_BOOT__ : null;
const BRAND_SVG = `<svg class="mk" viewBox="0 0 24 24" fill="none"><circle cx="12" cy="12" r="9" stroke="currentColor" opacity=".24" stroke-width="1.6"/><path d="M5.6 18.4A9 9 0 1 1 18.7 6M12 12l6.2-6.2" stroke="var(--ac)" stroke-width="2" stroke-linecap="round"/><circle cx="16.5" cy="9.6" r="1.7" fill="var(--ok)"/></svg>`;
const REFRESH_SVG = `<svg width="12" height="12" viewBox="0 0 16 16" fill="none"><path d="M13.5 8a5.5 5.5 0 1 1-1.4-3.7M13.5 2.5v2.7h-2.7" stroke="currentColor" stroke-width="1.6"/></svg>`;

const getPref = () => { try { const v = localStorage.getItem("cr_lang"); return v === "en" || v === "zh-CN" ? v : ""; } catch { return ""; } };
const setPref = (v) => { try { localStorage.setItem("cr_lang", v); } catch {} };
const parseLoc = (r) => { const s = String(r || "").trim().toLowerCase(); return !s || s === "auto" ? "" : s.startsWith("zh") ? "zh-CN" : "en"; };

let langLocked = Boolean(getPref() || boot?.settings?.lang);
let lang = getPref() || parseLoc(boot?.settings?.lang) || parseLoc(typeof navigator !== "undefined" ? navigator.language : "") || parseLoc(boot?.lang) || "en";
DE.lang = lang;

const isZh = () => lang === "zh-CN", tr = (e, z) => (lang === "zh-CN" ? z : e);
const subTxt = (n) => (isZh() ? `${n} 个子代理` : `${n} subagent${n === 1 ? "" : "s"}`);
const langBtn = (en) => `<button class="lg" data-a="lng" aria-label="Language"><span class="lo${en ? " on" : ""}">EN</span><span class="lo${!en ? " on" : ""}">中文</span></button>`;

const T_CP = ["Copy /compact", "复制 /compact"], T_LN = ["Launch New Chat", "拉起新会话"], T_KP = ["Keep warm prefix", "保持热前缀"];
const T_RD = ["Ready", "已就绪"], T_LB = ["Launching…", "拉起中…"], T_AH = ["Auto handoff", "自动接力"];
const T_AHT = ["Auto-launch when 0 subagents active", "无活跃子代理时自动拉起新会话"];
const T_RC = ["✓ Copied reminder", "✓ 已复制提醒"], T_WS = ["Wait for active subagents", "存在活跃子代理"];

let viewMode = boot?.view === "handoff" ? "handoff_card" : "dashboard";
let snapshot = boot?.mode === "snapshot" ? boot : null, lastDigest = boot?.digest || "";
let handoffCard = null, handoffStatus = "pending", loadingInitial = !(boot?.mode === "snapshot");
let refreshing = false, autoRefresh = true, filterTab = "active", allowAutoHandoff = boot?.settings?.allow_auto_handoff ?? true;
let busyGrantId = null, errorText = "", canMessage = false, ownerCid = "", copiedId = null, confirmSendId = null, microMsg = "";
let ewmaRttMs = null, serverMs = boot?.server_ms ?? null, pollMs = 3e3, unchangedHits = 0, pausedHidden = false, pollErrors = 0;
let lastSyncTs = boot?.mode === "snapshot" ? Date.now() : 0, spinTimer = null, showSpinner = false;
let openRowId = null, loadingKey = "", subTrayRev = 0;
const openTrays = new Map(), cTurns = new Map(), cHot = new Map(), cDist = new Map(), cAdv = new Map(), attemptedGrants = new Set();
const trayMap = { turns: cTurns, hotspots: cHot, advice: cAdv };
let canCallTools = true, inFlightPoll = false, queuedForce = false, pollTimer = null, microTimer = null, copiedTimer = null;

// Persistent DOM mount state (mount ONCE, patch strictly in place — zero full-shell, hero, or row-tray wipes!)
let mountedMode = "", ui = null, lastPendingSig = "", lastHandoffSig = "";
const rowNodes = new Map();
let uMi, uMt, uEn, uZh, uRb, uRi, uEr, uPg, uHd, uHt, uHi, uHm, uHs, uHv, uHdBlk, uBr, uSg, uTa, uTs, uTr, uAb, uSw, uSwL, uLs, uEm, uHl;

const callTool = (name, args) => bridge.callServerTool({ name, arguments: args });
const uiAct = (args) => callTool("context_radar_ui_action", args);
const num = (v) => Number(v) || 0;
const esc = (v) => String(v ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
const safeLvl = (l) => (l === "critical" || l === "warn" ? l : "ok");
const lvlCls = (l) => (l === "critical" ? "lc" : l === "warn" ? "lw" : "");
const dv = (c, s = "") => `<div class="${c}">${s}</div>`;
const sp = (c, s = "", ex = "") => `<span class="${c}"${ex}>${s}</span>`;
const nt = (s, c = "") => dv(`nt${c}`, s);
const btn = (cls, act, txt, id = "", ex = "") => `<button class="bt${cls ? ` ${cls}` : ""}" data-a="${act}"${id ? ` data-i="${esc(id)}"` : ""}${ex}>${txt}</button>`;
const bdg = (cls, txt) => sp(`bd ${cls}`, txt);
const bx = (inner, cls = "") => dv(`bx${cls}`, inner);
const sw = (lbl) => `<button role="switch" aria-checked="${allowAutoHandoff}" class="cr-switch cr-cap-msg${allowAutoHandoff ? " on" : ""}" data-a="aho" title="${tr(...T_AHT)}"><span class="tk"><span class="th"></span></span><span>${lbl}</span></button>`;

const fmtK = (n) => { const x = num(n); return x >= 1e6 ? `${(x / 1e6).toFixed(2)}M` : x >= 1e3 ? `${(x / 1e3).toFixed(1)}k` : String(x); };
const fmtMs = (ms) => { const v = num(ms); return !v ? "-" : v >= 1e3 ? `${(v / 1e3).toFixed(1)}s` : `${v}ms`; };
const fmtClock = (iso) => { if (!iso) return "-"; const d = new Date(iso); return Number.isNaN(d.getTime()) ? "-" : d.toLocaleTimeString(isZh() ? "zh-CN" : "en-US", { hour12: false }); };
const fmtAge = (ts) => {
  if (!ts) return tr("idle", "空闲");
  const d = Math.max(0, Math.floor(Date.now() / 1e3 - ts));
  const [n, u] = d < 60 ? [d, tr("s", "秒前")] : d < 3600 ? [Math.floor(d / 60), tr("m", "分前")] : d < 86400 ? [Math.floor(d / 3600), tr("h", "小时前")] : [Math.floor(d / 86400), tr("d", "天前")];
  return `${n}${u}`;
};
const fmtMoney = (usd, cny, dec = 2, known = true) => {
  if (known === false && !num(usd) && !num(cny)) return tr("Unpriced", "未定价");
  if (snapshot?.currency_explicit) return `${snapshot.currency_symbol || "$"}${(String(snapshot.currency_code || "").toUpperCase() === "USD" ? num(usd) : num(cny)).toFixed(dec)}`;
  return isZh() ? `¥${num(cny).toFixed(dec)}` : `$${num(usd).toFixed(dec)}`;
};

const normWs = (r) => { const s = String(r || "").trim().replace(/^file:\/\//, ""); return s.length > 1 ? s.replace(/\/+$/, "") : s; };
const sameWs = (ws, allWs) => {
  const dws = normWs(snapshot?.daemon_workspace);
  return !dws || [ws, ...(Array.isArray(allWs) ? allWs : [])].map(normWs).filter(Boolean).some((w) => w === dws || w.startsWith(`${dws}/`) || dws.startsWith(`${w}/`));
};
const getHero = () => { const m = snapshot?.sessions || []; return m.find((x) => x.is_active) || m[0] || null; };
const canDirectMsg = (it) => {
  if (!canMessage || !it || it.is_subagent) return false;
  if (ownerCid) return ownerCid === it.id;
  const hero = getHero();
  return Boolean(hero && hero.id === it.id && sameWs(it.workspace, it.workspaces) && num(snapshot?.summary?.running_main_count) <= 1);
};
const getCold = ({ cold_cache_breakdown: c, turn_start_cold_count: t, mid_loop_cold_count: m, post_compact_cold_count: p }) =>
  ({ t: num(c?.turn_start_cold ?? t), m: num(c?.mid_loop_cold ?? m), p: num(c?.post_compact_cold ?? p) });

function getVerdict(it) {
  const lvl = safeLvl(it.level);
  if (it.just_compacted) return { cls: "info", text: tr("Compacted", "已压缩") };
  if (it.suggest_compact || lvl === "critical") return { cls: lvl === "critical" ? "critical" : "warn", text: tr("Handoff ready", "建议交接") };
  const ctx = num(it.latest_context_tokens);
  if (ctx >= (num(it.min_compact_tokens) || 27e4) * 0.72 || (ctx >= 12e4 && getCold(it).m > 0)) return { cls: "warn", text: tr("Slice ready", "可提炼切片") };
  return { cls: "ok", text: "" };
}

function renderIdPill(id, compact = false) {
  if (!id) return "";
  const ok = copiedId === id;
  return `<button class="id${ok ? " ok" : ""}" data-a="copy-id" data-i="${esc(id)}" title="${esc(id)}">${ok ? tr("✓ Copied", "✓ 已复制") : compact ? tr("Copy ID", "复制 ID") : esc(id.length > 12 ? `${id.slice(0, 6)}…${id.slice(-4)}` : id)}</button>`;
}

const coldKind = (k, p) => k === "mid_loop_cold" ? (p ? tr("Partial drop", "前缀跌落") : tr("Mid-turn", "中途断缓")) : k === "post_compact_cold" ? tr("Post-compact", "压缩后") : tr("New turn", "新轮次");
const COLD_CLS = { mid_loop_cold: "mid", post_compact_cold: "post" };

function renderSubTray(it) {
  const { id: cid, pricing_known: pk, running_subagent_count: rsc, min_compact_tokens: mct, recommend_compact_tokens: rct, urgent_compact_tokens: uct, workspace: ws, workspaces: wss } = it;
  const tab = openTrays.get(cid);
  if (!tab) return "";
  if (loadingKey === `${tab}:${cid}`) return bx(sp("nt", tr("Loading…", "加载中…")));
  if (tab === "turns") {
    const d = cTurns.get(cid);
    if (!d?.turns?.length) return bx(sp("nt", tr("No turns yet", "暂无轮次")));
    const chrono = d.turns.slice(0, 24).reverse(), mx = Math.max(1, ...chrono.map((t) => num(t.context_tokens)));
    const bars = chrono.map(({ turn: tn, context_tokens: ct, cache_hit_rate: ch, is_checkpoint: ck, is_cold_cache: cc, cold_cache_kind: k, is_partial_cold: pc }) =>
      `<span class="sr ${ck ? "ckpt" : cc ? "cold" : ""}" style="height:${Math.max(12, Math.round((num(ct) / mx) * 100))}%" title="${esc(`#${num(tn)} · ${fmtK(ct)} · ${tr("Cache", "缓存")} ${num(ch).toFixed(0)}%${cc ? ` (${coldKind(k, pc)})` : ""}`)}"></span>`).join("");
    const cellRow = (tag, arr) => arr.map((v) => `<${tag}>${v}</${tag}>`).join("");
    const rows = d.turns.slice(0, 10).map(({ turn: tn, time_iso: ti, context_tokens: ct, cache_hit_rate: ch, is_checkpoint: ck, is_cold_cache: cc, cold_cache_kind: k, is_partial_cold: pc, ttft_ms: tt, stream_tps: st, cost_usd: cu, cost_cny: cy }) => {
      const hr = num(ch).toFixed(0), cCell = !cc ? `${hr}%` : `${sp("ch", `${hr}%`)} ${sp(`ct ${COLD_CLS[k] || "turn"}`, coldKind(k, pc))}`;
      return `<tr>${cellRow("td", [ck ? `#${num(tn)}·${tr("Compact", "压缩")}` : `#${num(tn)}`, fmtClock(ti), `<b>${fmtK(ct)}</b>`, cCell, ck ? tr("Ckpt", "检查点") : `${fmtMs(tt)}/${num(st) > 0 ? num(st).toFixed(0) : "-"}`, fmtMoney(cu, cy, 2, pk)])}</tr>`;
    }).join("");
    const ths = cellRow("th", [tr("Turn", "轮次"), tr("Time", "时间"), tr("Ctx", "上下文"), tr("Cache", "缓存"), "TTFT/s", tr("Cost", "费用")]);
    return bx(`${nt(tr(`Recent ${chrono.length}/${num(d.total_turns)}`, `最近 ${chrono.length}/${num(d.total_turns)} 轮`))}${dv("sk", bars)}${dv("ts", `<table class="tb"><thead><tr>${ths}</tr></thead><tbody>${rows}</tbody></table>`)}`);
  }
  if (tab === "hotspots") {
    const h = cHot.get(cid);
    if (!h?.found) return bx(sp("nt", esc(h?.error || tr("No hotspots", "暂无热点"))));
    const list = (h.hotspots || []).slice(0, 5), rec = h.recommended_distill_range, spath = cDist.get(cid)?.saved_note_path, busy = loadingKey === `distill:${cid}`;
    const rows = list.length ? list.map((x) => nt(`· <b>#${num(x.turn)}</b>(s${num(x.step_idx)}) <b class="ch">+${fmtK(x.delta_tokens)}</b>→${fmtK(x.context_after)} · ${esc(x.cause)}`, " wr")).join("") : nt(tr("No spikes.", "无激增。"));
    const saved = spath ? `<div class="nt wr" style="color:var(--ok)">✓ ${tr("Saved:", "已写入")} <code>${esc(spath)}</code></div>` : "";
    const lbl = busy ? tr("Saving…", "提炼中…") : rec ? tr(`Save ${rec[0]}–${rec[1]}`, `提炼 ${rec[0]}–${rec[1]}`) : tr("Save slice", "提炼切片");
    return bx(`${nt(`<b>${tr("Hotspots", "膨胀热点")}</b>`)}${rows}${saved}${dv("ac ct0", `${btn("p", "dst", lbl, cid, busy ? " disabled" : "")}${spath ? btn("", "cdp", tr("Copy path", "复制路径"), cid) : ""}`)}`);
  }
  if (tab === "advice") {
    const p = cAdv.get(cid);
    if (!p?.found) return bx(sp("nt", esc(p?.reason || tr("Unable to evaluate", "暂无法评估"))), " ac");
    const { gate_allowed: ok, break_even_turns: bet, pricing_known: pk2, compact_overhead_usd: cou, compact_overhead_cny: coc, per_turn_saving_usd: psu, per_turn_saving_cny: psc, gate_reasons: gr, headline: hl } = p;
    const runSubs = num(rsc), subBdg = runSubs > 0 ? bdg("warn", subTxt(runSubs)) : bdg("ok", tr("0 subagents", "无子代理"));
    const k = (v, d) => Math.round((num(v) || d) / 1e3), fm = (u, c, d) => fmtMoney(u, c, d, 1);
    const tier = `${tr("Thresholds", "阈值")} ${k(mct, 27e4)}k/${k(rct, 31e4)}k/${k(uct, 42e4)}k · ${tr("break-even", "回本")} >=${num(bet)}${tr("t", "轮")}`;
    const econ = pk2 !== false ? ` · ${tr("Rebuild", "重建")} ${fm(cou, coc, 2)} · ${tr("Saves", "省")} ${fm(psu, psc, 3)}/t` : "";
    const canLaunch = ok && canMessage && !runSubs && sameWs(ws, wss), busy = loadingKey === `handoff:${cid}`;
    return bx(`${dv("hh", dv("bds", `${bdg(ok ? "ok" : "warn", ok ? tr("Ready for handoff", "满足交接条件") : tr(...T_KP))}${subBdg}`))}${nt(esc(tier + econ))}<div class="nt wr" style="color:var(--t1)">${esc(hl)}</div>${(gr || []).map((r) => nt(`· ${esc(r)}`, " wr")).join("")}${dv("ac", `${ok ? btn("", "up", busy ? tr("Preparing…", "生成中…") : tr("Prepare /compact", "生成 /compact"), cid, busy ? " disabled" : "") : btn("", "cc", tr(...T_CP))}${canLaunch ? btn("p cr-cap-msg", "ul", tr(...T_LN), cid, busy ? " disabled" : "") : ""}${sw(tr(...T_AH))}`)}`, " ac");
  }
  return "";
}

function renderExtraActions(it) {
  const cid = it.id, lvl = safeLvl(it.level);
  if (!((it.suggest_compact || lvl === "critical") && !it.just_compacted)) return "";
  if (it.is_subagent) return btn(lvl === "critical" ? "c" : "w", "copy-subagent-relay", tr("Copy baton", "复制切棒"), cid);
  const cpBtn = btn(lvl === "critical" ? "c" : "w", "cc", tr(...T_CP));
  if (!canDirectMsg(it)) return cpBtn + btn("", "crm", tr("Copy reminder", "复制提醒"), cid);
  return cpBtn + (confirmSendId === cid
    ? btn("c cr-cap-msg", "yes", tr("Confirm", "确认"), cid) + btn("cr-cap-msg", "no", tr("Cancel", "取消"))
    : btn("cr-cap-msg", "ask", tr("Remind", "提醒"), cid));
}

function mountDetailBlock() {
  const el = D.createElement("div"); el.className = "dtl";
  const pl = dv("pl", `${sp("pll")}${sp("pv")}${sp("ps")}`).repeat(3);
  const bts = ["turns", "hotspots", "advice"].map((t, i) => `<button class="bt${i === 2 ? " p" : ""} ct0" data-a="t-${t}"></button>`).join("");
  el.innerHTML = `${dv("gg", `${dv("gt", `${dv("gf ok", "")}${sp("gk").repeat(2)}${sp("gk u")}`)}${dv("gl", "<span></span><span></span>")}`)}${dv("pls", pl)}${dv("ac", `${bts}<button class="id" data-a="copy-id"></button>${sp("ac", "", " hidden")}`)}<div hidden></div>`;
  const [gg, pls, acts, trEl] = el.children, gt = gg.firstElementChild;
  sa(gt, "role", "progressbar"); sa(gt, "aria-valuemin", "0"); sa(gt, "aria-valuemax", "100");
  const [gf, t1, t2, t3] = gt.children, [g1, g2] = gg.lastElementChild.children, [bt, bh, ba, bi, ex] = acts.children;
  return [el, gt, gf, [t1, t2, t3], g1, g2, [...pls.children], [bt, bh, ba], bi, ex, trEl, "", ""];
}

const setCls = (el, c) => { if (el && el.className !== c) el.className = c; };
const setTxt = (el, v) => { const s = String(v ?? ""); if (el && el.textContent !== s) el.textContent = s; };
const sa = (e, k, v) => e.setAttribute(k, v);
const cl = (e, c, b) => e.classList.toggle(c, b);
const patchIdBtn = (el, cid, show) => {
  if (!show || !cid) { el.hidden = true; return; }
  const ok = copiedId === cid; el.hidden = false; el.dataset.i = cid;
  setCls(el, `id${ok ? " ok" : ""}`); setTxt(el, ok ? tr("✓ Copied", "✓ 已复制") : tr("Copy ID", "复制 ID")); el.title = cid;
};
const patchBadge = (el, cls, txt) => { el.hidden = !txt; if (txt) { setCls(el, `bd ${cls}`); setTxt(el, txt); } };
const setPill = (p, lbl, v1, v2, cls, tip) => {
  const [l, a, b] = p.children; setTxt(l, lbl); a.textContent = v1; b.textContent = v2;
  if (cls) b.className = cls; if (tip || b.title) b.title = tip;
};

function patchDetailBlock(d, it, showCopyId) {
  const { id: cid = "", level, running_subagent_count: rsc, context_limit: cLim, latest_context_tokens: lct, min_compact_tokens: mct, recommend_compact_tokens: rct, urgent_compact_tokens: uct, context_pct: cpct, just_compacted: jc, pre_compact_tokens: pctPre, turns: tn, cold_cache_count: ccc, median_stream_tps: mst, peak_context_tokens: peak, cache_hit_rate: chr, median_ttft_ms: mtt, cost_usd: cu, cost_cny: cy, pricing_known: pk, checkpoint_count: ckc, suggest_compact: sc, is_subagent: sub } = it;
  const [, gt, gf, ts, g1, g2, ps, bs, bi, ex, trEl] = d;
  const lvl = safeLvl(level), runSubs = num(rsc), lim = num(cLim || snapshot?.thresholds?.max) || 512_000, ctx = num(lct);
  const pct = Math.min(100, Math.max(0, (ctx / lim) * 100));
  sa(gt, "aria-label", tr("Context usage", "上下文占用")); sa(gt, "aria-valuenow", String(Math.round(pct)));
  gf.className = `gf ${lvl}`; gf.style.width = `${pct.toFixed(1)}%`;
  const tick = (el, v, d0, lb) => { const n = num(v) || d0; el.style.left = `${Math.min(100, (n / lim) * 100).toFixed(1)}%`; el.title = `${lb} ${Math.round(n / 1e3)}k`; };
  tick(ts[0], mct, 27e4, tr("Floor", "下限")); tick(ts[1], rct, 31e4, tr("Target", "推荐")); tick(ts[2], uct, 42e4, tr("Urgent", "紧急"));
  g1.textContent = `${fmtK(ctx)} / ${fmtK(lim)} (${num(cpct).toFixed(1)}%)`;
  g2.textContent = jc && pctPre ? `${tr("Before", "压缩前")} ${fmtK(pctPre)}` : isZh() ? `${num(tn)} 轮` : `${num(tn)}t`;

  const c = getCold(it), cN = num(ccc), tps = num(mst) > 0 ? `${num(mst).toFixed(0)} tok/s` : "-";
  setPill(ps[0], tr("Context", "上下文"), `${fmtK(ctx)} · ${num(cpct).toFixed(1)}%`, `${tr("Peak", "峰值")} ${fmtK(peak || ctx)}`, "", "");
  setPill(ps[1], tr("KV Cache", "KV 缓存"), `${num(chr).toFixed(1)}%`, cN ? (isZh() ? `冷未命中 ${cN} 次` : `${cN} cold miss${cN === 1 ? "" : "es"}`) : tr("0 cold misses", "无冷未命中"), `ps${cN && c.m > 0 ? " ch" : ""}`, cN ? (isZh() ? `新:${c.t} 途:${c.m} 压:${c.p}` : `New:${c.t} Mid:${c.m} Compact:${c.p}`) : "");
  setPill(ps[2], tr("Speed & Cost", "速度与费用"), `${fmtMs(mtt)} · ${tps}`, fmtMoney(cu, cy, 2, pk), "", ckc ? (isZh() ? `压缩 ${num(ckc)} 次` : `Compacted ${num(ckc)}x`) : "");

  const cur = openTrays.get(cid) || "", tKeys = ["turns", "hotspots", "advice"], tLbls = [tr("Turns", "轮次"), tr("Hotspots", "热点"), tr("Handoff", "交接")];
  for (let i = 0; i < 3; i++) { setTxt(bs[i], tLbls[i]); bs[i].dataset.i = cid; cl(bs[i], "on", cur === tKeys[i]); }
  patchIdBtn(bi, cid, showCopyId);

  const exSig = `${lang}|${cid}|${lvl}|${Boolean(sc)}|${Boolean(jc)}|${Boolean(sub)}|${canDirectMsg(it)}|${confirmSendId === cid}`;
  if (exSig !== d[11]) { d[11] = exSig; const h = renderExtraActions(it); ex.hidden = !h; ex.innerHTML = h; }

  const traySig = cur ? `${lang}|${cid}|${cur}|${loadingKey}|${subTrayRev}|${runSubs}|${allowAutoHandoff}|${canMessage}` : "";
  if (traySig !== d[12]) { d[12] = traySig; trEl.hidden = !cur; trEl.innerHTML = cur ? renderSubTray(it) : ""; }
}

function ensureDashboardMount(root) {
  if (mountedMode === "dashboard" && ui) return;
  rowNodes.clear(); lastPendingSig = ""; mountedMode = "dashboard";
  const tabs = ["active", "subs", "archived"].map((t) => `<button role="tab" class="sb" data-a="tab" data-i="${t}"></button>`).join("");
  root.innerHTML = dv("sh", `<header class="ms">${dv("bd0", `${BRAND_SVG}<h1>Context Radar</h1>`)}${dv("mr", `${sp("mi", "", " hidden")}${sp("mt")}${langBtn(true)}<button class="ib ct0" data-a="ref" aria-label="Refresh">${REFRESH_SVG}</button>`)}</header>${dv("er", "", )}${dv("", "")}<section class="hr" hidden>${dv("hh", `${dv("hm", `${sp("dt idle")}${sp("tt")}<button class="id" data-a="copy-id"></button>`)}${dv("bds", `${sp("bd muted")}${sp("bd info", "", " hidden")}${sp("bd ok", "", " hidden")}`)}`)}</section>${dv("br", `${dv("sg ct0", tabs)}${dv("ac", `${btn("ct0", "aut", "")}<button role="switch" class="cr-switch cr-cap-msg" data-a="aho">${sp("tk", sp("th"))}<span></span></button>`)}`)}${dv("ls", "")}${dv("em", "")}`);
  const sh = root.firstElementChild, [ms, er, pg, he, br, ls, em] = sh.children;
  er.hidden = pg.hidden = br.hidden = ls.hidden = em.hidden = true;
  const [mi, mt, lgBtn, rb] = ms.lastElementChild.children;
  [uEn, uZh] = lgBtn.children;
  const hh = he.firstElementChild;
  [uHd, uHt, uHi] = hh.firstElementChild.children; [uHm, uHs, uHv] = hh.lastElementChild.children;
  uHdBlk = mountDetailBlock(); he.appendChild(uHdBlk[0]);
  [uTa, uTs, uTr] = br.firstElementChild.children; sa(br.firstElementChild, "role", "tablist");
  [uAb, uSw] = br.lastElementChild.children;
  uMi = mi; uMt = mt; uRb = rb; uRi = rb.firstElementChild; uEr = er; uPg = pg; uBr = br; uSg = br.firstElementChild; uSwL = uSw.lastElementChild; uLs = ls; uEm = em;
  ui = { he };
}

function ensureHandoffMount(root) {
  if (mountedMode === "handoff_card" && ui) return;
  rowNodes.clear(); lastHandoffSig = ""; mountedMode = "handoff_card";
  root.innerHTML = dv("sh inl", "<div></div>");
  uHl = root.firstElementChild.firstElementChild;
  ui = { he: null };
}

function renderRelayCard(cls, active, title, cid, rightHtml, newCid, preserve, next, slice, acts, extra = "") {
  return dv(cls, `${dv("hh", `${dv("hm", `${sp(`dt ${active ? "run" : "idle"}`)}${sp("tt", esc(title))}${cid ? renderIdPill(cid, true) : ""}`)}${dv("bds", rightHtml)}`)}${newCid ? nt(`${tr("New chat:", "新会话：")} ${renderIdPill(newCid)}`) : ""}${preserve ? nt(`${tr("Preserve:", "保留：")} ${esc(preserve)}`, " wr") : ""}${next ? nt(`${tr("Next:", "续跑：")} ${esc(next)}`, " wr") : ""}${slice ? nt(`${tr("Slice:", "摘要：")} ${esc(slice)}`, " wr") : ""}${extra}${acts ? dv("ac", acts) : ""}`);
}

function patchPendingSlot() {
  const g = snapshot?.pending_grant;
  if (!g || !sameWs(g.workspace, g.workspaces)) { if (!uPg.hidden) { uPg.hidden = true; uPg.innerHTML = ""; lastPendingSig = ""; } return; }
  const { grant_id: gid, running_subagent_count: rsc, handoff_conversation_id: newCid = "", conversation_id: cid, preserve_points: pp, resume_task: rt, compact_command: cc } = g;
  const busy = busyGrantId === gid, runSubs = num(rsc), sig = `${lang}|${gid}|${busy}|${runSubs}|${newCid}|${copiedId}|${pp}|${rt}`;
  if (sig === lastPendingSig) return;
  lastPendingSig = sig;
  const rBdg = newCid ? bdg("ok", tr("Done", "已完成")) : runSubs > 0 ? bdg("warn", subTxt(runSubs)) : bdg("info", busy ? tr(...T_LB) : tr("Pending", "待触发"));
  uPg.hidden = false;
  uPg.innerHTML = renderRelayCard(
    `bn${newCid ? " dn" : ""}`, Boolean(newCid),
    newCid ? tr("New chat launched", "已拉起新会话") : tr("Handoff ready", "交接已就绪"), cid,
    `${rBdg}${btn("", "dis", "×", gid, ` aria-label="${tr("Dismiss", "关闭")}"`)}`,
    newCid, pp, rt, "",
    `${cc ? btn("", "cpc", tr(...T_CP)) : ""}${!newCid && !runSubs && !busy ? btn("p cr-cap-msg", "lph", tr(...T_LN), gid) : ""}`
  );
}

function patchHeroSlot(hero) {
  if (!hero) { if (!ui.he.hidden) ui.he.hidden = true; return; }
  const { id: cid = "", level, running_subagent_count: rsc, is_active: act, title, model_short: ms, model_display: md } = hero;
  const lvl = safeLvl(level), v = getVerdict(hero), runSubs = num(rsc);
  ui.he.hidden = false; sa(ui.he, "aria-label", tr("Primary", "主会话"));
  setCls(ui.he, `hr ${lvlCls(lvl)}`); setCls(uHd, `dt ${act ? "run" : "idle"}`);
  const tip = cid ? `${title} (${cid})` : title;
  if (uHt.textContent !== title) uHt.textContent = title;
  if (uHt.title !== tip) uHt.title = tip;
  patchIdBtn(uHi, cid, true); setTxt(uHm, ms || md || "");
  uHs.hidden = !(runSubs > 0); if (runSubs > 0) setTxt(uHs, subTxt(runSubs));
  patchBadge(uHv, v.cls, v.text); patchDetailBlock(uHdBlk, hero, false);
}

function createRowEntry(cid) {
  const el = D.createElement("div"); el.className = "it"; el.dataset.i = cid;
  el.innerHTML = `<div class="rw" role="button" tabindex="0" data-a="row" data-i="${esc(cid)}">${sp("cv", "▸")}${sp("dt idle")}${sp("bd ac", "", " hidden")}${sp("bd muted", "", " hidden")}${sp("rt")}${sp("bd", "", " hidden")}${sp("cx")}${sp("ag")}</div><div class="ty" hidden></div>`;
  const r = el.firstElementChild, ch = r.children;
  return { el, r, c: ch[0], d: ch[1], p: ch[2], sb: ch[3], t: ch[4], v: ch[5], x: ch[6], a: ch[7], tr: el.lastElementChild, dt: null };
}

function patchSessionList(items) {
  if (!items.length) { if (!uLs.hidden) uLs.hidden = true; return; }
  uLs.hidden = false;
  const seen = new Set();
  for (let i = 0; i < items.length; i++) {
    const [it, isSub] = items[i], { id: cid, level, is_active: act, pinned, title, workspace_name: wn, latest_context_tokens: lct, sidebar_sort_ts: s1, effective_last_active_ts: s2, last_active_ts: s3 } = it;
    seen.add(cid);
    let rec = rowNodes.get(cid);
    if (!rec) { rec = createRowEntry(cid); rowNodes.set(cid, rec); }
    const lvl = safeLvl(level), isOpen = openRowId === cid, v = getVerdict(it);

    setCls(rec.r, `rw${isSub ? " sub" : ""} ${lvlCls(lvl)}`); sa(rec.r, "aria-expanded", String(isOpen));
    setTxt(rec.c, isOpen ? "▾" : "▸"); setCls(rec.d, `dt ${act ? "run" : "idle"}`);
    rec.p.hidden = isSub || !pinned; if (!rec.p.hidden) setTxt(rec.p, tr("Pinned", "置顶"));
    rec.sb.hidden = !isSub; if (isSub) setTxt(rec.sb, tr("Subagent", "子代理"));
    const rowTip = `${title}${wn ? ` · ${wn}` : ""} (${cid})`;
    if (rec.t.textContent !== title) rec.t.textContent = title;
    if (rec.t.title !== rowTip) rec.t.title = rowTip;
    patchBadge(rec.v, v.cls, v.text); setCls(rec.x, `cx ${lvl}`);
    setTxt(rec.x, fmtK(lct)); setTxt(rec.a, fmtAge(s1 || s2 || s3));

    if (isOpen) { if (!rec.dt) { rec.dt = mountDetailBlock(); rec.tr.appendChild(rec.dt[0]); } rec.tr.hidden = false; patchDetailBlock(rec.dt, it, true); }
    else if (!rec.tr.hidden) rec.tr.hidden = true;

    if (uLs.children[i] !== rec.el) uLs.insertBefore(rec.el, uLs.children[i] || null);
  }
  for (const [cid, rec] of rowNodes) if (!seen.has(cid)) { rec.el.remove(); rowNodes.delete(cid); }
}

function patchHandoffView() {
  const c = handoffCard;
  const sig = c ? `${lang}|${c.mode}|${c.blocked}|${c.grant_id}|${handoffStatus}|${c.handoff_conversation_id}|${copiedId}|${microMsg}` : `${lang}|empty`;
  if (sig === lastHandoffSig) return;
  lastHandoffSig = sig;
  if (!c) { uHl.innerHTML = dv("em", tr("Loading…", "加载中…")); return; }
  if (c.mode === "handoff_blocked" || c.blocked) {
    uHl.innerHTML = renderRelayCard("cd", false, tr("Handoff gated", "暂缓交接"), c.conversation_id, bdg("info", tr(...T_KP)), "", "", "", "", "", (c.reasons || []).map((r) => nt(`· ${esc(r)}`, " wr")).join(""));
    return;
  }
  const runSubs = num(c.running_subagent_count), newCid = c.handoff_conversation_id || "";
  const badges = {
    pending: ["info", c.allow_auto_handoff ? tr(...T_LB) : tr(...T_RD)], sending: ["warn", tr(...T_LB)],
    sent: ["ok", tr("New chat started", "已拉起新会话")], manual: ["info", runSubs > 0 ? subTxt(runSubs) : tr(...T_RD)],
    claimed_old: ["muted", tr("Used", "已使用")], blocked: ["info", tr("Gated", "已拦截")],
  };
  const [bCls, bTxt] = badges[handoffStatus] || badges.manual;
  const canLaunch = !newCid && !runSubs && !["sent", "sending", "claimed_old"].includes(handoffStatus);
  uHl.innerHTML = renderRelayCard(
    "cd", handoffStatus === "sent",
    c.kind === "compact" ? tr("Compaction Ready", "压缩已就绪") : tr("Session Handoff", "新会话交接"),
    c.conversation_id, `${langBtn(!isZh())}${bdg(bCls, bTxt)}`,
    newCid, c.preserve_points, c.resume_task, c.scratch_note_path,
    `${btn("", "ccc", tr(...T_CP))}${canLaunch ? btn("p cr-cap-msg", "fhn", tr(...T_LN)) : ""}${microMsg ? sp("mi", esc(microMsg)) : ""}`
  );
}

function render() {
  const root = D.getElementById("root");
  if (!root) return;
  if (viewMode === "handoff_card") { ensureHandoffMount(root); patchHandoffView(); return; }

  ensureDashboardMount(root);
  const snap = snapshot, pollSec = Math.round(pollMs / 1e3);
  const netTip = pausedHidden ? tr("Paused", "已暂停") : `${pollSec}s · ${serverMs ?? "-"}/${ewmaRttMs ?? "-"}ms · ${tr("skip", "跳过")} ${unchangedHits}`;

  cl(uEn, "on", !isZh()); cl(uZh, "on", isZh()); sa(uRb, "aria-label", tr("Refresh", "刷新"));
  uMi.hidden = !microMsg; if (microMsg) setTxt(uMi, microMsg);
  uMt.hidden = false;
  setTxt(uMt, pausedHidden ? tr("Paused", "已暂停") : snap ? fmtClock(snap.generated_at) : loadingInitial ? tr("Loading…", "加载中…") : "-");
  uMt.title = uRb.title = netTip; cl(uRi, "spn", showSpinner);
  uEr.hidden = !errorText; if (errorText) uEr.textContent = errorText;

  if (!snap) {
    uPg.hidden = ui.he.hidden = uBr.hidden = uLs.hidden = true; uEm.hidden = false;
    uEm.textContent = loadingInitial ? tr("Loading…", "加载中…") : tr("No sessions", "暂无会话");
    return;
  }

  const s = snap.summary, mains = snap.sessions || [], hero = getHero(), showSubs = filterTab === "subs", items = [];
  for (const m of mains) {
    if (!hero || m.id !== hero.id) items.push([m, false]);
    for (const sub of (m.subagents || []).filter((x) => showSubs || x.is_active)) items.push([sub, true]);
  }
  for (const o of (snap.orphan_subagents || []).filter((x) => showSubs || x.is_active)) items.push([o, true]);

  patchPendingSlot(); patchHeroSlot(hero);
  uBr.hidden = false; sa(uSg, "aria-label", tr("Filter", "筛选"));
  const tabCfg = [[uTa, "active", tr("Active", "活跃"), num(s.running_main_count)], [uTs, "subs", tr("Subagents", "子代理"), 0], [uTr, "archived", tr("Archived", "归档"), num(s.archived_hidden_count)]];
  for (const [el, id, lbl, n] of tabCfg) {
    el.textContent = `${lbl}${n ? ` (${n})` : ""}`;
    const on = filterTab === id; cl(el, "on", on); sa(el, "aria-selected", String(on));
  }
  uAb.textContent = isZh() ? `自动 ${pollSec}s` : `Auto ${pollSec}s`;
  uAb.title = netTip; cl(uAb, "on", autoRefresh);
  cl(uSw, "on", allowAutoHandoff); sa(uSw, "aria-checked", String(allowAutoHandoff));
  uSw.title = tr(...T_AHT); setTxt(uSwL, tr(...T_AH));

  patchSessionList(items);
  uEm.hidden = Boolean(hero || items.length);
  if (!uEm.hidden) uEm.textContent = tr("No sessions", "暂无会话");
}

function flashMicro(msg) { microMsg = msg; render(); clearTimeout(microTimer); microTimer = setTimeout(() => { microMsg = ""; render(); }, 2200); }

async function copyText(text, okMsg) {
  try { if (navigator.clipboard?.writeText) { await navigator.clipboard.writeText(text); if (okMsg) flashMicro(okMsg); return true; } } catch {}
  const ta = D.createElement("textarea");
  ta.value = text; sa(ta, "readonly", ""); ta.style.cssText = "position:fixed;top:-9999px;opacity:0";
  D.body.appendChild(ta); ta.focus(); ta.select(); ta.setSelectionRange(0, text.length);
  let ok = false; try { ok = D.execCommand("copy"); } catch {} finally { ta.remove(); }
  if (ok) { if (okMsg) flashMicro(okMsg); return true; }
  flashMicro(tr("Copy failed", "复制失败")); return false;
}

const cpCompact = (cmd = "/compact") => copyText(cmd, tr("✓ Copied /compact", "✓ 已复制 /compact"));

async function copyCid(cid) {
  if (!(await copyText(cid, tr("✓ Copied ID", "✓ 已复制 ID")))) return;
  copiedId = cid; render(); clearTimeout(copiedTimer);
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
  const { grant_id: gid = "", handoff_prompt: prompt = "", compact_command: cmd = "/compact", running_subagent_count: rsc = 0, conversation_id: cid = "", workspace: ws = "", workspaces: wss } = payload;
  if (!prompt) return;
  const fail = (msg) => { if (isInline) handoffStatus = "manual"; if (msg) flashMicro(msg); render(); };
  if (!canMessage) { if (claimMode === "manual_handoff") await cpCompact(cmd); return fail(""); }
  if (!isInline && !sameWs(ws, wss)) { await copyText(cmd); return fail(tr("Other workspace; copied", "跨工作区不拉起，已复制")); }
  if (rsc > 0) return fail(claimMode === "manual_handoff" ? tr(...T_WS) : "");
  if (canCallTools && gid) {
    try {
      const sc = (await uiAct({ action: "claim_grant", grant_id: gid, conversation_id: cid, workspace: snapshot?.daemon_workspace || ws, claim_mode: claimMode }))?.structuredContent;
      if (!sc?.claimed) {
        if (isInline) {
          if (sc?.handoff_conversation_id && handoffCard) { handoffCard.handoff_conversation_id = sc.handoff_conversation_id; handoffStatus = "sent"; }
          else handoffStatus = ["already_claimed", "already_compacted", "session_advanced"].includes(sc?.reason) ? "claimed_old" : "manual";
          return render();
        }
        return fail(claimMode === "manual_handoff" ? (sc?.reason === "running_subagents_active" ? tr(...T_WS) : sc?.reason?.endsWith("mismatch") ? tr("Other workspace", "属其他工作区") : tr("Grant used", "票据已核销")) : "");
      }
    } catch { return fail(claimMode === "manual_handoff" ? tr("Claim failed", "核销失败") : ""); }
  }
  if (isInline) handoffStatus = "sending"; else busyGrantId = gid || "pending";
  render();
  try {
    const res = await bridge.sendMessage({ role: "user", content: [{ type: "text", text: prompt }], _meta: { "com.google.antigravity/message": { target: "new" } } });
    if (res?.isError) return fail(tr("Kept /compact", "已保留 /compact"));
    const newCid = extractRelayCid(res), pg = snapshot?.pending_grant;
    if (canCallTools && gid) try { await uiAct({ action: "record_relay", grant_id: gid, handoff_conversation_id: newCid }); } catch {}
    if (isInline && handoffCard) { handoffCard.handoff_conversation_id = newCid; handoffStatus = "sent"; }
    else if (pg?.grant_id === gid) { pg.claimed = true; pg.handoff_conversation_id = newCid; }
    flashMicro(tr("✓ New chat launched", "✓ 已拉起新会话"));
  } catch { fail(tr("Launch failed", "拉起失败")); }
  finally { busyGrantId = null; render(); }
}

function maybeAutoRelay(p, isInline) {
  if (!p?.grant_id || !p?.handoff_prompt || !canMessage || D.hidden) return;
  if (!isInline) {
    if (!sameWs(p.workspace, p.workspaces) || (ownerCid && ownerCid !== p.conversation_id)) return;
    if (!ownerCid && (!snapshot?.daemon_workspace || num(snapshot?.summary?.running_main_count) > 1)) return;
  }
  const allow = isInline ? Boolean(p.allow_auto_handoff) : Boolean(allowAutoHandoff && p.allow_auto_handoff !== false);
  if (!allow || p.auto_fire === false || p.claimed || p.handoff_conversation_id || (p.running_subagent_count ?? 0) > 0 || attemptedGrants.has(p.grant_id)) return;
  attemptedGrants.add(p.grant_id);
  void executeRelay(p, "auto_handoff", isInline);
}

function applySnapshot(snap) {
  if (typeof snap.settings?.allow_auto_handoff === "boolean") allowAutoHandoff = snap.settings.allow_auto_handoff;
  if (!langLocked && snap.settings?.lang) { const p = parseLoc(snap.settings.lang); if (p) { lang = p; DE.lang = lang; } }
  if (snap.digest) lastDigest = snap.digest;
  snapshot = snap;
  maybeAutoRelay(snap.pending_grant, false);
}

const nextPollMs = () => { const r = ewmaRttMs ?? 0, b = r > 800 ? 12e3 : r > 350 ? 8e3 : r > 150 ? 5e3 : 3e3; return pollErrors > 0 ? Math.min(45e3, b * Math.pow(2, Math.min(pollErrors, 3))) : b; };
const cancelPoll = () => { if (pollTimer !== null) { clearTimeout(pollTimer); pollTimer = null; } };

function scheduleNextPoll() {
  cancelPoll(); pausedHidden = Boolean(D.hidden);
  if (viewMode !== "dashboard" || !autoRefresh || !canCallTools || pausedHidden) return;
  pollMs = nextPollMs();
  pollTimer = setTimeout(() => { pollTimer = null; void refreshSnapshot(false); }, pollMs);
}

async function refreshSnapshot(force = false, showSynced = false) {
  if (viewMode !== "dashboard" || !canCallTools) return;
  if (!force && D.hidden) { pausedHidden = true; return; }
  if (inFlightPoll) { if (force) queuedForce = true; return; }
  cancelPoll(); inFlightPoll = true; clearTimeout(spinTimer);
  if (force || !snapshot) { refreshing = true; spinTimer = setTimeout(() => { if (refreshing) { showSpinner = true; render(); } }, 150); }
  const t0 = performance.now();
  try {
    const args = { action: "snapshot", limit: 25, show_history: filterTab === "subs", include_archived: filterTab === "archived" };
    if (force) args.fresh = true; else if (lastDigest) args.if_digest = lastDigest;
    const res = await callTool("context_radar_overview", args);
    const totalMs = Math.max(0, Math.round(performance.now() - t0)), sc = res?.structuredContent, srvMs = Math.max(0, Math.round(num(sc?.server_ms))), netRtt = Math.max(0, totalMs - srvMs);
    serverMs = srvMs; ewmaRttMs = ewmaRttMs === null ? netRtt : Math.round(ewmaRttMs * 0.65 + netRtt * 0.35);
    pollErrors = 0; lastSyncTs = Date.now(); clearTimeout(spinTimer); showSpinner = false;

    const doneRefresh = () => { if (refreshing) { refreshing = false; showSynced ? flashMicro(tr("✓ Synced", "✓ 已同步")) : render(); } };
    if (res?.isError) { errorText = tr("Sync error", "刷新失败"); refreshing = false; render(); }
    else if (sc?.unchanged || (sc?.digest && sc.digest === lastDigest && snapshot)) {
      unchangedHits += 1; errorText = "";
      if (sc?.generated_at && snapshot) snapshot.generated_at = sc.generated_at;
      doneRefresh();
    } else if (sc) {
      applySnapshot(sc); loadingInitial = false; errorText = ""; doneRefresh();
    }
  } catch (err) {
    pollErrors += 1; errorText = `${tr("Sync:", "同步：")} ${err?.message || err}`;
    clearTimeout(spinTimer); showSpinner = false; refreshing = false; render();
  } finally {
    inFlightPoll = false;
    queuedForce ? ((queuedForce = false), void refreshSnapshot(true, false)) : scheduleNextPoll();
  }
}

async function withBusy(key, failMsg, fn) {
  loadingKey = key; render();
  try { await fn(); } catch { flashMicro(failMsg); } finally { loadingKey = ""; render(); }
}

async function openTray(cid, tab) {
  if (openTrays.get(cid) === tab) { openTrays.delete(cid); return render(); }
  openTrays.set(cid, tab);
  if (!canCallTools) return render();
  const [m, a] = { turns: ["turns", { conversation_id: cid, limit: 24 }], hotspots: ["hotspots", { conversation_id: cid, mode: "hotspots", top_k: 5 }], advice: ["prepare_handoff", { conversation_id: cid, estimated_remaining_turns: 12 }] }[tab];
  await withBusy(`${tab}:${cid}`, tr("Load failed", "加载失败"), async () => {
    const res = await callTool(`context_radar_${m}`, a);
    if (!res?.isError && res?.structuredContent) { trayMap[tab].set(cid, res.structuredContent); subTrayRev += 1; }
  });
}

async function runDistill(cid) {
  if (!canCallTools || !cid) return;
  const rec = cHot.get(cid)?.recommended_distill_range;
  await withBusy(`distill:${cid}`, tr("Distill failed", "提炼失败"), async () => {
    const args = { action: "distill_slice", conversation_id: cid };
    if (Array.isArray(rec) && rec.length === 2) { args.start_step = rec[0]; args.end_step = rec[1]; }
    const sc = (await uiAct(args))?.structuredContent;
    if (sc?.found) { cDist.set(cid, sc); subTrayRev += 1; flashMicro(tr("✓ Saved to scratch/", "✓ 已写入 scratch/")); }
    else flashMicro(sc?.error || tr("Distill failed", "提炼失败"));
  });
}

async function prepareOrLaunch(cid, launchNow) {
  if (!canCallTools || !cid) return;
  await withBusy(`handoff:${cid}`, tr("Prep failed", "生成失败"), async () => {
    const dist = cDist.get(cid), args = { action: "create_handoff", kind: launchNow ? "handoff" : "compact", conversation_id: cid, auto_fire: false, estimated_remaining_turns: 12 };
    if (dist?.saved_note_path) args.scratch_note_path = dist.saved_note_path;
    const sc = (await uiAct(args))?.structuredContent;
    if (!sc || sc.blocked || sc.mode === "handoff_blocked") return flashMicro(sc?.reasons?.[0] || tr("Handoff gated", "暂缓交接"));
    if (snapshot) snapshot.pending_grant = sc;
    subTrayRev += 1;
    if (launchNow) { loadingKey = ""; await executeRelay(sc, "manual_handoff", false); }
    else await cpCompact(sc.compact_command);
  });
}

function toggleLang() {
  lang = isZh() ? "en" : "zh-CN"; langLocked = true; DE.lang = lang; setPref(lang); subTrayRev += 1; render();
  if (canCallTools) void uiAct({ action: "set_prefs", lang }).catch(() => {});
}

async function toggleAutoHandoff() {
  const next = !allowAutoHandoff; allowAutoHandoff = next; render();
  if (!canCallTools) return;
  try {
    const val = (await uiAct({ action: "set_prefs", allow_auto_handoff: next }))?.structuredContent?.settings?.allow_auto_handoff;
    if (typeof val === "boolean") allowAutoHandoff = val;
    flashMicro(allowAutoHandoff ? tr("✓ Auto handoff on", "✓ 已开启接力") : tr("✓ Auto handoff off", "✓ 已暂停接力"));
    if (allowAutoHandoff && snapshot?.pending_grant) { snapshot.pending_grant.allow_auto_handoff = true; maybeAutoRelay(snapshot.pending_grant, false); }
  } catch { allowAutoHandoff = !next; flashMicro(tr("Save failed", "保存失败")); render(); }
}

const buildReminder = (it) => {
  const lim = num(it.context_limit) || 512_000, rec = Math.round((num(it.recommend_compact_tokens) || 31e4) / 1e3);
  return isZh()
    ? `会话《${it.title}》上下文达 ${fmtK(it.latest_context_tokens)}/${fmtK(lim)}（超 ${rec}k），请收尾或准备 /compact。`
    : `Context for "${it.title}" is ${fmtK(it.latest_context_tokens)}/${fmtK(lim)} (>${rec}k); wrap up or prepare /compact.`;
};

async function sendReminder(it) {
  if (!it || it.is_subagent) return;
  confirmSendId = null;
  const msg = buildReminder(it);
  // Never inject into a potentially different chat in a global or cross-session side pane!
  if (!canDirectMsg(it)) { await copyText(msg, tr(...T_RC)); render(); return; }
  try {
    const res = await bridge.sendMessage({ role: "user", content: [{ type: "text", text: msg }] });
    res?.isError ? await copyText(msg, tr(...T_RC)) : flashMicro(tr("✓ Sent", "✓ 已发送"));
  } catch { await copyText(msg, tr(...T_RC)); }
  render();
}

function setFilterTab(nextTab, focusTab = false) {
  if (filterTab === nextTab) return;
  filterTab = nextTab; lastDigest = ""; render();
  if (focusTab) D.querySelector(`[data-a="tab"][data-i="${nextTab}"]`)?.focus();
  void refreshSnapshot(true, false);
}

D.addEventListener("click", (ev) => {
  const el = ev.target?.closest?.("[data-a]");
  if (!el) return;
  ev.stopPropagation();
  const a = el.dataset.a, id = el.dataset.i || "";
  if (a.startsWith("t-") && id) return void openTray(id, a.slice(2));
  if (a === "copy-id" && id) { ev.preventDefault(); return void copyCid(id); }
  if (a === "lng") return toggleLang();
  if (a === "ref") return void refreshSnapshot(true, true);
  if (a === "aut") { autoRefresh = !autoRefresh; autoRefresh ? scheduleNextPoll() : cancelPoll(); return render(); }
  if (a === "tab" && id) return setFilterTab(id);
  if (a === "aho") return void toggleAutoHandoff();
  if (a === "row" && id) { openRowId = openRowId === id ? null : id; return render(); }
  if (a === "dst" && id) return void runDistill(id);
  if (a === "cdp" && id) { const p = cDist.get(id)?.saved_note_path; return p && void copyText(p, tr("✓ Copied path", "✓ 已复制路径")); }
  if ((a === "up" || a === "ul") && id) return void prepareOrLaunch(id, a === "ul");
  if (a === "cc" || a === "ccc" || a === "cpc") {
    const m = a === "cc" ? "/compact" : (a === "ccc" ? handoffCard : snapshot?.pending_grant)?.compact_command;
    return m && void cpCompact(m);
  }
  if (a === "crm") { const it = findSession(id); return it && void copyText(buildReminder(it), tr(...T_RC)); }
  if (a === "copy-subagent-relay") {
    const it = findSession(id);
    return it && void copyText(isZh() ? `子代理 ${it.agent_name || it.title} (${it.id}) 达 ${fmtK(it.latest_context_tokens)}，请收尾并切新子代理` : `Subagent ${it.agent_name || it.title} (${it.id}) at ${fmtK(it.latest_context_tokens)}; wrap up and spawn fresh subagent`, tr("✓ Copied baton", "✓ 已复制切棒提示"));
  }
  if (a === "fhn" || a === "lph") { const g = a === "fhn" ? handoffCard : snapshot?.pending_grant; return g && void executeRelay(g, "manual_handoff", a === "fhn"); }
  if (a === "dis" && id) { if (snapshot?.pending_grant?.grant_id === id) { delete snapshot.pending_grant; render(); } return canCallTools && void uiAct({ action: "dismiss_grant", grant_id: id }).catch(() => {}); }
  if (a === "ask") { confirmSendId = id; return render(); }
  if (a === "no") { confirmSendId = null; return render(); }
  if (a === "yes") { const it = findSession(id); return it && void sendReminder(it); }
});

D.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    if (confirmSendId || openTrays.size || openRowId) { confirmSendId = null; openTrays.clear(); openRowId = null; render(); }
    return;
  }
  const el = ev.target?.closest?.("[data-a]");
  if (!el) return;
  if ((ev.key === "Enter" || ev.key === " ") && el.dataset.a === "row") {
    ev.preventDefault(); const id = el.dataset.i || ""; openRowId = openRowId === id ? null : id; render();
  } else if ((ev.key === "ArrowLeft" || ev.key === "ArrowRight") && el.dataset.a === "tab") {
    ev.preventDefault(); const o = ["active", "subs", "archived"]; setFilterTab(o[(o.indexOf(filterTab) + (ev.key === "ArrowRight" ? 1 : 2)) % 3], true);
  }
});

D.addEventListener("visibilitychange", () => {
  if (D.hidden) { pausedHidden = true; cancelPoll(); render(); }
  else { pausedHidden = false; if (viewMode === "dashboard" && autoRefresh && canCallTools) Date.now() - lastSyncTs > 2e3 ? void refreshSnapshot(false) : scheduleNextPoll(); render(); }
});

bridge.ontoolinput = (p) => { const c = p?.arguments?.conversation_id; if (typeof c === "string" && c.trim()) ownerCid = c.trim(); if (!snapshot && viewMode === "dashboard") { loadingInitial = true; render(); } };

bridge.ontoolresult = (result) => {
  if (result?.isError) { errorText = tr("Tool error; please refresh", "工具错误，请刷新"); loadingInitial = false; render(); return; }
  const sc = result?.structuredContent;
  if (sc && (sc.mode === "handoff_card" || sc.mode === "handoff_blocked")) {
    viewMode = "handoff_card"; handoffCard = sc;
    if (sc.conversation_id) ownerCid = sc.conversation_id;
    if (!langLocked && sc.lang) { const p = parseLoc(sc.lang); if (p) { lang = p; DE.lang = lang; } }
    if (typeof sc.allow_auto_handoff === "boolean") allowAutoHandoff = sc.allow_auto_handoff;
    handoffStatus = sc.mode === "handoff_blocked" ? "blocked" : sc.allow_auto_handoff && sc.auto_fire !== false && !sc.running_subagent_count ? "pending" : "manual";
    loadingInitial = false; cancelPoll(); render();
    if (sc.mode === "handoff_card") maybeAutoRelay(sc, true);
    return;
  }
  if (sc?.mode === "snapshot") {
    viewMode = "dashboard"; loadingInitial = false; errorText = ""; lastSyncTs = Date.now();
    if (sc.server_ms !== undefined) serverMs = Math.max(0, Math.round(num(sc.server_ms)));
    scheduleNextPoll();
    // Strictly skip DOM mutation when digest matches SSR boot or previous snapshot!
    if (sc.unchanged || (sc.digest && sc.digest === lastDigest && snapshot)) return;
    applySnapshot(sc); render();
  }
};

function parseColorLum(raw) {
  const s = String(raw || "").trim(), p = (x) => parseInt(x, 16);
  if (!s || /^transparent$/i.test(s)) return null;
  const h = s.match(/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i)?.[1];
  if (h) {
    const k = h.length;
    if ((k === 4 && p(h[3] + h[3]) < 128) || (k === 8 && p(h.slice(6)) < 128)) return null;
    const [r, g, b] = k <= 4
      ? [p(h[0] + h[0]), p(h[1] + h[1]), p(h[2] + h[2])]
      : [p(h.slice(0, 2)), p(h.slice(2, 4)), p(h.slice(4, 6))];
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

function syncHostContext(ctx) {
  const st = ctx?.styles;
  if (st?.variables) applyHostStyleVariables(st.variables);
  if (st?.css?.fonts) applyHostFonts(st.css.fonts);
  if (!langLocked && ctx?.locale) { const h = parseLoc(ctx.locale); if (h && h !== lang) { lang = h; DE.lang = lang; render(); } }
  let res = null;
  const raw = String(ctx?.theme || "").trim().toLowerCase();
  if (raw.includes("dark") || raw.includes("night") || raw.includes("black")) res = "dark";
  else if (raw.includes("light") || raw.includes("day") || raw.includes("white")) res = "light";
  if (!res) {
    const lum = parseColorLum(st?.variables?.["--color-background-primary"]) ?? parseColorLum(getComputedStyle(DE).getPropertyValue("--color-background-primary"));
    if (lum !== null) res = lum < 0.45 ? "dark" : "light";
  }
  if (res) applyDocumentTheme(res);
}

bridge.onhostcontextchanged = (ctx) => syncHostContext(ctx);
bridge.onteardown = () => { cancelPoll(); return {}; };

async function bootPanel() {
  if (snapshot) applySnapshot(snapshot);
  render();
  try {
    await bridge.connect();
    syncHostContext(bridge.getHostContext());
    const caps = bridge.getHostCapabilities();
    canMessage = Boolean(caps?.message);
    canCallTools = caps?.serverTools !== undefined ? Boolean(caps.serverTools) : true;
    DE.dataset.canMsg = canMessage ? "1" : "0"; DE.dataset.ct = canCallTools ? "1" : "0";
    render(); scheduleNextPoll();
    if (viewMode === "handoff_card" && handoffCard && !handoffCard.blocked) maybeAutoRelay(handoffCard, true);
    else if (snapshot?.pending_grant) maybeAutoRelay(snapshot.pending_grant, false);
    if (viewMode === "dashboard" && canCallTools) {
      if (!snapshot) setTimeout(() => { if (!snapshot && !inFlightPoll) void refreshSnapshot(true); }, 250);
      else if (boot?.stale) setTimeout(() => { if (!inFlightPoll && !D.hidden) void refreshSnapshot(false); }, 380);
    }
  } catch (err) {
    if (!snapshot) { errorText = `${tr("Connect failed:", "连接失败：")} ${err?.message || err}`; loadingInitial = false; render(); }
  }
}

void bootPanel();
