import { join } from "node:path";
import type { Attributed } from "./classify.ts";
import type { WptResult } from "./wpt-run.ts";

type Page = { id: string; url: string; score: number | null; pass: boolean | null; excluded?: string; parts?: { text: number; layout: number; pixels: number }; attributed?: Attributed[]; pngs?: string[] };
type Input = { out: string; pages: Page[]; wpt: { commit?: string; results: WptResult[] }; pass: number; commit: string; bin: string; ms: { sites: number; total: number }; jobs: number };
type Row = { category: string; cause: string; pages: number; points: number; share: number; examples: { id: string; pngs: string[] }[] };

const pct = (n: number, d: number) => (d === 0 ? "-" : `${((100 * n) / d).toFixed(1)}%`);
const r2 = (n: number) => Math.round(n * 100) / 100;

function rank(failing: Page[], key: (a: Attributed) => string): Row[] {
  const lostTotal = failing.reduce((s, p) => s + (100 - (p.score ?? 0)), 0);
  const rows = new Map<string, Row>();
  for (const p of failing) {
    const seen = new Set<string>();
    for (const a of p.attributed ?? []) {
      const k = key(a);
      const row = rows.get(k) ?? { category: a.category, cause: k === a.category ? "" : a.cause, pages: 0, points: 0, share: 0, examples: [] };
      row.points += a.points;
      if (!seen.has(k)) {
        seen.add(k);
        row.pages++;
        if (row.examples.length < 3) row.examples.push({ id: p.id, pngs: (p.pngs ?? []).slice(0, 2) });
      }
      rows.set(k, row);
    }
  }
  return [...rows.values()].map((r) => ({ ...r, points: r2(r.points), share: lostTotal === 0 ? 0 : r2((100 * r.points) / lostTotal) })).sort((a, b) => b.points - a.points);
}

export async function writeReport(i: Input) {
  const scored = i.pages.filter((p) => p.score !== null);
  const scores = scored.map((p) => p.score!).sort((a, b) => a - b);
  const mean = scores.length ? r2(scores.reduce((s, x) => s + x, 0) / scores.length) : 0;
  const median = scores.length ? scores[Math.floor(scores.length / 2)] : 0;
  const passing = scored.filter((p) => p.pass).length;
  const failing = scored.filter((p) => !p.pass);
  const categories = rank(failing, (a) => a.category);
  const causes = rank(failing, (a) => `${a.category}: ${a.cause}`);
  const ref = i.wpt.results.filter((r) => r.kind === "reftest");
  const harness = i.wpt.results.filter((r) => r.kind === "testharness");
  const valid = (rs: WptResult[]) => rs.filter((r) => r.webkitPass);
  const wptSummary = {
    commit: i.wpt.commit ?? null,
    reftests: { total: ref.length, pass: ref.filter((r) => r.pass).length, webkitPass: valid(ref).length, passWhereWebkitPasses: valid(ref).filter((r) => r.pass).length },
    testharness: { total: harness.length, pass: harness.filter((r) => r.pass).length, webkitPass: valid(harness).length, passWhereWebkitPasses: valid(harness).filter((r) => r.pass).length },
  };
  const byGroup = new Map<string, { total: number; pass: number }>();
  for (const r of ref) {
    const g = r.path.split("/").slice(0, r.path.startsWith("css/CSS2/") ? 3 : 2).join("/");
    const e = byGroup.get(g) ?? { total: 0, pass: 0 };
    e.total++;
    if (r.pass) e.pass++;
    byGroup.set(g, e);
  }
  const summary = {
    date: new Date().toISOString(), commit: i.commit, bin: i.bin, jobs: i.jobs, passThreshold: i.pass, runtimeSeconds: { sites: Math.round(i.ms.sites / 1000), total: Math.round(i.ms.total / 1000) },
    sites: { total: i.pages.length, scored: scored.length, excluded: i.pages.filter((p) => p.score === null).map((p) => ({ id: p.id, why: p.excluded })), mean, median, pass: passing, passRate: scored.length ? r2((100 * passing) / scored.length) : 0 },
    wpt: { ...wptSummary, groups: Object.fromEntries(byGroup), results: i.wpt.results.map(({ pngs, ...r }) => r) },
    categories, causes,
    pages: i.pages.map((p) => ({ id: p.id, url: p.url, score: p.score, pass: p.pass, parts: p.parts ?? null })),
  };
  await Bun.write(join(i.out, "summary.json"), JSON.stringify(summary, null, 1));

  const md: string[] = [];
  md.push(`# Corpus run ${summary.date.slice(0, 10)}`, "");
  md.push(`Renderer ${i.commit} (${i.bin}), ${i.jobs} jobs, ${summary.runtimeSeconds.total} s in total (${summary.runtimeSeconds.sites} s for sites).`, "");
  md.push(`## Sites`, "", `${scored.length} of ${i.pages.length} sites scored. Mean ${mean}, median ${median}. ${passing} pass (score >= ${i.pass}): ${pct(passing, scored.length)}.`, "");
  if (summary.sites.excluded.length) md.push(`Excluded (no WebKit reference): ${summary.sites.excluded.map((e) => `${e.id} (${e.why})`).join(", ")}.`, "");
  md.push(`## WPT (commit ${wptSummary.commit?.slice(0, 12)})`, "");
  md.push(`Reftests: ${wptSummary.reftests.pass}/${wptSummary.reftests.total} pass (${pct(wptSummary.reftests.pass, wptSummary.reftests.total)}); on the ${wptSummary.reftests.webkitPass} WebKit passes, ${wptSummary.reftests.passWhereWebkitPasses} (${pct(wptSummary.reftests.passWhereWebkitPasses, wptSummary.reftests.webkitPass)}).`);
  md.push(`DOM testharness: ${wptSummary.testharness.pass}/${wptSummary.testharness.total} pass; on the ${wptSummary.testharness.webkitPass} WebKit passes, ${wptSummary.testharness.passWhereWebkitPasses}.`, "");
  md.push(`| group | pass | total |`, `|---|---|---|`, ...[...byGroup].map(([g, e]) => `| ${g} | ${e.pass} | ${e.total} |`), "");
  md.push(`## Why sites fail`, "", `Each failing page's lost points (100 - score) are split equally over the cause categories found on it, then equally over the causes in a category. Share = points / all points lost by failing pages.`, "");
  md.push(`| category | pages | points lost | share |`, `|---|---|---|---|`, ...categories.map((c) => `| ${c.category} | ${c.pages} | ${c.points} | ${c.share}% |`), "");
  md.push(`| # | cause | pages | share | example PNGs (ours, WebKit) |`, `|---|---|---|---|---|`);
  causes.slice(0, 40).forEach((c, n) => md.push(`| ${n + 1} | ${c.category}: ${c.cause.replace(/\|/g, "\\|")} | ${c.pages} | ${c.share}% | ${c.examples.map((e) => `${e.id} ${e.pngs.map((p) => p.slice(i.out.length + 1)).join(" ")}`).join("; ")} |`));
  md.push("", `## Pages`, "", `| page | score | text | layout | pixels | pass |`, `|---|---|---|---|---|---|`);
  for (const p of [...scored].sort((a, b) => a.score! - b.score!)) md.push(`| ${p.id} | ${p.score} | ${p.parts?.text.toFixed(2)} | ${p.parts?.layout.toFixed(2)} | ${p.parts?.pixels.toFixed(2)} | ${p.pass ? "yes" : "no"} |`);
  await Bun.write(join(i.out, "report.md"), md.join("\n") + "\n");
}
