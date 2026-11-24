import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DIST_HTML = path.join(__dirname, "dist", "panel.html");
const THEME_CSS = path.join(__dirname, "theme.css");
const VIEW_JS = path.join(__dirname, "view.js");
const BRIDGE_JS = path.join(__dirname, "bridge.js");

test("panel/dist/panel.html meets <45 KiB, CSP, warm editorial, dark-first anti-white-flash, and anti-flicker invariants", () => {
  assert.ok(fs.existsSync(DIST_HTML), "panel/dist/panel.html must exist");
  const html = fs.readFileSync(DIST_HTML, "utf8");
  const bytes = Buffer.byteLength(html, "utf8");
  assert.ok(bytes < 45 * 1024, `panel.html must be <45 KiB, got ${bytes} B`);
  assert.ok(html.includes('<meta name="color-scheme" content="dark">'), "must declare dark-only initial meta color-scheme to prevent remote white flash");
  assert.ok(html.includes('style="background:#141311;color-scheme:dark"'), "must lock dark canvas inline on <html> from byte 0");
  assert.ok(html.includes("/*__CONTEXT_RADAR_BOOT__*/"), "must contain SSR boot marker");
  assert.ok(html.includes("window.__CONTEXT_RADAR_BOOT__"), "must read SSR boot data");
  assert.ok(html.includes("background-color:var(--color-background-primary,#141311)"), "default root background must be warm dark obsidian (#141311)");
  assert.ok(/:root\[data-theme="?light"?\]/.test(html), "light theme must be gated on data-theme=light");
  assert.equal(/prefers-color-scheme\s*:\s*light/i.test(html), false, "must NOT auto-trigger light mode via prefers-color-scheme:light");
  assert.ok(html.includes("#D97757") || html.includes("#d97757"), "must include warm terracotta accent");
  assert.ok(html.includes("prefers-reduced-motion"), "must respect prefers-reduced-motion");
  assert.equal(html.includes("cr-enter"), false, "must NOT use opacity:0 entrance animation that causes refresh flicker");
  assert.ok(html.includes("com.google.antigravity/message"), "must support official target:new relay");
  assert.ok(html.includes("context_radar_overview"), "must call context_radar_overview");

  // Strict CSP default-src 'none': zero external URLs or tags
  assert.equal(/https?:\/\//i.test(html), false, "must contain zero http/https URLs");
  assert.equal(/<link\b/i.test(html), false, "must contain zero external <link> tags");
  assert.equal(/<script[^>]+src=/i.test(html), false, "must contain zero external <script src>");
});

test("bridge.js skips size-changed RPCs in aux_pane/global and enables them in inline mode", async () => {
  const { McpAppBridge } = await import(BRIDGE_JS);

  async function runConnect(hostContext) {
    const sent = [];
    const listeners = new Map();
    const origWin = globalThis.window;
    const origDoc = globalThis.document;
    const origRO = globalThis.ResizeObserver;
    const origRAF = globalThis.requestAnimationFrame;

    let observedCount = 0;
    const parentTarget = {
      postMessage(msg) {
        sent.push(msg);
        if (msg.method === "ui/initialize") {
          queueMicrotask(() => {
            const cb = listeners.get("message");
            cb?.({
              source: parentTarget,
              data: {
                jsonrpc: "2.0",
                id: msg.id,
                result: {
                  hostCapabilities: { serverTools: {} },
                  hostInfo: { name: "test-host", version: "1.0" },
                  hostContext,
                },
              },
            });
          });
        }
      },
    };

    globalThis.window = {
      parent: parentTarget,
      innerWidth: 380,
      addEventListener(type, fn) { listeners.set(type, fn); },
      removeEventListener(type) { listeners.delete(type); },
    };
    globalThis.document = {
      documentElement: {
        style: { height: "" },
        getBoundingClientRect: () => ({ height: 320 }),
      },
      body: {},
    };
    globalThis.ResizeObserver = class {
      constructor(fn) { this._fn = fn; }
      observe() { observedCount += 1; }
      disconnect() {}
    };
    globalThis.requestAnimationFrame = (fn) => { fn(); return 1; };

    try {
      const bridge = new McpAppBridge({ name: "context-radar-mcp", version: "1.0.0" }, {});
      await bridge.connect();
      bridge.close();
      return { sent, observedCount };
    } finally {
      globalThis.window = origWin;
      globalThis.document = origDoc;
      globalThis.ResizeObserver = origRO;
      globalThis.requestAnimationFrame = origRAF;
    }
  }

  const paneRes = await runConnect({ theme: "dark", displayMode: "inline" });
  assert.equal(paneRes.observedCount, 0, "aux_pane/global must not create ResizeObserver");
  assert.equal(
    paneRes.sent.some((m) => m.method === "ui/notifications/size-changed"),
    false,
    "aux_pane/global must not emit size-changed RPCs"
  );

  const inlineRes = await runConnect({
    theme: "dark",
    displayMode: "inline",
    containerDimensions: { maxHeight: 600 },
  });
  assert.ok(inlineRes.observedCount > 0, "inline view should observe size changes");
  assert.equal(
    inlineRes.sent.some((m) => m.method === "ui/notifications/size-changed"),
    true,
    "inline view should emit size-changed notification"
  );
});

test("view.js mounts shell & hero ONCE, skips identical digests, and patches strictly in place", () => {
  const viewSrc = fs.readFileSync(VIEW_JS, "utf8");
  const cssSrc = fs.readFileSync(THEME_CSS, "utf8");

  // Structural anti-flicker & dark-first invariants
  assert.ok(cssSrc.includes("html,body{") && cssSrc.includes("background-color:var(--color-background-primary,#141311)"), "root must default dark obsidian");
  assert.ok(cssSrc.includes(':root[data-theme="light"]'), "light theme must be gated on data-theme=light");
  assert.equal(/prefers-color-scheme\s*:\s*light/i.test(cssSrc), false, "must not use prefers-color-scheme:light");
  assert.ok(cssSrc.includes('html:not([data-can-msg="1"]) .cr-cap-msg'), "message buttons must be hidden by default until hostCapabilities confirm");
  assert.equal(cssSrc.includes("cr-enter"), false, "must not use opacity:0 entrance animation");
  assert.ok(viewSrc.includes('if (sc.unchanged || (sc.digest && sc.digest === lastDigest && snapshot)) return;'), "identical digest ontoolresult must no-op");
  assert.equal(viewSrc.includes("ui.he.innerHTML ="), false, "patchHeroSlot must never wipe ui.he.innerHTML");
  assert.equal(viewSrc.includes("rec.tr.innerHTML ="), false, "patchSessionList must never wipe rec.tr.innerHTML");

  // Verify bundled JS parses cleanly in a sandboxed VM (no syntax errors)
  const html = fs.readFileSync(DIST_HTML, "utf8");
  const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
  assert.equal(scripts.length, 2, "panel.html should contain boot script + main bundle script");
  assert.doesNotThrow(() => new vm.Script(scripts[1]), "bundled JS must be valid syntax");
});

test("bridge.js ignores transparent or translucent --color-background-primary and view.js parseColorLum rejects low alpha", async () => {
  const { applyHostStyleVariables } = await import(BRIDGE_JS);
  const applied = new Map();
  const mockTarget = {
    style: {
      setProperty(k, v) { applied.set(k, v); },
    },
  };

  for (const badBg of [
    "",
    "   ",
    "transparent",
    "TRANSPARENT",
    "rgba(0, 0, 0, 0)",
    "rgba(20, 19, 17, 0.5)",
    "rgba(20, 19, 17, 0.89)",
    "rgb(20 19 17 / 0)",
    "rgb(20 19 17 / 50%)",
    "hsla(0, 0%, 10%, 0.4)",
    "#14131100",
    "#14131180",
    "#0000",
  ]) {
    applied.clear();
    applyHostStyleVariables(
      {
        "--color-background-primary": badBg,
        "--color-text-primary": "#F4F1EA",
      },
      mockTarget
    );
    assert.equal(
      applied.has("--color-background-primary"),
      false,
      `must ignore unsafe --color-background-primary=${JSON.stringify(badBg)}`
    );
    assert.equal(applied.get("--color-text-primary"), "#F4F1EA", "other valid vars should still apply");
  }

  for (const okBg of [
    "#141311",
    "#FAF8F5",
    "#141311ff",
    "#141311f0",
    "rgb(20, 19, 17)",
    "rgba(20, 19, 17, 1)",
    "rgba(20, 19, 17, 0.95)",
    "rgb(20 19 17 / 100%)",
  ]) {
    applied.clear();
    applyHostStyleVariables({ "--color-background-primary": okBg }, mockTarget);
    assert.equal(applied.get("--color-background-primary"), okBg, `opaque ${okBg} should apply`);
  }

  // Extract and execute parseColorLum from view.js to test alpha filtering directly
  const viewSrc = fs.readFileSync(VIEW_JS, "utf8");
  const match = viewSrc.match(/function parseColorLum\(raw\)\s*\{[\s\S]*?\n\}/);
  assert.ok(match, "view.js must define parseColorLum");
  const sandbox = { num: (v) => Number(v) || 0 };
  vm.runInNewContext(`${match[0]}; this.parseColorLum = parseColorLum;`, sandbox);
  const { parseColorLum } = sandbox;

  assert.equal(parseColorLum("transparent"), null);
  assert.equal(parseColorLum("rgba(255, 255, 255, 0)"), null);
  assert.equal(parseColorLum("rgba(255, 255, 255, 0.49)"), null);
  assert.equal(parseColorLum("rgb(255 255 255 / 20%)"), null);
  assert.equal(parseColorLum("#ffffff00"), null);
  assert.equal(parseColorLum("#ffffff7f"), null);
  assert.equal(parseColorLum("#fff0"), null);
  assert.ok(parseColorLum("#141311") < 0.15, "dark obsidian hex should have low luminance");
  assert.ok(parseColorLum("#FAF8F5") > 0.9, "warm light hex should have high luminance");
  assert.ok(parseColorLum("rgba(20, 19, 17, 0.8)") < 0.15, "rgba with alpha >= 0.5 should return luminance");
  assert.ok(parseColorLum("#faf8f5cc") > 0.9, "8-digit hex with alpha >= 0.5 should return luminance");
});

test("panel renders bilingual mid-loop sub-reasons and whisper-quiet Carbon concurrency warning without jargon", () => {
  const html = fs.readFileSync(DIST_HTML, "utf8");
  const viewSrc = fs.readFileSync(VIEW_JS, "utf8");

  // Clean #92 UI rules: zero backend/infra jargon in panel HTML
  for (const forbidden of ["HBM", "radix", "SHA-1", "0600 Unix Socket", "原生 Rust"]) {
    assert.equal(html.includes(forbidden), false, `panel.html must not expose jargon '${forbidden}'`);
  }

  // Carbon concurrency warning is gated on carbon_concurrency_warning && >= 3
  assert.ok(viewSrc.includes("carbon_concurrency_warning"), "view.js must check carbon_concurrency_warning");
  assert.ok(viewSrc.includes("active_carbon_count"), "view.js must read active_carbon_count");
  assert.ok(html.includes("Concurrency tip:"), "panel.html must include English Carbon concurrency tip");
  assert.ok(html.includes("并发提示："), "panel.html must include Chinese Carbon concurrency tip");

  // Verify coldKind + getCold handle all 3 mid-loop sub-reasons in EN and zh-CN
  const subMatch = viewSrc.match(/const SUB_L = \{[\s\S]*?\};/);
  const kindMatch = viewSrc.match(/const coldKind = \(t\) => \{[\s\S]*?\n\};/);
  const getColdMatch = viewSrc.match(/const getCold = \([\s\S]*?=>[\s\S]*?;\n/);
  assert.ok(subMatch && kindMatch && getColdMatch, "view.js must define SUB_L, coldKind, getCold");

  let curLang = "en";
  const sandbox = {
    num: (v) => Number(v) || 0,
    tr: (e, z) => (curLang === "zh-CN" ? z : e),
  };
  vm.runInNewContext(`${subMatch[0]}\n${kindMatch[0]}\n${getColdMatch[0]}\nthis.coldKind = coldKind; this.getCold = getCold;`, sandbox);

  assert.equal(sandbox.coldKind({ cold_cache_kind: "mid_loop_cold", cold_cache_sub_reason: "slow_turn_self_expiry" }), "Slow-turn TTL");
  assert.equal(sandbox.coldKind({ cold_cache_kind: "mid_loop_cold", cold_cache_sub_reason: "replica_spillover" }), "Replica spillover");
  assert.equal(sandbox.coldKind({ cold_cache_kind: "mid_loop_cold", cold_cache_sub_reason: "model_switch_cold" }), "Model switch");

  curLang = "zh-CN";
  assert.equal(sandbox.coldKind({ cold_cache_kind: "mid_loop_cold", cold_cache_sub_reason: "slow_turn_self_expiry" }), "慢轮自过期");
  assert.equal(sandbox.coldKind({ cold_cache_kind: "mid_loop_cold", cold_cache_sub_reason: "replica_spillover" }), "副本溢出");
  assert.equal(sandbox.coldKind({ cold_cache_kind: "mid_loop_cold", cold_cache_sub_reason: "model_switch_cold" }), "切换模型");

  const parsed = sandbox.getCold({
    cold_cache_breakdown: { turn_start_cold: 1, mid_loop_cold: 3, post_compact_cold: 0 },
    mid_loop_breakdown: { slow_turn_self_expiry: 1, replica_spillover: 1, model_switch_cold: 1 },
  });
  assert.deepEqual({ t: parsed.t, m: parsed.m, p: parsed.p, s: parsed.s, r: parsed.r, w: parsed.w }, { t: 1, m: 3, p: 0, s: 1, r: 1, w: 1 });
});

test("panel supports immediate open/focus refresh, continuous background polling, and official ui/message Send /compact", () => {
  const html = fs.readFileSync(DIST_HTML, "utf8");
  const viewSrc = fs.readFileSync(VIEW_JS, "utf8");

  // 1. Instant refresh on mount, focus, pageshow, and visibility return
  assert.ok(viewSrc.includes('window.addEventListener("focus"'), "must refresh immediately on window focus");
  assert.ok(viewSrc.includes('window.addEventListener("pageshow"'), "must refresh immediately on pageshow");
  assert.ok(viewSrc.includes("void refreshSnapshot(true, false)"), "bootPanel and focus must trigger immediate fresh refresh");

  // 2. Background polling continues via lightweight if_digest instead of freezing forever while hidden
  assert.equal(viewSrc.includes("pausedHidden"), false, "must not freeze polling completely when hidden");
  assert.ok(viewSrc.includes("D.hidden ?"), "nextPollMs must adapt cadence cleanly in background");

  // 3. Official ui/message Send /compact action and record_compact persistence
  assert.ok(viewSrc.includes("record_compact"), "view.js must record compact dispatch status");
  assert.ok(viewSrc.includes("manual_compact"), "view.js must claim grants with manual_compact mode");
  assert.ok(viewSrc.includes("auto_compact"), "view.js must claim grants with auto_compact mode");
  assert.ok(html.includes("Send /compact"), "panel.html must include EN Send /compact label");
  assert.ok(html.includes("执行 /compact"), "panel.html must include ZH Send /compact label");
});

test("auto_compact gating enforces inline ownership, side-pane canDirectCid, subagent/auto flags, and official ui/message without target:new", async () => {
  const viewSrc = fs.readFileSync(VIEW_JS, "utf8");
  const normWsM = viewSrc.match(/const normWs = [\s\S]*?;\n/);
  const sameWsM = viewSrc.match(/const sameWs = [\s\S]*?\n\};/);
  const getHeroM = viewSrc.match(/const getHero = [\s\S]*?;\n/);
  const canDirectM = viewSrc.match(/const canDirectCid = [\s\S]*?\n\};/);
  const canAutoM = viewSrc.match(/function canAutoRelay\([\s\S]*?\n\}/);
  const maybeAutoM = viewSrc.match(/function maybeAutoRelay\([\s\S]*?\n\}/);
  const execM = viewSrc.match(/async function executeDispatch\([\s\S]*?\n\}/);
  assert.ok(normWsM && sameWsM && getHeroM && canDirectM && canAutoM && maybeAutoM && execM, "view.js must define auto_compact helpers");

  const timers = [];
  const sentMessages = [];
  const uiActions = [];
  const copied = [];
  const sandbox = {
    D: { hidden: false },
    AG_M: "com.google.antigravity/message",
    K_CID: "conversation_id",
    K_HCID: "handoff_conversation_id",
    K_AAH: "allow_auto_handoff",
    K_RSC: "running_subagent_count",
    K_LCT: "latest_context_tokens",
    T_WS: ["Wait", "等待"],
    num: (v) => Number(v) || 0,
    tr: (e) => e,
    canMessage: true,
    canCallTools: true,
    allowAutoHandoff: true,
    ownerCid: "",
    snapshot: {
      daemon_workspace: "/ws/a",
      summary: { running_main_count: 1 },
      sessions: [{ id: "c1", is_active: true, workspace: "/ws/a" }],
      pending_grant: null,
    },
    handoffCard: null,
    handoffStatus: "pending",
    busyGrantId: null,
    attemptedGrants: new Set(),
    autoCmpTimer: null,
    pg: () => sandbox.snapshot?.pending_grant,
    cancelAutoCmp: () => { sandbox.autoCmpTimer = null; },
    isHandoffLocked: () => /^(sen|com|old)/.test(sandbox.handoffStatus),
    st: (fn, ms) => { const h = { fn, ms, cancelled: false }; timers.push(h); return h; },
    ct: (h) => { if (h) h.cancelled = true; },
    render: () => {},
    flashMicro: () => {},
    cpCompact: async (cmd) => { copied.push(cmd); return true; },
    copyText: async (txt) => { copied.push(txt); return true; },
    extractRelayCid: () => "",
    uiSc: async (args) => { uiActions.push(args); return { claimed: true }; },
    recAct: async (action, gid, extra) => { uiActions.push({ action, grant_id: gid, ...extra }); },
    sendTxt: async (text, meta) => { sentMessages.push({ text, meta }); return sandbox._sendReply(); },
    _sendReply: () => ({ isError: false }),
  };

  vm.runInNewContext(
    `${normWsM[0]}\n${sameWsM[0]}\n${getHeroM[0]}\n${canDirectM[0]}\n${canAutoM[0]}\n${maybeAutoM[0]}\n${execM[0]}\nthis.canAutoRelay = canAutoRelay; this.maybeAutoRelay = maybeAutoRelay; this.executeDispatch = executeDispatch;`,
    sandbox
  );

  const mkGrant = (ov = {}) => ({
    grant_id: "g1",
    kind: "compact",
    conversation_id: "c1",
    workspace: "/ws/a",
    compact_command: "/compact preserve X",
    handoff_prompt: "handoff...",
    auto_fire: true,
    allow_auto_handoff: true,
    acp_proxy_active: false,
    claimed: false,
    compact_status: "",
    handoff_conversation_id: "",
    running_subagent_count: 0,
    ...ov,
  });

  // 1. Inline card allows auto_compact even when running_main_count > 1
  sandbox.snapshot.summary.running_main_count = 3;
  assert.equal(sandbox.canAutoRelay(mkGrant(), true), "auto_compact");
  assert.equal(sandbox.canAutoRelay(mkGrant({ running_subagent_count: 1 }), true), "");
  assert.equal(sandbox.canAutoRelay(mkGrant({ auto_fire: false }), true), "");
  assert.equal(sandbox.canAutoRelay(mkGrant({ allow_auto_handoff: false }), true), "");
  assert.equal(sandbox.canAutoRelay(mkGrant({ claimed: true }), true), "");
  assert.equal(sandbox.canAutoRelay(mkGrant({ compact_status: "sent" }), true), "");
  assert.equal(sandbox.canAutoRelay(mkGrant({ acp_proxy_active: true }), true), "");

  // 2. Side pane (isInline=false) enforces canDirectCid: blocked when >1 main without ownerCid
  assert.equal(sandbox.canAutoRelay(mkGrant(), false), "");
  // Allowed when ownerCid matches cid even with >1 main
  sandbox.ownerCid = "c1";
  assert.equal(sandbox.canAutoRelay(mkGrant(), false), "auto_compact");
  // Blocked when ownerCid mismatches
  sandbox.ownerCid = "c-other";
  assert.equal(sandbox.canAutoRelay(mkGrant(), false), "");
  // Allowed when !ownerCid and single active main in matching workspace
  sandbox.ownerCid = "";
  sandbox.snapshot.summary.running_main_count = 1;
  assert.equal(sandbox.canAutoRelay(mkGrant(), false), "auto_compact");
  // Blocked when workspace mismatches or allowAutoHandoff is toggled off
  assert.equal(sandbox.canAutoRelay(mkGrant({ workspace: "/ws/other" }), false), "");
  sandbox.allowAutoHandoff = false;
  assert.equal(sandbox.canAutoRelay(mkGrant(), false), "");
  sandbox.allowAutoHandoff = true;

  // 3. Inline maybeAutoRelay waits 1500ms settle window, claims auto_compact, sends ui/message without target:new, records sent
  const gInline = mkGrant({ grant_id: "g-inline" });
  sandbox.handoffCard = gInline;
  sandbox.handoffStatus = "pending";
  sandbox.maybeAutoRelay(gInline, true);
  assert.equal(timers.length, 1);
  assert.equal(timers[0].ms, 1500, "inline auto_compact must wait 1500ms settle window");
  assert.equal(sentMessages.length, 0, "must not send before settle window elapses");
  const flush = () => new Promise((r) => setImmediate(r));
  timers[0].fn();
  await flush();
  assert.equal(uiActions[0].action, "claim_grant");
  assert.equal(uiActions[0].claim_mode, "auto_compact");
  assert.deepEqual(sentMessages[0], { text: "/compact preserve X", meta: 0 });
  assert.deepEqual(uiActions[1], { action: "record_compact", grant_id: "g-inline", status: "sent" });
  assert.equal(sandbox.handoffStatus, "compacted");

  // 4. Side-pane auto_compact waits 2000ms and records fallback_copied detail on host isError
  uiActions.length = 0; sentMessages.length = 0; copied.length = 0; timers.length = 0;
  const gPane = mkGrant({ grant_id: "g-pane" });
  sandbox.snapshot.pending_grant = gPane;
  sandbox._sendReply = () => ({ isError: true, content: [{ type: "text", text: "host rejected" }] });
  sandbox.maybeAutoRelay(gPane, false);
  assert.equal(timers.length, 1);
  assert.equal(timers[0].ms, 2000, "side-pane auto_compact must wait 2000ms settle window");
  timers[0].fn();
  await flush();
  assert.equal(uiActions[0].claim_mode, "auto_compact");
  assert.deepEqual(uiActions[1], { action: "record_compact", grant_id: "g-pane", status: "fallback_copied:host rejected" });
  assert.deepEqual(copied, ["/compact preserve X"]);

  // 5. Exception during ui/message records error detail and falls back to clipboard copy
  uiActions.length = 0; copied.length = 0;
  sandbox._sendReply = () => { throw new Error("Method not found"); };
  await sandbox.executeDispatch(mkGrant({ grant_id: "g-err" }), "auto_compact", true, true);
  assert.deepEqual(uiActions[1], { action: "record_compact", grant_id: "g-err", status: "error:Method not found" });
  assert.deepEqual(copied, ["/compact preserve X"]);
});

test("non-force periodic poll updates DOM on digest change, skips render on unchanged, clears error on recovery, and uses 15s timeout", async () => {
  const viewSrc = fs.readFileSync(VIEW_JS, "utf8");
  const applyM = viewSrc.match(/function applySnapshot\([\s\S]*?\n\}/);
  const refreshM = viewSrc.match(/async function refreshSnapshot\([\s\S]*?\n\}/);
  assert.ok(applyM && refreshM, "view.js must define applySnapshot and refreshSnapshot");

  let renderedTokens = 100000;
  let renderCount = 0;
  const calls = [];
  let replyQueue = [];

  const sandbox = {
    DB: "dashboard",
    D: { hidden: false },
    DE: { lang: "en" },
    M: Math,
    performance: { now: () => 100 },
    now: () => 1000,
    st: () => 1,
    ct: () => {},
    num: (v) => Number(v) || 0,
    tr: (e) => e,
    parseLoc: () => "",
    K_AAH: "allow_auto_handoff",
    viewMode: "dashboard",
    canCallTools: true,
    inFlightPoll: false,
    queuedForce: false,
    refreshing: false,
    showSpinner: false,
    spinTimer: null,
    filterTab: "active",
    lastDigest: "d1",
    snapshot: { mode: "snapshot", digest: "d1", sessions: [{ id: "c1", latest_context_tokens: 100000 }] },
    loadingInitial: false,
    errorText: "",
    serverMs: 0,
    ewmaRttMs: null,
    pollErrors: 0,
    lastSyncTs: 0,
    unchangedHits: 0,
    langLocked: true,
    lang: "en",
    allowAutoHandoff: true,
    cancelPoll: () => {},
    scheduleNextPoll: () => {},
    maybeAutoRelay: () => {},
    flashMicro: () => {},
    render: () => {
      renderCount += 1;
      renderedTokens = sandbox.snapshot?.sessions?.[0]?.latest_context_tokens ?? renderedTokens;
    },
    callTool: async (name, args, opts) => {
      calls.push({ name, args, opts });
      return replyQueue.shift();
    },
  };

  vm.runInNewContext(
    `${applyM[0]}\n${refreshM[0]}\nthis.refreshSnapshot = refreshSnapshot;`,
    sandbox
  );

  // 1. Non-force periodic poll with changed digest MUST render updated tokens and pass 15s timeout
  replyQueue.push({
    isError: false,
    structuredContent: {
      mode: "snapshot",
      digest: "d2",
      server_ms: 4,
      sessions: [{ id: "c1", latest_context_tokens: 245000 }],
    },
  });
  await sandbox.refreshSnapshot(false);
  assert.equal(calls[0].name, "context_radar_overview");
  assert.equal(calls[0].args.if_digest, "d1");
  assert.equal(calls[0].opts?.timeout, 15000, "overview poll must pass 15s timeout");
  assert.equal(renderCount, 1, "non-force poll with changed digest MUST call render()");
  assert.equal(renderedTokens, 245000, "rendered DOM tokens must update to 245000");
  assert.equal(sandbox.lastDigest, "d2");

  // 2. Non-force periodic poll with unchanged:true MUST NOT mutate DOM (renderCount unchanged)
  replyQueue.push({
    isError: false,
    structuredContent: { mode: "snapshot", unchanged: true, digest: "d2", server_ms: 2 },
  });
  await sandbox.refreshSnapshot(false);
  assert.equal(renderCount, 1, "unchanged non-force poll must keep 0 DOM churn");
  assert.equal(sandbox.unchangedHits, 1);

  // 3. Recovering from a transient error on an unchanged:true poll MUST re-render to clear error banner
  sandbox.errorText = "Sync: timeout";
  replyQueue.push({
    isError: false,
    structuredContent: { mode: "snapshot", unchanged: true, digest: "d2", server_ms: 2 },
  });
  await sandbox.refreshSnapshot(false);
  assert.equal(sandbox.errorText, "");
  assert.equal(renderCount, 2, "recovering from transient error on unchanged poll must call render()");
});

test("kept-alive pane reveal hooks (ResizeObserver, IntersectionObserver, onhostcontextchanged, pointerenter) trigger fresh overview refresh", () => {
  const viewSrc = fs.readFileSync(VIEW_JS, "utf8");
  const canForceM = viewSrc.match(/const canForceDb = [\s\S]*?;\n/);
  const forceNowM = viewSrc.match(/const forceNow = [\s\S]*?;\n/);
  const setupObsM = viewSrc.match(/function setupRevealObservers\([\s\S]*?\n\}/);
  const hostChgM = viewSrc.match(/bridge\.onhostcontextchanged = [\s\S]*?;\n/);
  const ptrM = viewSrc.match(/onD\("pointerenter"[\s\S]*?\);\n/);
  assert.ok(canForceM && forceNowM && setupObsM && hostChgM && ptrM, "view.js must define kept-alive reveal hooks");

  let roCb = null, ioCb = null, ptrCb = null;
  let fakeNow = 1000;
  const refreshes = [];
  const listeners = {};
  const sandbox = {
    DB: "dashboard",
    D: { hidden: false },
    DE: { clientWidth: 420, clientHeight: 800 },
    viewMode: "dashboard",
    canCallTools: true,
    lastForceTs: 0,
    lastSyncTs: 0,
    obsRo: null,
    obsIo: null,
    now: () => fakeNow,
    onD: (ev, fn, opts) => { listeners[ev] = { fn, opts }; if (ev === "pointerenter") ptrCb = fn; },
    ResizeObserver: class {
      constructor(cb) { roCb = cb; }
      observe() {}
      disconnect() {}
    },
    IntersectionObserver: class {
      constructor(cb) { ioCb = cb; }
      observe() {}
      disconnect() {}
    },
    bridge: {},
    syncHostContext: () => {},
    refreshSnapshot: (force, showSynced) => { refreshes.push({ force, showSynced }); },
  };

  vm.runInNewContext(
    `${canForceM[0]}\n${forceNowM[0]}\n${setupObsM[0]}\n${hostChgM[0]}\n${ptrM[0]}\nsetupRevealObservers();`,
    sandbox
  );

  // 1. ResizeObserver: 0x0 -> 420x800 triggers fresh refresh immediately
  roCb([{ contentRect: { width: 0, height: 0 } }]);
  assert.equal(refreshes.length, 0, "collapsing to 0x0 must not refresh");
  fakeNow += 500;
  roCb([{ contentRect: { width: 420, height: 800 } }]);
  assert.deepEqual(refreshes, [{ force: true, showSynced: false }]);

  // 2. IntersectionObserver: false -> true triggers fresh refresh immediately
  ioCb([{ isIntersecting: false }]);
  assert.equal(refreshes.length, 1);
  fakeNow += 500;
  ioCb([{ isIntersecting: true }]);
  assert.equal(refreshes.length, 2);
  assert.deepEqual(refreshes[1], { force: true, showSynced: false });

  // 3. onhostcontextchanged({ containerDimensions }) triggers fresh refresh immediately
  fakeNow += 500;
  sandbox.bridge.onhostcontextchanged({ containerDimensions: { width: 420, height: 800 } });
  assert.equal(refreshes.length, 3);
  assert.deepEqual(refreshes[2], { force: true, showSynced: false });

  // 4. passive pointerenter triggers fresh refresh when stale (>900ms since lastSyncTs)
  assert.equal(listeners.pointerenter?.opts?.passive, true, "pointerenter must be passive");
  sandbox.lastSyncTs = fakeNow;
  fakeNow += 400;
  ptrCb();
  assert.equal(refreshes.length, 3, "pointerenter within 900ms must not spam refresh");
  fakeNow += 600;
  ptrCb();
  assert.equal(refreshes.length, 4, "pointerenter after >900ms must trigger fresh refresh");
  assert.deepEqual(refreshes[3], { force: true, showSynced: false });
});
