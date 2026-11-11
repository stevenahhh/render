#!/usr/bin/env bun
// Compares this renderer with a real browser engine, for use while developing.
//
// The page is loaded in a headless browser (Bun.WebView). Every visible box is
// measured with getBoundingClientRect and its computed style is read. The same
// elements are then looked up in `render --boxes`. The report shows where the
// two layouts first diverge and which computed properties differ there, which
// usually names the missing or wrong CSS feature directly.
//
//   bun tools/check.ts <page.html> [--width 1280] [--height 800] [--tolerance 4]
//                      [--limit 20] [--explain <text>] [--out <dir>]
//
// --explain prints the ancestor chain of elements whose label contains <text>.
// --out saves the browser's screenshot (ref.png) and ours (ours.bmp).

import { dirname, basename, join, resolve } from "node:path";
import { mkdir } from "node:fs/promises";

type Rect = [number, number, number, number];
type Styles = Record<string, string>;
type Ref = { sel: string; label: string; rect: Rect; st: Styles };
type Row = { ref: Ref; rect: Rect | null; st: Styles | null; delta: number };

const args = Bun.argv.slice(2);
const take = (name: string) => {
  const i = args.indexOf(name);
  return i < 0 ? null : args.splice(i, 2)[1];
};
const width = Number(take("--width") ?? 1280);
const height = Number(take("--height") ?? 800);
const tolerance = Number(take("--tolerance") ?? 4);
const limit = Number(take("--limit") ?? 20);
const explain = take("--explain");
const out = take("--out");
const outDir = out && resolve(out);
const page = args[0] && resolve(args[0]);
if (!page || !(await Bun.file(page).exists())) {
  console.error("usage: bun tools/check.ts <page.html> [--width n] [--height n] [--tolerance px] [--limit n] [--explain text] [--out dir]");
  process.exit(2);
}
const root = dirname(page);
const renderer = resolve(import.meta.dir, "../target/release/render");
if (!(await Bun.file(renderer).exists())) {
  console.error(`missing ${renderer}: run cargo build --release first`);
  process.exit(2);
}

const SIDES = ["top", "right", "bottom", "left"];
const PROPS = [
  "display", "position", "box-sizing", "width", "height", "min-height", "max-width",
  ...SIDES.flatMap((s) => [`margin-${s}`, `padding-${s}`, `border-${s}-width`, s]),
  "font-size", "font-weight", "line-height", "text-align", "white-space", "opacity", "z-index",
  "flex-direction", "flex-wrap", "flex-grow", "flex-shrink", "justify-content", "align-items",
];

// Runs in the browser. Elements are addressed by an :nth-of-type path, which
// both engines can resolve without sharing node identities.
const collect = `(() => {
  const props = ${JSON.stringify(PROPS)};
  const skip = new Set(["script", "style", "noscript", "br", "source", "template", "option", "head", "meta", "link", "title"]);
  const path = (el) => {
    const parts = [];
    for (let e = el; e && e !== document.documentElement; e = e.parentElement) {
      let i = 1;
      for (let s = e.previousElementSibling; s; s = s.previousElementSibling) if (s.localName === e.localName) i++;
      parts.unshift(e.localName + ":nth-of-type(" + i + ")");
    }
    return "html > " + parts.join(" > ");
  };
  const out = [];
  for (const el of document.body.querySelectorAll("*")) {
    if (skip.has(el.localName) || (el.localName !== "svg" && el.closest("svg"))) continue;
    const cs = getComputedStyle(el);
    if (cs.display === "none" || cs.display === "inline" || cs.display === "contents" || cs.visibility === "hidden") continue;
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) continue;
    const cls = typeof el.className === "string" && el.className.trim() ? "." + el.className.trim().split(/\\s+/).slice(0, 2).join(".") : "";
    const st = {};
    for (const p of props) st[p] = cs.getPropertyValue(p);
    out.push({ sel: path(el), label: el.localName + (el.id ? "#" + el.id : "") + cls, rect: [r.x + scrollX, r.y + scrollY, r.width, r.height], st });
  }
  return JSON.stringify({ boxes: out, height: document.documentElement.scrollHeight });
})()`;

const server = Bun.serve({
  port: 0,
  async fetch(req: Request) {
    const file = Bun.file(join(root, decodeURIComponent(new URL(req.url).pathname)));
    return (await file.exists()) ? new Response(file) : new Response("not found", { status: 404 });
  },
});

let reference: { boxes: Ref[]; height: number };
let screenshot: Blob | null = null;
{
  await using view = new Bun.WebView({ width, height });
  await view.navigate(`http://localhost:${server.port}/${encodeURIComponent(basename(page))}`);
  const deadline = Date.now() + 15000;
  while ((await view.evaluate("document.readyState")) !== "complete" && Date.now() < deadline) await Bun.sleep(100);
  // Scripts keep changing the page after load; measure once the layout stops moving.
  let last = "";
  for (let i = 0; i < 20; i++) {
    const now = String(await view.evaluate(collect));
    if (now === last) break;
    last = now;
    await Bun.sleep(250);
  }
  reference = JSON.parse(last);
  if (outDir) screenshot = await view.screenshot();
}
server.stop(true);

const listFile = join(process.env.TMPDIR ?? "/tmp", `render-check-${process.pid}.txt`);
await Bun.write(listFile, reference.boxes.map((b) => b.sel).join("\n") + "\n");
const size = ["--width", String(width), "--height", String(height)];
const run = Bun.spawn([renderer, page, "--boxes", listFile, ...size], { stdout: "pipe", stderr: "ignore" });
const stdout = await new Response(run.stdout).text();
if ((await run.exited) !== 0) {
  console.error("render --boxes failed");
  process.exit(1);
}

const parseStyle = (s: string): Styles => Object.fromEntries(s.split(";").filter(Boolean).map((d) => d.split(":") as [string, string]));
const ours = new Map<number, { rect: Rect | null; st: Styles | null }>();
let ourHeight = 0;
for (const line of stdout.split("\n")) {
  const [head, style] = line.split(" | ");
  const f = head.split(" ");
  if (f[0] === "height") ourHeight = Number(f[1]);
  else if (f.length >= 2) ours.set(Number(f[0]), { rect: f.length >= 5 ? (f.slice(1, 5).map(Number) as Rect) : null, st: style ? parseStyle(style) : null });
}

const px = (v: string | undefined) => (v && /^-?[\d.]+px$/.test(v) ? parseFloat(v) : null);
const ALIAS: Record<string, string> = {
  "flow-root": "block", table: "block", "table-cell": "block", "flex-start": "start", "flex-end": "end", normal: "normal",
  left: "left", "-webkit-auto": "left", "pre-wrap": "pre", "break-spaces": "pre", none: "none",
  clip: "hidden",
};
// Browser and renderer values in one vocabulary, or null when the pair cannot be compared.
function comparable(prop: string, browser: string, render: string, a: Styles, b: Styles): [string, string] | null {
  const flex = /flex|grid/.test(a.display) || /flex/.test(b.display);
  if (/^flex-|^justify-|^align-/.test(prop) && !flex) return null;
  if (SIDES.includes(prop) && a.position === "static" && b.position === "static") return null;
  if (prop === "max-width" && browser === "none") return null;
  if (prop === "font-weight") return [Number(browser) >= 600 || browser === "bold" ? "bold" : "normal", render];
  if (prop === "justify-content" || prop === "align-items") {
    const fallback = prop === "justify-content" ? "start" : "stretch";
    const n = (v: string) => (v === "normal" ? fallback : (ALIAS[v] ?? v).replace("left", "start").replace("right", "end"));
    return [n(browser), n(render)];
  }
  if (prop === "text-align") return [(ALIAS[browser] ?? browser).replace("start", "left").replace("end", "right"), render];
  const [x, y] = [px(browser), px(render)];
  if (x !== null && y !== null) return Math.abs(x - y) <= 0.6 ? [browser, browser] : [browser, render];
  // The browser reports used lengths in px; ours may still be a percentage or auto.
  if (x !== null || y !== null) return /^(width|height|min-height|max-width|line-height)$/.test(prop) || /[%c?]|auto/.test(render) ? null : [browser, render];
  if (prop === "opacity" || prop.startsWith("flex-g") || prop.startsWith("flex-s")) return Number(browser) === Number(render) ? null : [browser, render];
  return [ALIAS[browser] ?? browser, ALIAS[render] ?? render];
}
function why(r: Row): string {
  if (!r.st) return "";
  const diffs: string[] = [];
  for (const p of PROPS) {
    const pair = comparable(p, r.ref.st[p] ?? "", r.st[p] ?? "", r.ref.st, r.st);
    if (pair && pair[0] !== pair[1]) diffs.push(`${p} ${pair[0]} -> ${pair[1]}`);
  }
  return diffs.join(", ");
}

const rows: Row[] = reference.boxes.map((ref, i) => {
  const o = ours.get(i);
  const rect = o?.rect ?? null;
  const delta = rect ? Math.max(...rect.map((v, k) => Math.abs(v - ref.rect[k]))) : Infinity;
  return { ref, rect, st: o?.st ?? null, delta };
});
const bySel = new Map(rows.map((r) => [r.ref.sel, r]));
const ok = (r: Row | undefined) => !!r && r.delta <= tolerance;
const parentOf = (sel: string) => sel.slice(0, sel.lastIndexOf(" > "));
const good = rows.filter(ok);
const missing = rows.filter((r) => !r.rect);
const off = rows.filter((r) => r.rect && !ok(r));
// A wrong box drags its descendants along, so the useful entries are the
// ones whose parent still agrees with the browser.
const measuredParent = (sel: string) => {
  for (let p = parentOf(sel); p.includes(" > "); p = parentOf(p)) if (bySel.has(p)) return bySel.get(p);
  return undefined;
};
const firstOff = off.filter((r) => {
  const parent = measuredParent(r.ref.sel);
  return !parent || ok(parent);
});

const pct = (n: number) => ((100 * n) / Math.max(1, rows.length)).toFixed(1) + "%";
const fmt = (v: Rect | null) => (v ? v.map((n) => n.toFixed(0).padStart(5)).join(" ") : "          (no box)     ");
const line = (r: Row, indent = "") => {
  console.log(`${indent}${fmt(r.ref.rect)}  |${fmt(r.rect)}  ${ok(r) ? "ok " : "OFF"} ${r.ref.label}`);
  const reason = why(r);
  if (reason && !ok(r)) console.log(`${indent}      why: ${reason}`);
};

console.log(`${basename(page)} at ${width}x${height}, tolerance ${tolerance}px`);
console.log(`document height: browser ${reference.height}, render ${ourHeight.toFixed(0)}`);
console.log(`boxes: ${rows.length}  match ${good.length} (${pct(good.length)})  off ${off.length} (${pct(off.length)})  no box ${missing.length} (${pct(missing.length)})`);
const header = "    x     y     w     h  |    x     y     w     h";
console.log(`\nfirst divergences (parent matches, element does not): ${firstOff.length}`);
console.log(`${header}       browser | render, "why" lists computed styles as browser -> render`);
for (const r of firstOff.sort((a, b) => a.ref.rect[1] - b.ref.rect[1]).slice(0, limit)) line(r);
if (missing.length) {
  console.log(`\nno box in render: ${missing.length}`);
  for (const r of missing.slice(0, limit)) line(r);
}
if (explain) {
  for (const r of rows.filter((r) => r.ref.label.includes(explain)).slice(0, 3)) {
    console.log(`\nancestors of ${r.ref.label}`);
    const chain: Row[] = [];
    for (let sel = r.ref.sel; sel.includes(" > "); sel = parentOf(sel)) {
      const row = bySel.get(sel);
      if (row) chain.unshift(row);
    }
    chain.forEach((row, depth) => line(row, " ".repeat(Math.min(depth, 12))));
  }
}
if (outDir) {
  await mkdir(outDir, { recursive: true });
  if (screenshot) await Bun.write(join(outDir, "ref.png"), screenshot);
  await Bun.spawn([renderer, page, "--screenshot", join(outDir, "ours.bmp"), ...size], { stdout: "ignore", stderr: "ignore" }).exited;
  console.log(`\nscreenshots: ${join(outDir, "ref.png")} and ${join(outDir, "ours.bmp")}`);
}
