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
  assert.ok(html.includes("Send /compact"), "panel.html must include EN Send /compact label");
  assert.ok(html.includes("执行 /compact"), "panel.html must include ZH Send /compact label");
});
