// Lightweight SEP-1865 (2026-01-26) MCP Apps PostMessage bridge for Context Radar MCP.

export const PROTOCOL_VERSION = "2026-01-26";

export function applyDocumentTheme(theme) {
  if (theme !== "dark" && theme !== "light") return;
  const el = document.documentElement;
  if (el.getAttribute("data-theme") !== theme) el.setAttribute("data-theme", theme);
  el.style.colorScheme = theme;
  const bg = theme === "light" ? "var(--color-background-primary,#FAF8F5)" : "var(--color-background-primary,#141311)";
  el.style.backgroundColor = bg;
  if (document.body) document.body.style.backgroundColor = bg;
  const meta = document.querySelector?.('meta[name="color-scheme"]');
  if (meta && meta.getAttribute("content") !== theme) meta.setAttribute("content", theme);
}

const alphaNum = (s) => (s.endsWith("%") ? parseFloat(s) / 100 : parseFloat(s));

function isOpaqueBg(raw) {
  const s = String(raw ?? "").trim();
  if (!s || /^(transparent|none|initial|inherit|unset|revert(?:-layer)?)$/i.test(s)) return false;
  const h = s.match(/^#([0-9a-f]{4}|[0-9a-f]{8})$/i)?.[1];
  if (h) return (h.length === 4 ? parseInt(h[3] + h[3], 16) : parseInt(h.slice(6), 16)) >= 230;
  const m = s.match(/^(?:rgba?|hsla?)\([^,/) ]+(?:[\s,]+[^,/) ]+){2}\s*[,/]\s*([^)\s]+)\s*\)$/i)
    || s.match(/^[a-z-]+\([^)]*\/\s*([^)\s]+)\s*\)$/i);
  if (m) { const a = alphaNum(m[1]); return !Number.isNaN(a) && a >= 0.9; }
  return true;
}

export function applyHostStyleVariables(vars, target = document.documentElement) {
  if (!vars || typeof vars !== "object") return;
  for (const [k, v] of Object.entries(vars)) {
    if (!k.startsWith("--") || v == null) continue;
    const val = String(v).trim();
    if (!val || (k === "--color-background-primary" && !isOpaqueBg(val))) continue;
    target.style.setProperty(k, val);
  }
}

export function applyHostFonts(css) {
  if (!css || document.getElementById("__cr-host-fonts")) return;
  const s = document.createElement("style");
  s.id = "__cr-host-fonts";
  s.textContent = css;
  document.head.appendChild(s);
}

export class McpAppBridge {
  constructor(appInfo, appCaps = {}, opts = { autoResize: true }) {
    this._i = appInfo;
    this._c = appCaps || {};
    this._o = { autoResize: true, ...opts };
    this._hc = undefined;
    this._hi = undefined;
    this._hx = undefined;
    this._n = 1;
    this._p = new Map();
    this._on = false;
    this._t = window.parent;
    this._ro = null;

    this.ontoolinput = undefined;
    this.ontoolinputpartial = undefined;
    this.ontoolresult = undefined;
    this.ontoolcancelled = undefined;
    this.onhostcontextchanged = undefined;
    this.onteardown = undefined;

    this._m = (ev) => {
      if (ev.source !== this._t) return;
      const m = ev.data;
      if (!m || typeof m !== "object" || m.jsonrpc !== "2.0") return;
      if (m.id !== undefined && !m.method) {
        const e = this._p.get(m.id);
        if (!e) return;
        this._p.delete(m.id);
        clearTimeout(e.t);
        if (m.error) e.rej(new Error(m.error.message || JSON.stringify(m.error)));
        else e.res(m.result);
        return;
      }
      if (m.id !== undefined && m.method) this._req(m);
      else if (m.method) this._ntf(m);
    };
  }

  getHostCapabilities() { return this._hc; }
  getHostVersion() { return this._hi; }
  getHostContext() { return this._hx; }
  _post(p) { this._t.postMessage(p, "*"); }

  async _req(m) {
    try {
      if (m.method === "ping") return this._post({ jsonrpc: "2.0", id: m.id, result: {} });
      if (m.method === "ui/resource-teardown") {
        const r = this.onteardown ? (await this.onteardown(m.params || {})) ?? {} : {};
        return this._post({ jsonrpc: "2.0", id: m.id, result: r });
      }
      this._post({ jsonrpc: "2.0", id: m.id, error: { code: -32601, message: `Method not found: ${m.method}` } });
    } catch (e) {
      this._post({ jsonrpc: "2.0", id: m.id, error: { code: -32603, message: e?.message || String(e) } });
    }
  }

  _ntf(m) {
    const p = m.params || {};
    const k = m.method;
    if (k === "ui/notifications/tool-input") this.ontoolinput?.(p);
    else if (k === "ui/notifications/tool-input-partial") this.ontoolinputpartial?.(p);
    else if (k === "ui/notifications/tool-result") this.ontoolresult?.(p);
    else if (k === "ui/notifications/tool-cancelled") this.ontoolcancelled?.(p);
    else if (k === "ui/notifications/host-context-changed") {
      const prev = this._hx?.styles || {};
      const styles = p.styles
        ? { ...prev, ...p.styles, variables: { ...(prev.variables || {}), ...(p.styles.variables || {}) } }
        : prev;
      this._hx = { ...this._hx, ...p, styles };
      this.onhostcontextchanged?.(p);
    } else if (k === "notifications/progress") {
      for (const e of this._p.values()) e.rst?.();
    }
  }

  request(method, params, { timeoutMs = 45_000 } = {}) {
    const id = this._n++;
    return new Promise((res, rej) => {
      const mk = () => setTimeout(() => { this._p.delete(id); rej(new Error(`Request timed out: ${method}`)); }, timeoutMs);
      const e = { res, rej, t: mk(), rst() { clearTimeout(e.t); e.t = mk(); } };
      this._p.set(id, e);
      this._post({ jsonrpc: "2.0", id, method, params });
    });
  }

  notification(method, params) {
    const p = { jsonrpc: "2.0", method };
    if (params !== undefined) p.params = params;
    this._post(p);
  }

  callServerTool(params, opts = {}) { return this.request("tools/call", params, { timeoutMs: opts.timeout ?? 45_000 }); }
  sendMessage(params, opts = {}) { return this.request("ui/message", params, { timeoutMs: opts.timeout ?? 30_000 }); }
  sendSizeChanged(params) { this.notification("ui/notifications/size-changed", params); }

  _autoSz() {
    if (typeof ResizeObserver === "undefined") return;
    let q = false, lw = 0, lh = 0;
    const go = () => {
      if (q) return;
      q = true;
      requestAnimationFrame(() => {
        q = false;
        const r = document.documentElement, prev = r.style.height;
        r.style.height = "max-content";
        const h = Math.ceil(r.getBoundingClientRect().height);
        r.style.height = prev;
        const w = Math.ceil(window.innerWidth);
        if (w !== lw || h !== lh) { lw = w; lh = h; this.sendSizeChanged({ width: w, height: h }); }
      });
    };
    go();
    const ro = new ResizeObserver(go);
    ro.observe(document.documentElement);
    if (document.body) ro.observe(document.body);
    this._ro = ro;
  }

  async connect() {
    if (this._on) return;
    this._on = true;
    window.addEventListener("message", this._m);
    try {
      const r = await this.request(
        "ui/initialize",
        { appInfo: this._i, appCapabilities: this._c, protocolVersion: PROTOCOL_VERSION },
        { timeoutMs: 20_000 }
      );
      if (!r || typeof r !== "object") throw new Error("Invalid ui/initialize result");
      this._hc = r.hostCapabilities;
      this._hi = r.hostInfo;
      this._hx = r.hostContext;
      this.notification("ui/notifications/initialized");
      // Only enable ResizeObserver RPCs for inline conversation frames (which report containerDimensions.maxHeight).
      if (this._o.autoResize && Boolean(r.hostContext?.containerDimensions?.maxHeight)) this._autoSz();
    } catch (e) {
      this.close();
      throw e;
    }
  }

  close() {
    this._on = false;
    window.removeEventListener("message", this._m);
    if (this._ro) { this._ro.disconnect(); this._ro = null; }
    for (const e of this._p.values()) { clearTimeout(e.t); e.rej(new Error("Bridge closed")); }
    this._p.clear();
  }
}
