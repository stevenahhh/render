import { existsSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { CORPUS, TOOL_DIR, pool } from "./corpus.ts";
import { runRenderer } from "./ours.ts";
import { exactDiff } from "./pixels.ts";
import { WPT_SIZE, type HarnessResult, type WptTest, harnessPass, reftestPass, serveWpt, wptUrl } from "../wpt.ts";

export type WptResult = { path: string; kind: WptTest["kind"]; pass: boolean; webkitPass: boolean | null; detail: string; pngs?: string[] };

export async function runWpt(o: { out: string; jobs: number; bin: string }): Promise<{ commit: string; results: WptResult[] }> {
  const spec = await Bun.file(join(TOOL_DIR, "wpt-tests.json")).json();
  const tests: WptTest[] = spec.tests;
  const webkitFile = join(CORPUS, "wpt-webkit.json");
  const webkit = new Map<string, boolean>(existsSync(webkitFile) ? (await Bun.file(webkitFile).json()).results.map((r: { path: string; pass: boolean }) => [r.path, r.pass]) : []);
  const tmp = join(o.out, "tmp", "wpt");
  const pngDir = join(o.out, "png", "wpt");
  mkdirSync(tmp, { recursive: true });
  mkdirSync(pngDir, { recursive: true });
  const server = serveWpt();
  const results = await pool(tests, o.jobs, async (t): Promise<WptResult> => {
    const webkitPass = webkit.get(t.path) ?? null;
    if (t.kind === "testharness") {
      const run = await runRenderer(o.bin, [wptUrl(t.path), "--screenshot", "/dev/null"], {}, WPT_SIZE);
      const line = run.stderr.split("\n").find((l) => l.startsWith("[js] WPT_RESULT "));
      const r: HarnessResult = line ? JSON.parse(line.slice("[js] WPT_RESULT ".length)) : null;
      const detail = run.outcome !== "ok" ? run.outcome : r ? `${r.tests.filter(([, s]) => s === 0).length}/${r.tests.length} subtests` : "no result";
      return { path: t.path, kind: t.kind, pass: run.outcome === "ok" && harnessPass(r), webkitPass, detail };
    }
    const name = t.path.replace(/[/.]/g, "_");
    const [a, b] = [join(tmp, `${name}-test.bmp`), join(tmp, `${name}-ref.bmp`)];
    const [ra, rb] = await Promise.all([runRenderer(o.bin, [wptUrl(t.path), "--screenshot", a], {}, WPT_SIZE), runRenderer(o.bin, [wptUrl(t.ref), "--screenshot", b], {}, WPT_SIZE)]);
    if (ra.outcome !== "ok" || rb.outcome !== "ok") return { path: t.path, kind: t.kind, pass: false, webkitPass, detail: `renderer ${ra.outcome}/${rb.outcome}` };
    const d = await exactDiff(a, b);
    const pass = reftestPass(d, t);
    const result: WptResult = { path: t.path, kind: t.kind, pass, webkitPass, detail: `${d.pixels} px differ, max ${d.maxDiff}` };
    if (!pass) {
      result.pngs = [join(pngDir, `${name}-test.png`), join(pngDir, `${name}-ref.png`)];
      await new Bun.Image(Bun.file(a)).png().write(result.pngs[0]);
      await new Bun.Image(Bun.file(b)).png().write(result.pngs[1]);
    }
    rmSync(a, { force: true });
    rmSync(b, { force: true });
    return result;
  });
  server.stop(true);
  return { commit: spec.commit, results };
}
