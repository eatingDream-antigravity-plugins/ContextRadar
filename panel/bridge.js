// Lightweight SEP-1865 (2026-01-26) MCP Apps PostMessage bridge for Context Radar MCP.
export const PROTOCOL_VERSION = "2026-01-26";

export function applyDocumentTheme(t) {
  if (t !== "dark" && t !== "light") return;
  const d = document, e = d.documentElement, bg = `var(--color-background-primary,${t === "light" ? "#FAF8F5" : "#141311"})`;
  if (e.getAttribute("data-theme") !== t) e.setAttribute("data-theme", t);
  e.style.colorScheme = t; e.style.backgroundColor = bg;
  if (d.body) d.body.style.backgroundColor = bg;
  const m = d.querySelector?.('meta[name="color-scheme"]');
  if (m && m.getAttribute("content") !== t) m.setAttribute("content", t);
}

const isOpaqueBg = (raw) => {
  const s = String(raw ?? "").trim();
  if (!s || /^(transparent|none|initial|inherit|unset|revert(?:-layer)?)$/i.test(s)) return false;
  const h = s.match(/^#([0-9a-f]{4}|[0-9a-f]{8})$/i)?.[1];
  if (h) return (h.length === 4 ? parseInt(h[3] + h[3], 16) : parseInt(h.slice(6), 16)) >= 230;
  const m = s.match(/^(?:rgba?|hsla?)\([^,/) ]+(?:[\s,]+[^,/) ]+){2}\s*[,/]\s*([^)\s]+)\s*\)$/i)
    || s.match(/^[a-z-]+\([^)]*\/\s*([^)\s]+)\s*\)$/i);
  if (!m) return true;
  const a = m[1].endsWith("%") ? parseFloat(m[1]) / 100 : parseFloat(m[1]);
  return !Number.isNaN(a) && a >= 0.9;
};

export function applyHostStyleVariables(vars, target = document.documentElement) {
  if (!vars || typeof vars !== "object") return;
  for (const [k, v] of Object.entries(vars)) {
    if (k.startsWith("--") && v != null) {
      const val = String(v).trim();
      if (val && (k !== "--color-background-primary" || isOpaqueBg(val))) target.style.setProperty(k, val);
    }
  }
}

export function applyHostFonts(css) {
  const d = document;
  if (!css || d.getElementById("__cr-hf")) return;
  const s = d.createElement("style"); s.id = "__cr-hf"; s.textContent = css; d.head.appendChild(s);
}

export class McpAppBridge {
  constructor(appInfo, appCaps = {}, opts = { autoResize: true }) {
    this._i = appInfo; this._c = appCaps || {}; this._o = { autoResize: true, ...opts };
    this._n = 1; this._p = new Map(); this._on = false; this._t = window.parent; this._ro = null;
    this._m = ({ source, data: m }) => {
      if (source !== this._t || !m || typeof m !== "object" || m.jsonrpc !== "2.0") return;
      if (m.id !== undefined && !m.method) {
        const e = this._p.get(m.id); if (!e) return;
        this._p.delete(m.id); clearTimeout(e.t);
        m.error ? e.rej(new Error(m.error.message || JSON.stringify(m.error))) : e.res(m.result);
      } else if (m.id !== undefined && m.method) this._req(m);
      else if (m.method) this._ntf(m);
    };
  }

  getHostCapabilities() { return this._hc; }
  getHostContext() { return this._hx; }
  _post(p) { this._t.postMessage(p, "*"); }

  async _req(m) {
    try {
      if (m.method === "ping") return this._post({ jsonrpc: "2.0", id: m.id, result: {} });
      if (m.method === "ui/resource-teardown") return this._post({ jsonrpc: "2.0", id: m.id, result: (await this.onteardown?.(m.params || {})) ?? {} });
      this._post({ jsonrpc: "2.0", id: m.id, error: { code: -32601, message: m.method } });
    } catch (e) { this._post({ jsonrpc: "2.0", id: m.id, error: { code: -32603, message: e?.message || String(e) } }); }
  }

  _ntf({ method: k, params: p = {} }) {
    const N = "ui/notifications/";
    if (k === `${N}tool-input`) this.ontoolinput?.(p);
    else if (k === `${N}tool-result`) this.ontoolresult?.(p);
    else if (k === `${N}host-context-changed`) {
      const prev = this._hx?.styles || {};
      this._hx = { ...this._hx, ...p, styles: p.styles ? { ...prev, ...p.styles, variables: { ...(prev.variables || {}), ...(p.styles.variables || {}) } } : prev };
      this.onhostcontextchanged?.(p);
    } else if (k === "notifications/progress") for (const e of this._p.values()) e.rst?.();
  }

  request(method, params, { timeoutMs = 45_000 } = {}) {
    const id = this._n++;
    return new Promise((res, rej) => {
      const mk = () => setTimeout(() => { this._p.delete(id); rej(new Error(method)); }, timeoutMs);
      const e = { res, rej, t: mk(), rst() { clearTimeout(e.t); e.t = mk(); } };
      this._p.set(id, e); this._post({ jsonrpc: "2.0", id, method, params });
    });
  }

  notification(method, params) { this._post(params === undefined ? { jsonrpc: "2.0", method } : { jsonrpc: "2.0", method, params }); }
  callServerTool(params, opts = {}) { return this.request("tools/call", params, { timeoutMs: opts.timeout ?? 45_000 }); }
  sendMessage(params, opts = {}) { return this.request("ui/message", params, { timeoutMs: opts.timeout ?? 30_000 }); }

  _autoSz() {
    if (typeof ResizeObserver === "undefined") return;
    let q = false, lw = 0, lh = 0;
    const go = () => {
      if (q) return; q = true;
      requestAnimationFrame(() => {
        q = false;
        const r = document.documentElement, prev = r.style.height; r.style.height = "max-content";
        const h = Math.ceil(r.getBoundingClientRect().height); r.style.height = prev;
        const w = Math.ceil(window.innerWidth);
        if (w !== lw || h !== lh) { lw = w; lh = h; this.notification("ui/notifications/size-changed", { width: w, height: h }); }
      });
    };
    go();
    this._ro = new ResizeObserver(go);
    this._ro.observe(document.documentElement);
    if (document.body) this._ro.observe(document.body);
  }

  async connect() {
    if (this._on) return; this._on = true;
    window.addEventListener("message", this._m);
    try {
      const r = await this.request("ui/initialize", { appInfo: this._i, appCapabilities: this._c, protocolVersion: PROTOCOL_VERSION }, { timeoutMs: 20_000 });
      if (!r || typeof r !== "object") throw new Error("Bad init");
      this._hc = r.hostCapabilities; this._hx = r.hostContext;
      this.notification("ui/notifications/initialized");
      // Only enable ResizeObserver RPCs for inline conversation frames (which report containerDimensions.maxHeight).
      if (this._o.autoResize && Boolean(r.hostContext?.containerDimensions?.maxHeight)) this._autoSz();
    } catch (e) { this.close(); throw e; }
  }

  close() {
    this._on = false; window.removeEventListener("message", this._m);
    this._ro?.disconnect(); this._ro = null;
    for (const e of this._p.values()) { clearTimeout(e.t); e.rej(new Error("Closed")); }
    this._p.clear();
  }
}
