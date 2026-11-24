import { McpAppBridge, applyDocumentTheme, applyHostFonts, applyHostStyleVariables } from "./bridge.js";

const D = document, DE = D.documentElement, M = Math, st = setTimeout, ct = clearTimeout, now = Date.now, S = String, fc = (e) => e.firstElementChild, lc = (e) => e.lastElementChild, ch = (e) => e.children, onD = (e, f, o) => D.addEventListener(e, f, o);
const bridge = new McpAppBridge({ name: "context-radar-mcp", version: "1.0.0" }, {});
const boot = typeof window !== "undefined" ? window.__CONTEXT_RADAR_BOOT__ : null;
const BRAND_SVG = `<svg class="mk" viewBox="0 0 24 24" fill="none"><circle cx="12" cy="12" r="9" stroke="currentColor" opacity=".24" stroke-width="1.6"/><path d="M5.6 18.4A9 9 0 1 1 18.7 6M12 12l6.2-6.2" stroke="var(--ac)" stroke-width="2" stroke-linecap="round"/><circle cx="16.5" cy="9.6" r="1.7" fill="var(--ok)"/></svg>`;
const REFRESH_SVG = `<svg width="12" height="12" viewBox="0 0 16 16" fill="none"><path d="M13.5 8a5.5 5.5 0 1 1-1.4-3.7M13.5 2.5v2.7h-2.7" stroke="currentColor" stroke-width="1.6"/></svg>`;

const ls0 = (v) => { try { const s = localStorage; return v ? s.setItem("cr_lang", v) : s.getItem("cr_lang"); } catch { return ""; } };
const getPref = () => { const v = ls0(); return v === "en" || v === "zh-CN" ? v : ""; };
const parseLoc = (r) => { const s = S(r || "").trim().toLowerCase(); return !s || s === "auto" ? "" : s.startsWith("zh") ? "zh-CN" : "en"; };

let langLocked = !!(getPref() || boot?.settings?.lang);
let lang = getPref() || parseLoc(boot?.settings?.lang) || parseLoc(typeof navigator !== "undefined" ? navigator.language : "") || parseLoc(boot?.lang) || "en";
DE.lang = lang;

const isZh = () => lang === "zh-CN", tr = (e, z) => z !== undefined ? (isZh() ? z : e) : e.split("|")[isZh() ? 1 : 0];
const subTxt = (n) => (isZh() ? `${n} 个子代理` : `${n} subagent${n === 1 ? "" : "s"}`);
const langBtn = (en) => bt0("lg", "lng", `${sp(`lo${en ? " on" : ""}`, "EN")}${sp(`lo${!en ? " on" : ""}`, "中文")}`, ' aria-label="Language"');

const CM = "cr-cap-msg", HC = "handoff_card", HB = "handoff_blocked", DB = "dashboard", AG_M = "com.google.antigravity/message", BG_V = "--color-background-primary";
const K_CID = "conversation_id", K_HCID = "handoff_conversation_id", K_AAH = "allow_auto_handoff", K_RSC = "running_subagent_count", K_LCT = "latest_context_tokens";
const K_CC = "compact_command", K_CS = "compact_status", K_JC = "just_compacted", K_SC = "suggest_compact", K_IS = "is_subagent";
const K_MCT = "min_compact_tokens", K_RCT = "recommend_compact_tokens", K_UCT = "urgent_compact_tokens", K_SC2 = "structuredContent", K_GID = "grant_id", K_WS = "workspace", K_WSS = "workspaces";
const K_CT = "context_tokens", K_CR = "cache_hit_rate", K_PP = "preserve_points", K_RT = "resume_task", K_PK = "pricing_known", K_SN = "saved_note_path", K_TN = "turns";
const DEF_T = [27e4, 31e4, 42e4], thrs = (it) => [it[K_MCT], it[K_RCT], it[K_UCT]];
const T_LD = "Loading…|加载中…", T_OK = "✓ Copied|✓ 已复制", T_CP = "Copy /compact|复制 /compact", T_KP = "Keep warm prefix|保持热前缀";
const T_RD = "Ready|已就绪", T_LB = "Launching…|拉起中…", T_AH = "Auto handoff|自动接力";
const T_AHT = "Auto-launch with 0 subagents|0 子代理时自动拉起新会话";
const T_RC = "✓ Copied reminder|✓ 已复制提醒", T_WS = ["Wait for active subagents", "存在活跃子代理"];
const T_CM = "Compacting…|压缩中…", T_CI = "Copy ID|复制 ID", T_LN = "Launch New Chat|拉起新会话", T_HG = "Handoff gated|暂缓交接", T_US = "Used|已使用";
const T_NS = "New chat started|已拉起新会话", T_CS = "Compact sent|已发 /compact", T_CR = "Compact ready|压缩已就绪";

let viewMode = boot?.view === "handoff" ? HC : DB;
let snapshot = boot?.mode === "snapshot" ? boot : null, lastDigest = boot?.digest || "";
let handoffCard = null, handoffStatus = "pending", loadingInitial = !(boot?.mode === "snapshot");
let refreshing = false, autoRefresh = true, filterTab = "active", allowAutoHandoff = boot?.settings?.[K_AAH] ?? true;
let busyGrantId = null, errorText = "", canMessage = false, ownerCid = "", copiedId = null, confirmSendId = null, microMsg = "";
let ewmaRttMs = null, serverMs = boot?.server_ms ?? null, pollMs = 1e3, unchangedHits = 0, bgHidden = false, pollErrors = 0;
let lastSyncTs = boot?.mode === "snapshot" ? now() : 0, lastForceTs = 0, spinTimer = null, showSpinner = false;
let openRowId = null, loadingKey = "", subTrayRev = 0;
const openTrays = new Map(), cTurns = new Map(), cHot = new Map(), cDist = new Map(), cAdv = new Map(), attemptedGrants = new Set();
const trayMap = { turns: cTurns, hotspots: cHot, advice: cAdv };
let canCallTools = true, inFlightPoll = false, queuedForce = false, pollTimer = null, microTimer = null, copiedTimer = null, autoCmpTimer = null;
const pg = () => snapshot?.pending_grant;
const cancelAutoCmp = () => { if (autoCmpTimer !== null) { ct(autoCmpTimer); autoCmpTimer = null; } };

// Persistent DOM mount state (mount ONCE, patch strictly in place — zero full-shell, hero, or row-tray wipes!)
let mountedMode = "", ui = null, lastPendingSig = "", lastHandoffSig = "";
const rowNodes = new Map();
let uMi, uMt, uEn, uZh, uRb, uRi, uEr, uCw, uPg, uHd, uHt, uHi, uHm, uHp, uHs, uHv, uHdBlk, uBr, uSg, uTa, uTs, uTr, uAb, uSw, uSwL, uLs, uEm, uHl;

const callTool = (name, args, opts) => bridge.callServerTool({ name, arguments: args }, opts);
const uiAct = (args) => callTool("context_radar_ui_action", args);
const uiSc = async (a) => (await uiAct(a))?.[K_SC2];
const sendTxt = (text, meta) => bridge.sendMessage(meta ? { role: "user", content: [{ type: "text", text }], _meta: meta } : { role: "user", content: [{ type: "text", text }] });
const num = (v) => Number(v) || 0, fx = (n, d = 0) => num(n).toFixed(d);
const esc = (v) => S(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const ti = (s) => s ? ` title="${esc(s)}"` : "";
const safeLvl = (l) => (l === "critical" || l === "warn" ? l : "ok");
const lvlCls = (l) => (l === "critical" ? "lc" : l === "warn" ? "lw" : "");
const dv = (c, s = "", h = 0) => `<div${c ? ` class="${c}"` : ""}${h ? " hidden" : ""}>${s}</div>`;
const sp = (c, s = "", ex = "") => `<span class="${c}"${ex}>${s}</span>`;
const shd = (c) => sp(c, "", " hidden");
const nt = (s, c = "") => dv(`nt${c}`, s);
const bt0 = (c, a, tx = "", ex = "") => `<button class="${c}" data-a="${a}"${ex}>${tx}</button>`;
const btn = (cls, act, txt, id = "", ex = "") => bt0(`bt${cls ? ` ${cls}` : ""}`, act, txt, `${id ? ` data-i="${esc(id)}"` : ""}${ex}`);
const bdg = (cls, txt) => sp(`bd ${cls}`, txt);
const bx = (inner, cls = "") => dv(`bx${cls}`, inner);
const sw = (lbl = "") => bt0(`csw ${CM}${allowAutoHandoff ? " on" : ""}`, "aho", `<span class="tk"><span class="th"></span></span><span>${lbl}</span>`, ` role="switch" aria-checked="${allowAutoHandoff}"${ti(tr(T_AHT))}`);

const fmtK = (n) => { const x = num(n); return x >= 1e6 ? `${fx(x / 1e6, 2)}M` : x >= 1e3 ? `${fx(x / 1e3, 1)}k` : S(x); };
const fmtMs = (ms) => { const v = num(ms); return !v ? "-" : v >= 1e3 ? `${fx(v / 1e3, 1)}s` : `${v}ms`; };
const fmtClock = (iso) => { if (!iso) return "-"; const d = new Date(iso); return Number.isNaN(d.getTime()) ? "-" : d.toLocaleTimeString(isZh() ? "zh-CN" : "en-US", { hour12: false }); };
const fmtAge = (ts) => {
  if (!ts) return tr("idle|空闲");
  const d = M.max(0, M.floor(now() / 1e3 - ts));
  const [n, u] = d < 60 ? [d, tr("s|秒前")] : d < 3600 ? [M.floor(d / 60), tr("m|分前")] : d < 86400 ? [M.floor(d / 3600), tr("h|小时前")] : [M.floor(d / 86400), tr("d|天前")];
  return `${n}${u}`;
};
const fmtMoney = (usd, cny, dec = 2, known = true) => {
  if (known === false && !num(usd) && !num(cny)) return tr("Unpriced|未定价");
  if (snapshot?.currency_explicit) return `${snapshot.currency_symbol || "$"}${(S(snapshot.currency_code || "").toUpperCase() === "USD" ? fx(usd, dec) : fx(cny, dec))}`;
  return isZh() ? `¥${fx(cny, dec)}` : `$${fx(usd, dec)}`;
};

const normWs = (r) => { const s = String(r || "").trim().replace(/^file:\/\//, ""); return s.length > 1 ? s.replace(/\/+$/, "") : s; };
const sameWs = (ws, allWs) => {
  const dws = normWs(snapshot?.daemon_workspace);
  return !dws || [ws, ...(Array.isArray(allWs) ? allWs : [])].map(normWs).filter(Boolean).some((w) => w === dws || w.startsWith(`${dws}/`) || dws.startsWith(`${w}/`));
};
const getHero = () => { const m = snapshot?.sessions || []; return m.find((x) => x.is_active) || m[0] || null; };
const canDirectCid = (cid, ws, allWs) => {
  if (!canMessage || !cid) return false;
  if (ownerCid) return ownerCid === cid;
  const hero = getHero();
  return !!(hero && hero.id === cid && sameWs(ws, allWs) && num(snapshot?.summary?.running_main_count) <= 1);
};
const canDirectMsg = (it) => !!(it && !it.is_subagent && canDirectCid(it.id, it.workspace, it.workspaces));
const getCold = ({ cold_cache_breakdown: c, mid_loop_breakdown: mb, turn_start_cold_count: t, mid_loop_cold_count: m, post_compact_cold_count: p }) =>
  ({ t: num(c?.turn_start_cold ?? t), m: num(c?.mid_loop_cold ?? m), p: num(c?.post_compact_cold ?? p), s: num(mb?.slow_turn_self_expiry), r: num(mb?.replica_spillover), w: num(mb?.model_switch_cold) });

const getVerdict = (it) => {
  const lvl = safeLvl(it.level);
  if (it[K_JC]) return { cls: "info", text: tr("Compacted|已压缩") };
  if (it[K_SC] || lvl === "critical") return { cls: lvl === "critical" ? "critical" : "warn", text: tr("Handoff ready|建议交接") };
  const ctx = num(it[K_LCT]);
  if (ctx >= (num(it[K_MCT]) || 27e4) * 0.72 || (ctx >= 12e4 && getCold(it).m > 0)) return { cls: "warn", text: tr("Slice ready|可提炼切片") };
  return { cls: "ok", text: "" };
};

const DOT_CLS = { running: "run", waiting: "wait", error: "err", ready: "idle" };
const getLiveDot = (it) => DOT_CLS[it?.live_status] || (it?.is_active ? "run" : "idle");
const getLivePhaseBadge = (it) => {
  const s = it?.live_status, p = it?.live_phase, sec = M.max(0, M.round(num(it?.live_elapsed_sec))), el = sec > 0 ? ` ${sec}s` : "";
  if (p === "compacting") return { cls: s === "waiting" ? "warn" : "info", text: `${tr(T_CM)}${el}` };
  if (s === "waiting") return { cls: "warn", text: `${tr("Waiting|等待")}${el}` };
  if (s === "error") return { cls: "critical", text: tr("Error|异常") };
  return null;
};

const renderIdPill = (id, compact = false) => {
  if (!id) return "";
  const ok = copiedId === id;
  return `<button class="id${ok ? " ok" : ""}" data-a="ci" data-i="${esc(id)}"${ti(id)}>${ok ? tr(T_OK) : compact ? tr(T_CI) : esc(id.length > 12 ? `${id.slice(0, 6)}…${id.slice(-4)}` : id)}</button>`;
};

const SUB_L = { slow_turn_self_expiry: ["Slow-turn TTL", "慢轮自过期"], replica_spillover: ["Replica spillover", "副本溢出"], model_switch_cold: ["Model switch", "切换模型"] };
const coldKind = (t) => {
  const k = t.cold_cache_kind, sr = t.cold_cache_sub_reason;
  return k === "mid_loop_cold" ? (SUB_L[sr] ? tr(...SUB_L[sr]) : t.is_partial_cold ? tr("Partial drop", "前缀跌落") : tr("Mid-turn", "中途断缓")) : k === "post_compact_cold" ? tr("Post-compact", "压缩后") : tr("New turn", "新轮次");
};
const COLD_CLS = { mid_loop_cold: "mid", post_compact_cold: "post" };

const renderSubTray = (it) => {
  const cid = it.id, tab = openTrays.get(cid), pk = it[K_PK], fm = (u, c, d0 = 2) => fmtMoney(u, c, d0, pk);
  if (!tab) return "";
  if (loadingKey === `${tab}:${cid}`) return bx(sp("nt", tr(T_LD)));
  if (tab === K_TN) {
    const d = cTurns.get(cid), ts0 = d?.[K_TN];
    if (!ts0?.length) return bx(sp("nt", tr("No turns yet|暂无轮次")));
    const chrono = ts0.slice(0, 24).reverse(), mx = M.max(1, ...chrono.map((t) => num(t[K_CT])));
    const tLbl = (t) => { const k = num(t.thinking_tokens), r = num(t.response_tokens); return k > 0 ? (r > 0 ? `T${M.round((k / (k + r)) * 100)}%` : `think ${fmtK(k)}`) : ""; };
    const bars = chrono.map((t) => {
      const th = tLbl(t);
      return `<span class="sr ${t.is_checkpoint ? "k0" : t.is_cold_cache ? "c0" : ""}" style="height:${M.max(12, M.round((num(t[K_CT]) / mx) * 100))}%"${ti(`#${num(t.turn)} · ${fmtK(t[K_CT])}${th ? ` · ${th}` : ""} · ${tr("Cache|缓存")} ${fx(t[K_CR])}%${t.is_cold_cache ? ` (${coldKind(t)})` : ""}`)}></span>`;
    }).join("");
    const cells = (g, a) => a.map((v) => `<${g}>${v}</${g}>`).join("");
    const rows = ts0.slice(0, 10).map((t) => {
      const ck = t.is_checkpoint, hr = fx(t[K_CR]), ct0 = fmtK(t[K_CT]), tn = num(t.turn);
      const cCell = !t.is_cold_cache ? `${hr}%` : `${sp("ch", `${hr}%`)} ${sp(`ct ${COLD_CLS[t.cold_cache_kind] || "turn"}`, coldKind(t), ti(t.cold_cache_reason))}`;
      const tl = tLbl(t), st0 = num(t.stream_tps), e20 = num(t.e2e_tps), tt0 = fmtMs(t.ttft_ms);
      const spd = st0 > 0 ? `${tt0}${e20 > 0 ? ` · ${fx(st0)}/${fx(e20)}` : `/${fx(st0)}`}` : `${tt0}/-`;
      return `<tr>${cells("td", [ck ? `#${tn}·${tr("Compact|压缩")}` : `#${tn}`, fmtClock(t.time_iso), `<b>${ct0}</b>${tl ? ` ${sp("ag", tl)}` : ""}`, cCell, ck ? tr("Ckpt|检查点") : spd, fm(t.cost_usd, t.cost_cny)])}</tr>`;
    }).join("");
    const ths = cells("th", [tr("Turn|轮次"), tr("Time|时间"), tr("Ctx|上下文"), tr("Cache|缓存"), "TTFT/s", tr("Cost|费用")]);
    return bx(`${nt(tr(`Recent ${chrono.length}/${num(d.total_turns)}`, `最近 ${chrono.length}/${num(d.total_turns)} 轮`))}${dv("sk", bars)}${dv("ts", `<table class="tb"><thead><tr>${ths}</tr></thead><tbody>${rows}</tbody></table>`)}`);
  }
  if (tab === "hotspots") {
    const h = cHot.get(cid);
    if (!h?.found) return bx(sp("nt", esc(h?.error || tr("No hotspots|暂无热点"))));
    const list = (h.hotspots || []).slice(0, 5), rec = h.recommended_distill_range, spath = cDist.get(cid)?.[K_SN], busy = loadingKey === `distill:${cid}`;
    const rows = list.length ? list.map((x) => nt(`· <b>#${num(x.turn)}</b>(s${num(x.step_idx)}) <b class="ch">+${fmtK(x.delta_tokens)}</b>→${fmtK(x.context_after)} · ${esc(x.cause)}`, " wr")).join("") : nt(tr("No spikes.|无激增。"));
    const saved = spath ? nt(`✓ ${tr("Saved:|已写入")} <code>${esc(spath)}</code>`, " wr ok1") : "";
    const lbl = busy ? tr("Saving…|提炼中…") : rec ? tr(`Save ${rec[0]}–${rec[1]}`, `提炼 ${rec[0]}–${rec[1]}`) : tr("Save slice|提炼切片");
    return bx(`${nt(`<b>${tr("Hotspots|膨胀热点")}</b>`)}${rows}${saved}${dv("ac ct0", `${btn("p", "dst", lbl, cid, busy ? " disabled" : "")}${spath ? btn("", "cdp", tr("Copy path|复制路径"), cid) : ""}`)}`);
  }
  if (tab === "advice") {
    const p = cAdv.get(cid);
    if (!p?.found) return bx(sp("nt", esc(p?.reason || tr("Unable to evaluate|暂无法评估"))), " ac");
    const ok = p.gate_allowed, runSubs = num(it[K_RSC]), subBdg = runSubs > 0 ? bdg("warn", subTxt(runSubs)) : bdg("ok", tr("0 subagents|无子代理"));
    const k = (v, d) => M.round((num(v) || d) / 1e3), f1 = (u, c, d) => fmtMoney(u, c, d, 1);
    const tier = `${tr("Thresholds|阈值")} ${thrs(it).map((v, i) => `${k(v, DEF_T[i])}k`).join("/")} · ${tr("break-even|回本")} >=${num(p.break_even_turns)}${tr("t|轮")}`;
    const econ = p[K_PK] !== false ? ` · ${tr("Rebuild|重建")} ${f1(p.compact_overhead_usd, p.compact_overhead_cny, 2)} · ${tr("Saves|省")} ${f1(p.per_turn_saving_usd, p.per_turn_saving_cny, 3)}/t` : "";
    const canLaunch = ok && canMessage && !runSubs && sameWs(it[K_WS], it[K_WSS]), busy = loadingKey === `handoff:${cid}`;
    return bx(`${dv("hh", dv("bds", `${bdg(ok ? "ok" : "warn", ok ? tr("Ready for handoff|满足交接条件") : tr(T_KP))}${subBdg}`))}${nt(esc(tier + econ))}${nt(esc(p.headline), " wr t1")}${(p.gate_reasons || []).map((r) => nt(`· ${esc(r)}`, " wr")).join("")}${dv("ac", `${ok ? btn("", "up", busy ? tr("Preparing…|生成中…") : tr("Prepare /compact|生成 /compact"), cid, busy ? " disabled" : "") : btn("", "cc", tr(T_CP))}${canLaunch ? btn(`p ${CM}`, "ul", tr(T_LN), cid, busy ? " disabled" : "") : ""}${sw(tr(T_AH))}`)}`, " ac");
  }
  return "";
};

const renderExtraActions = (it) => {
  const cid = it.id, lvl = safeLvl(it.level);
  if (!((it[K_SC] || lvl === "critical") && !it[K_JC])) return "";
  if (it[K_IS]) return btn(lvl === "critical" ? "c" : "w", "csr", tr("Copy baton|复制切棒"), cid);
  const cpBtn = btn(lvl === "critical" ? "c" : "w", "cc", tr(T_CP));
  if (!canDirectMsg(it)) return cpBtn + btn("", "crm", tr("Copy reminder|复制提醒"), cid);
  return cpBtn + (confirmSendId === cid
    ? btn(`c ${CM}`, "yes", tr("Confirm|确认"), cid) + btn(CM, "no", tr("Cancel|取消"))
    : btn(CM, "ask", tr("Remind|提醒"), cid));
};

const ce = (t) => D.createElement(t), mk = (c, h) => { const e = ce("div"); e.className = c; ih(e, h); return e; };
const TABS = [K_TN, "hotspots", "advice"];
const IDB = bt0("id", "ci");
const mountDetailBlock = () => {
  const pl = dv("pl", `${sp("pll")}${sp("pv")}${sp("ps")}`).repeat(3);
  const bts = TABS.map((t, i) => bt0(`bt${i === 2 ? " p" : ""} ct0`, `t-${t}`)).join("");
  const el = mk("dl", `${dv("gg", `<div class="gt" role="progressbar" aria-valuemin="0" aria-valuemax="100">${dv("gf ok", "")}${sp("gk").repeat(2)}${sp("gk u")}</div>${dv("gl", "<span></span><span></span>")}`)}${dv("pls", pl)}${dv("nt wr", "", 1)}${dv("", "", 1)}${dv("ac", `${bts}${IDB}${shd("ac")}`)}${dv("", "", 1)}`);
  const [gg, pls, tmEl, wnEl, acts, trEl] = ch(el), gt = fc(gg);
  const [gf, t1, t2, t3] = ch(gt), [g1, g2] = ch(lc(gg)), [bt, bh, ba, bi, ex] = ch(acts);
  return [el, gt, gf, [t1, t2, t3], g1, g2, [...ch(pls)], [bt, bh, ba], bi, ex, trEl, "", "", tmEl, wnEl];
}

const setCls = (el, c) => { if (el && el.className !== c) el.className = c; };
const setTxt = (el, v) => { const s = S(v ?? ""); if (el && el.textContent !== s) el.textContent = s; };
const sa = (e, k, v) => e.setAttribute(k, v), al = (e, v) => sa(e, "aria-label", v), hd = (e, b) => { e.hidden = !!b; }, ih = (e, s) => { e.innerHTML = s; };
const cl = (e, c, b) => e.classList.toggle(c, b);
const patchIdBtn = (el, cid, show) => {
  if (!show || !cid) return hd(el, 1);
  const ok = copiedId === cid; hd(el, 0); el.dataset.i = cid;
  setCls(el, `id${ok ? " ok" : ""}`); setTxt(el, ok ? tr(T_OK) : tr(T_CI)); el.title = cid;
};
const patchBadge = (el, cls, txt) => { hd(el, !txt); if (txt) { setCls(el, `bd ${cls}`); setTxt(el, txt); } };
const setPill = (p, lbl, v1, v2, cls, tip) => {
  const [l, a, b] = ch(p); setTxt(l, lbl); setTxt(a, v1); setTxt(b, v2);
  if (cls) b.className = cls; if (tip || p.title) p.title = tip;
};

const jDot = (a, b) => (a && b ? `${a} · ${b}` : a || b);
const patchDetailBlock = (d, it, showCopyId) => {
  const cid = it.id || "", lvl = safeLvl(it.level), runSubs = num(it[K_RSC]), pk = it[K_PK], fm = (u, c) => fmtMoney(u, c, 2, pk);
  const lim = num(it.context_limit || snapshot?.thresholds?.max) || 512e3, ctx = num(it[K_LCT]), ckc = num(it.checkpoint_count);
  const pct = M.min(100, M.max(0, (ctx / lim) * 100)), cpct = fx(it.context_pct ?? pct, 1);
  const rem = num(it.remaining_context_tokens ?? M.max(0, lim - ctx));
  const [, gt, gf, ts, g1, g2, ps, bs, bi, ex, trEl, , , tmEl, wnEl] = d;
  al(gt, tr("Context usage|上下文占用")); sa(gt, "aria-valuenow", S(M.round(pct)));
  gf.className = `gf ${lvl}`; gf.style.width = `${pct.toFixed(1)}%`;
  thrs(it).forEach((v, i) => { const n = num(v) || DEF_T[i]; ts[i].style.left = `${M.min(100, (n / lim) * 100).toFixed(1)}%`; ts[i].title = `${tr(["Floor|下限", "Target|推荐", "Urgent|紧急"][i])} ${M.round(n / 1e3)}k`; });
  setTxt(g1, `${fmtK(ctx)} / ${fmtK(lim)} (${cpct}%) · ${isZh() ? `余 ${fmtK(rem)}` : `${fmtK(rem)} left`}`);
  const jc = !!it[K_JC], kb = jc && num(it.compact_summary_kb) > 0 ? `~${fx(it.compact_summary_kb, 1)} KB` : "", pc = it.pre_compact_tokens;
  setTxt(g2, jc && (pc || kb) ? jDot(pc ? `${tr("Before|压缩前")} ${fmtK(pc)}` : "", kb) : isZh() ? `${num(it[K_TN])} 轮` : `${num(it[K_TN])}t`);

  const c = getCold(it), cN = num(it.cold_cache_count);
  const ttft = num(it.latest_ttft_ms) || num(it.median_ttft_ms), stps = num(it.latest_stream_tps) || num(it.median_stream_tps), etps = num(it.e2e_tps);
  const s0 = fx(stps), e0 = fx(etps), spd = stps > 0 ? (etps > 0 ? `${s0}/${e0} t/s` : `${s0} t/s`) : "-";
  const midSub = c.m > 0 && (c.s || c.r || c.w) ? (isZh() ? `（慢轮${c.s}/溢出${c.r}/切模${c.w}）` : ` (Slow ${c.s}/Spill ${c.r}/Switch ${c.w})`) : "";
  const coldTip = cN ? (isZh() ? `新轮次 ${c.t} · 中途 ${c.m}${midSub} · 压缩后 ${c.p}` : `New ${c.t} · Mid ${c.m}${midSub} · Compact ${c.p}`) : "";
  const sTip = jDot(stps > 0 && etps > 0 ? `${tr("Stream|流式")} ${s0} · E2E ${e0} t/s` : "", ckc ? (isZh() ? `压缩 ${ckc} 次` : `Compacted ${ckc}x`) : "");
  setPill(ps[0], tr("Context|上下文"), `${fmtK(ctx)} · ${cpct}%`, `${tr("Peak|峰值")} ${fmtK(it.peak_context_tokens || ctx)}`, "", "");
  setPill(ps[1], tr("KV Cache|KV 缓存"), `${fx(it[K_CR], 1)}%`, cN ? (isZh() ? `冷未命中 ${cN} 次` : `${cN} cold miss${cN === 1 ? "" : "es"}`) : tr("0 cold misses|无冷未命中"), `ps${cN && c.m > 0 ? " ch" : ""}`, coldTip);
  setPill(ps[2], tr("Speed & Cost|速度与费用"), `TTFT ${fmtMs(ttft)} · ${spd}`, fm(it.cost_usd, it.cost_cny), "", sTip);

  const thk = num(it.total_thinking_tokens), outT = thk + num(it.total_response_tokens), ls = num(it.last_loop_steps);
  const thkP = M.round(num(it.thinking_share_pct ?? (outT > 0 ? (thk / outT) * 100 : 0)));
  const outPart = thk > 0 ? (isZh() ? `输出 ${fmtK(outT)}（思考 ${thkP}%）` : `Out ${fmtK(outT)} (${thkP}% think)`) : "";
  const loopOut = num(it.last_loop_output_tokens) || num(it.last_loop_thinking_tokens), lCost = fm(it.last_loop_cost_usd, it.last_loop_cost_cny);
  const loopPart = ls > 0 ? (isZh() ? `本轮 ${ls} 步 · ${fmtK(loopOut)} · ${lCost}` : `Loop ${ls}× · ${fmtK(loopOut)} · ${lCost}`) : "";
  const metaTxt = jDot(outPart, loopPart);
  hd(tmEl, !metaTxt); if (metaTxt) setTxt(tmEl, metaTxt);

  const errMsg = S(it.live_error || "").trim();
  const noteMsg = jDot(errMsg, it.cross_model_compact ? tr("Cross-model /compact cannot reuse prior KV cache; slower rebuild expected.|跨模型 /compact 无法复用原 KV 缓存，全量重算耗时较长属正常。") : "");
  hd(wnEl, !noteMsg); if (noteMsg) { setCls(wnEl, `${errMsg ? "er" : "cw"} wr`); setTxt(wnEl, noteMsg); }

  const cur = openTrays.get(cid) || "", tLbls = [tr("Turns|轮次"), tr("Hotspots|热点"), tr("Handoff|交接")];
  for (let i = 0; i < 3; i++) { setTxt(bs[i], tLbls[i]); bs[i].dataset.i = cid; cl(bs[i], "on", cur === TABS[i]); }
  patchIdBtn(bi, cid, showCopyId);

  const exSig = `${lang}|${cid}|${lvl}|${!!it[K_SC]}|${jc}|${!!it[K_IS]}|${canDirectMsg(it)}|${confirmSendId === cid}`;
  if (exSig !== d[11]) { d[11] = exSig; const h = renderExtraActions(it); hd(ex, !h); ih(ex, h); }

  const traySig = cur ? `${lang}|${cid}|${cur}|${loadingKey}|${subTrayRev}|${runSubs}|${allowAutoHandoff}|${canMessage}` : "";
  if (traySig !== d[12]) { d[12] = traySig; hd(trEl, !cur); ih(trEl, cur ? renderSubTray(it) : ""); }
};

const ensureDashboardMount = (root) => {
  if (mountedMode === DB && ui) return;
  rowNodes.clear(); lastPendingSig = ""; mountedMode = DB;
  const tabs = ["active", "subs", "archived"].map((t) => bt0("sb", "tab", "", ` role="tab" data-i="${t}"`)).join("");
  ih(root, dv("sh", `<header class="ms">${dv("bd0", `${BRAND_SVG}<h1>Context Radar</h1>`)}${dv("mr", `${shd("mi")}${sp("mt")}${langBtn(true)}${bt0("ib ct0", "ref", REFRESH_SVG, ' aria-label="Refresh"')}`)}</header>${dv("er", "", 1)}${dv("cw", "", 1)}${dv("", "", 1)}<section class="hr" hidden>${dv("hh", `${dv("hm", `${sp("dt idle")}${sp("tt")}${IDB}`)}${dv("bds", `${sp("bd muted")}${shd("bd")}${shd("bd")}${shd("bd")}`)}`)}</section>${dv("br", `<div class="sg ct0" role="tablist">${tabs}</div>${dv("ac", `${btn("ct0", "aut", "")}${sw()}`)}`, 1)}${dv("ls", "", 1)}${dv("em", "", 1)}`));
  let ms, he, lgBtn;
  [ms, uEr, uCw, uPg, he, uBr, uLs, uEm] = ch(fc(root));
  [uMi, uMt, lgBtn, uRb] = ch(lc(ms)); [uEn, uZh] = ch(lgBtn); uRi = fc(uRb);
  const hh = fc(he); [uHd, uHt, uHi] = ch(fc(hh)); [uHm, uHp, uHs, uHv] = ch(lc(hh));
  uHdBlk = mountDetailBlock(); he.appendChild(uHdBlk[0]);
  uSg = fc(uBr); [uTa, uTs, uTr] = ch(uSg);
  [uAb, uSw] = ch(lc(uBr)); uSwL = lc(uSw);
  ui = { he };
};

const ensureHandoffMount = (root) => {
  if (mountedMode === HC && ui) return;
  rowNodes.clear(); lastHandoffSig = ""; mountedMode = HC;
  ih(root, dv("sh inl", "<div></div>"));
  uHl = fc(fc(root));
  ui = { he: null };
};

const renderRelayCard = (cls, active, title, g, rightHtml, acts, extra = "", slice = "") => {
  const cid = g[K_CID], newCid = g[K_HCID] || "", pr = g[K_PP], nx = g[K_RT];
  return dv(cls, `${dv("hh", `${dv("hm", `${sp(`dt ${active ? "run" : "idle"}`)}${sp("tt", esc(title))}${cid ? renderIdPill(cid, true) : ""}`)}${dv("bds", rightHtml)}`)}${newCid ? nt(`${tr("New chat:|新会话：")} ${renderIdPill(newCid)}`) : ""}${pr ? nt(`${tr("Preserve:|保留：")} ${esc(pr)}`, " wr") : ""}${nx ? nt(`${tr("Next:|续跑：")} ${esc(nx)}`, " wr") : ""}${slice ? nt(`${tr("Slice:|摘要：")} ${esc(slice)}`, " wr") : ""}${extra}${acts ? dv("ac", acts) : ""}`);
};

const cardActs = (canCmp, canNew, isCmpKind, cmpA, cpA, newA, gid = "", hasCp = true) =>
  `${canCmp ? btn(isCmpKind ? `p ${CM}` : CM, cmpA, tr("Send /compact|执行 /compact"), gid) : ""}${hasCp ? btn("", cpA, tr(T_CP)) : ""}${canNew ? btn(isCmpKind && canCmp ? CM : `p ${CM}`, newA, tr(T_LN), gid) : ""}`;

const patchPendingSlot = () => {
  const g = pg();
  if (!g || !sameWs(g[K_WS], g[K_WSS])) { if (!uPg.hidden) { hd(uPg, 1); ih(uPg, ""); lastPendingSig = ""; } return; }
  const gid = g[K_GID], newCid = g[K_HCID] || "", cStat = g[K_CS] || "", cc = g[K_CC], cid = g[K_CID], isCmp = g.kind === "compact";
  const busy = busyGrantId === gid, runSubs = num(g[K_RSC]), cDone = cStat === "sent" || cStat === "acp_compacted", done = !!(newCid || cDone), canNew = !done && !runSubs && !busy, canCmp = canNew && !!cc && canDirectCid(cid, g[K_WS], g[K_WSS]);
  const sig = `${lang}|${gid}|${busy}|${runSubs}|${newCid}|${cStat}|${canCmp}|${copiedId}|${g[K_PP]}|${g[K_RT]}`;
  if (sig === lastPendingSig) return;
  lastPendingSig = sig;
  const rBdg = done ? bdg("ok", tr("Done|已完成")) : runSubs > 0 ? bdg("warn", subTxt(runSubs)) : bdg("info", busy ? tr(isCmp ? T_CM : T_LB) : tr("Pending|待触发"));
  hd(uPg, 0);
  ih(uPg, renderRelayCard(
    `bn${done ? " dn" : ""}`, done,
    tr(newCid ? T_NS : cDone ? T_CS : isCmp ? T_CR : "Handoff ready|交接已就绪"), g,
    `${rBdg}${btn("", "dis", "×", gid, ` aria-label="${tr("Dismiss|关闭")}"`)}`,
    cardActs(canCmp, canNew, isCmp, "c1", "cpc", "r1", gid, !!cc)
  ));
};

const patchLive = (dtEl, pEl, it) => {
  setCls(dtEl, `dt ${getLiveDot(it)}`);
  const pb = getLivePhaseBadge(it);
  patchBadge(pEl, pb?.cls || "", pb?.text || "");
};

const patchHeroSlot = (hero) => {
  if (!hero) { if (!ui.he.hidden) hd(ui.he, 1); return; }
  const cid = hero.id || "", lvl = safeLvl(hero.level), v = getVerdict(hero), runSubs = num(hero[K_RSC]), title = hero.title;
  hd(ui.he, 0); al(ui.he, tr("Primary|主会话"));
  setCls(ui.he, `hr ${lvlCls(lvl)}`); patchLive(uHd, uHp, hero);
  const tip = cid ? `${title} (${cid})` : title;
  setTxt(uHt, title);
  if (uHt.title !== tip) uHt.title = tip;
  patchIdBtn(uHi, cid, true); setTxt(uHm, hero.model_short || hero.model_display || "");
  hd(uHs, !(runSubs > 0)); if (runSubs > 0) setTxt(uHs, subTxt(runSubs));
  patchBadge(uHv, v.cls, v.text); patchDetailBlock(uHdBlk, hero, false);
};

const createRowEntry = (cid) => {
  const el = mk("it", `<div class="rw" role="button" tabindex="0" data-a="row" data-i="${esc(cid)}">${sp("cv", "▸")}${sp("dt idle")}${shd("bd ac")}${shd("bd muted")}${sp("rt")}${shd("bd")}${shd("bd")}${sp("cx")}${sp("ag")}</div>${dv("ty", "", 1)}`);
  el.dataset.i = cid;
  const r = fc(el);
  return [el, r, ch(r), lc(el), null];
};

const patchSessionList = (items) => {
  if (!items.length) { if (!uLs.hidden) hd(uLs, 1); return; }
  hd(uLs, 0);
  const seen = new Set();
  for (let i = 0; i < items.length; i++) {
    const [it, isSub] = items[i], cid = it.id, title = it.title, wn = it.workspace_name;
    seen.add(cid);
    let rec = rowNodes.get(cid);
    if (!rec) { rec = createRowEntry(cid); rowNodes.set(cid, rec); }
    const [el, r, [c, d, p, sb, t, pEl, vEl, x, a], trBox] = rec, lvl = safeLvl(it.level), isOpen = openRowId === cid, v = getVerdict(it);

    setCls(r, `rw${isSub ? " sub" : ""} ${lvlCls(lvl)}`); sa(r, "aria-expanded", S(isOpen));
    setTxt(c, isOpen ? "▾" : "▸"); patchLive(d, pEl, it);
    hd(p, isSub || !it.pinned); if (!p.hidden) setTxt(p, tr("Pinned|置顶"));
    hd(sb, !isSub); if (isSub) setTxt(sb, tr("Subagent|子代理"));
    const rowTip = `${title}${wn ? ` · ${wn}` : ""} (${cid})`;
    setTxt(t, title);
    if (t.title !== rowTip) t.title = rowTip;
    patchBadge(vEl, v.cls, v.text); setCls(x, `cx ${lvl}`);
    setTxt(x, fmtK(it[K_LCT])); setTxt(a, fmtAge(it.sidebar_sort_ts || it.effective_last_active_ts || it.last_active_ts));

    if (isOpen) { if (!rec[4]) { rec[4] = mountDetailBlock(); trBox.appendChild(rec[4][0]); } hd(trBox, 0); patchDetailBlock(rec[4], it, true); }
    else if (!trBox.hidden) hd(trBox, 1);

    if (ch(uLs)[i] !== el) uLs.insertBefore(el, ch(uLs)[i] || null);
  }
  for (const [cid, rec] of rowNodes) if (!seen.has(cid)) { rec[0].remove(); rowNodes.delete(cid); }
};

const isHandoffLocked = () => /^(sen|com|old)/.test(handoffStatus);
const patchHandoffView = () => {
  const c = handoffCard;
  const sig = c ? `${lang}|${c.mode}|${c.blocked}|${c[K_GID]}|${handoffStatus}|${c[K_HCID]}|${c[K_CS]}|${copiedId}|${microMsg}` : `${lang}|empty`;
  if (sig === lastHandoffSig) return;
  lastHandoffSig = sig;
  if (!c) { ih(uHl, dv("em", tr(T_LD))); return; }
  const newCid = c[K_HCID] || "";
  if (c.mode === HB || c.blocked) {
    ih(uHl, renderRelayCard("cd", false, tr(T_HG), c, bdg("info", tr(T_KP)), "", (c.reasons || []).map((r) => nt(`· ${esc(r)}`, " wr")).join("")));
    return;
  }
  const runSubs = num(c[K_RSC]), done = handoffStatus === "sent" || handoffStatus === "compacted", isCmp = c.kind === "compact", runLbl = tr(isCmp ? T_CM : T_LB);
  const badges = {
    pending: ["info", c[K_AAH] ? runLbl : tr(T_RD)], sending: ["warn", runLbl],
    sent: ["ok", tr(T_NS)], compacted: ["ok", tr(T_CS)],
    manual: ["info", runSubs > 0 ? subTxt(runSubs) : tr(T_RD)],
    old: ["muted", tr(T_US)], blocked: ["info", tr("Gated|已拦截")],
  };
  const [bCls, bTxt] = badges[handoffStatus] || badges.manual;
  const canAct = !newCid && !runSubs && !isHandoffLocked();
  ih(uHl, renderRelayCard(
    "cd", done,
    isCmp ? tr(T_CR) : tr("Session Handoff|新会话交接"),
    c, `${langBtn(!isZh())}${bdg(bCls, bTxt)}`,
    `${cardActs(canAct && !!c[K_CC], canAct, isCmp, "c0", "ccc", "r0")}${microMsg ? sp("mi", esc(microMsg)) : ""}`,
    "", c.scratch_note_path
  ));
};

const render = () => {
  const root = D.getElementById("root");
  if (!root) return;
  if (viewMode === HC) { ensureHandoffMount(root); patchHandoffView(); return; }

  ensureDashboardMount(root);
  const snap = snapshot, pollSec = M.round(pollMs / 1e3);
  const netTip = `${bgHidden ? tr("BG|后台") + " " : ""}${pollSec}s · ${serverMs ?? "-"}/${ewmaRttMs ?? "-"}ms · ${tr("skip|跳过")} ${unchangedHits}`;

  cl(uEn, "on", !isZh()); cl(uZh, "on", isZh()); al(uRb, tr("Refresh|刷新"));
  hd(uMi, !microMsg); if (microMsg) setTxt(uMi, microMsg);
  hd(uMt, 0);
  setTxt(uMt, snap ? fmtClock(snap.generated_at) : loadingInitial ? tr(T_LD) : "-");
  uMt.title = uRb.title = netTip; cl(uRi, "spn", showSpinner);
  hd(uEr, !errorText); if (errorText) setTxt(uEr, errorText);

  if (!snap) {
    uCw.hidden = uPg.hidden = ui.he.hidden = uBr.hidden = uLs.hidden = true; hd(uEm, 0);
    setTxt(uEm, loadingInitial ? tr(T_LD) : tr("No sessions|暂无会话"));
    return;
  }

  const s = snap.summary, mains = snap.sessions || [], hero = getHero(), showSubs = filterTab === "subs", items = [];
  const cwN = num(s.active_carbon_count), showCw = !!(s.carbon_concurrency_warning && cwN >= 3);
  hd(uCw, !showCw);
  if (showCw) setTxt(uCw, tr(`Concurrency tip: ${cwN} active Gemini 4 Carbon sessions — cap <= 2 to avoid spillover.`, `并发提示：${cwN} 个活跃会话正运行 Gemini 4 Carbon，建议并发 <= 2 以免缓存溢出。`));
  const pushSub = (a) => (a || []).forEach((x) => (showSubs || x.is_active) && items.push([x, true]));
  for (const m of mains) { if (!hero || m.id !== hero.id) items.push([m, false]); pushSub(m.subagents); }
  pushSub(snap.orphan_subagents);

  patchPendingSlot(); patchHeroSlot(hero);
  hd(uBr, 0); al(uSg, tr("Filter|筛选"));
  for (const [el, id, lbl, n] of [[uTa, "active", "Active|活跃", s.running_main_count], [uTs, "subs", "Subagents|子代理", 0], [uTr, "archived", "Archived|归档", s.archived_hidden_count]]) {
    const k = num(n); setTxt(el, `${tr(lbl)}${k ? ` (${k})` : ""}`);
    const on = filterTab === id; cl(el, "on", on); sa(el, "aria-selected", S(on));
  }
  setTxt(uAb, isZh() ? `自动 ${pollSec}s` : `Auto ${pollSec}s`);
  uAb.title = netTip; cl(uAb, "on", autoRefresh);
  cl(uSw, "on", allowAutoHandoff); sa(uSw, "aria-checked", S(allowAutoHandoff));
  uSw.title = tr(T_AHT); setTxt(uSwL, tr(T_AH));

  patchSessionList(items);
  hd(uEm, hero || items.length);
  if (!uEm.hidden) setTxt(uEm, tr("No sessions|暂无会话"));
};

const flashMicro = (msg) => { microMsg = msg; render(); ct(microTimer); microTimer = st(() => { microMsg = ""; render(); }, 2200); };

const copyText = async (text, okMsg) => {
  let ok = false;
  try { if (navigator.clipboard?.writeText) { await navigator.clipboard.writeText(text); ok = true; } } catch {}
  if (!ok) {
    const ta = ce("textarea"); ta.value = text; sa(ta, "readonly", ""); ta.style.cssText = "position:fixed;top:-9999px;opacity:0";
    D.body.appendChild(ta); ta.select();
    try { ok = D.execCommand("copy"); } catch {} finally { ta.remove(); }
  }
  if (ok) { if (okMsg) flashMicro(okMsg); return true; }
  flashMicro(tr("Copy failed|复制失败")); return false;
};

const cpCompact = (cmd = "/compact") => copyText(cmd, tr("✓ Copied /compact|✓ 已复制 /compact"));

const copyCid = async (cid) => {
  if (!(await copyText(cid, tr("✓ Copied ID|✓ 已复制 ID")))) return;
  copiedId = cid; render(); ct(copiedTimer);
  copiedTimer = st(() => { if (copiedId === cid) { copiedId = null; render(); } }, 1500);
};

const findSession = (cid) => {
  if (!snapshot) return undefined;
  for (const s of snapshot.sessions) {
    if (s.id === cid) return s;
    for (const sub of s.subagents || []) if (sub.id === cid) return sub;
  }
  return (snapshot.orphan_subagents || []).find((x) => x.id === cid);
}

const extractRelayCid = (res) => { const c = res?._meta?.[AG_M]?.conversationId; return typeof c === "string" ? c.trim() : ""; };

const recAct = (action, gid, extra) => canCallTools && gid ? uiAct({ action, grant_id: gid, ...extra }).catch(() => {}) : 0;
async function executeDispatch(p, claimMode, isInline, isCmp = false) {
  cancelAutoCmp();
  const gid = p.grant_id || "", cmd = p.compact_command || "/compact", cid = p[K_CID] || "", ws = p.workspace || "";
  const txt = isCmp ? cmd : p.handoff_prompt || "", isMan = claimMode.startsWith("manual");
  if (!txt) return;
  const fail = (msg) => { if (isInline) handoffStatus = "manual"; if (msg) flashMicro(msg); render(); };
  if (!canMessage || (isCmp && !isInline && !canDirectCid(cid, ws, p.workspaces))) { if (isMan) await cpCompact(cmd); return fail(""); }
  if (!isInline && !sameWs(ws, p.workspaces)) { if (isMan) await copyText(cmd); return fail(isMan ? tr("Other workspace", "属其他工作区") : ""); }
  if ((p[K_RSC] || 0) > 0) return fail(isMan ? tr(...T_WS) : "");
  if (canCallTools && gid) {
    try {
      const sc = await uiSc({ action: "claim_grant", grant_id: gid, [K_CID]: cid, workspace: snapshot?.daemon_workspace || ws, claim_mode: claimMode });
      if (!sc?.claimed) {
        if (isInline) {
          if (sc?.[K_HCID] && handoffCard) { handoffCard[K_HCID] = sc[K_HCID]; handoffStatus = "sent"; }
          else handoffStatus = /^(already_|session_adv)/.test(sc?.reason || "") ? "old" : "manual";
          return render();
        }
        return fail(isMan ? (sc?.reason === "running_subagents_active" ? tr(...T_WS) : sc?.reason?.endsWith("mismatch") ? tr("Other workspace", "属其他工作区") : tr("Used", "已核销")) : "");
      }
    } catch { return fail(isMan ? tr("Claim failed", "核销失败") : ""); }
  }
  if (isInline) handoffStatus = "sending"; else busyGrantId = gid || "pending";
  render();
  const dst = isInline ? handoffCard : pg()?.grant_id === gid ? pg() : null;
  const failCmp = async (st0, msg) => {
    if (isCmp) { await recAct("record_compact", gid, { status: st0 }); if (dst) dst.compact_status = st0; await cpCompact(cmd); }
    fail(msg);
  };
  const clip = (v) => String(v || "").trim().slice(0, 120);
  try {
    const res = await sendTxt(txt, isCmp ? 0 : { [AG_M]: { target: "new" } });
    if (res?.isError) {
      const d = clip(res?.content?.[0]?.text || res?.error?.message);
      return await failCmp(d ? `fallback_copied:${d}` : "fallback_copied", tr("Kept /compact", "已保留 /compact"));
    }
    const newCid = isCmp ? "" : extractRelayCid(res);
    await recAct(isCmp ? "record_compact" : "record_relay", gid, isCmp ? { status: "sent" } : { [K_HCID]: newCid });
    if (dst) { if (isCmp) dst.compact_status = "sent"; else dst[K_HCID] = newCid; if (!isInline) dst.claimed = true; }
    if (isInline) handoffStatus = isCmp ? "compacted" : "sent";
    flashMicro(isCmp ? tr("✓ Sent /compact", "✓ 已发 /compact") : tr("✓ New chat launched", "✓ 已拉起新会话"));
  } catch (err) {
    await failCmp(`error:${clip(err?.message || err || "failed")}`, tr("Failed", "失败"));
  }
  finally { busyGrantId = null; render(); }
}

function canAutoRelay(p, isInline) {
  if (!p?.grant_id || !canMessage || D.hidden) return "";
  const isCmp = p.kind === "compact", gid = p.grant_id, cid = p[K_CID];
  if (isCmp ? (!p.compact_command || p.acp_proxy_active) : !p.handoff_prompt) return "";
  if (!isInline && (isCmp ? !canDirectCid(cid, p.workspace, p.workspaces) : (!sameWs(p.workspace, p.workspaces) || (ownerCid ? ownerCid !== cid : (!snapshot?.daemon_workspace || num(snapshot?.summary?.running_main_count) > 1))))) return "";
  if (!(isInline ? !!p[K_AAH] : !!(allowAutoHandoff && p[K_AAH] !== false)) || p.auto_fire === false || p.claimed || p[K_HCID] || p.compact_status || (p[K_RSC] ?? 0) > 0 || attemptedGrants.has(gid)) return "";
  return isCmp ? "auto_compact" : "auto_handoff";
}

function maybeAutoRelay(p, isInline) {
  const mode = canAutoRelay(p, isInline);
  if (!mode) return;
  const gid = p.grant_id, isCmp = mode === "auto_compact";
  attemptedGrants.add(gid);
  if (!isCmp) return void executeDispatch(p, mode, isInline, false);
  cancelAutoCmp();
  autoCmpTimer = st(() => {
    autoCmpTimer = null;
    const cur = isInline ? handoffCard : pg();
    if (!cur || cur.grant_id !== gid || cur.claimed || cur.compact_status || cur[K_HCID] || (cur[K_RSC] ?? 0) > 0) return;
    if (isInline && isHandoffLocked()) return;
    if (D.hidden) { attemptedGrants.delete(gid); return; }
    void executeDispatch(cur, mode, isInline, true);
  }, isInline ? 1500 : 2000);
}

function applySnapshot(snap) {
  if (typeof snap.settings?.[K_AAH] === "boolean") allowAutoHandoff = snap.settings[K_AAH];
  if (!langLocked && snap.settings?.lang) { const p = parseLoc(snap.settings.lang); if (p) { lang = p; DE.lang = lang; } }
  if (snap.digest) lastDigest = snap.digest;
  snapshot = snap;
  maybeAutoRelay(snap.pending_grant, false);
}

const nextPollMs = () => { const r = ewmaRttMs ?? 0, b = D.hidden ? (r > 800 ? 12e3 : r > 350 ? 8e3 : r > 150 ? 4e3 : 2e3) : (r > 800 ? 10e3 : r > 350 ? 5e3 : r > 150 ? 2500 : 1e3); return pollErrors > 0 ? M.min(45e3, b * M.pow(2, M.min(pollErrors, 3))) : b; };
const cancelPoll = () => { if (pollTimer !== null) { ct(pollTimer); pollTimer = null; } };

const scheduleNextPoll = () => {
  cancelPoll(); bgHidden = !!D.hidden;
  if (viewMode !== DB || !autoRefresh || !canCallTools) return;
  pollMs = nextPollMs();
  pollTimer = st(() => { pollTimer = null; void refreshSnapshot(false); }, pollMs);
};

async function refreshSnapshot(force = false, showSynced = false) {
  if (viewMode !== DB || !canCallTools) return;
  if (inFlightPoll) { if (force) queuedForce = true; return; }
  cancelPoll(); inFlightPoll = true; ct(spinTimer);
  if (force || !snapshot) { refreshing = true; spinTimer = st(() => { if (refreshing) { showSpinner = true; render(); } }, 150); }
  const t0 = performance.now();
  try {
    const args = { action: "snapshot", limit: 25, show_history: filterTab === "subs", include_archived: filterTab === "archived" };
    if (force) args.fresh = true; else if (lastDigest) args.if_digest = lastDigest;
    const res = await callTool("context_radar_overview", args, { timeout: 15e3 });
    const rz = (x) => M.max(0, M.round(x));
    const totalMs = rz(performance.now() - t0), sc = res?.structuredContent, srvMs = rz(num(sc?.server_ms)), netRtt = M.max(0, totalMs - srvMs);
    serverMs = srvMs; ewmaRttMs = ewmaRttMs === null ? netRtt : M.round(ewmaRttMs * 0.65 + netRtt * 0.35);
    pollErrors = 0; lastSyncTs = now(); ct(spinTimer); showSpinner = false;

    const finish = () => { refreshing = false; showSynced ? flashMicro(tr("✓ Synced", "✓ 已同步")) : render(); };
    if (res?.isError) { errorText = tr("Sync error", "刷新失败"); refreshing = false; render(); }
    else if (sc?.unchanged || (sc?.digest && sc.digest === lastDigest && snapshot)) {
      const hadErr = !!errorText;
      unchangedHits += 1; errorText = "";
      if (sc?.generated_at && snapshot) snapshot.generated_at = sc.generated_at;
      if (refreshing) finish(); else if (hadErr) render();
    } else if (sc) {
      applySnapshot(sc); loadingInitial = false; errorText = ""; finish();
    }
  } catch (err) {
    pollErrors += 1; errorText = `${tr("Sync:", "同步：")} ${err?.message || err}`;
    ct(spinTimer); showSpinner = false; refreshing = false; render();
  } finally {
    inFlightPoll = false;
    queuedForce ? ((queuedForce = false), void refreshSnapshot(true, false)) : scheduleNextPoll();
  }
}

const withBusy = async (key, failMsg, fn) => {
  loadingKey = key; render();
  try { await fn(); } catch { flashMicro(failMsg); } finally { loadingKey = ""; render(); }
};

const openTray = async (cid, tab) => {
  if (openTrays.get(cid) === tab) { openTrays.delete(cid); return render(); }
  openTrays.set(cid, tab);
  if (!canCallTools) return render();
  const [m, a] = { [K_TN]: [K_TN, { limit: 24 }], hotspots: ["hotspots", { mode: "hotspots", top_k: 5 }], advice: ["prepare_handoff", { estimated_remaining_turns: 12 }] }[tab];
  await withBusy(`${tab}:${cid}`, tr("Load failed|加载失败"), async () => {
    const res = await callTool(`context_radar_${m}`, { [K_CID]: cid, ...a });
    if (!res?.isError && res?.[K_SC2]) { trayMap[tab].set(cid, res[K_SC2]); subTrayRev += 1; }
  });
};

const runDistill = async (cid) => {
  if (!canCallTools || !cid) return;
  const rec = cHot.get(cid)?.recommended_distill_range;
  await withBusy(`distill:${cid}`, tr("Distill failed|提炼失败"), async () => {
    const args = { action: "distill_slice", [K_CID]: cid };
    if (Array.isArray(rec) && rec.length === 2) { args.start_step = rec[0]; args.end_step = rec[1]; }
    const sc = await uiSc(args);
    if (sc?.found) { cDist.set(cid, sc); subTrayRev += 1; flashMicro(tr("✓ Saved to scratch/|✓ 已写入 scratch/")); }
    else flashMicro(sc?.error || tr("Distill failed|提炼失败"));
  });
};

const prepareOrLaunch = async (cid, launchNow) => {
  if (!canCallTools || !cid) return;
  await withBusy(`handoff:${cid}`, tr("Prep failed|生成失败"), async () => {
    const dist = cDist.get(cid), args = { action: "create_handoff", kind: launchNow ? "handoff" : "compact", [K_CID]: cid, auto_fire: false, estimated_remaining_turns: 12 };
    if (dist?.[K_SN]) args.scratch_note_path = dist[K_SN];
    const sc = await uiSc(args);
    if (!sc || sc.blocked || sc.mode === HB) return flashMicro(sc?.reasons?.[0] || tr(T_HG));
    if (snapshot) snapshot.pending_grant = sc;
    subTrayRev += 1;
    if (launchNow) { loadingKey = ""; await executeDispatch(sc, "manual_handoff", false, false); }
    else await cpCompact(sc[K_CC]);
  });
};

const toggleLang = () => {
  lang = isZh() ? "en" : "zh-CN"; langLocked = true; DE.lang = lang; ls0(lang); subTrayRev += 1; render();
  if (canCallTools) void uiAct({ action: "set_prefs", lang }).catch(() => {});
};

const toggleAutoHandoff = async () => {
  const next = !allowAutoHandoff; allowAutoHandoff = next; if (!next) cancelAutoCmp(); render();
  if (!canCallTools) return;
  try {
    const val = (await uiSc({ action: "set_prefs", [K_AAH]: next }))?.settings?.[K_AAH];
    if (typeof val === "boolean") allowAutoHandoff = val;
    flashMicro(allowAutoHandoff ? tr("✓ Auto on|✓ 已开启接力") : tr("✓ Auto off|✓ 已暂停接力"));
    if (allowAutoHandoff && pg()) { pg()[K_AAH] = true; maybeAutoRelay(pg(), false); }
  } catch { allowAutoHandoff = !next; flashMicro(tr("Save failed|保存失败")); render(); }
};

const buildReminder = (it) => {
  const lim = num(it.context_limit) || 512e3, rec = M.round((num(it[K_RCT]) || 31e4) / 1e3);
  return isZh()
    ? `会话《${it.title}》上下文达 ${fmtK(it[K_LCT])}/${fmtK(lim)}（超 ${rec}k），请收尾或 /compact。`
    : `Context for "${it.title}" is ${fmtK(it[K_LCT])}/${fmtK(lim)} (>${rec}k); wrap up or /compact.`;
};

const sendReminder = async (it) => {
  if (!it || it[K_IS]) return;
  confirmSendId = null;
  const msg = buildReminder(it);
  if (!canDirectMsg(it)) { await copyText(msg, tr(T_RC)); render(); return; }
  try {
    const res = await sendTxt(msg);
    res?.isError ? await copyText(msg, tr(T_RC)) : flashMicro(tr("✓ Sent|✓ 已发送"));
  } catch { await copyText(msg, tr(T_RC)); }
  render();
};

const setFilterTab = (nextTab, focusTab = false) => {
  if (filterTab === nextTab) return;
  filterTab = nextTab; lastDigest = ""; render();
  if (focusTab) D.querySelector(`[data-a="tab"][data-i="${nextTab}"]`)?.focus();
  void refreshSnapshot(true, false);
}

const toggleRow = (id) => { openRowId = openRowId === id ? null : id; render(); };
onD("click", (ev) => {
  const el = ev.target?.closest?.("[data-a]");
  if (!el) return;
  ev.stopPropagation();
  const { a, i: id = "" } = el.dataset;
  if (a.startsWith("t-") && id) return void openTray(id, a.slice(2));
  if (a === "ci" && id) { ev.preventDefault(); return void copyCid(id); }
  if (a === "lng") return toggleLang();
  if (a === "ref") return void refreshSnapshot(true, true);
  if (a === "aut") { autoRefresh = !autoRefresh; autoRefresh ? scheduleNextPoll() : cancelPoll(); return render(); }
  if (a === "tab" && id) return setFilterTab(id);
  if (a === "aho") return void toggleAutoHandoff();
  if (a === "row" && id) return toggleRow(id);
  if (a === "dst" && id) return void runDistill(id);
  if (a === "cdp" && id) { const p = cDist.get(id)?.[K_SN]; return p && void copyText(p, tr(T_OK)); }
  if ((a === "up" || a === "ul") && id) return void prepareOrLaunch(id, a === "ul");
  if (a === "cc" || a === "ccc" || a === "cpc") {
    const m = a === "cc" ? "/compact" : (a === "ccc" ? handoffCard : pg())?.[K_CC];
    return m && void cpCompact(m);
  }
  if (/^[cr][01]$/.test(a)) {
    const isIn = a[1] === "0", isCmp = a[0] === "c", g = isIn ? handoffCard : pg();
    return g && void executeDispatch(g, isCmp ? "manual_compact" : "manual_handoff", isIn, isCmp);
  }
  if (a === "dis" && id) { cancelAutoCmp(); if (pg()?.[K_GID] === id) { delete snapshot.pending_grant; render(); } return recAct("dismiss_grant", id, {}); }
  if (a === "ask" || a === "no") { confirmSendId = a === "ask" ? id : null; return render(); }
  const it = findSession(id);
  if (!it) return;
  if (a === "crm") return void copyText(buildReminder(it), tr(T_RC));
  if (a === "csr") return void copyText(isZh() ? `子代理 ${it.agent_name || it.title} (${id}) 达 ${fmtK(it[K_LCT])}，请切棒` : `Subagent ${it.agent_name || it.title} (${id}) at ${fmtK(it[K_LCT])}; pass baton`, tr(T_OK));
  if (a === "yes") return void sendReminder(it);
});

onD("keydown", (ev) => {
  if (ev.key === "Escape") {
    if (confirmSendId || openTrays.size || openRowId) { confirmSendId = null; openTrays.clear(); openRowId = null; render(); }
    return;
  }
  const el = ev.target?.closest?.("[data-a]");
  if (!el) return;
  if (/^(Enter| )$/.test(ev.key) && el.dataset.a === "row") {
    ev.preventDefault(); toggleRow(el.dataset.i || "");
  } else if (/^Arrow(Left|Right)$/.test(ev.key) && el.dataset.a === "tab") {
    ev.preventDefault(); const o = ["active", "subs", "archived"]; setFilterTab(o[(o.indexOf(filterTab) + (ev.key === "ArrowRight" ? 1 : 2)) % 3], true);
  }
});

const canForceDb = () => !D.hidden && viewMode === DB && canCallTools;
const forceNow = () => { const t = now(); if (t - lastForceTs < 250) return; lastForceTs = t; void refreshSnapshot(true, false); };
const onVis = () => { if (canForceDb()) forceNow(); };
const kickRelay = () => viewMode === HC ? handoffCard && !handoffCard.blocked && maybeAutoRelay(handoffCard, true) : pg() && maybeAutoRelay(pg(), false);
const syncNow = () => { bgHidden = !!D.hidden; if (!D.hidden) kickRelay(); if (viewMode === DB && canCallTools) { if (!D.hidden) forceNow(); else if (autoRefresh) scheduleNextPoll(); } render(); };
onD("visibilitychange", syncNow);
onD("pointerenter", () => { if (canForceDb() && now() - lastSyncTs > 900) forceNow(); }, { passive: true });
window.addEventListener("focus", onVis);
window.addEventListener("pageshow", onVis);

let obsRo = null, obsIo = null;
function setupRevealObservers() {
  if (typeof ResizeObserver !== "undefined") {
    let col = !(DE.clientWidth > 0 && DE.clientHeight > 0);
    obsRo = new ResizeObserver((es) => {
      const r = es[es.length - 1]?.contentRect, vis = (r ? r.width : DE.clientWidth) > 0 && (r ? r.height : DE.clientHeight) > 0;
      if (col && vis && canForceDb()) forceNow();
      col = !vis;
    });
    obsRo.observe(DE);
  }
  if (typeof IntersectionObserver !== "undefined") {
    let was = !!(DE.clientWidth > 0 && DE.clientHeight > 0);
    obsIo = new IntersectionObserver((es) => {
      const vis = !!es[es.length - 1]?.isIntersecting;
      if (!was && vis && canForceDb()) forceNow();
      was = vis;
    }, { threshold: 0 });
    obsIo.observe(DE);
  }
}

bridge.ontoolinput = (p) => { const c = p?.arguments?.[K_CID]; if (typeof c === "string" && c.trim()) ownerCid = c.trim(); if (!snapshot && viewMode === DB) { loadingInitial = true; render(); } };

bridge.ontoolresult = (result) => {
  if (result?.isError) { errorText = tr("Tool error; refresh|工具错误，请刷新"); loadingInitial = false; render(); return; }
  const sc = result?.[K_SC2];
  if (sc && (sc.mode === HC || sc.mode === HB)) {
    viewMode = HC; handoffCard = sc;
    if (sc[K_CID]) ownerCid = sc[K_CID];
    if (!langLocked && sc.lang) { const p = parseLoc(sc.lang); if (p) { lang = p; DE.lang = lang; } }
    if (typeof sc[K_AAH] === "boolean") allowAutoHandoff = sc[K_AAH];
    handoffStatus = sc.mode === HB ? "blocked" : sc[K_AAH] && sc.auto_fire !== false && !sc[K_RSC] && !sc.acp_proxy_active ? "pending" : "manual";
    loadingInitial = false; cancelPoll(); render();
    if (sc.mode === HC) maybeAutoRelay(sc, true);
    return;
  }
  if (sc?.mode === "snapshot") {
    viewMode = DB; loadingInitial = false; errorText = ""; lastSyncTs = now();
    if (sc.server_ms !== undefined) serverMs = M.max(0, M.round(num(sc.server_ms)));
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
    const k = h.length, x = (i) => k <= 4 ? p(h[i] + h[i]) : p(h.slice(i * 2, i * 2 + 2));
    if ((k === 4 || k === 8) && x(3) < 128) return null;
    return (.2126 * x(0) + .7152 * x(1) + .0722 * x(2)) / 255;
  }
  const m = s.match(/^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:\s*[,/]\s*([\d.]+%?))?\s*\)/i);
  if (!m) return null;
  if (m[4]) { const a = parseFloat(m[4]) / (m[4].endsWith("%") ? 100 : 1); if (Number.isNaN(a) || a < .5) return null; }
  return (.2126 * num(m[1]) + .7152 * num(m[2]) + .0722 * num(m[3])) / 255;
}

const syncHostContext = (ctx) => {
  const st = ctx?.styles;
  if (st?.variables) applyHostStyleVariables(st.variables);
  if (st?.css?.fonts) applyHostFonts(st.css.fonts);
  if (!langLocked && ctx?.locale) { const h = parseLoc(ctx.locale); if (h && h !== lang) { lang = h; DE.lang = lang; render(); } }
  let res = null;
  const raw = String(ctx?.theme || "").trim().toLowerCase();
  if (raw) res = /dark|night|black/.test(raw) ? "dark" : /light|day|white/.test(raw) ? "light" : null;
  if (!res) {
    const lum = parseColorLum(st?.variables?.[BG_V]) ?? parseColorLum(getComputedStyle(DE).getPropertyValue(BG_V));
    if (lum !== null) res = lum < 0.45 ? "dark" : "light";
  }
  if (res) applyDocumentTheme(res);
}

bridge.onhostcontextchanged = (ctx) => { syncHostContext(ctx); if ((ctx?.containerDimensions || ctx?.displayMode) && canForceDb()) forceNow(); };
bridge.onteardown = () => { cancelPoll(); cancelAutoCmp(); obsRo?.disconnect(); obsIo?.disconnect(); return {}; };

const bootPanel = async () => {
  setupRevealObservers();
  if (snapshot) applySnapshot(snapshot);
  render();
  try {
    await bridge.connect();
    syncHostContext(bridge.getHostContext());
    const caps = bridge.getHostCapabilities();
    canMessage = !!caps?.message;
    canCallTools = caps?.serverTools !== undefined ? !!caps.serverTools : true;
    DE.dataset.canMsg = canMessage ? "1" : "0"; DE.dataset.ct = canCallTools ? "1" : "0";
    render(); scheduleNextPoll(); kickRelay();
    if (viewMode === DB && canCallTools) void refreshSnapshot(true, false);
  } catch (err) {
    if (!snapshot) { errorText = `${tr("Connect failed:|连接失败：")} ${err?.message || err}`; loadingInitial = false; render(); }
  }
}

void bootPanel();
