export const PROTOCOL_VERSION = "2026-01-26";
const BG = "--color-background-primary", NTF = "ui/notifications/", JR = "2.0", S = String, ST = setTimeout, CT = clearTimeout;

export function applyDocumentTheme(t) {
  if (t !== "dark" && t !== "light") return;
  const d = document, e = d.documentElement, bg = `var(${BG},${t === "light" ? "#FAF8F4" : "#141311"})`, sb = (x) => x && (x.style.backgroundColor = bg);
  e.setAttribute("data-theme", t); e.style.colorScheme = t; sb(e); sb(d.body);
  d.querySelector?.('meta[name="color-scheme"]')?.setAttribute("content", t);
}

const isOpaqueBg = (r) => {
  const s = S(r ?? "").trim();
  if (!s || /^(transparent|none|initial|inherit|unset|revert(?:-layer)?)$/i.test(s)) return false;
  const h = s.match(/^#([0-9a-f]{4}|[0-9a-f]{8})$/i)?.[1];
  if (h) return parseInt(h.length === 4 ? h[3] + h[3] : h.slice(6), 16) >= 230;
  const m = s.match(/^(?:rgba?|hsla?)\([^,/) ]+(?:[\s,]+[^,/) ]+){2}\s*[,/]\s*([^)\s]+)\s*\)$/i) || s.match(/^[a-z-]+\([^)]*\/\s*([^)\s]+)\s*\)$/i);
  return !m || parseFloat(m[1]) / (m[1].endsWith("%") ? 100 : 1) >= .9;
};

export function applyHostStyleVariables(v, target = document.documentElement) {
  if (!v || typeof v !== "object") return;
  for (const k in v) if (k.startsWith("--") && v[k] != null) {
    const s = S(v[k]).trim();
    if (s && (k !== BG || isOpaqueBg(s))) target.style.setProperty(k, s);
  }
}

export function applyHostFonts(css) {
  const d = document;
  if (!css || d.getElementById("__cr-hf")) return;
  const s = d.createElement("style"); s.id = "__cr-hf"; s.textContent = css; d.head.appendChild(s);
}

export class McpAppBridge {
  constructor(appInfo, appCaps = {}, opts = {}) {
    let n = 1, ok = 0, cp = null, ro = null, hc, hx;
    const w = window, d = document, p = new Map(), t = w.parent, post = (x) => t.postMessage({ jsonrpc: JR, ...x }, "*");
    const ntf = (method, params) => post(params === undefined ? { method } : { method, params });
    const onMsg = async ({ source, data: m }) => {
      if (source !== t || !m || typeof m !== "object" || m.jsonrpc !== JR) return;
      const { id, method: k, params: a = {} } = m;
      if (id !== undefined) {
        if (!k) {
          const e = p.get(id); if (!e) return;
          p.delete(id); CT(e.t);
          m.error ? e.rej(new Error(m.error.message || JSON.stringify(m.error))) : e.res(m.result);
        } else try {
          post(k === "ping" ? { id, result: {} } : k === "ui/resource-teardown" ? { id, result: (await this.onteardown?.(a)) ?? {} } : { id, error: { code: -32601, message: k } });
        } catch (e) { post({ id, error: { code: -32603, message: e?.message || S(e) } }); }
      } else if (k?.startsWith(NTF)) {
        const s = k.slice(17);
        if (s === "tool-input") this.ontoolinput?.(a);
        else if (s === "tool-result") this.ontoolresult?.(a);
        else if (s === "host-context-changed") {
          const pv = hx?.styles || {};
          hx = { ...hx, ...a, styles: a.styles ? { ...pv, ...a.styles, variables: { ...pv.variables, ...a.styles.variables } } : pv };
          this.onhostcontextchanged?.(a);
        }
      } else if (k === "notifications/progress") for (const e of p.values()) e.rst();
    };
    const autoSz = () => {
      if (typeof ResizeObserver > "u") return;
      let q = 0, lw = 0, lh = 0;
      const r = d.documentElement, go = () => {
        if (q) return; q = 1;
        requestAnimationFrame(() => {
          q = 0; const pv = r.style.height; r.style.height = "max-content";
          const h = Math.ceil(r.getBoundingClientRect().height), wd = Math.ceil(w.innerWidth); r.style.height = pv;
          if (wd !== lw || h !== lh) { lw = wd; lh = h; ntf(`${NTF}size-changed`, { width: wd, height: h }); }
        });
      };
      go(); ro = new ResizeObserver(go); ro.observe(r); d.body && ro.observe(d.body);
    };
    const req = (method, params, timeoutMs = 45e3) => new Promise((res, rej) => {
      const id = n++, mk = () => ST(() => { p.delete(id); rej(new Error(method)); }, timeoutMs);
      const e = { res, rej, t: mk(), rst() { CT(e.t); e.t = mk(); } };
      p.set(id, e); post({ id, method, params });
    });
    Object.assign(this, {
      getHostCapabilities: () => hc,
      getHostContext: () => hx,
      isConnected: () => !!ok,
      request: (m, a, o0 = {}) => req(m, a, o0.timeoutMs),
      notification: ntf,
      callServerTool: (a, o0) => req("tools/call", a, o0?.timeout),
      sendMessage: (a, o0) => req("ui/message", a, o0?.timeout ?? 3e4),
      connect: () => {
        if (ok) return Promise.resolve();
        if (cp) return cp;
        w.addEventListener("message", onMsg);
        return (cp = (async () => {
          try {
            const r = await req("ui/initialize", { appInfo, appCapabilities: appCaps || {}, protocolVersion: PROTOCOL_VERSION }, 2e4);
            if (!r || typeof r !== "object") throw new Error("Bad init");
            hc = r.hostCapabilities; hx = r.hostContext; ok = 1;
            ntf(`${NTF}initialized`);
            if (opts?.autoResize !== false && hx?.containerDimensions?.maxHeight) autoSz();
          } catch (e) { this.close(); throw e; }
          finally { cp = null; }
        })());
      },
      close: () => {
        ok = 0; cp = null; w.removeEventListener("message", onMsg); ro?.disconnect(); ro = null;
        for (const e of p.values()) { CT(e.t); e.rej(new Error("Closed")); }
        p.clear();
      },
    });
  }
}
