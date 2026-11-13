// Text rewriting shared by recording and replay, so both engines get the same bytes:
// every absolute or protocol-relative URL of a host points at that host's loopback port.

export const LOOPBACK = "127.0.0.1";

// Namespace URIs are identifiers, never fetched; rewriting them breaks SVG and XHTML.
const KEEP_HOSTS = new Set(["www.w3.org", "w3.org"]);

// `(https?:)?` then `//` or JSON-escaped `\/\/`, then a host name with a letter TLD and an optional port.
// Not after a slash, backslash or word character, so the `\//i.test` in a regex literal is no URL.
const URL_RE = /(?<![\\/\w])(https?:)?(\/\/|\\\/\\\/)((?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,24})(:\d{1,5})?(?![a-z0-9.-])/gi;

export function hostsIn(text: string): string[] {
  const found = new Set<string>();
  for (const m of text.matchAll(URL_RE)) {
    const host = (m[3] + (m[4] ?? "")).toLowerCase();
    if (!KEEP_HOSTS.has(host)) found.add(host);
  }
  return [...found];
}

export function rewriteUrls(text: string, portOf: (host: string) => number): string {
  return text.replace(URL_RE, (all, proto: string | undefined, slashes: string, name: string, port: string | undefined) => {
    const host = (name + (port ?? "")).toLowerCase();
    if (KEEP_HOSTS.has(host)) return all;
    return `${proto ? "http:" : ""}${slashes}${LOOPBACK}:${portOf(host)}`;
  });
}

// The page sees a fixed clock, seeded Math.random, en-US, and no animation time.
const FIXED_TIME = Date.UTC(2026, 10, 13, 12, 0, 0);
const SHIM =
  `<script>(function(){window.__corpusErrors=[];addEventListener("error",function(e){__corpusErrors.push(String(e.message))});` +
  `addEventListener("unhandledrejection",function(e){__corpusErrors.push("unhandled rejection: "+String(e.reason&&e.reason.message||e.reason))});` +
  `var s=0x2f6e2b1;Math.random=function(){s=s+0x6d2b79f5|0;var t=Math.imul(s^s>>>15,1|s);` +
  `t=t+Math.imul(t^t>>>7,61|t)^t;return((t^t>>>14)>>>0)/4294967296};var R=Date,B=${FIXED_TIME},` +
  `p=function(){return typeof performance!=="undefined"&&performance.now?performance.now():0};` +
  `function D(){if(!(this instanceof D))return new R(B+p()).toString();var a=arguments;` +
  `return a.length===0?new R(B+p()):a.length===1?new R(a[0]):new R(R.UTC.apply(null,a)+new R().getTimezoneOffset()*6e4)}` +
  `D.prototype=R.prototype;D.now=function(){return Math.floor(B+p())};D.UTC=R.UTC;D.parse=R.parse;Date=D;` +
  `try{Object.defineProperty(Navigator.prototype,"language",{get:function(){return"en-US"}});` +
  `Object.defineProperty(Navigator.prototype,"languages",{get:function(){return["en-US","en"]}})}catch(e){}})();</script>` +
  `<style>*,*::before,*::after{transition-duration:0s!important;transition-delay:0s!important;` +
  `animation-duration:0s!important;animation-delay:0s!important;animation-iteration-count:1!important;scroll-behavior:auto!important}</style>`;

// WebKit follows the host's dark mode; our renderer always answers light. Both get light.
function lightOnly(text: string): string {
  return text
    .replace(/\(\s*prefers-color-scheme\s*:\s*dark\s*\)/gi, "(max-width:0px)")
    .replace(/\(\s*prefers-color-scheme\s*:\s*light\s*\)/gi, "(min-width:0px)")
    .replace(/(color-scheme["']?\s*[:=]\s*["']?)(?:only\s+)?(?:light\s+dark|dark\s+light|dark)/gi, "$1light");
}

export function rewriteText(text: string, portOf: (host: string) => number): string {
  return lightOnly(rewriteUrls(text, portOf));
}

export function rewriteHtml(text: string, portOf: (host: string) => number): string {
  const body = rewriteText(text, portOf)
    .replace(/\sintegrity\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)/gi, "")
    .replace(/<meta[^>]+http-equiv\s*=\s*["']?content-security-policy[^>]*>/gi, "");
  const head = /<head(\s[^>]*)?>/i.exec(body) ?? /<html(\s[^>]*)?>/i.exec(body);
  const at = head ? head.index + head[0].length : 0;
  return process.env.CORPUS_NO_SHIM === "1" ? body : body.slice(0, at) + SHIM + body.slice(at);
}

const TEXT_TYPE = /^(text\/|application\/(javascript|x-javascript|ecmascript|json|xml|xhtml\+xml|manifest\+json|ld\+json)|image\/svg\+xml)/i;

export type BodyKind = "html" | "text" | "binary";

export function bodyKind(contentType: string, url: string): BodyKind {
  if (/^text\/html|application\/xhtml/i.test(contentType)) return "html";
  if (TEXT_TYPE.test(contentType)) return "text";
  if (!contentType && /\.(m?js|css|json|svg)(\?|$)/i.test(url)) return "text";
  return "binary";
}

// URLs are ASCII, so a latin1 round trip rewrites them without touching other bytes (EUC-KR pages included).
export function rewriteBody(bytes: Uint8Array, kind: BodyKind, portOf: (host: string) => number): Uint8Array {
  if (kind === "binary") return bytes;
  const text = Buffer.from(bytes).toString("latin1");
  const out = kind === "html" ? rewriteHtml(text, portOf) : rewriteText(text, portOf);
  return Buffer.from(out, "latin1");
}
