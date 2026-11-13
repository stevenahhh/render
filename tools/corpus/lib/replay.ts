// One frozen site: a store of responses keyed by method and original URL, served on one
// loopback port per original host. In record mode a miss is fetched live once and kept.
import { mkdirSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { LOOPBACK, bodyKind, hostsIn, rewriteBody } from "./rewrite.ts";

export type Mode = "record" | "replay";
type Entry = { status: number; headers: [string, string][]; body: string | null; error?: string };
type Index = Record<string, Entry>;

export const HOSTS_PER_SITE = 150;
const SINK = HOSTS_PER_SITE - 1;
const PER_HOST = 6;
const UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
const DROP_HEADERS = /^(content-encoding|content-length|transfer-encoding|connection|keep-alive|content-security-policy.*|strict-transport-security|x-frame-options|alt-svc|set-cookie|report-to|nel|access-control-.*|cross-origin-.*|timing-allow-origin)$/i;
const MAX_BODY = 12 << 20;

export class Site {
  readonly hosts: string[];
  readonly misses: string[] = [];
  readonly portErrors: number[] = [];
  readonly served: { url: string; status: number; type: string }[] = [];
  private readonly index: Index;
  private readonly servers = new Map<number, ReturnType<typeof Bun.serve>>();
  private readonly busy = new Map<string, number>();
  private readonly waiting = new Map<string, (() => void)[]>();

  constructor(readonly dir: string, readonly base: number, readonly mode: Mode, entryHost: string) {
    mkdirSync(join(dir, "bodies"), { recursive: true });
    this.index = existsSync(join(dir, "index.json")) ? JSON.parse(readFileSync(join(dir, "index.json"), "utf8")) : {};
    this.hosts = mode === "replay" ? JSON.parse(readFileSync(join(dir, "hosts.json"), "utf8")) : [entryHost];
    this.listen(base + SINK);
    if (mode === "replay") this.hosts.forEach((_, i) => this.listen(base + i));
    else this.listen(base);
  }

  url(original: string): string {
    const u = new URL(original);
    return `http://${LOOPBACK}:${this.portOf(u.host)}${u.pathname}${u.search}`;
  }

  portOf = (host: string): number => {
    let i = this.hosts.indexOf(host);
    if (i < 0 && this.mode === "record" && this.hosts.length < SINK) {
      i = this.hosts.push(host) - 1;
      this.listen(this.base + i);
    }
    return i < 0 ? this.base + SINK : this.base + i;
  };

  private listen(port: number) {
    if (this.servers.has(port) || this.portErrors.includes(port)) return;
    try {
      this.servers.set(port, Bun.serve({ hostname: LOOPBACK, port, idleTimeout: 60, fetch: (req) => this.answer(req, port - this.base) }));
    } catch (e) {
      this.portErrors.push(port);
      console.error(`port ${port} unavailable: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  // Ports follow a sorted host list in replay, so every run serves identical bytes: hosts
  // that were requested come first, then hosts only named in text, until the ports run out.
  seal() {
    const requested = new Set(Object.keys(this.index).map((k) => new URL(k.slice(k.indexOf(" ") + 1)).host));
    const named = new Set<string>();
    for (const e of Object.values(this.index)) {
      if (e.body && bodyKind(header(e, "content-type"), "") !== "binary") for (const h of hostsIn(readFileSync(join(this.dir, "bodies", e.body), "latin1"))) if (!requested.has(h)) named.add(h);
    }
    const sorted = [...[...requested].sort(), ...[...named].sort()].slice(0, SINK);
    writeFileSync(join(this.dir, "hosts.json"), JSON.stringify(sorted, null, 1));
    writeFileSync(join(this.dir, "index.json"), JSON.stringify(this.index));
  }

  stop() {
    for (const s of this.servers.values()) s.stop(true);
    this.servers.clear();
  }

  private async answer(req: Request, slot: number): Promise<Response> {
    const origin = req.headers.get("origin");
    const cors: [string, string][] = [["access-control-allow-origin", origin ?? "*"], ["access-control-allow-credentials", "true"], ["timing-allow-origin", "*"]];
    if (req.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: [...cors, ["access-control-allow-methods", "GET, POST, PUT, OPTIONS"], ["access-control-allow-headers", req.headers.get("access-control-request-headers") ?? "*"], ["access-control-max-age", "600"]] });
    }
    const u = new URL(req.url);
    const host = slot === SINK ? null : this.hosts[slot];
    if (!host) return this.miss(`sink ${u.pathname}`, cors);
    const original = `https://${host}${u.pathname}${u.search}`;
    const method = req.method === "HEAD" ? "GET" : req.method;
    const key = `${method} ${original}`;
    let entry = this.index[key] ?? (this.mode === "replay" ? this.nearest(method, host, u.pathname) : undefined);
    if (!entry && this.mode === "record") entry = await this.record(key, req, original);
    if (!entry) return this.miss(original, cors);
    const type = header(entry, "content-type");
    this.served.push({ url: original, status: entry.status, type });
    if (entry.error) return new Response(null, { status: 502, headers: cors });
    const headers = entry.headers.filter(([k]) => !DROP_HEADERS.test(k)).map(([k, v]): [string, string] => [k, k.toLowerCase() === "location" ? rewriteLocation(v, original, this.portOf) : v]);
    const raw = entry.body ? new Uint8Array(readFileSync(join(this.dir, "bodies", entry.body))) : new Uint8Array();
    const body = rewriteBody(raw, bodyKind(type, original), this.portOf);
    const nobody = req.method === "HEAD" || entry.status === 204 || entry.status === 304 || (entry.status >= 300 && entry.status < 400);
    return new Response(nobody ? null : body, { status: entry.status, headers: [...headers, ...cors] });
  }

  private miss(what: string, cors: [string, string][]): Response {
    this.misses.push(what);
    return new Response(null, { status: 404, headers: cors });
  }

  // A replayed request whose query differs (a cache buster) gets the stored one with the same path.
  private nearest(method: string, host: string, path: string): Entry | undefined {
    const prefix = `${method} https://${host}${path}?`;
    const exact = `${method} https://${host}${path}`;
    const keys = Object.keys(this.index).filter((k) => k === exact || k.startsWith(prefix)).sort();
    return keys.length ? this.index[keys[0]] : undefined;
  }

  private async record(key: string, req: Request, original: string): Promise<Entry> {
    const host = new URL(original).host;
    await this.acquire(host);
    try {
      if (this.index[key]) return this.index[key];
      const entry = await fetchLive(req, original, this.hosts, this.base);
      if (entry.body !== null && entry.body.length > 0) {
        const bytes = Buffer.from(entry.body, "base64");
        const name = new Bun.CryptoHasher("sha1").update(bytes).digest("hex");
        writeFileSync(join(this.dir, "bodies", name), bytes);
        entry.body = name;
      } else entry.body = null;
      this.index[key] = entry;
      return entry;
    } finally {
      this.release(host);
    }
  }

  private async acquire(host: string) {
    while ((this.busy.get(host) ?? 0) >= PER_HOST) await new Promise<void>((r) => this.waiting.set(host, [...(this.waiting.get(host) ?? []), r]));
    this.busy.set(host, (this.busy.get(host) ?? 0) + 1);
  }

  private release(host: string) {
    this.busy.set(host, (this.busy.get(host) ?? 1) - 1);
    const next = this.waiting.get(host)?.shift();
    if (next) next();
  }
}

function header(e: Entry, name: string): string {
  return e.headers.find(([k]) => k.toLowerCase() === name)?.[1] ?? "";
}

function rewriteLocation(v: string, from: string, portOf: (h: string) => number): string {
  const abs = new URL(v, from);
  return `http://${LOOPBACK}:${portOf(abs.host)}${abs.pathname}${abs.search}${abs.hash}`;
}

// Loopback URLs in the request's referer and origin are mapped back to the original hosts.
function unmap(v: string | null, hosts: string[], base: number): string | null {
  if (!v) return null;
  return v.replace(/http:\/\/127\.0\.0\.1:(\d+)/g, (all, p: string) => {
    const h = hosts[Number(p) - base];
    return h ? `https://${h}` : all;
  });
}

async function fetchLive(req: Request, url: string, hosts: string[], base: number): Promise<Entry> {
  const headers: Record<string, string> = { "user-agent": UA, "accept-language": "en-US,en;q=0.9", accept: req.headers.get("accept") ?? "*/*" };
  for (const [k, v] of [["referer", unmap(req.headers.get("referer"), hosts, base)], ["origin", unmap(req.headers.get("origin"), hosts, base)], ["content-type", req.headers.get("content-type")]] as const) if (v) headers[k] = v;
  try {
    const body = req.method === "GET" || req.method === "HEAD" ? undefined : await req.arrayBuffer();
    const res = await fetch(url, { method: req.method === "HEAD" ? "GET" : req.method, headers, body, redirect: "manual", signal: AbortSignal.timeout(20_000) });
    const list = [...res.headers.entries()] as [string, string][];
    const type = res.headers.get("content-type") ?? "";
    const big = Number(res.headers.get("content-length") ?? 0) > MAX_BODY || /^(video|audio)\//i.test(type);
    if (big) {
      await res.body?.cancel();
      return { status: res.status, headers: list, body: null, error: undefined };
    }
    const bytes = new Uint8Array(await res.arrayBuffer());
    return { status: res.status, headers: list, body: Buffer.from(bytes.subarray(0, MAX_BODY)).toString("base64") };
  } catch (e) {
    return { status: 0, headers: [], body: null, error: e instanceof Error ? e.message : String(e) };
  }
}
