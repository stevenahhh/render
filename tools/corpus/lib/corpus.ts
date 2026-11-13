import { resolve, join } from "node:path";
import { HOSTS_PER_SITE } from "./replay.ts";

export const TOOL_DIR = resolve(import.meta.dir, "..");
export const REPO = resolve(TOOL_DIR, "../..");
export const CORPUS = process.env.CORPUS_DIR ?? "/Users/gahn/projects/1111-render/.omo/corpus";
export const DEFAULT_BIN = resolve(process.env.CARGO_TARGET_DIR ?? join(REPO, "target"), "release/render");
export const WPT_PORT = 20000;
// Below 45103 and the ephemeral range (49152+); 21420 and 45103 are taken on the owner's machine.
const SITE_PORT_BASE = 22000;
const PORTS_PER_SITE = HOSTS_PER_SITE;

export type SiteSpec = { readonly id: string; readonly index: number; readonly url: string; readonly host: string; readonly rank: number | null; readonly dir: string; readonly base: number };

export async function sites(): Promise<SiteSpec[]> {
  const doc = await Bun.file(join(TOOL_DIR, "sites.json")).json();
  const included = (doc.sites as { rank: number | null; url?: string; status: string }[]).filter((s) => s.status === "included" && s.url);
  return included.map((s, index) => {
    const host = new URL(s.url!).host;
    const id = `${String(index).padStart(3, "0")}-${host}`;
    return { id, index, url: s.url!, host, rank: s.rank, dir: join(CORPUS, "sites", id), base: SITE_PORT_BASE + index * PORTS_PER_SITE };
  });
}

export type Flags = { readonly jobs: number; readonly only: string | null; readonly bin: string; readonly rest: string[] };

export function flags(argv: string[]): Flags {
  const args = [...argv];
  const take = (name: string) => {
    const i = args.indexOf(name);
    return i < 0 ? null : args.splice(i, 2)[1];
  };
  return { jobs: Number(take("--jobs") ?? 4), only: take("--only"), bin: resolve(take("--bin") ?? DEFAULT_BIN), rest: args };
}

export async function pool<T, R>(items: T[], jobs: number, work: (item: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await work(items[i]);
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, jobs) }, worker));
  return out;
}
