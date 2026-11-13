#!/usr/bin/env bun
// Freezes the site corpus: each page is loaded once through the recording server by WebKit
// and by our renderer (so the store holds what either engine asks for), then sealed, then
// the WebKit reference is taken from the sealed store alone.
//
//   bun tools/corpus/freeze.ts [--jobs 4] [--only <id substring>] [--bin target/release/render] [--phase all|record|reference|check]
//
// --phase check lists stored scripts that parse before the URL rewrite and not after it.
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Site } from "./lib/replay.ts";
import { rewriteText } from "./lib/rewrite.ts";
import { open, screenshot, scrollTo, snapshot } from "./lib/webkit.ts";
import { runRenderer } from "./lib/ours.ts";
import { flags, pool, sites, type SiteSpec } from "./lib/corpus.ts";

const f = flags(Bun.argv.slice(2));
const phaseAt = f.rest.indexOf("--phase");
const phase = phaseAt < 0 ? "all" : f.rest[phaseAt + 1];

async function record(s: SiteSpec) {
  rmSync(join(s.dir, "store"), { recursive: true, force: true });
  const site = new Site(join(s.dir, "store"), s.base, "record", s.host);
  const entry = site.url(s.url);
  const activity = () => site.served.length;
  const visit = await open(entry, activity);
  try {
    if (visit.loaded !== "error") await scrollTo(visit.view, 800, activity);
  } finally {
    visit.view.close();
  }
  const tmp = join(s.dir, "record-tmp.bmp");
  const ours = [];
  for (const scroll of ["0", "800"]) ours.push(await runRenderer(f.bin, [entry, "--screenshot", tmp, "--scroll", scroll], { RENDER_LOOPBACK_ONLY: "0" }));
  rmSync(tmp, { force: true });
  site.seal();
  site.stop();
  const log = { entry: s.url, webkit: visit.loaded, webkitError: visit.error ?? null, ours: ours.map((r) => r.outcome), requests: site.served.length, hosts: site.hosts.length, recordedAt: new Date().toISOString() };
  await Bun.write(join(s.dir, "record.json"), JSON.stringify(log, null, 1));
  return log;
}

async function reference(s: SiteSpec) {
  const refDir = join(s.dir, "ref");
  mkdirSync(refDir, { recursive: true });
  const site = new Site(join(s.dir, "store"), s.base, "replay", s.host);
  const activity = () => site.served.length + site.misses.length;
  const visit = await open(site.url(s.url), activity);
  try {
    if (visit.loaded === "error") throw new Error(visit.error);
    const snap = await snapshot(visit.view);
    await screenshot(visit.view, join(refDir, "ref-0.png"));
    await scrollTo(visit.view, 800, activity);
    await screenshot(visit.view, join(refDir, "ref-1.png"));
    const doc = { ...snap, loaded: visit.loaded, misses: site.misses.slice(0, 200), missCount: site.misses.length };
    await Bun.write(join(refDir, "ref.json"), JSON.stringify(doc));
    return { id: s.id, ok: true, words: snap.words.length, boxes: snap.boxes.length, misses: site.misses.length };
  } catch (e) {
    await Bun.write(join(refDir, "ref.json"), JSON.stringify({ error: e instanceof Error ? e.message : String(e) }));
    return { id: s.id, ok: false, error: e instanceof Error ? e.message : String(e) };
  } finally {
    visit.view.close();
    site.stop();
  }
}

function check(s: SiteSpec): string[] {
  const store = join(s.dir, "store");
  const index: Record<string, { headers: [string, string][]; body: string | null }> = JSON.parse(readFileSync(join(store, "index.json"), "utf8"));
  const parser = new Bun.Transpiler({ loader: "js" });
  const parses = (src: string) => {
    try {
      parser.scan(src);
      return true;
    } catch {
      return false;
    }
  };
  const broken: string[] = [];
  for (const [key, e] of Object.entries(index)) {
    const type = e.headers.find(([k]) => k.toLowerCase() === "content-type")?.[1] ?? "";
    if (!e.body) continue;
    const text = readFileSync(join(store, "bodies", e.body), "latin1");
    const scripts = /javascript|ecmascript/i.test(type) ? [text] : /html/i.test(type) ? [...text.matchAll(/<script(?![^>]*type=["']?(?:application\/(?:ld\+)?json|text\/template))[^>]*>([\s\S]*?)<\/script>/gi)].map((m) => m[1]) : [];
    if (scripts.some((src) => parses(src) && !parses(rewriteText(src, () => 40000)))) broken.push(key);
  }
  return broken;
}

const list = (await sites()).filter((s) => !f.only || s.id.includes(f.only));
if (phase === "check") {
  const broken = list.filter((s) => existsSync(join(s.dir, "store", "index.json"))).map((s) => [s.id, check(s)] as const).filter(([, b]) => b.length);
  for (const [id, b] of broken) console.log(id, b.join(" "));
  console.log(`${broken.length} sites with scripts the rewrite breaks`);
  process.exit(0);
}
const started = Date.now();
await pool(list, f.jobs, async (s) => {
  mkdirSync(s.dir, { recursive: true });
  if (phase === "all" || phase === "record") console.log("record", s.id, JSON.stringify(await record(s)));
  if ((phase === "all" || phase === "reference") && existsSync(join(s.dir, "store", "hosts.json"))) console.log("reference", s.id, JSON.stringify(await reference(s)));
});
console.log(`froze ${list.length} sites in ${((Date.now() - started) / 1000).toFixed(0)} s`);
