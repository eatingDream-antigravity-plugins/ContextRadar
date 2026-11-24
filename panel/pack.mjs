import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const MAX_BUNDLE_BYTES = 45 * 1024;

function tryLoadEsbuild() {
  const candidates = [process.env.ESBUILD_PATH, "esbuild"].filter(Boolean);
  for (const mod of candidates) {
    try {
      return require(mod);
    } catch {}
  }
  return null;
}

function tryCliBundle(entryJs, bridgeJs) {
  let tmpDir = "";
  try {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cr-pack-"));
    fs.writeFileSync(path.join(tmpDir, "bridge.js"), fs.readFileSync(bridgeJs, "utf8"), "utf8");
    const compactedView = compactHtmlLiterals(fs.readFileSync(entryJs, "utf8"));
    const tmpEntry = path.join(tmpDir, "view.js");
    fs.writeFileSync(tmpEntry, compactedView, "utf8");

    for (const esbBin of ["esbuild", "/opt/homebrew/bin/esbuild", "/usr/local/bin/esbuild"]) {
      try {
        const out = execFileSync(
          esbBin,
          [
            tmpEntry,
            "--bundle",
            "--minify",
            "--tree-shaking=true",
            "--charset=utf8",
            "--legal-comments=none",
            "--format=iife",
            "--platform=browser",
            "--target=es2022",
          ],
          { encoding: "utf8", maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] }
        );
        if (out.trim()) return { code: out.trim(), bundler: "esbuild-cli" };
      } catch {}
    }

    const bunBins = [
      "bun",
      "/opt/homebrew/bin/bun",
      path.join(os.homedir(), ".bun", "bin", "bun"),
    ];
    for (const bunBin of bunBins) {
      try {
        const out = execFileSync(
          bunBin,
          ["build", tmpEntry, "--bundle", "--minify", "--format=iife", "--target=browser"],
          { encoding: "utf8", maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] }
        );
        if (out.trim()) return { code: out.trim(), bundler: "bun" };
      } catch {}
    }
    return null;
  } catch {
    return null;
  } finally {
    if (tmpDir) {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {}
    }
  }
}

function compactHtmlLiterals(src) {
  return src
    .replace(/>\s+</g, "><")
    .replace(/>\s+\$\{/g, ">${")
    .replace(/\}\s+</g, "}<");
}

function fallbackMinifyCss(css) {
  return css
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\s+/g, " ")
    .replace(/\s*([{}:;,>+~])\s*/g, "$1")
    .replace(/;}/g, "}")
    .trim();
}

function stripJsCommentsAndIndent(code) {
  let out = "";
  let i = 0;
  const n = code.length;
  let state = "code";
  let tplDepth = [];

  while (i < n) {
    const ch = code[i];
    const nx = code[i + 1] || "";

    if (state === "code") {
      if (ch === "/" && nx === "/") {
        i += 2;
        while (i < n && code[i] !== "\n") i++;
        continue;
      }
      if (ch === "/" && nx === "*") {
        i += 2;
        while (i < n && !(code[i] === "*" && code[i + 1] === "/")) i++;
        i += 2;
        continue;
      }
      if (ch === "'" || ch === '"') {
        const q = ch;
        out += ch;
        i++;
        while (i < n) {
          const c = code[i];
          out += c;
          i++;
          if (c === "\\") {
            if (i < n) {
              out += code[i];
              i++;
            }
          } else if (c === q) break;
        }
        continue;
      }
      if (ch === "`") {
        state = "tpl";
        out += ch;
        i++;
        continue;
      }
      if (ch === "}" && tplDepth.length > 0) {
        const top = tplDepth[tplDepth.length - 1];
        if (top === 0) {
          tplDepth.pop();
          state = "tpl";
          out += ch;
          i++;
          continue;
        }
        tplDepth[tplDepth.length - 1] = top - 1;
      } else if (ch === "{" && tplDepth.length > 0) {
        tplDepth[tplDepth.length - 1]++;
      }
      if (ch === "\n" || ch === "\r") {
        out += "\n";
        i++;
        while (i < n && (code[i] === " " || code[i] === "\t")) i++;
        continue;
      }
      if (ch === " " || ch === "\t") {
        out += " ";
        i++;
        while (i < n && (code[i] === " " || code[i] === "\t")) i++;
        continue;
      }
      out += ch;
      i++;
    } else if (state === "tpl") {
      if (ch === "\\") {
        out += ch + nx;
        i += 2;
        continue;
      }
      if (ch === "$" && nx === "{") {
        tplDepth.push(0);
        state = "code";
        out += "${";
        i += 2;
        continue;
      }
      if (ch === "`") {
        state = "code";
        out += ch;
        i++;
        continue;
      }
      out += ch;
      i++;
    }
  }
  return out.replace(/\n{2,}/g, "\n").trim();
}

function fallbackBundleJs(bridgeSrc, viewSrc) {
  const cleanBridge = bridgeSrc.replace(/^\s*export\s+/gm, "");
  const cleanView = compactHtmlLiterals(
    viewSrc.replace(/^\s*import\s+[\s\S]*?from\s+["']\.\/bridge\.js["'];?\s*/m, "")
  );
  return `(()=>{${stripJsCommentsAndIndent(`${cleanBridge}\n${cleanView}`)}})();`;
}

async function main() {
  const entryJs = path.join(__dirname, "view.js");
  const bridgeJs = path.join(__dirname, "bridge.js");
  const cssPath = path.join(__dirname, "theme.css");
  const distDir = path.join(__dirname, "dist");
  const outHtml = path.join(distDir, "panel.html");

  const rawCss = fs.readFileSync(cssPath, "utf8");
  const esbuild = tryLoadEsbuild();

  let jsCode;
  let cssCode;
  let bundler = "fallback";

  if (esbuild) {
    bundler = "esbuild";
    const compactPlugin = {
      name: "compact-html-templates",
      setup(build) {
        build.onLoad({ filter: /\/view\.js$/ }, async (args) => {
          const src = await fs.promises.readFile(args.path, "utf8");
          return { contents: compactHtmlLiterals(src), loader: "js" };
        });
      },
    };
    const res = await esbuild.build({
      entryPoints: [entryJs],
      bundle: true,
      minify: true,
      treeShaking: true,
      charset: "utf8",
      legalComments: "none",
      format: "iife",
      platform: "browser",
      target: ["es2022"],
      plugins: [compactPlugin],
      write: false,
    });
    jsCode = res.outputFiles[0].text.trim().replace(/<\/script/gi, "<\\/script");
    cssCode = fallbackMinifyCss(rawCss).replace(/<\/style/gi, "<\\/style");
  } else {
    const cliOut = tryCliBundle(entryJs, bridgeJs);
    if (cliOut) {
      bundler = cliOut.bundler;
      jsCode = cliOut.code.replace(/<\/script/gi, "<\\/script");
    } else {
      jsCode = fallbackBundleJs(
        fs.readFileSync(bridgeJs, "utf8"),
        fs.readFileSync(entryJs, "utf8")
      ).replace(/<\/script/gi, "<\\/script");
    }
    cssCode = fallbackMinifyCss(rawCss).replace(/<\/style/gi, "<\\/style");
  }

  const html = `<!doctype html><html lang="en" style="background:#141311;color-scheme:dark"><head><meta charset="utf-8"><meta name="color-scheme" content="dark"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Context Radar</title><style>${cssCode}</style></head><body style="background:#141311"><div id="root"></div><script>/*__CONTEXT_RADAR_BOOT__*/</script><script>${jsCode}</script></body></html>`;

  fs.mkdirSync(distDir, { recursive: true });
  fs.writeFileSync(outHtml, html, "utf8");
  const totalBytes = Buffer.byteLength(html, "utf8");
  const jsKb = (Buffer.byteLength(jsCode, "utf8") / 1024).toFixed(1);
  const cssKb = (Buffer.byteLength(cssCode, "utf8") / 1024).toFixed(1);
  const totalKb = (totalBytes / 1024).toFixed(1);
  console.log(
    `Packed ${outHtml} (${totalKb} KiB / ${totalBytes} B total: JS ${jsKb} KiB + CSS ${cssKb} KiB, bundler=${bundler})`
  );
  if (totalBytes >= MAX_BUNDLE_BYTES) {
    throw new Error(`Panel bundle ${totalBytes} B exceeds ${MAX_BUNDLE_BYTES} B (<45 KiB) limit`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
