import { createHash } from "node:crypto";

const css = `:root{color-scheme:light dark;font:16px/1.55 system-ui,sans-serif;color:#192737;background:#cbe5fc}*{box-sizing:border-box}body{margin:0;min-height:100svh;display:grid;place-items:center;padding:24px;isolation:isolate}
body::before{content:'';position:fixed;inset:-4px;z-index:-1;pointer-events:none;border-image:url('/meadow/desktop.webp') 224 0 416 fill / 14.583vw 0 27.083vw / 0 stretch;filter:blur(2px);opacity:.86}
@media(max-width:600px) and (orientation:portrait){body::before{border-image-source:url('/meadow/mobile.webp');border-image-slice:410 0 825 fill;border-image-width:56.63vw 0 113.95vw}}
main{width:min(100%,24rem);text-align:center}img{width:76px;height:76px;object-fit:contain}h1{font-size:clamp(1.6rem,6vw,2rem);line-height:1.15;letter-spacing:-.045em;margin:18px 0 10px}p{margin:0;color:#30475b;text-wrap:balance}.mark{position:relative;display:inline-flex}.mark:after{content:'✓';background:#267252;color:white;font:bold 14px/22px system-ui;width:22px;height:22px;border:3px solid #fff;border-radius:50%;position:absolute;right:-3px;bottom:1px}.issue:after{content:'!';background:#875c18}@media(prefers-color-scheme:dark){:root{color:#eff7fc;background:#111a21}body::before{opacity:.3}.mark:after{border-color:#1c2931}p{color:#dfebf4}}`;
const policy = `default-src 'none'; img-src 'self'; style-src 'sha256-${createHash('sha256').update(css).digest('base64')}'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'`;

// Copy is static relay-owned text. Never render upstream errors or callback parameters.
export function resultPage(headers: Record<string, string>, title: string, detail: string, success = false, status = 200) {
  const escape = (s: string) => s.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
  return new Response(`<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${escape(title)} · Milo</title><link rel="icon" href="/milo.png"><style>${css}</style><main><span class="mark${success ? '' : ' issue'}"><img src="/milo.png" width="76" height="76" alt="Milo"></span><h1>${escape(title)}</h1><p>${escape(detail)}</p></main></html>`, {status, headers:{...headers,'Content-Type':'text/html; charset=utf-8','Content-Security-Policy':policy}});
}
