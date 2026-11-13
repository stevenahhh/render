// Compares our HTML parser's tree with WebKit's on the frozen corpus pages, before any
// script runs. WebKit parses each page's main HTML with scripts blocked by a CSP (so
// <noscript> parses as it does with scripting on); our tree comes from the domdump example
// (`cargo build --release --example domdump`). Both are written as outlines, one line per
// element or non-blank text node ("<depth> <tag>" or '<depth> "<first 40 chars>"').
//
//   bun tools/corpus/domdiff.ts [--only <id substring>] [--out dir] [--dump path]
//
// Prints, per page that differs, the first line where the outlines part and its context.
// Known differences that are not parser bugs: WebKit splits text nodes longer than 65536
// characters and cuts text at 40 UTF-16 units where we cut at 40 characters.
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { CORPUS, REPO } from "./lib/corpus.ts";

const args = process.argv.slice(2);
const opt = (name: string) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const only = opt("--only");
const out = resolve(opt("--out") ?? join(CORPUS, "domdiff"));
const dump = resolve(opt("--dump") ?? join(process.env.CARGO_TARGET_DIR ?? join(REPO, "target"), "release/examples/domdump"));
mkdirSync(out, { recursive: true });

const OUTLINE = `(() => { const out = []; const walk = (n, d) => { for (let c = n.firstChild; c; c = c.nextSibling) {
  if (c.nodeType === 1) { out.push(d + ' ' + c.localName); walk(c, d + 1); }
  else if (c.nodeType === 3) { const t = c.data.replace(/\\s+/g, ' ').trim(); if (t) out.push(d + ' "' + t.slice(0, 40) + '"'); } } };
  walk(document, 0); return out.join('\\n'); })()`;
const CSP = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'">`;

type Entry = { status: number; headers: [string, string][]; body: string | null };

/// The main HTML of a frozen site, following redirects in its store, and the
/// charset its response declared.
function mainHtml(dir: string): { bytes: Buffer; charset: string } | null {
  const entry: string = JSON.parse(readFileSync(join(dir, "record.json"), "utf8")).entry;
  const raw = JSON.parse(readFileSync(join(dir, "store", "index.json"), "utf8"));
  const index = new Map<string, Entry>(Array.isArray(raw) ? raw : Object.entries(raw));
  let url = entry;
  let e = index.get(`GET ${url}`);
  for (let hops = 0; e && e.status >= 300 && e.status < 400 && hops < 5; hops++) {
    const loc = e.headers.find(([k]) => k.toLowerCase() === "location")?.[1];
    if (!loc) break;
    url = new URL(loc, url).href;
    e = index.get(`GET ${url}`);
  }
  if (!e?.body) return null;
  const type = e.headers.find(([k]) => k.toLowerCase() === "content-type")?.[1] ?? "";
  return { bytes: readFileSync(join(dir, "store", "bodies", e.body)), charset: /charset=["']?([\w-]+)/i.exec(type)?.[1] ?? "" };
}

const ids = readdirSync(join(CORPUS, "sites"))
  .sort()
  .filter((id) => (!only || id.includes(only)) && existsSync(join(CORPUS, "sites", id, "record.json")));
let same = 0;
for (const id of ids) {
  const page = mainHtml(join(CORPUS, "sites", id));
  if (!page) continue;
  const { bytes } = page;
  // A file has no HTTP header: the charset the server sent goes in a meta tag.
  const head = Buffer.from((page.charset ? `<meta charset="${page.charset}">` : "") + CSP);
  // The CSP goes after the doctype, which must stay first to keep the page out of quirks mode.
  const cut = (/^\s*(<!doctype[^>]*>)?/i.exec(bytes.subarray(0, 300).toString("latin1")) ?? [""])[0].length;
  const html = join(out, `${id}.html`);
  writeFileSync(html, Buffer.concat([bytes.subarray(0, cut), head, bytes.subarray(cut)]));
  const view = new Bun.WebView({ width: 1280, height: 800 });
  let webkit = "";
  try {
    await Promise.race([view.navigate("file://" + html), Bun.sleep(20_000)]);
    webkit = String(await view.evaluate(OUTLINE));
  } finally {
    view.close();
  }
  const ours = join(out, `${id}.ours.txt`);
  const proc = Bun.spawn([dump, html, ours], { env: process.env, stderr: "pipe" });
  if ((await proc.exited) !== 0) {
    console.log(`${id}: domdump failed: ${(await new Response(proc.stderr).text()).slice(-300)}`);
    continue;
  }
  const w = webkit.toLowerCase().split("\n").filter(Boolean);
  const o = readFileSync(ours, "utf8").toLowerCase().split("\n").filter(Boolean);
  let i = 0;
  while (i < w.length && i < o.length && w[i] === o[i]) i++;
  if (i === w.length && i === o.length) {
    same++;
    continue;
  }
  console.log(`${id}: parts at line ${i + 1} (${w.length} lines in WebKit, ${o.length} in ours)`);
  console.log(`  webkit: ${w.slice(Math.max(0, i - 2), i + 3).join(" | ")}`);
  console.log(`  ours:   ${o.slice(Math.max(0, i - 2), i + 3).join(" | ")}`);
}
console.log(`${same} of ${ids.length} pages parse to the same tree`);
