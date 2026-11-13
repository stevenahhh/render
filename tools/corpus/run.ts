#!/usr/bin/env bun
// Scores the renderer on the frozen corpus: every frozen site is replayed to it and compared
// with the WebKit reference taken at freeze time, and every WPT test is rendered and checked.
//
//   bun tools/corpus/run.ts [--jobs 4] [--only <id substring>] [--bin path] [--out dir] [--skip-wpt] [--skip-sites] [--pass 70]
//   bun tools/corpus/run.ts --compare <results dir A> <results dir B> [--tolerance points]
import { existsSync, mkdirSync, rmSync, copyFileSync } from "node:fs";
import { join } from "node:path";
import { CORPUS, REPO, flags, pool, sites, type SiteSpec } from "./lib/corpus.ts";
import { Site } from "./lib/replay.ts";
import { parseBoxes, runRenderer } from "./lib/ours.ts";
import { small, ssim } from "./lib/pixels.ts";
import { layoutScore, textScore, total } from "./lib/score.ts";
import { attribute, classify } from "./lib/classify.ts";
import { runWpt } from "./lib/wpt-run.ts";
import { writeReport } from "./lib/report.ts";
import type { Snapshot } from "./lib/webkit.ts";

// Calibrated by eye on PNG pairs: see README, "Pass threshold". `--pass 101` keeps every PNG pair.
const DEFAULT_PASS = 58;

const argv = Bun.argv.slice(2);
if (argv[0] === "--compare") {
  const [a, b] = [argv[1], argv[2]].map((d) => Bun.file(join(d, "summary.json")).json());
  const [sa, sb] = await Promise.all([a, b]);
  const scores = (s: { pages: { id: string; score: number | null }[]; wpt: { results: { path: string; pass: boolean }[] } }) => new Map([...s.pages.map((p) => [p.id, p.score] as const), ...s.wpt.results.map((r) => [`wpt ${r.path}`, r.pass ? 1 : 0] as const)]);
  const [ma, mb] = [scores(sa), scores(sb)];
  const tolAt = argv.indexOf("--tolerance");
  const tolerance = tolAt < 0 ? 0 : Number(argv[tolAt + 1]);
  const delta = (k: string) => Math.abs((ma.get(k) ?? 0) - (mb.get(k) ?? 0));
  const diffs = [...ma.keys()].filter((k) => ma.get(k) !== mb.get(k));
  const over = diffs.filter((k) => delta(k) > tolerance);
  console.log(`${ma.size} entries compared, ${diffs.length} differ, largest ${Math.max(0, ...diffs.map(delta)).toFixed(2)} points, ${over.length} over the tolerance of ${tolerance}`);
  console.log(`sites: mean ${sa.sites.mean} -> ${sb.sites.mean}, pass ${sa.sites.pass} -> ${sb.sites.pass}; WPT reftests ${sa.wpt.reftests.pass} -> ${sb.wpt.reftests.pass}, testharness ${sa.wpt.testharness.pass} -> ${sb.wpt.testharness.pass}`);
  for (const k of diffs) console.log(`  ${k}: ${ma.get(k)} -> ${mb.get(k)}`);
  process.exit(over.length === 0 ? 0 : 1);
}

const f = flags(argv);
const outAt = f.rest.indexOf("--out");
const day = new Date().toISOString().slice(0, 10);
const out = outAt >= 0 ? f.rest[outAt + 1] : join(CORPUS, "results", day);
const skipWpt = f.rest.includes("--skip-wpt");
const skipSites = f.rest.includes("--skip-sites");
const passAt = f.rest.indexOf("--pass");
const PASS = passAt >= 0 ? Number(f.rest[passAt + 1]) : DEFAULT_PASS;
mkdirSync(join(out, "pages"), { recursive: true });
mkdirSync(join(out, "png"), { recursive: true });

async function uaDrops(): Promise<Set<string>> {
  const blank = join(out, "blank.html");
  const sel = join(out, "blank.sel");
  await Bun.write(blank, "<!doctype html><title>x</title><p>x");
  await Bun.write(sel, "p\n");
  const run = await runRenderer(f.bin, [blank, "--boxes", sel], { RENDER_CSS_DROPS: "1" });
  rmSync(blank);
  rmSync(sel);
  return new Set(Object.keys(parseBoxes(run, 1).cssDrops));
}

async function scoreSite(s: SiteSpec, ua: Set<string>) {
  const refPath = join(s.dir, "ref", "ref.json");
  const ref = existsSync(refPath) ? ((await Bun.file(refPath).json()) as Snapshot & { error?: string }) : null;
  if (!ref || ref.error || !ref.words) return { id: s.id, url: s.url, score: null, pass: null, excluded: ref?.error ?? "no WebKit reference" };
  // Scripts of some sites (bot defences, hydration that checks the host) leave WebKit blank on a loopback replay.
  if (ref.words.length < 5 && ref.boxes.length <= 2) return { id: s.id, url: s.url, score: null, pass: null, excluded: `WebKit shows a blank page under replay (${ref.words.length} words, ${ref.boxes.length} boxes)` };
  const site = new Site(join(s.dir, "store"), s.base, "replay", s.host);
  const entry = site.url(s.url);
  const tmp = join(out, "tmp", s.id);
  mkdirSync(tmp, { recursive: true });
  const sel = join(tmp, "sel.txt");
  await Bun.write(sel, ref.boxes.map((b) => b.sel).join("\n") + "\n");
  const shots = [join(tmp, "ours-0.bmp"), join(tmp, "ours-1.bmp")];
  const [r0, r1, rb] = await Promise.all([
    runRenderer(f.bin, [entry, "--screenshot", shots[0]], {}),
    runRenderer(f.bin, [entry, "--screenshot", shots[1], "--scroll", "800"], {}),
    runRenderer(f.bin, [entry, "--boxes", sel], { RENDER_TEXT_RUNS: "1", RENDER_CSS_DROPS: "1" }),
  ]);
  site.stop();
  const worst = [r0, r1, rb].find((r) => r.outcome !== "ok")?.outcome ?? "ok";
  const boxes = parseBoxes(rb, ref.boxes.length);
  const text = textScore(ref.words, rb.outcome === "ok" ? boxes.words : []);
  const layout = layoutScore(ref.boxes, rb.outcome === "ok" ? boxes.rects : ref.boxes.map(() => "nomatch" as const));
  const refShots = [join(s.dir, "ref", "ref-0.png"), join(s.dir, "ref", "ref-1.png")];
  const sim = async (i: number, r: typeof r0) => (r.outcome === "ok" && existsSync(shots[i]) ? ssim(await small(shots[i]), await small(refShots[i])) : 0);
  const pixels = ((await sim(0, r0)) + (await sim(1, r1))) / 2;
  const score = Math.round(total(text.score, layout.score, pixels) * 100) / 100;
  const pass = score >= PASS;
  const servedTypes = site.served.map((x) => x.type);
  const causes = classify({ outcome: worst, boxes, webkitErrors: ref.errors ?? [], uaDrops: ua, text, layout, misses: site.misses, servedTypes });
  const attributed = pass ? [] : attribute(100 - score, causes);
  const pngs: string[] = [];
  if (!pass) {
    for (const i of [0, 1]) {
      const [o, w] = [join(out, "png", `${s.id}-ours-${i}.png`), join(out, "png", `${s.id}-webkit-${i}.png`)];
      if (existsSync(shots[i])) await new Bun.Image(Bun.file(shots[i])).png().write(o);
      copyFileSync(refShots[i], w);
      pngs.push(o, w);
    }
  }
  rmSync(tmp, { recursive: true, force: true });
  const page = {
    id: s.id, url: s.url, rank: s.rank, score, pass,
    parts: { text: text.score, layout: layout.score, pixels },
    text: { recall: text.recall, precision: text.precision, totalChars: text.totalChars, missingChars: text.missingChars, misplacedChars: Math.round(text.misplacedChars), missingWords: text.missingWords },
    layout: layout.boxes.filter((b) => b.iou < 0.5).slice(0, 25).map((b) => ({ sel: b.sel, tag: b.tag, iou: Math.round(b.iou * 100) / 100, ours: b.ours, webkit: b.ref.rect })),
    run: { outcome: worst, ms: [r0.ms, r1.ms, rb.ms], exit: [r0.code, r1.code, rb.code], stderrTail: boxes.stderrTail },
    jsErrors: boxes.jsErrors.slice(0, 20), webkitErrors: ref.errors ?? [], cssDrops: boxes.cssDrops, misses: site.misses.slice(0, 50), missCount: site.misses.length,
    causes, attributed, pngs,
  };
  await Bun.write(join(out, "pages", `${s.id}.json`), JSON.stringify(page, null, 1));
  console.log(`${s.id} ${score.toFixed(1)} ${pass ? "PASS" : "fail"} text ${text.score.toFixed(2)} layout ${layout.score.toFixed(2)} pixels ${pixels.toFixed(2)} ${worst}`);
  return page;
}

const started = Date.now();
const ua = await uaDrops();
const list = skipSites ? [] : (await sites()).filter((s) => !f.only || s.id.includes(f.only));
const pages = await pool(list, f.jobs, (s) => scoreSite(s, ua));
const sitesMs = Date.now() - started;
const wpt = skipWpt ? { results: [] } : await runWpt({ out, jobs: f.jobs, bin: f.bin });
rmSync(join(out, "tmp"), { recursive: true, force: true });
const commit = (await Bun.$`git -C ${REPO} rev-parse --short HEAD`.nothrow().quiet().text()).trim();
await writeReport({ out, pages, wpt, pass: PASS, commit, bin: f.bin, ms: { sites: sitesMs, total: Date.now() - started }, jobs: f.jobs });
console.log(`done in ${((Date.now() - started) / 1000).toFixed(0)} s -> ${out}`);
