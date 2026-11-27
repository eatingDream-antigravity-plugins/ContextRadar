import { McpAppBridge, applyDocumentTheme, applyHostFonts, applyHostStyleVariables } from "./bridge.js";

const D = document, DE = D.documentElement, M = Math, st = (...a) => setTimeout(...a), ct = (i) => clearTimeout(i), now = Date.now, S = String, onD = (e, f, o) => D.addEventListener(e, f, o);
const bridge = new McpAppBridge({ name: "context-radar-mcp", version: "1.0.0" }, {});
const boot = typeof window < "u" ? window.__CONTEXT_RADAR_BOOT__ : null;
const NS = (DE.namespaceURI || "").replace("1999/xhtml", "2000/svg") || ["ht", "tp://www.w3.org/2000/svg"].join("");

// Shared DOM builder (~200 B): pure createElement / textContent only
const h = (t, c, a, ...ch) => {
  const n = D.createElement(t);
  if (c) n.className = c;
  if (a) for (const k in a) {
    const v = a[k]; if (v == null || v === false) continue;
    k === "h" ? (n.hidden = 1) : k === "s" ? (n.style.cssText = v) : (n.setAttribute(k, v === true ? "" : S(v)), k[4] === "-" && k[0] === "d" && n.dataset && (n.dataset[k.slice(5)] = S(v)));
  }
  const add = (x) => { if (x != null && x !== false) x.forEach ? x.forEach(add) : n.appendChild(typeof x === "object" ? x : D.createTextNode(S(x))); };
  add(ch); return n;
};
const dv = (c, ...x) => h("div", c, 0, ...x), sp = (c, ...x) => h("span", c, 0, ...x), hd0 = (t, c, ...x) => h(t, c, { h: 1 }, ...x), hdv = (c, ...x) => hd0("div", c, ...x), hb = (c = "") => hd0("span", `bd${c ? ` ${c}` : ""}`), al = (v) => ({ "aria-label": v });
const bt = (c, a, tx, id, ex) => h("button", c, { "data-a": a, ...(id ? { "data-i": id } : {}), ...ex }, tx);
const setCh = (e, a) => { while (e.firstChild) e.firstChild.remove(); a.forEach((n) => n && e.appendChild(n)); };
const sv = (z, ps, c0) => {
  const mk = (t, a) => { const n = D.createElementNS(NS, t); for (const k in a) n.setAttribute(k, S(a[k])); return n; };
  const s = mk("svg", { width: z, height: z, viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", "stroke-width": 1.5, "stroke-linecap": "round", "aria-hidden": 1 });
  ps.forEach((d, i) => s.appendChild(mk("path", c0 && i === 1 ? { d, opacity: .55 } : { d })));
  if (c0) s.appendChild(mk("circle", { cx: 8, cy: 8, r: 1, fill: "currentColor", stroke: "none" }));
  return s;
};
const mkEmb = (dc, big) => { const d = sp(`dt ${dc || K_IE}`); return [h("span", "eb", big ? { s: "width:32px;height:32px;border-radius:8px" } : 0, sv(big ? 18 : 14, ["M3.2 12.8A6.5 6.5 0 1 1 13.4 4.4", "M5.6 10.4A3.4 3.4 0 1 1 10.8 5.9", "M8 8l4.4-4.4"], 1), d), d]; };

// Flat [en, zh] i18n table
const I18N = `Loading…|加载中…
✓ Copied|✓ 已复制
Copy /compact|复制 /compact
Keep warm|保持热前缀
Ready|已就绪
Launching…|拉起中…
Auto handoff|自动接力
Auto|无子代理自动
✓ Copied reminder|✓ 已复制提醒
Active subs|存在活跃子代理
Compacting…|压缩中…
Copy ID|复制 ID
Launch New Chat|拉起新会话
Handoff gated|暂缓交接
Used|已使用
New chat|已拉起新会话
Compact sent|已发 /compact
Compact ready|压缩已就绪
Handoff ready|交接已就绪
Error|异常
Refresh|刷新
Dismiss|关闭
Send /compact|执行 /compact
Other ws|其他工作区
No sessions|暂无会话
Pinned|置顶
Recent|近期会话
Failed|失败
Context|上下文
Subagent|子代理`.split("\n").map((r) => r.split("|"));

const K_ZH = "zh-CN", K_SN0 = "snapshot", K_IE = "idle", K_SP = "set_prefs", K_ERR = "isError", K_DIG = "digest", K_PP = "preserve_points", K_RT = "resume_task", K_SM = "server_ms", K_CP = "context_pct", K_HP = "handoff_prompt", K_RC = "record_compact", K_MH = "manual_handoff";
const K_CMP = "compact", K_SCMP = "/compact", K_CRIT = "critical", K_HS = "hotspots", K_CG = "compacting", K_HO = "handoff", K_WT = "waiting", K_MN = "manual", K_PD = "pending", K_AC = "active", K_CU = "cost_usd", K_CCY = "cost_cny", K_AN = "agent_name", K_MLC = "mid_loop_cold", K_PCC = "post_compact_cold";
const ls0 = (v) => { try { return v ? localStorage.setItem("cr_lang", v) : localStorage.getItem("cr_lang"); } catch {} };
const parseLoc = (r) => { const s = S(r || "").trim().toLowerCase(); return !s || s === "auto" ? "" : s.startsWith("zh") ? K_ZH : "en"; };
const p0 = ls0(), pLang = p0 === "en" || p0 === K_ZH ? p0 : "";
let langLocked = !!(pLang || boot?.settings?.lang), lang = pLang || parseLoc(boot?.settings?.lang) || parseLoc(typeof navigator < "u" ? navigator.language : "") || parseLoc(boot?.lang) || "en";
DE.lang = lang;

const isZh = () => lang === K_ZH, L = (i) => I18N[i][+isZh()], tr = (e, z) => z !== undefined ? (isZh() ? z : e) : e.split("|")[+isZh()];
const subTxt = (n) => (isZh() ? `${n} 个子代理` : `${n} subagent${n === 1 ? "" : "s"}`);

const CM = "cr-cap-msg", HC = "handoff_card", HB = "handoff_blocked", DB = "dashboard", AG_M = "com.google.antigravity/message", BG_V = "--color-background-primary";
const K_CID = "conversation_id", K_HCID = "handoff_conversation_id", K_AAH = "allow_auto_handoff", K_RSC = "running_subagent_count", K_LCT = "latest_context_tokens";
const K_CC = "compact_command", K_CS = "compact_status", K_JC = "just_compacted", K_SC = "suggest_compact", K_IS = "is_subagent";
const K_SC2 = "structuredContent", K_GID = "grant_id", K_SN = "saved_note_path", K_TN = "turns", K_SA = "subagents", K_AR = "archived", K_OS = "orphan_subagents";
const K_WS = "workspace", K_WSS = "workspaces", K_CL = "context_limit", K_CT = "context_tokens", K_CR = "cache_hit_rate", K_RCT = "recommend_compact_tokens";
const K_ST = "settings", K_ACT = "is_active", K_PG = "pending_grant", K_DW = "daemon_workspace", K_GA = "generated_at", K_PK = "pricing_known", K_PF = "context_radar_";
const DEF_T = [30e4, 36e4, 43e4], thrs = (it) => [it.min_compact_tokens, it[K_RCT], it.urgent_compact_tokens], TABS = [K_TN, K_HS, "advice"], FIDS = [K_AC, "subs", K_AR];

let viewMode = boot?.view === K_HO ? HC : DB, snapshot = boot?.mode === K_SN0 ? boot : null, lastDigest = boot?.[K_DIG] || "";
let handoffCard = null, handoffStatus = K_PD, loadingInitial = !(boot?.mode === K_SN0);
let refreshing = false, autoRefresh = true, filterTab = K_AC, allowAutoHandoff = boot?.[K_ST]?.[K_AAH] ?? true;
let busyGrantId = null, errorText = "", canMessage = false, ownerCid = "", copiedId = null, confirmSendId = null, microMsg = "";
let ewmaRttMs = null, serverMs = boot?.[K_SM] ?? null, pollMs = 1e3, unchangedHits = 0, bgHidden = false, pollErrors = 0;
let lastSyncTs = boot?.mode === K_SN0 ? now() : 0, lastForceTs = 0, spinTimer = null, showSpinner = false;
let openRowId = null, loadingKey = "", subTrayRev = 0, bnrPri = false, mountedMode = "", ui = null, lastHandoffSig = "";
const openTrays = new Map(), cTurns = new Map(), cHot = new Map(), cDist = new Map(), cAdv = new Map(), attemptedGrants = new Set(), rowNodes = new Map(), pinNodes = new Map(), heroChipNodes = new Map();
const trayMap = { turns: cTurns, hotspots: cHot, advice: cAdv };
let canCallTools = false, inFlightPoll = false, queuedForce = false, pollTimer = null, microTimer = null, copiedTimer = null, autoCmpTimer = null;
let uDot, uSwp, uMi, uMt, uAb, uEn, uZh, uRb, uRi, uB, uH, uF, uPG, uPGh, uPLs, uRG, uRGh, uLs, uEm, uHl;
const pg = () => snapshot?.[K_PG], cancelAutoCmp = () => { if (autoCmpTimer !== null) { ct(autoCmpTimer); autoCmpTimer = null; } };

const callTool = (name, args, opts) => bridge.callServerTool({ name, arguments: args }, opts);
const uiAct = (args) => callTool(`${K_PF}ui_action`, args), uiSc = async (a) => (await uiAct(a))?.[K_SC2];
const sendTxt = (text, meta) => bridge.sendMessage({ role: "user", content: [{ type: "text", text }], ...(meta ? { _meta: meta } : {}) });
const num = (v) => Number(v) || 0, fx = (n, d = 0) => num(n).toFixed(d), clp = (v) => M.min(100, M.max(0, v));
const safeLvl = (l) => (l === K_CRIT || l === "warn" ? l : "ok"), lvlTone = (l) => (l === K_CRIT ? "er" : l === "warn" ? "wn" : "ok"), lvlCls = (l) => (l === K_CRIT ? "lc" : l === "warn" ? "lw" : "");
const setCls = (e, c) => e && e.className !== c && (e.className = c), setTxt = (e, v) => e && e.textContent !== (v = S(v ?? "")) && (e.textContent = v);
const sa = (e, k, v) => e && e.getAttribute?.(k) !== (v = S(v)) && e.setAttribute(k, v), ar = (e, k, v) => sa(e, `aria-${k}`, v), hd = (e, b) => { if (e) e.hidden = !!b; }, hdA = (...a) => a.forEach((e) => hd(e, 1)), cl = (e, c, b) => e?.classList?.toggle(c, !!b);

const fmtK = (n) => (n = num(n)) >= 1e6 ? `${fx(n / 1e6, 2)}M` : n >= 1e3 ? `${fx(n / 1e3, 1)}k` : S(n);
const fmtMs = (v) => !(v = num(v)) ? "-" : v >= 1e3 ? `${fx(v / 1e3, 1)}s` : `${v}ms`;
const fmtClock = (iso) => { if (!iso) return "-"; const d = new Date(iso); return Number.isNaN(d.getTime()) ? "-" : d.toTimeString().slice(0, 8); };
const fmtAge = (ts) => { if (!ts) return tr("idle|空闲"); const d = M.max(0, (now() / 1e3 - ts) | 0), u = (k, s) => `${(d / k) | 0}${tr(s)}`; return d < 60 ? u(1, "s|秒前") : d < 3600 ? u(60, "m|分前") : d < 86400 ? u(3600, "h|小时前") : u(86400, "d|天前"); };
const fmtMoney = (u, c, d = 2, k = true) => k === false && !num(u) && !num(c) ? tr("Unpriced|未定价") : snapshot?.currency_explicit ? `${snapshot.currency_symbol || "$"}${fx(S(snapshot.currency_code).toUpperCase() === "USD" ? u : c, d)}` : `${isZh() ? "¥" : "$"}${fx(isZh() ? c : u, d)}`;
const shortId = (id) => (id.length > 12 ? `${id.slice(0, 6)}…${id.slice(-4)}` : id), jDot = (a, b) => (a && b ? `${a} · ${b}` : a || b);

const normWs = (r) => { let s = S(r || "").trim(); if (s.startsWith("file://")) { s = s.slice(7); try { s = decodeURIComponent(s); } catch {} } return s.length > 1 ? s.replace(/\/+$/, "") : s; };
const sameWs = (ws, allWs) => { const dws = normWs(snapshot?.[K_DW]); return !dws || [ws, ...(Array.isArray(allWs) ? allWs : [])].map(normWs).filter(Boolean).some((w) => w === dws || w.startsWith(`${dws}/`) || dws.startsWith(`${w}/`)); };
const getHero = () => { const m = (snapshot?.sessions || []).filter((x) => !x[K_AR]); return m.find((x) => x[K_ACT]) || m[0] || null; };
const canDirectCid = (cid, ws, allWs) => !canMessage || !cid ? false : ownerCid ? ownerCid === cid : !!(getHero()?.id === cid && sameWs(ws, allWs) && num(snapshot?.summary?.running_main_count) <= 1);
const canDirectMsg = (it) => !!(it && !it[K_IS] && canDirectCid(it.id, it[K_WS], it[K_WSS]));
const getCold = ({ cold_cache_breakdown: c, mid_loop_breakdown: mb, turn_start_cold_count: t, mid_loop_cold_count: m, post_compact_cold_count: p }) =>
  ({ t: num(c?.turn_start_cold ?? t), m: num(c?.[K_MLC] ?? m), p: num(c?.[K_PCC] ?? p), s: num(mb?.slow_turn_self_expiry), r: num(mb?.replica_spillover), w: num(mb?.model_switch_cold) });

const getVerdict = (it) => {
  const lvl = safeLvl(it.level);
  if (it[K_JC]) return { cls: "in", text: tr("Compacted|已压缩") };
  if (it[K_SC] || lvl === K_CRIT) return { cls: lvl === K_CRIT ? "er" : "wn", text: L(18) };
  const ctx = num(it[K_LCT]);
  return ctx >= (num(it.min_compact_tokens) || DEF_T[0]) * .72 || (ctx >= 12e4 && getCold(it).m > 0) ? { cls: "wn", text: tr("Slice ready|可提炼") } : { cls: "ok", text: "" };
};

const DOT_CLS = { running: "run", [K_WT]: "wait", error: "err", ready: K_IE };
const getLiveDot = (it) => DOT_CLS[it?.live_status] || (it?.[K_ACT] ? "run" : K_IE);
const getLivePhaseBadge = (it) => {
  const s = it?.live_status, p = it?.live_phase, sec = M.max(0, M.round(num(it?.live_elapsed_sec))), el = sec > 0 ? ` ${sec}s` : "";
  return p === K_CG ? { cls: s === K_WT ? "wn" : "in", text: `${L(10)}${el}` } : s === K_WT ? { cls: "wn", text: `${tr("Waiting|等待")}${el}` } : s === "error" ? { cls: "er", text: L(19) } : null;
};

const SUB_L = { slow_turn_self_expiry: "Slow-turn TTL|慢轮自过期", replica_spillover: "Replica spillover|副本溢出", model_switch_cold: "Model switch|切换模型" };
const coldKind = (t) => { const k = t.cold_cache_kind, sr = t.cold_cache_sub_reason; return k === K_MLC ? (SUB_L[sr] ? tr(SUB_L[sr]) : tr(t.is_partial_cold ? "Partial drop|前缀跌落" : "Mid-turn|中途断缓")) : tr(k === K_PCC ? "Post-compact|压缩后" : "New turn|新轮次"); };

const mkB = (c, a, tx, id, ex) => bt(`bt${c ? ` ${c}` : ""}`, a, tx, id, ex), pc0 = (p) => `${p ? "p" : "s"} ${CM}`;
const mkId = (id, cmp) => id ? bt(`id${copiedId === id ? " ok" : ""}`, "ci", copiedId === id ? L(1) : cmp ? L(11) : shortId(id), id, { title: id }) : null;
const mkLang = (en) => h("div", "lg", { role: "group", ...al("Language") }, bt(`lo${en ? " on" : ""}`, "lng", "EN", "en", { "aria-pressed": S(en) }), bt(`lo${en ? "" : " on"}`, "lng", "中文", K_ZH, { "aria-pressed": S(!en) }));
const mkSw = (lbl = "") => bt(`sw ${CM}${allowAutoHandoff ? " on" : ""}`, "aho", [sp("tk", sp("th")), sp("", lbl)], "", { role: "switch", "aria-checked": S(allowAutoHandoff), title: L(7) });
const mkSeg = (fn) => { const sth = sp("sth"), bs = [0, 1, 2].map(fn); return [h("div", "sg ct0", { role: "tablist" }, sth, ...bs), sth, bs]; };
const wr = (l, v) => v && dv("wr", `${tr(l)} ${v}`);
const relayNotes = (g, sl) => [g[K_HCID] && dv("ac", sp("", tr("New:|新：")), mkId(g[K_HCID])), wr("Keep:|保留：", g[K_PP]), wr("Next:|续跑：", g[K_RT]), wr("Slice:|摘要：", sl)].filter(Boolean);
const relayBtns = (g, isIn, canC, canN, id) => {
  const p = canC && g.kind === K_CMP;
  return [g[K_CC] && mkB("s", isIn ? "ccc" : "cpc", L(2), id), canC && mkB(pc0(p), isIn ? "c0" : "c1", L(22), id), canN && mkB(pc0(!p), isIn ? "r0" : "r1", L(12), id)].filter(Boolean);
};
const patchId = (el, cid, show, cmp = true) => { if (!show || !cid) return hd(el, 1); const ok = copiedId === cid; hd(el, 0); el.dataset.i = cid; setCls(el, `id${ok ? " ok" : ""}`); setTxt(el, ok ? L(1) : cmp ? L(11) : shortId(cid)); el.title = cid; };
const patchBdg = (el, cls, txt, tip = "") => { hd(el, !txt); if (txt) { setCls(el, `bd ${cls}`); setTxt(el, txt); if (tip || el.title) el.title = tip; } };

const buildSubTray = (it) => {
  const cid = it.id, tab = openTrays.get(cid), msg = (s) => dv("bx", dv("t2", s));
  if (!tab) return null;
  if (loadingKey === `${tab}:${cid}`) return msg(L(0));
  const d = trayMap[tab].get(cid), nd = tr("No data|无数据");
  if (tab === K_TN) {
    const ts = d?.[K_TN]; if (!ts?.length) return msg(nd);
    const ch = ts.slice(0, 24).reverse(), mx = M.max(1, ...ch.map((t) => num(t[K_CT])));
    return dv("bx", dv("t2", `${ch.length}/${num(d.total_turns)}`),
      dv("sk", ch.map((t) => h("span", `sr${t.is_checkpoint ? " in" : t.is_cold_cache ? " er" : ""}`, { s: `height:${M.max(12, M.round((num(t[K_CT]) / mx) * 100))}%`, title: `#${num(t.turn)} · ${fmtK(t[K_CT])}` }))),
      dv("c g4", ts.slice(0, 10).map((t) => {
        const { is_checkpoint: ck, thinking_tokens: k0, response_tokens: r0, stream_tps: s1, e2e_tps: e1 } = t, hr = fx(t[K_CR]), k = num(k0), r = num(r0), s0 = num(s1), e0 = num(e1), tt = fmtMs(t.ttft_ms);
        const cc = t.is_cold_cache ? [sp("er tc", `${hr}%`), " ", h("span", `bd s ${{ [K_MLC]: "er", [K_PCC]: "in" }[t.cold_cache_kind] || "wn"}`, { title: t.cold_cache_reason }, coldKind(t))] : [`${hr}%`];
        return dv("tr", sp("", `#${num(t.turn)}${ck ? "·C" : ""}`), sp("", fmtClock(t.time_iso)), sp("e", h("b", "", 0, fmtK(t[K_CT])), k > 0 ? (r > 0 ? ` T${M.round((k / (k + r)) * 100)}%` : ` think ${fmtK(k)}`) : "", " · ", ...cc), sp("", ck ? tr("Ckpt|节点") : s0 > 0 ? `${tt}${e0 > 0 ? ` · ${fx(s0)}/${fx(e0)}` : `/${fx(s0)}`}` : `${tt}/-`), sp("", fmtMoney(t[K_CU], t[K_CCY], 2, it[K_PK])));
      })));
  }
  if (tab === K_HS) {
    if (!d?.found) return msg(d?.error || nd);
    const list = (d[K_HS] || []).slice(0, 5), rec = d.recommended_distill_range, spath = cDist.get(cid)?.[K_SN], busy = loadingKey === `distill:${cid}`, mx = M.max(1, ...list.map((x) => num(x.delta_tokens)));
    const rows = list.length ? list.map((x) => { const dl = num(x.delta_tokens), r = dl / mx; return h("div", "r g8", { title: fmtK(x.context_after) }, sp("m s b k", `#${num(x.turn)}`), sp("f e t2", `s${num(x.step_idx)} · ${x.cause || ""}`), sp("ht", h("span", `bf${r >= .85 ? " er" : r >= .6 ? " wn" : ""}`, { s: `width:${M.max(8, M.round(r * 100))}%` })), sp("m s b cx", `+${fmtK(dl)}`)); }) : [dv("t2", "-")];
    return dv("bx", dv("lb", tr("Hotspots|膨胀热点")), ...rows, spath && dv("wr ok tc", "✓ ", h("code", "", 0, spath)), dv("ac ct0", mkB("s", "dst", busy ? L(0) : `${tr("Save|提炼")}${rec ? ` ${rec[0]}–${rec[1]}` : ""}`, cid, { disabled: busy }), spath && mkB("", "cdp", tr("Copy|复制"), cid)));
  }
  if (!d?.found) return msg(d?.reason || "N/A");
  const ok = d.gate_allowed, rs = num(it[K_RSC]), ds = loadingKey === `${K_HO}:${cid}`;
  return dv("bx", dv("ac", sp(`bd ${ok ? "ok" : "wn"}`, L(ok ? 4 : 3)), sp(`bd ${rs > 0 ? "wn" : "ok"}`, subTxt(rs))),
    dv("t2", `${thrs(it).map((v, i) => `${((num(v) || DEF_T[i]) / 1e3) | 0}k`).join("/")} · >=${num(d.break_even_turns)}t${d[K_PK] !== false ? ` · ${fmtMoney(d.compact_overhead_usd, d.compact_overhead_cny)} / -${fmtMoney(d.per_turn_saving_usd, d.per_turn_saving_cny, 3)}/t` : ""}`),
    ...[d.headline && dv("wr", d.headline), ...(d.gate_reasons || []).map((r) => dv("t2 wr", `· ${r}`))].filter(Boolean),
    dv("ac", ok ? mkB("s", "up", ds ? L(0) : L(22), cid, { disabled: ds }) : mkB("s", "cc", L(2)), ok && canMessage && !rs && sameWs(it[K_WS], it[K_WSS]) && mkB(pc0(!bnrPri), "ul", L(12), cid, { disabled: ds }), mkSw(L(6))));
};

const getLim = (it) => num(it[K_CL] || snapshot?.thresholds?.max) || 512e3;
const mountDetailBlock = () => {
  const hn = dv("hn", "0%"), hs = dv("m s t3"), gf = dv("bf"), tks = [sp("gk"), sp("gk"), sp("gk er")];
  const gt = h("div", "gt", { role: "progressbar", "aria-valuemin": 0, "aria-valuemax": 100 }, gf, ...tks);
  const g1 = sp("e"), g2 = sp("k"), kf = dv("bf"), k1 = sp("e"), k2 = sp("m k");
  const ws = [0, 1, 2].map(() => { const a = sp("lb"), b = sp("m b e"), c = sp("m s t2 e"); return [dv("pl", a, b, c), a, b, c]; });
  const tm = hdv("t2 wr"), wn = hdv("bnr wr"), [sg, sth, tbs] = mkSeg((i) => bt("sb", `t-${TABS[i]}`, "")), trE = hdv(), ft = hdv("ac je bt0");
  return [dv("dl", dv("hb", dv("c hl k", hn, hs), dv("f c g6", dv("c g4", gt, dv("jb s t3", g1, g2)), dv("c g4", dv("kt", kf), dv("jb s t2", k1, k2)))), dv("pls", ws[0][0], ws[1][0], ws[2][0]), tm, wn, sg, trE, ft), hn, hs, gt, gf, tks, g1, g2, kf, k1, k2, ws, tm, wn, sth, tbs, trE, ft, "", ""];
};

const patchDetailBlock = (d, it, showId, isHero = false) => {
  const cid = it.id || "", lvl = safeLvl(it.level), rs = num(it[K_RSC]), pk = it[K_PK], m0 = (a, b) => num(it[a]) || num(it[b]);
  const lim = getLim(it), ctx = num(it[K_LCT]), ck = fmtK(ctx), ckc = num(it.checkpoint_count);
  const pct = clp((ctx / lim) * 100), cpct = fx(it[K_CP] ?? pct, 1), rem = num(it.remaining_context_tokens ?? M.max(0, lim - ctx)), tn = lvlTone(lvl), tc = tn !== "ok" ? ` ${tn}` : "";
  const [, hn, hs, gt, gf, tks, g1, g2, kf, k1, k2, ws, tm, wn, sth, tbs, trE, ft] = d;
  setTxt(hn, `${cpct}%`); setCls(hn, `hn${tc}`); setTxt(hs, `${ck} / ${fmtK(lim)}`);
  ar(gt, "label", L(28)); ar(gt, "valuenow", M.round(pct)); setCls(gf, `bf${tc}`); gf.style.width = `${pct.toFixed(1)}%`;
  thrs(it).forEach((v, i) => { const n = num(v) || DEF_T[i]; tks[i].style.left = `${M.min(100, (n / lim) * 100).toFixed(1)}%`; tks[i].title = `${(n / 1e3) | 0}k`; });
  setTxt(g1, isZh() ? `余 ${fmtK(rem)}` : `${fmtK(rem)} left`);
  const jc = !!it[K_JC], kb = jc && num(it.compact_summary_kb) > 0 ? `~${fx(it.compact_summary_kb, 1)} KB` : "", pc = it.pre_compact_tokens;
  setTxt(g2, jc && (pc || kb) ? jDot(pc ? `${tr("Before|压缩前")} ${fmtK(pc)}` : "", kb) : `${num(it[K_TN])}${tr("t| 轮")}`);

  const chr = clp(num(it[K_CR])), cf = fx(chr, 1), ttft = m0("latest_ttft_ms", "median_ttft_ms"), stps = m0("median_stream_tps", "latest_stream_tps"), etps = num(it.e2e_tps);
  const s0 = fx(stps), e0 = fx(etps), sp0 = stps > 0 ? (etps > 0 ? `${s0}/${e0}` : s0) : "", cost = fmtMoney(it[K_CU], it[K_CCY], 2, pk);
  setCls(kf, `bf${chr >= 80 ? "" : chr >= 50 ? " wn" : " er"}`); kf.style.width = `${chr.toFixed(1)}%`;
  setTxt(k1, `${tr("Cache|缓存命中")} ${cf}%${isZh() ? "" : " hit"}`); setTxt(k2, `${cost} · ${sp0 ? `${sp0} tok/s` : "-"}`);

  const c = getCold(it), cN = num(it.cold_cache_count), mid = c.m > 0 && (c.s || c.r || c.w) ? `(${c.s}/${c.r}/${c.w})` : "";
  [
    [L(28), `${ck} · ${cpct}%`, `${tr("Peak|峰值")} ${fmtK(it.peak_context_tokens || ctx)}`],
    [tr("KV Cache|KV 缓存"), `${cf}%`, cN ? `${cN} ${tr("cold|冷")}` : "0", `m s e${cN && c.m > 0 ? " er tc" : " t2"}`, cN ? `${c.t}/${c.m}${mid}/${c.p}` : ""],
    [tr("Speed & Cost|速度与费用"), `TTFT ${fmtMs(ttft)} · ${sp0 ? `${sp0} t/s` : "-"}`, cost, "", jDot(stps > 0 && etps > 0 ? `${tr("Stream|流式")} ${s0} · E2E ${e0} t/s` : "", ckc ? `C×${ckc}` : "")],
  ].forEach(([lb, v1, v2, cl0 = "m s t2 e", tp = ""], i) => { const [el, l, a, b] = ws[i]; setTxt(l, lb); setTxt(a, v1); setTxt(b, v2); setCls(b, cl0); if (tp || el.title) el.title = tp; });

  const thk = num(it.total_thinking_tokens), outT = thk + num(it.total_response_tokens), ls = num(it.last_loop_steps), tp = M.round(num(it.thinking_share_pct ?? (outT > 0 ? (thk / outT) * 100 : 0)));
  const mt = jDot(thk > 0 ? `${tr("Out|输出")} ${fmtK(outT)} (${isZh() ? `思考 ${tp}%` : `${tp}% think`})` : "", ls > 0 ? `${tr(`Loop ${ls}×`, `本轮 ${ls} 步`)} · ${fmtK(m0("last_loop_output_tokens", "last_loop_thinking_tokens"))} · ${fmtMoney(it.last_loop_cost_usd, it.last_loop_cost_cny, 2, pk)}` : "");
  hd(tm, !mt); if (mt) setTxt(tm, mt);

  const em = S(it.live_error || "").trim(), nm = jDot(em, it.cross_model_compact ? tr("Cross-model /compact cannot reuse prior KV cache; slower rebuild expected.|跨模型 /compact 无法复用 KV 缓存，重算较慢。") : "");
  hd(wn, !nm); if (nm) { const k0 = em ? "er" : "wn"; setCls(wn, `bnr wr ${k0}`); sa(wn, "data-k", k0); setTxt(wn, nm); }

  const cur = openTrays.get(cid) || "", ti = TABS.indexOf(cur);
  hd(sth, ti < 0); if (ti >= 0) sth.style.transform = `translateX(${ti * 100}%)`;
  ["Turns|轮次", "Hotspots|热点", "Handoff|交接"].forEach((lb, i) => { setTxt(tbs[i], tr(lb)); tbs[i].dataset.i = cid; cl(tbs[i], "on", cur === TABS[i]); ar(tbs[i], "selected", S(cur === TABS[i])); });

  const tSig = cur ? [lang, cid, cur, loadingKey, subTrayRev, rs, allowAutoHandoff, canMessage, bnrPri] + "" : "";
  if (tSig !== d[18]) { d[18] = tSig; hd(trE, !cur); setCh(trE, cur ? [buildSubTray(it)] : []); }
  const dm = canDirectMsg(it), fSig = [lang, cid, lvl, !!it[K_SC], jc, !!it[K_IS], dm, confirmSendId === cid, showId, copiedId === cid, bnrPri, isHero] + "";
  if (fSig !== d[19]) {
    d[19] = fSig;
    const a = (it[K_SC] || lvl === K_CRIT) && !jc ? (it[K_IS] ? [mkB("s", "csr", tr("Copy baton|复制切棒"), cid)] : !dm ? [mkB("s", "cc", L(2)), mkB("", "crm", tr("Copy reminder|复制提醒"), cid)] : confirmSendId === cid ? [mkB("", "ask", tr("Cancel|取消")), mkB("s", "cc", L(2)), mkB(pc0(!bnrPri), "yes", tr("Confirm|确认"), cid)] : [mkB("s", "cc", L(2)), mkB(`s ${CM}`, "ask", tr("Remind|提醒"), cid), mkB(pc0(isHero && !bnrPri), "up", L(22), cid)]) : [];
    if (showId && cid) a.push(mkB("", "ci", L(copiedId === cid ? 1 : 11), cid, { title: cid }));
    hd(ft, !a.length); setCh(ft, a);
  }
};

const reconcile = (box, map, list, getId, mk, patch) => {
  const seen = new Set();
  list.forEach((item, i) => {
    const id = getId(item); seen.add(id);
    let rec = map.get(id); if (!rec) { rec = mk(id); map.set(id, rec); }
    patch(rec, item);
    if (box.children[i] !== rec[0]) box.insertBefore(rec[0], box.children[i] || null);
  });
  for (const [k, rec] of map) if (!seen.has(k)) { rec[0].remove(); map.delete(k); }
};

const patchOpen = (box, arr, idx, it) => { if (it) { if (!arr[idx]) { arr[idx] = mountDetailBlock(); box.appendChild(arr[idx][0]); } hd(box, 0); patchDetailBlock(arr[idx], it, true, false); } else if (!box.hidden) hd(box, 1); };
const clearMaps = () => { rowNodes.clear(); pinNodes.clear(); heroChipNodes.clear(); };
const dt0 = () => sp(`dt ${K_IE}`), id0 = () => bt("id", "ci", "", "", { h: 1 });
const ensureDashboardMount = (root) => {
  if (mountedMode === DB && ui) return;
  clearMaps(); mountedMode = DB;
  const [emb, dot] = mkEmb(), lg = mkLang(!isZh()), mkG = () => { const gh = hdv("lb"), ls = dv("cd ls"); return [hdv("c g6", gh, ls), gh, ls]; };
  uDot = dot; uSwp = hdv("swp"); uMi = hd0("span", "mi e"); uMt = sp("m s t3 e"); uAb = bt("id ct0", "aut", ""); [uEn, uZh] = lg.children;
  uRi = sv(14, ["M13 8a5 5 0 1 1-1.3-3.4M13 2.5v2.6h-2.6"]); uRb = bt("ib", "ref", uRi, "", al(L(20)));
  const hdEl = h("header", "hd", 0, emb, h("h1", "wm", 0, "ContextRadar"), dv("r f", uMi, uMt), dv("r k", uAb, lg, uRb), uSwp);

  const bDt = dt0(), bTt = sp("b e"), bId = id0(), bBg = hb("in"), bDs = bt("ib", "dis", "×", "", al(L(21))), bBd = hdv("c g4 t2"), bAc = hdv("ac je");
  const bnr = h("div", "bnr", { "data-k": "in", h: 1 }, dv("jb", dv("r f", bDt, bTt, bId), dv("r k", bBg, bDs)), bBd, bAc);
  uB = [bnr, bDt, bTt, bId, bBg, bDs, bBd, bAc, ""];

  const hDt = dt0(), hTt = sp("wm e"), hMd = hb("t3"), hPh = hb(), hCw = hb("wn"), hVd = hb(), hId = id0();
  const hBlk = mountDetailBlock(), hCh = dv("r w"), hST = hdv("ty"), hDk = hdv("dk bt0", hCh, hST);
  const he = hd0("section", "cd hero", dv("jb", dv("r f", hDt, hTt), dv("ac je", hMd, hPh, hCw, hVd, hId)), hBlk[0], hDk);
  uH = [he, hDt, hTt, hMd, hPh, hCw, hVd, hId, hBlk, hDk, hCh, hST, null];

  const [sg, sth, tabs] = mkSeg((i) => bt("sb", "tab", "", FIDS[i], { role: "tab" })), sw = mkSw(""), br = hdv("jb w", sg, sw);
  uF = [br, sg, sth, tabs, sw, sw.lastElementChild];
  [uPG, uPGh, uPLs] = mkG(); [uRG, uRGh, uLs] = mkG();
  const emP = dv(""), emB = mkB("s", "ref", L(20)), em = hdv("cd em", mkEmb(0, 1)[0], emP, emB);
  uEm = [em, emP, emB]; setCh(root, [dv("c sh", hdEl, bnr, he, br, dv("c g10", uPG, uRG), em)]); ui = { he };
};

const ensureHandoffMount = (root) => { if (mountedMode === HC && ui) return; clearMaps(); lastHandoffSig = ""; mountedMode = HC; uHl = dv(""); setCh(root, [h("div", "c sh", { s: "padding:4px" }, uHl)]); ui = { he: null }; };

const errTxt = (s) => s === "Closed" ? tr("Closed|已断开") : s === "ui/initialize" ? tr("Timeout|连接超时") : s;
const patchBannerSlot = () => {
  const [bnr, bDt, bTt, bId, bBg, bDs, bBd, bAc] = uB;
  bnrPri = false;
  const g = !errorText && pg();
  if (!errorText && (!g || !sameWs(g[K_WS], g[K_WSS]))) return hd(bnr, 1);
  const setK = (k) => { hd(bnr, 0); setCls(bnr, `bnr ${k}`); sa(bnr, "data-k", k); ar(bDs, "label", L(21)); };
  if (errorText) { setK("er"); setCls(bDt, "dt err"); setTxt(bTt, errTxt(errorText)); patchBdg(bBg, "er", L(19)); bDs.dataset.i = ""; hdA(bId, bBd, bAc); uB[8] = ""; return; }
  const gid = g[K_GID], nc = g[K_HCID] || "", cs = g[K_CS] || "", cid = g[K_CID], isCmp = g.kind === K_CMP;
  const busy = busyGrantId === gid, rs = num(g[K_RSC]), cDone = cs === "sent" || cs === "acp_compacted", done = !!(nc || cDone);
  const canN = !done && !rs && !busy && canCallTools && !!gid, canC = canN && !!g[K_CC] && canDirectCid(cid, g[K_WS], g[K_WSS]);
  bnrPri = canC || canN; setK(done ? "ok" : rs > 0 ? "wn" : "ac"); setCls(bDt, `dt ${done || !rs ? "run" : "wait"}`);
  setTxt(bTt, L(nc ? 15 : cDone ? 16 : isCmp ? 17 : 18)); patchId(bId, cid, !!cid, true);
  patchBdg(bBg, done ? "ok" : rs > 0 ? "wn" : "in", done ? tr("Done|已完成") : rs > 0 ? subTxt(rs) : busy ? L(isCmp ? 10 : 5) : tr("Pending|待触发"));
  bDs.dataset.i = gid;
  const sig = [lang, gid, nc, cs, busy, rs, canC, canN, copiedId, g[K_PP], g[K_RT]] + "";
  if (sig !== uB[8]) { uB[8] = sig; const ls = relayNotes(g), bs = relayBtns(g, false, canC, canN, gid); hd(bBd, !ls.length); setCh(bBd, ls); hd(bAc, !bs.length); setCh(bAc, bs); }
};

const createRowEntry = (cid) => {
  const cs = [sp("t3 s k", "▸"), dt0(), hb("ac"), hb("t3"), sp("rt e"), hb(), hb(), sp("cx"), sp("ag")];
  const r = h("div", "rw", { role: "button", tabindex: 0, "data-a": "row", "data-i": cid }, ...cs), trB = hdv("ty");
  return [h("div", "it", { "data-i": cid }, r, trB), r, cs, trB, null];
};

const patchRowList = (box, items, map) => {
  if (!box) return;
  hd(box, !items.length);
  reconcile(box, map, items, ([it]) => it.id, createRowEntry, (rec, [it, isSub]) => {
    const [, r, [c, d, p, sb, t, pEl, vEl, x, a], trB] = rec, cid = it.id, lvl = safeLvl(it.level), tn = lvlTone(lvl), open = openRowId === cid, v = getVerdict(it), pb = getLivePhaseBadge(it);
    setCls(r, `rw${isSub ? " sub" : ""}${lvlCls(lvl) ? ` ${lvlCls(lvl)}` : ""}`); ar(r, "expanded", S(open));
    setTxt(c, open ? "▾" : "▸"); setCls(d, `dt ${getLiveDot(it)}`); patchBdg(pEl, pb?.cls || "", pb?.text || "");
    patchBdg(p, "ac", !isSub && it.pinned ? L(25) : ""); patchBdg(sb, "t3", isSub ? L(29) : "");
    setTxt(t, it.title); t.title = `${it.title} (${cid})`;
    patchBdg(vEl, v.cls, v.text); setCls(x, `cx${tn !== "ok" ? ` ${tn}` : ""}`);
    setTxt(x, fmtK(it[K_LCT])); setTxt(a, fmtAge(it.sidebar_sort_ts || it.effective_last_active_ts || it.last_active_ts));
    patchOpen(trB, rec, 4, open ? it : null);
  });
};

const patchHeroDock = (subs) => {
  hd(uH[9], !subs.length);
  let openSub = null;
  reconcile(uH[10], heroChipNodes, subs, (s) => s.id, (cid) => { const dt = dt0(), nm = sp("e"), pc = sp("m"); return [bt("id sc", "row", [dt, nm, pc], cid), dt, nm, pc]; }, (rec, sub) => {
    const cid = sub.id, open = openRowId === cid, pct = sub[K_CP] ?? clp((num(sub[K_LCT]) / getLim(sub)) * 100);
    if (open) openSub = sub;
    setCls(rec[1], `dt ${getLiveDot(sub)}`); setTxt(rec[2], sub[K_AN] || sub.title); setTxt(rec[3], `${fx(pct, 0)}%`);
    rec[0].title = `${sub.title} · ${fmtK(sub[K_LCT])}`; cl(rec[0], "on", open); ar(rec[0], "expanded", S(open));
  });
  patchOpen(uH[11], uH, 12, openSub);
};

const patchHeroSlot = (hero, heroSubs = [], cwN = 0, showCw = false) => {
  const [he, hDt, hTt, hMd, hPh, hCw, hVd, hId, hBlk] = uH;
  if (!hero) { hd(he, 1); return patchHeroDock([]); }
  const cid = hero.id || "", lvl = safeLvl(hero.level), v = getVerdict(hero), pb = getLivePhaseBadge(hero);
  hd(he, 0); ar(he, "label", tr("Focus|当前焦点")); setCls(he, `cd hero${lvlCls(lvl) ? ` ${lvlCls(lvl)}` : ""}`);
  setCls(hDt, `dt ${getLiveDot(hero)}`); patchBdg(hPh, pb?.cls || "", pb?.text || ""); setTxt(hTt, hero.title); hTt.title = `${hero.title} (${cid})`;
  patchBdg(hMd, "t3", hero.model_short || hero.model_display);
  patchBdg(hCw, "wn", showCw ? `${cwN}× Carbon` : "", showCw ? `${tr("Concurrency tip:|并发提示：")} ${cwN}× Carbon (<=2)` : "");
  patchBdg(hVd, v.cls, v.text); patchId(hId, cid, true, false);
  patchDetailBlock(hBlk, hero, false, true); patchHeroDock(heroSubs);
};

const patchSessionList = (items) => patchRowList(uLs, items, rowNodes);

const isHandoffLocked = () => /^(sen|com|old)/.test(handoffStatus);
const patchHandoffView = () => {
  const c = handoffCard, sig = c ? [lang, c.mode, c.blocked, c[K_GID], handoffStatus, c[K_HCID], c[K_CS], copiedId, microMsg, canCallTools] + "" : lang;
  if (sig === lastHandoffSig) return;
  lastHandoffSig = sig;
  if (!c) return setCh(uHl, [dv("cd em", mkEmb(0, 1)[0], dv("", L(0)))]);
  const card = (dc, tt, bc, bt0, ns, as) => dv("cd", dv("jb", dv("r f", mkEmb(dc)[0], sp("wm e", tt), mkId(c[K_CID], 1)), dv("ac je", mkLang(!isZh()), sp(`bd ${bc}`, bt0))), ns.length && dv("pl g4 t2", ...ns), as.length && dv("ac je bt0", ...as));
  if (c.mode === HB || c.blocked) return setCh(uHl, [card(0, L(13), "in", L(3), (c.reasons || []).map((r) => dv("wr", `· ${r}`)), [])]);
  const rs = num(c[K_RSC]), done = handoffStatus === "sent" || handoffStatus === "compacted", isCmp = c.kind === K_CMP, rL = L(isCmp ? 10 : 5);
  const [bCls, bTxt] = ({ sending: ["wn", rL], sent: ["ok", L(15)], compacted: ["ok", L(16)], old: ["t3", L(14)] })[handoffStatus] || ["in", handoffStatus === K_PD && c[K_AAH] ? rL : rs > 0 ? subTxt(rs) : L(4)];
  const canA = !c[K_HCID] && !rs && !isHandoffLocked() && canCallTools && !!c[K_GID];
  setCh(uHl, [card(done ? "run" : "wait", isCmp ? L(17) : tr("Handoff|交接"), bCls, bTxt, relayNotes(c, c.scratch_note_path), [...relayBtns(c, true, canA && !!c[K_CC], canA), microMsg && sp("mi", microMsg)].filter(Boolean))]);
};

const render = () => {
  const root = D.getElementById("root"); if (!root) return;
  if (viewMode === HC) { ensureHandoffMount(root); return patchHandoffView(); }

  ensureDashboardMount(root);
  const snap = snapshot, pollSec = M.round(pollMs / 1e3), [br, sg, sth, tabs, sw, swL] = uF, [em, emP, emB] = uEm, en = !isZh();
  const clk = snap ? fmtClock(snap[K_GA]) : loadingInitial ? L(0) : "-", netTip = `${clk} · ${pollSec}s · ${serverMs ?? "-"}/${ewmaRttMs ?? "-"}ms`;

  [uEn, uZh].forEach((b, i) => { const on = !i === en; cl(b, "on", on); ar(b, "pressed", S(on)); }); ar(uRb, "label", L(20));
  hd(uMi, !microMsg); if (microMsg) setTxt(uMi, microMsg);
  setTxt(uMt, clk); uAb.title = uMt.title = uRb.title = netTip; cl(uRi, "spn", showSpinner);
  setTxt(uAb, `${tr("Auto|自动")} ${pollSec}s`); cl(uAb, "ac", autoRefresh);
  hd(uSwp, !(loadingInitial || (refreshing && !snap)));

  patchBannerSlot();
  if (!snap) { setCls(uDot, `dt ${errorText ? "err" : K_IE}`); hdA(ui.he, br, uPG, uRG); hd(em, 0); setTxt(emP, L(loadingInitial ? 0 : 24)); setTxt(emB, L(20)); hd(emB, loadingInitial); return; }

  const s = snap.summary, mains = snap.sessions || [], isAct = filterTab === K_AC, showSubs = filterTab === "subs", isArch = filterTab === K_AR;
  const hero = isAct ? getHero() : null, heroSubs = hero ? (hero[K_SA] || []).filter((x) => x[K_ACT]) : [];
  const cwN = num(s.active_carbon_count), showCw = !!(s.carbon_concurrency_warning && cwN >= 3);
  setCls(uDot, `dt ${errorText ? "err" : hero ? getLiveDot(hero) : mains.some((x) => x[K_ACT]) ? "run" : K_IE}`);

  const pinItems = [], recItems = [], orpItems = [], pushS = (dst, a) => (a || []).forEach((x) => (showSubs || x[K_ACT]) && dst.push([x, true]));
  let pinCnt = 0, recCnt = 0, arN = 0;
  for (const m of mains) {
    if (m[K_AR]) arN++;
    if (isAct ? m[K_AR] || (hero && m.id === hero.id) : showSubs ? m[K_AR] || !(m[K_SA] || []).length : !m[K_AR]) continue;
    const dst = isAct && m.pinned ? (pinCnt++, pinItems) : (recCnt++, recItems);
    dst.push([m, false]); pushS(dst, m[K_SA]);
  }
  if (!isArch) pushS(orpItems, snap[K_OS]);

  patchHeroSlot(hero, heroSubs, cwN, showCw);
  hd(br, 0); ar(sg, "label", tr("Filter|筛选")); sth.style.transform = `translateX(${M.max(0, FIDS.indexOf(filterTab)) * 100}%)`;
  const cnts = [arN ? mains.length - arN : s.shown_main_count, num(s.shown_subagent_count) + num(s.history_subagents_hidden_count), arN || s.archived_hidden_count], lbls = ["Sessions|会话", "Subagents|子代理", "Archived|归档"];
  FIDS.forEach((id, i) => { const k = num(cnts[i]), on = filterTab === id; setTxt(tabs[i], `${tr(lbls[i])}${k ? ` (${k})` : ""}`); cl(tabs[i], "on", on); ar(tabs[i], "selected", S(on)); });
  cl(sw, "on", allowAutoHandoff); ar(sw, "checked", S(allowAutoHandoff)); sw.title = L(7); setTxt(swL, L(6));

  const split = isAct && pinCnt > 0 && recCnt > 0, all = split ? [...recItems, ...orpItems] : [...pinItems, ...recItems, ...orpItems];
  hd(uPG, !split); hd(uPGh, !split); if (split) setTxt(uPGh, `${L(25)} · ${pinCnt}`); patchRowList(uPLs, split ? pinItems : [], pinNodes);
  hd(uRG, !all.length); hd(uRGh, !split); if (split) setTxt(uRGh, `${L(26)} · ${recCnt}`); patchSessionList(all);
  hd(em, hero || pinItems.length || all.length);
  if (!em.hidden) { setTxt(emP, [L(24), tr("No subagents|暂无子代理"), tr("No archived|暂无归档")][FIDS.indexOf(filterTab)]); setTxt(emB, L(20)); hd(emB, 0); }
};

const flashMicro = (msg) => { if (!msg) return; microMsg = msg; render(); ct(microTimer); microTimer = st(() => { microMsg = ""; render(); }, 2200); };
const copyText = async (text, okMsg) => {
  let ok = false;
  try { await navigator.clipboard.writeText(text); ok = true; } catch { const ta = h("textarea", "", { readonly: 1, s: "position:fixed;opacity:0" }); ta.value = text; D.body.appendChild(ta); ta.select(); try { ok = D.execCommand("copy"); } catch {} ta.remove(); }
  flashMicro(ok ? okMsg : L(27)); return ok;
};
const cpCompact = (cmd = K_SCMP) => copyText(cmd, `${L(1)} ${K_SCMP}`);
const copyCid = async (cid) => { if (await copyText(cid, `${L(1)} ID`)) { copiedId = cid; render(); ct(copiedTimer); copiedTimer = st(() => { if (copiedId === cid) { copiedId = null; render(); } }, 1500); } };
const findSession = (cid) => snapshot && snapshot.sessions.flatMap((s) => [s, ...(s[K_SA] || [])]).concat(snapshot[K_OS] || []).find((x) => x.id === cid);
const extractRelayCid = (r) => S(r?._meta?.[AG_M]?.conversationId || "").trim();

const recAct = (action, gid, extra) => canCallTools && gid ? uiAct({ action, grant_id: gid, ...extra }).catch(() => {}) : 0;
async function executeDispatch(p, claimMode, isInline, isCmp = false) {
  cancelAutoCmp();
  const gid = p[K_GID] || "", cmd = p[K_CC] || "/compact", cid = p[K_CID] || "", ws = p[K_WS] || "", dws = snapshot?.[K_DW];
  const txt = isCmp ? cmd : p[K_HP] || "", isMan = claimMode.startsWith("manual");
  if (!txt) return;
  const fail = (msg) => { if (isInline) handoffStatus = "manual"; msg ? flashMicro(msg) : render(); }, mf = (m) => fail(isMan ? m : ""), ow = L(23);
  if (!canMessage || (isCmp && !isInline && !canDirectCid(cid, ws, p[K_WSS]))) { if (isMan) await cpCompact(cmd); return fail(""); }
  if (!isInline && !sameWs(ws, p[K_WSS])) { if (isMan) await copyText(cmd); return mf(ow); }
  if ((p[K_RSC] || 0) > 0) return mf(L(9));
  if (!canCallTools || !gid) { if (isMan) await cpCompact(cmd); return mf(L(14)); }
  try {
    const sc = await uiSc({ action: "claim_grant", [K_GID]: gid, [K_CID]: cid, [K_WS]: isInline ? ws || dws : dws || ws, claim_mode: claimMode });
    if (!sc?.claimed) {
      const r = sc?.reason || "";
      if (isInline) { const nc = sc?.[K_HCID]; if (nc && handoffCard) { handoffCard[K_HCID] = nc; handoffStatus = "sent"; } else handoffStatus = /^(already_|session_adv)/.test(r) ? "old" : "manual"; return render(); }
      return mf(r === "running_subagents_active" ? L(9) : r.endsWith("mismatch") ? ow : L(14));
    }
  } catch { return mf(L(27)); }
  if (isInline) handoffStatus = "sending"; else busyGrantId = gid || "pending";
  render();
  const dst = isInline ? handoffCard : pg()?.[K_GID] === gid ? pg() : null, rc = (st0) => recAct(K_RC, gid, { status: st0 });
  const failCmp = async (st0, msg) => { if (isCmp) { await rc(st0); if (dst) dst[K_CS] = st0; await cpCompact(cmd); } fail(msg); };
  const clip = (v) => S(v || "").trim().slice(0, 120);
  try {
    const res = await sendTxt(txt, isCmp ? 0 : { [AG_M]: { target: "new" } });
    if (res?.[K_ERR]) { const d = clip(res?.content?.[0]?.text || res?.error?.message); return await failCmp(`fallback_copied${d ? `:${d}` : ""}`, `${L(1)} /compact`); }
    const newCid = isCmp ? "" : extractRelayCid(res);
    await (isCmp ? rc("sent") : recAct("record_relay", gid, { [K_HCID]: newCid }));
    if (dst) { dst[isCmp ? K_CS : K_HCID] = isCmp ? "sent" : newCid; if (!isInline) dst.claimed = true; }
    if (isInline) handoffStatus = isCmp ? "compacted" : "sent";
    flashMicro(`✓ ${L(isCmp ? 16 : 15)}`);
  } catch (err) { await failCmp(`error:${clip(err?.message || err || "failed")}`, L(27)); }
  finally { busyGrantId = null; render(); }
}

function canAutoRelay(p, isInline) {
  if (!p?.[K_GID] || !canMessage || !canCallTools || D.hidden) return "";
  const isCmp = p.kind === "compact", gid = p[K_GID], cid = p[K_CID];
  if (isCmp ? (!p[K_CC] || p.acp_proxy_active) : !p[K_HP]) return "";
  if (!isInline && (isCmp ? !canDirectCid(cid, p[K_WS], p[K_WSS]) : (!sameWs(p[K_WS], p[K_WSS]) || (ownerCid ? ownerCid !== cid : (!snapshot?.[K_DW] || num(snapshot?.summary?.running_main_count) > 1))))) return "";
  if (!(isInline ? !!p[K_AAH] : !!(allowAutoHandoff && p[K_AAH] !== false)) || p.auto_fire === false || p.claimed || p[K_HCID] || p[K_CS] || (p[K_RSC] ?? 0) > 0 || attemptedGrants.has(gid)) return "";
  return isCmp ? "auto_compact" : "auto_handoff";
}

function maybeAutoRelay(p, isInline) {
  const mode = canAutoRelay(p, isInline);
  if (!mode) return;
  const gid = p[K_GID], isCmp = mode === "auto_compact";
  attemptedGrants.add(gid);
  if (!isCmp) return void executeDispatch(p, mode, isInline, false);
  cancelAutoCmp();
  autoCmpTimer = st(() => {
    autoCmpTimer = null;
    const cur = isInline ? handoffCard : pg();
    if (!cur || cur[K_GID] !== gid || cur.claimed || cur[K_CS] || cur[K_HCID] || (cur[K_RSC] ?? 0) > 0 || (isInline && isHandoffLocked())) return;
    if (D.hidden) { attemptedGrants.delete(gid); return; }
    void executeDispatch(cur, mode, isInline, true);
  }, isInline ? 1500 : 2000);
}

function applySnapshot(snap) {
  if (typeof snap[K_ST]?.[K_AAH] === "boolean") allowAutoHandoff = snap[K_ST][K_AAH];
  if (!langLocked && snap[K_ST]?.lang) { const p = parseLoc(snap[K_ST].lang); if (p) DE.lang = lang = p; }
  if (snap[K_DIG]) lastDigest = snap[K_DIG];
  snapshot = snap; maybeAutoRelay(snap[K_PG], false);
}

const nextPollMs = () => { const r = ewmaRttMs ?? 0, b = (D.hidden ? [12, 8, 4, 2] : [10, 5, 2.5, 1])[r > 800 ? 0 : r > 350 ? 1 : r > 150 ? 2 : 3] * 1e3; return pollErrors > 0 ? M.min(45e3, b * (1 << M.min(pollErrors, 3))) : b; };
const cancelPoll = () => { if (pollTimer !== null) { ct(pollTimer); pollTimer = null; } };
const scheduleNextPoll = () => { cancelPoll(); bgHidden = !!D.hidden; if (viewMode !== DB || !autoRefresh || !canCallTools) return; pollMs = nextPollMs(); pollTimer = st(() => { pollTimer = null; void refreshSnapshot(false); }, pollMs); };

async function refreshSnapshot(force = false, showSynced = false) {
  if (viewMode !== DB || !canCallTools) return;
  if (inFlightPoll) { if (force) queuedForce = true; return; }
  cancelPoll(); inFlightPoll = true; ct(spinTimer);
  if (force || !snapshot) { refreshing = true; spinTimer = st(() => { if (refreshing) { showSpinner = true; render(); } }, 150); }
  const t0 = performance.now();
  try {
    const isAr = filterTab === K_AR, args = { action: K_SN0, limit: isAr ? 50 : 25, show_history: filterTab === "subs", include_archived: isAr };
    if (force) args.fresh = true; else if (lastDigest) args.if_digest = lastDigest;
    const res = await callTool("context_radar_overview", args, { timeout: 15e3 });
    const rz = (x) => M.max(0, M.round(x)), sc = res?.[K_SC2], srvMs = rz(num(sc?.[K_SM])), netRtt = rz(performance.now() - t0 - srvMs);
    serverMs = srvMs; ewmaRttMs = ewmaRttMs === null ? netRtt : M.round(ewmaRttMs * .65 + netRtt * .35);
    pollErrors = 0; lastSyncTs = now(); ct(spinTimer); showSpinner = false;
    const finish = () => { refreshing = false; showSynced ? flashMicro(tr("✓ Synced|✓ 已同步")) : render(); };
    if (res?.[K_ERR]) { errorText = L(27); refreshing = false; render(); }
    else if (sc?.unchanged || (sc?.[K_DIG] && sc[K_DIG] === lastDigest && snapshot)) {
      const hadErr = !!errorText; unchangedHits += 1; errorText = "";
      if (sc?.[K_GA] && snapshot) snapshot[K_GA] = sc[K_GA];
      if (refreshing) finish(); else if (hadErr) render();
    } else if (sc) { applySnapshot(sc); loadingInitial = false; errorText = ""; finish(); }
  } catch (err) {
    pollErrors += 1; errorText = `${err?.message || err}`; ct(spinTimer); showSpinner = false; refreshing = false; render();
  } finally { inFlightPoll = false; queuedForce ? ((queuedForce = false), void refreshSnapshot(true, false)) : scheduleNextPoll(); }
}

const ERT = { estimated_remaining_turns: 12 };
const withBusy = async (key, fn) => { if (!canCallTools) return; loadingKey = key; render(); try { await fn(); } catch { flashMicro(L(27)); } finally { loadingKey = ""; render(); } };
const openTray = (cid, tab) => {
  if (openTrays.get(cid) === tab) { openTrays.delete(cid); return render(); }
  openTrays.set(cid, tab);
  const [m, a] = { [K_TN]: [`${K_PF}${K_TN}`, { limit: 24 }], [K_HS]: [`${K_PF}${K_HS}`, { mode: K_HS, top_k: 5 }], advice: ["radar_prepare_handoff", ERT] }[tab];
  return withBusy(`${tab}:${cid}`, async () => { const res = await callTool(m, { [K_CID]: cid, ...a }); if (!res?.[K_ERR] && res?.[K_SC2]) { trayMap[tab].set(cid, res[K_SC2]); subTrayRev += 1; } });
};
const runDistill = (cid) => cid && withBusy(`distill:${cid}`, async () => {
  const rec = cHot.get(cid)?.recommended_distill_range, res = await uiAct({ action: "distill_slice", [K_CID]: cid, ...(rec?.length === 2 ? { start_step: rec[0], end_step: rec[1] } : {}) }), sc = res?.[K_SC2];
  if (!res?.[K_ERR] && sc?.found && sc?.[K_SN]) { cDist.set(cid, sc); subTrayRev += 1; flashMicro(tr("✓ Saved|✓ 已保存")); } else flashMicro(sc?.error || res?.content?.[0]?.text || L(27));
});
const prepareOrLaunch = (cid, launchNow) => cid && withBusy(`${K_HO}:${cid}`, async () => {
  const sn = cDist.get(cid)?.[K_SN], sc = await uiSc({ action: "create_handoff", kind: launchNow ? K_HO : K_CMP, [K_CID]: cid, auto_fire: false, ...ERT, ...(sn ? { scratch_note_path: sn } : {}) });
  if (!sc || sc.blocked || sc.mode === HB) return flashMicro(sc?.reasons?.[0] || L(13));
  if (snapshot) snapshot[K_PG] = sc; subTrayRev += 1;
  if (launchNow) { loadingKey = ""; await executeDispatch(sc, K_MH, false, false); } else await cpCompact(sc[K_CC]);
});
const setLangPref = (t) => { if (t === lang && langLocked) return; DE.lang = lang = t; langLocked = true; ls0(lang); subTrayRev += 1; render(); if (canCallTools) void uiAct({ action: K_SP, lang }).catch(() => {}); };
const toggleAutoHandoff = async () => {
  const prev = allowAutoHandoff, next = !prev;
  allowAutoHandoff = next; if (!next) cancelAutoCmp(); render(); if (!canCallTools) return;
  try {
    const res = await uiAct({ action: K_SP, [K_AAH]: next }), v = res?.[K_SC2]?.[K_ST]?.[K_AAH];
    if (res?.[K_ERR] || typeof v !== "boolean") throw new Error(res?.content?.[0]?.text || L(27));
    allowAutoHandoff = v; flashMicro(tr(v ? "✓ On|✓ 已开" : "✓ Off|✓ 已关"));
    if (v && pg()) { pg()[K_AAH] = true; maybeAutoRelay(pg(), false); }
    render();
  } catch (e) { allowAutoHandoff = prev; flashMicro(e?.message || L(27)); render(); }
};
const buildReminder = (it) => `${L(28)} "${it.title}" ${fmtK(it[K_LCT])}/${fmtK(getLim(it))} (>${((num(it[K_RCT]) || 31e4) / 1e3) | 0}k); ${tr("wrap up or /compact.|请收尾或 /compact。")}`;
const setFilterTab = (next, focus = false) => { if (filterTab === next) return; filterTab = next; lastDigest = ""; render(); if (focus) D.querySelector(`[data-a="tab"][data-i="${next}"]`)?.focus(); void refreshSnapshot(true, false); };

const ACT = {
  ci: (id, ev) => { ev.preventDefault(); copyCid(id); }, lng: setLangPref, ref: async () => { if (await ensureConnected()) void refreshSnapshot(true, true); },
  aut: () => { autoRefresh = !autoRefresh; autoRefresh ? scheduleNextPoll() : cancelPoll(); render(); },
  tab: (id) => id && setFilterTab(id), aho: toggleAutoHandoff, row: (id) => { if (id) { openRowId = openRowId === id ? null : id; render(); } }, dst: runDistill,
  cdp: (id) => { const p = cDist.get(id)?.[K_SN]; p && copyText(p, L(1)); },
  up: (id) => prepareOrLaunch(id, false), ul: (id) => prepareOrLaunch(id, true),
  dis: (id) => { if (!id) { errorText = ""; return render(); } cancelAutoCmp(); if (pg()?.[K_GID] === id) { delete snapshot[K_PG]; render(); } recAct("dismiss_grant", id, {}); },
  ask: (id) => { confirmSendId = id || null; render(); },
};
onD("click", async (ev) => {
  const el = ev.target?.closest?.("[data-a]"); if (!el) return; ev.stopPropagation();
  const { a, i: id = "" } = el.dataset;
  if (a.startsWith("t-") && id) return openTray(id, a.slice(2));
  if (ACT[a]) return ACT[a](id, ev);
  if (/^c[cp]?c$/.test(a)) { const m = a === "cc" ? K_SCMP : (a === "ccc" ? handoffCard : pg())?.[K_CC]; return m && cpCompact(m); }
  if (/^[cr][01]$/.test(a)) { const isIn = a[1] === "0", isCmp = a[0] === "c", g = isIn ? handoffCard : pg(); return g && executeDispatch(g, isCmp ? "manual_compact" : K_MH, isIn, isCmp); }
  const it = findSession(id); if (!it) return;
  if (a === "crm") copyText(buildReminder(it), L(8));
  else if (a === "csr") copyText(`${L(29)} ${it[K_AN] || it.title} (${id}) ${fmtK(it[K_LCT])} ${tr("pass baton|请切棒")}`, L(1));
  else if (a === "yes" && !it[K_IS]) {
    confirmSendId = null; const msg = buildReminder(it);
    if (canDirectMsg(it)) try { if (!(await sendTxt(msg))?.[K_ERR]) return flashMicro(tr("✓ Sent|✓ 已发送")); } catch {}
    await copyText(msg, L(8)); render();
  }
});
onD("keydown", (ev) => {
  if (ev.key === "Escape") { if (confirmSendId || openTrays.size || openRowId) { confirmSendId = openRowId = null; openTrays.clear(); render(); } return; }
  const el = ev.target?.closest?.("[data-a]"); if (!el) return;
  if (/^(Enter| )$/.test(ev.key) && el.dataset.a === "row") { ev.preventDefault(); ACT.row(el.dataset.i || ""); }
  else if (/^Arrow(Left|Right)$/.test(ev.key) && el.dataset.a === "tab") { ev.preventDefault(); setFilterTab(FIDS[(FIDS.indexOf(filterTab) + (ev.key === "ArrowRight" ? 1 : 2)) % 3], true); }
});

const canForceDb = () => !D.hidden && viewMode === DB && canCallTools;
const forceNow = () => { const t = now(); if (t - lastForceTs < 250) return; lastForceTs = t; void refreshSnapshot(true, false); };
const kickRelay = () => viewMode === HC ? handoffCard && !handoffCard.blocked && maybeAutoRelay(handoffCard, true) : pg() && maybeAutoRelay(pg(), false);
const syncNow = () => { bgHidden = !!D.hidden; if (!D.hidden) kickRelay(); if (viewMode === DB && canCallTools) !D.hidden ? forceNow() : autoRefresh && scheduleNextPoll(); render(); };
onD("visibilitychange", syncNow);
onD("pointerenter", () => { if (canForceDb() && now() - lastSyncTs > 900) forceNow(); }, { passive: true });
["focus", "pageshow"].forEach((e) => window.addEventListener(e, () => canForceDb() && forceNow()));

let obsRo = null, obsIo = null;
function setupRevealObservers() {
  const dim = () => DE.clientWidth > 0 && DE.clientHeight > 0;
  if (typeof ResizeObserver < "u") {
    let col = !dim();
    obsRo = new ResizeObserver((es) => { const r = es[es.length - 1]?.contentRect, vis = r ? r.width > 0 && r.height > 0 : dim(); if (col && vis && canForceDb()) forceNow(); col = !vis; });
    obsRo.observe(DE);
  }
  if (typeof IntersectionObserver < "u") {
    let was = dim();
    obsIo = new IntersectionObserver((es) => { const vis = !!es[es.length - 1]?.isIntersecting; if (!was && vis && canForceDb()) forceNow(); was = vis; }, { threshold: 0 });
    obsIo.observe(DE);
  }
}

const syncLoc = (r) => { if (!langLocked && r) { const p = parseLoc(r); if (p && p !== lang) { DE.lang = lang = p; return 1; } } };
bridge.ontoolinput = (p) => { const c = p?.arguments?.[K_CID]?.trim?.(); if (c) ownerCid = c; if (!snapshot && viewMode === DB) { loadingInitial = true; render(); } };
bridge.ontoolresult = (result) => {
  if (result?.[K_ERR]) { errorText = L(27); loadingInitial = false; render(); return; }
  const sc = result?.[K_SC2], m = sc?.mode;
  if (m === HC || m === HB) {
    viewMode = HC; handoffCard = sc; if (sc[K_CID]) ownerCid = sc[K_CID]; syncLoc(sc.lang);
    if (typeof sc[K_AAH] === "boolean") allowAutoHandoff = sc[K_AAH];
    handoffStatus = m === HB ? "blocked" : sc[K_AAH] && sc.auto_fire !== false && !sc[K_RSC] && !sc.acp_proxy_active ? K_PD : K_MN;
    loadingInitial = false; cancelPoll(); render(); if (m === HC) maybeAutoRelay(sc, true); return;
  }
  if (m === K_SN0) {
    viewMode = DB; loadingInitial = false; errorText = ""; lastSyncTs = now();
    if (sc[K_SM] !== undefined) serverMs = M.max(0, M.round(num(sc[K_SM])));
    scheduleNextPoll(); if (sc.unchanged || (sc[K_DIG] && sc[K_DIG] === lastDigest && snapshot)) return;
    applySnapshot(sc); render();
  }
};

function parseColorLum(raw) {
  const s = S(raw || "").trim(), p = (x) => parseInt(x, 16);
  if (!s || /^transparent$/i.test(s)) return null;
  const h0 = s.match(/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i)?.[1];
  if (h0) { const k = h0.length, x = (i) => k <= 4 ? p(h0[i] + h0[i]) : p(h0.slice(i * 2, i * 2 + 2)); if ((k === 4 || k === 8) && x(3) < 128) return null; return (.2126 * x(0) + .7152 * x(1) + .0722 * x(2)) / 255; }
  const m = s.match(/^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:\s*[,/]\s*([\d.]+%?))?\s*\)/i);
  if (!m) return null;
  if (m[4]) { const a = parseFloat(m[4]) / (m[4].endsWith("%") ? 100 : 1); if (Number.isNaN(a) || a < .5) return null; }
  return (.2126 * num(m[1]) + .7152 * num(m[2]) + .0722 * num(m[3])) / 255;
}

const syncHostContext = (ctx) => {
  const st0 = ctx?.styles, vs = st0?.variables;
  vs && applyHostStyleVariables(vs); st0?.css?.fonts && applyHostFonts(st0.css.fonts);
  syncLoc(ctx?.locale) && render();
  const raw = S(ctx?.theme || "");
  let res = /dark|night|black/i.test(raw) ? "dark" : /light|day|white/i.test(raw) ? "light" : null;
  if (!res) { const lum = parseColorLum(vs?.[BG_V]) ?? parseColorLum(getComputedStyle(DE).getPropertyValue(BG_V)); if (lum !== null) res = lum < .45 ? "dark" : "light"; }
  res && applyDocumentTheme(res);
};

bridge.onhostcontextchanged = (ctx) => { syncHostContext(ctx); if ((ctx?.containerDimensions || ctx?.displayMode) && canForceDb()) forceNow(); };
bridge.onteardown = () => { cancelPoll(); cancelAutoCmp(); obsRo?.disconnect(); obsIo?.disconnect(); return {}; };

const ensureConnected = async () => {
  if (bridge.isConnected?.()) return canCallTools;
  try {
    await bridge.connect(); syncHostContext(bridge.getHostContext());
    const caps = bridge.getHostCapabilities();
    canMessage = !!caps?.message; canCallTools = !!caps?.serverTools;
    Object.assign(DE.dataset, { canMsg: +canMessage, ct: +canCallTools });
    errorText = ""; render(); scheduleNextPoll(); kickRelay();
    return canCallTools;
  } catch (err) {
    errorText = `${err?.message || err}`; loadingInitial = false; render();
    return false;
  }
};

const bootPanel = async () => {
  setupRevealObservers(); if (snapshot) applySnapshot(snapshot); render();
  if (await ensureConnected() && viewMode === DB) void refreshSnapshot(true, false);
};

void bootPanel();
