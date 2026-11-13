#!/usr/bin/env bun
// Web Platform Tests slice, served from a pinned sparse checkout.
//
//   bun tools/corpus/wpt.ts fetch            clone WPT at the pinned commit into $CORPUS/wpt
//   bun tools/corpus/wpt.ts select           write tools/corpus/wpt-tests.json from the checkout
//   bun tools/corpus/wpt.ts webkit [--jobs]  run every test in WebKit once, as the sanity reference
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { CORPUS, TOOL_DIR, WPT_PORT, flags, pool } from "./lib/corpus.ts";
import { withTimeout } from "./lib/webkit.ts";
import { type Diff, exactDiff } from "./lib/pixels.ts";

export const WPT_COMMIT = "828aa8251840c71d49ea3e38c9ff529c3db8006b";
export const WPT_ROOT = join(CORPUS, "wpt");
export const WPT_SIZE = [800, 600] as const;
const SPARSE = ["css/CSS2/box-display", "css/CSS2/margin-padding-clear", "css/CSS2/normal-flow", "css/CSS2/floats", "css/CSS2/floats-clear", "css/CSS2/positioning", "css/CSS2/backgrounds", "css/CSS2/borders", "css/CSS2/text", "css/CSS2/linebox", "css/CSS2/selectors", "css/CSS2/support", "css/CSS2/reference", "css/css-flexbox", "css/css-grid", "css/css-backgrounds", "css/css-text", "css/css-box", "css/selectors", "css/support", "css/reference", "css/css-display", "fonts", "resources", "dom/nodes", "common", "images"];
const GROUPS = SPARSE.filter((d) => d.startsWith("css/") && !/support|reference/.test(d));
const PER_GROUP = 18;
const DOM_TESTS = 25;

export type Reftest = { kind: "reftest"; path: string; ref: string; match: boolean; fuzzy: { maxDiff: number; pixels: number } | null };
export type HarnessTest = { kind: "testharness"; path: string };
export type WptTest = Reftest | HarnessTest;

const REPORT_JS = `add_completion_callback(function (tests, status) {
  var r = { status: status.status, tests: tests.map(function (t) { return [t.name, t.status]; }) };
  window.__wptResult = r;
  console.log("WPT_RESULT " + JSON.stringify(r));
});`;

const TYPES: Record<string, string> = { html: "text/html", htm: "text/html", xht: "application/xhtml+xml", xhtml: "application/xhtml+xml", css: "text/css", js: "text/javascript", png: "image/png", jpg: "image/jpeg", gif: "image/gif", svg: "image/svg+xml", ttf: "font/ttf", otf: "font/otf", woff: "font/woff", woff2: "font/woff2", json: "application/json", txt: "text/plain" };

export function serveWpt() {
  return Bun.serve({
    hostname: "127.0.0.1",
    port: WPT_PORT,
    fetch(req) {
      const path = decodeURIComponent(new URL(req.url).pathname);
      if (path === "/resources/testharnessreport.js") return new Response(REPORT_JS, { headers: { "content-type": "text/javascript" } });
      const file = join(WPT_ROOT, path);
      if (!file.startsWith(WPT_ROOT) || !existsSync(file) || statSync(file).isDirectory()) return new Response("not found", { status: 404 });
      const ext = file.slice(file.lastIndexOf(".") + 1).toLowerCase();
      return new Response(Bun.file(file), { headers: { "content-type": TYPES[ext] ?? "application/octet-stream" } });
    },
  });
}

export const wptUrl = (path: string) => `http://127.0.0.1:${WPT_PORT}/${path}`;

function walk(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)]));
}

function parseFuzzy(src: string): Reftest["fuzzy"] {
  const m = /<meta\s+name=["']?fuzzy["']?\s+content=["']([^"']+)["']/i.exec(src);
  if (!m) return null;
  const top = (name: string) => Number(new RegExp(`${name}=(?:\\d+-)?(\\d+)`).exec(m[1])?.[1] ?? 0);
  return { maxDiff: top("maxDifference"), pixels: top("totalPixels") };
}

function select(): WptTest[] {
  const tests: WptTest[] = [];
  for (const group of GROUPS) {
    const found: Reftest[] = [];
    for (const file of walk(join(WPT_ROOT, group)).sort()) {
      const rel = relative(WPT_ROOT, file);
      if (!/\.(html?|xht|xhtml)$/.test(rel) || /\/(support|reference|resources)\//.test(rel) || /(-ref|-notref|ref-)\d*\.\w+$/.test(rel) || /-manual\./.test(rel)) continue;
      const src = readFileSync(file, "utf8");
      const link = /<link[^>]+rel=["']?(match|mismatch)["']?[^>]*>/i.exec(src);
      if (!link || /name=["']?flags["']?[^>]+content=["'][^"']*(paged|interact|animated|userstyle|should)/i.test(src)) continue;
      const href = /href=["']?([^"'\s>]+)/i.exec(link[0])?.[1];
      if (!href) continue;
      const ref = relative(WPT_ROOT, join(file, "..", href.split("?")[0]));
      if (!existsSync(join(WPT_ROOT, ref))) continue;
      found.push({ kind: "reftest", path: rel, ref, match: link[1].toLowerCase() === "match", fuzzy: parseFuzzy(src) });
    }
    const step = Math.max(1, found.length / PER_GROUP);
    for (let i = 0; i < Math.min(PER_GROUP, found.length); i++) tests.push(found[Math.floor(i * step)]);
  }
  const dom = walk(join(WPT_ROOT, "dom/nodes")).sort().map((f) => relative(WPT_ROOT, f)).filter((p) => /\.html$/.test(p) && /testharness\.js/.test(readFileSync(join(WPT_ROOT, p), "utf8")) && !/\/support\//.test(p));
  const step = dom.length / DOM_TESTS;
  for (let i = 0; i < DOM_TESTS; i++) tests.push({ kind: "testharness", path: dom[Math.floor(i * step)] });
  return tests;
}

export function reftestPass(d: Diff, t: Reftest): boolean {
  const same = d.pixels === 0 || (t.fuzzy !== null && d.pixels <= t.fuzzy.pixels && d.maxDiff <= t.fuzzy.maxDiff);
  return t.match ? same : !same;
}

export type HarnessResult = { status: number; tests: [string, number][] } | null;
export const harnessPass = (r: HarnessResult) => r !== null && r.status === 0 && r.tests.length > 0 && r.tests.every(([, s]) => s === 0);

async function webkitShot(url: string, out: string): Promise<boolean> {
  const view = new Bun.WebView({ width: WPT_SIZE[0], height: WPT_SIZE[1] });
  try {
    if ((await withTimeout(view.navigate(url), 20_000)) === "timeout") return false;
    await Bun.sleep(300);
    await Bun.write(out, await view.screenshot({ format: "png" }));
    return true;
  } finally {
    view.close();
  }
}

async function webkitHarness(url: string): Promise<HarnessResult> {
  const view = new Bun.WebView({ width: WPT_SIZE[0], height: WPT_SIZE[1] });
  try {
    if ((await withTimeout(view.navigate(url), 20_000)) === "timeout") return null;
    for (let i = 0; i < 50; i++) {
      const r = (await view.evaluate("window.__wptResult || null")) as HarnessResult;
      if (r) return r;
      await Bun.sleep(100);
    }
    return null;
  } finally {
    view.close();
  }
}

async function webkitAll(jobs: number) {
  const tests: WptTest[] = await Bun.file(join(TOOL_DIR, "wpt-tests.json")).json().then((d) => d.tests);
  const dir = join(CORPUS, "wpt-webkit");
  const server = serveWpt();
  const results = await pool(tests, jobs, async (t) => {
    if (t.kind === "testharness") {
      const r = await webkitHarness(wptUrl(t.path));
      return { path: t.path, pass: harnessPass(r), result: r };
    }
    const name = t.path.replace(/[/.]/g, "_");
    const [a, b] = [join(dir, `${name}-test.png`), join(dir, `${name}-ref.png`)];
    if (!(await webkitShot(wptUrl(t.path), a)) || !(await webkitShot(wptUrl(t.ref), b))) return { path: t.path, pass: false, result: "timeout" };
    const d = await exactDiff(a, b);
    return { path: t.path, pass: reftestPass(d, t), result: d };
  });
  server.stop(true);
  await Bun.write(join(CORPUS, "wpt-webkit.json"), JSON.stringify({ commit: WPT_COMMIT, results }, null, 1));
  console.log(`webkit: ${results.filter((r) => r.pass).length}/${results.length} pass`);
}

if (import.meta.main) {
  const f = flags(Bun.argv.slice(2));
  const cmd = f.rest[0];
  if (cmd === "fetch") {
    const sh = (s: string) => Bun.spawn(["bash", "-c", s], { env: process.env, stdin: "ignore", stdout: "inherit", stderr: "inherit" }).exited;
    await sh(`rm -rf '${WPT_ROOT}' && git init -q '${WPT_ROOT}' && cd '${WPT_ROOT}' && git remote add origin https://github.com/web-platform-tests/wpt.git && git sparse-checkout init --no-cone && printf '/%s/\\n' ${SPARSE.join(" ")} > .git/info/sparse-checkout && git fetch -q --depth 1 --filter=blob:none origin ${WPT_COMMIT} && git checkout -q FETCH_HEAD`);
  } else if (cmd === "select") {
    const tests = select();
    await Bun.write(join(TOOL_DIR, "wpt-tests.json"), JSON.stringify({ commit: WPT_COMMIT, groups: GROUPS, perGroup: PER_GROUP, tests }, null, 1) + "\n");
    console.log(`${tests.length} tests (${tests.filter((t) => t.kind === "reftest").length} reftests)`);
  } else if (cmd === "webkit") await webkitAll(f.jobs);
  else console.error("usage: wpt.ts fetch | select | webkit [--jobs n]");
}
