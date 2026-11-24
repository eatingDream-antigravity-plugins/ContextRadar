// Lightweight SEP-1865 (2026-01-26) MCP Apps PostMessage bridge for Context Radar MCP.
export const PROTOCOL_VERSION = "2026-01-26";

const BG = "--color-background-primary", NTF = "ui/notifications/", JR = "2.0";
export function applyDocumentTheme(t) {
  if (t !== "dark" && t !== "light") return;
  const d = document, e = d.documentElement, s = e.style, b = d.body, bg = `var(${BG},${t === "light" ? "#FAF8F5" : "#141311"})`;
  if (e.getAttribute("data-theme") !== t) e.setAttribute("data-theme", t);
  s.colorScheme = t; s.backgroundColor = bg;
  if (b) b.style.backgroundColor = bg;
  const m = d.querySelector?.('meta[name="color-scheme"]');
  if (m && m.getAttribute("content") !== t) m.setAttribute("content", t);
}

const isOpaqueBg = (raw) => {
  const s = String(raw ?? "").trim();
  if (!s || /^(transparent|none|initial|inherit|unset|revert(?:-layer)?)$/i.test(s)) return false;
  const h = s.match(/^#([0-9a-f]{4}|[0-9a-f]{8})$/i)?.[1];
  if (h) return parseInt(h.length === 4 ? h[3] + h[3] : h.slice(6), 16) >= 230;
  const m = s.match(/^(?:rgba?|hsla?)\([^,/) ]+(?:[\s,]+[^,/) ]+){2}\s*[,/]\s*([^)\s]+)\s*\)$/i)
    || s.match(/^[a-z-]+\([^)]*\/\s*([^)\s]+)\s*\)$/i);
  if (!m) return true;
  const a = parseFloat(m[1]) / (m[1].endsWith("%") ? 100 : 1);
  return !Number.isNaN(a) && a >= 0.9;
};

export function applyHostStyleVariables(vars, target = document.documentElement) {
  if (!vars || typeof vars !== "object") return;
  for (const [k, v] of Object.entries(vars)) {
    if (k.startsWith("--") && v != null) {
      const val = String(v).trim();
      if (val && (k !== BG || isOpaqueBg(val))) target.style.setProperty(k, val);
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
    let n = 1, on = false, ro = null, hc, hx;
    const w = window, d = document, p = new Map(), t = w.parent, c = appCaps || {}, o = { autoResize: true, ...opts };
    const post = (x) => t.postMessage({ jsonrpc: JR, ...x }, "*");
    const req = async (m) => {
      try {
        if (m.method === "ping") return post({ id: m.id, result: {} });
        if (m.method === "ui/resource-teardown") return post({ id: m.id, result: (await this.onteardown?.(m.params || {})) ?? {} });
        post({ id: m.id, error: { code: -32601, message: m.method } });
      } catch (e) { post({ id: m.id, error: { code: -32603, message: e?.message || String(e) } }); }
    };
    const ntf = ({ method: k, params: a = {} }) => {
      if (k.startsWith(NTF)) {
        const s = k.slice(NTF.length);
        if (s === "tool-input") this.ontoolinput?.(a);
        else if (s === "tool-result") this.ontoolresult?.(a);
        else if (s === "host-context-changed") {
          const prev = hx?.styles || {};
          hx = { ...hx, ...a, styles: a.styles ? { ...prev, ...a.styles, variables: { ...(prev.variables || {}), ...(a.styles.variables || {}) } } : prev };
          this.onhostcontextchanged?.(a);
        }
      } else if (k === "notifications/progress") for (const e of p.values()) e.rst?.();
    };
    const onMsg = ({ source, data: m }) => {
      if (source !== t || !m || typeof m !== "object" || m.jsonrpc !== JR) return;
      if (m.id !== undefined && !m.method) {
        const e = p.get(m.id); if (!e) return;
        p.delete(m.id); clearTimeout(e.t);
        m.error ? e.rej(new Error(m.error.message || JSON.stringify(m.error))) : e.res(m.result);
      } else if (m.id !== undefined && m.method) req(m);
      else if (m.method) ntf(m);
    };
    const autoSz = () => {
      if (typeof ResizeObserver === "undefined") return;
      let q = false, lw = 0, lh = 0;
      const r = d.documentElement, go = () => {
        if (q) return; q = true;
        requestAnimationFrame(() => {
          q = false;
          const prev = r.style.height; r.style.height = "max-content";
          const h = Math.ceil(r.getBoundingClientRect().height); r.style.height = prev;
          const wd = Math.ceil(w.innerWidth);
          if (wd !== lw || h !== lh) { lw = wd; lh = h; this.notification(`${NTF}size-changed`, { width: wd, height: h }); }
        });
      };
      go(); ro = new ResizeObserver(go); ro.observe(r);
      if (d.body) ro.observe(d.body);
    };
    this.getHostCapabilities = () => hc;
    this.getHostContext = () => hx;
    this.request = (method, params, { timeoutMs = 45_000 } = {}) => {
      const id = n++;
      return new Promise((res, rej) => {
        const mk = () => setTimeout(() => { p.delete(id); rej(new Error(method)); }, timeoutMs);
        const e = { res, rej, t: mk(), rst() { clearTimeout(e.t); e.t = mk(); } };
        p.set(id, e); post({ id, method, params });
      });
    };
    this.notification = (method, params) => post(params === undefined ? { method } : { method, params });
    this.callServerTool = (params, opts0 = {}) => this.request("tools/call", params, { timeoutMs: opts0.timeout ?? 45_000 });
    this.sendMessage = (params, opts0 = {}) => this.request("ui/message", params, { timeoutMs: opts0.timeout ?? 30_000 });
    this.connect = async () => {
      if (on) return; on = true;
      w.addEventListener("message", onMsg);
      try {
        const r = await this.request("ui/initialize", { appInfo, appCapabilities: c, protocolVersion: PROTOCOL_VERSION }, { timeoutMs: 20_000 });
        if (!r || typeof r !== "object") throw new Error("Bad init");
        hc = r.hostCapabilities; hx = r.hostContext;
        this.notification(`${NTF}initialized`);
        // Only enable ResizeObserver RPCs for inline conversation frames (which report containerDimensions.maxHeight).
        if (o.autoResize && Boolean(r.hostContext?.containerDimensions?.maxHeight)) autoSz();
      } catch (e) { this.close(); throw e; }
    };
    this.close = () => {
      on = false; w.removeEventListener("message", onMsg);
      ro?.disconnect(); ro = null;
      for (const e of p.values()) { clearTimeout(e.t); e.rej(new Error("Closed")); }
      p.clear();
    };
  }
}
