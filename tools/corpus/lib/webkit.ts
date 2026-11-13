// The WebKit side: load a page in a headless Bun.WebView, let it settle, and read what a
// person sees in the first two screens (y < 1600 at 1280x800): word rects and the boxes of
// landmarks and large blocks, addressed by :nth-of-type paths our --boxes can resolve.

export const WIDTH = 1280;
export const HEIGHT = 800;
export const REGION = 2 * HEIGHT;

export type Word = [text: string, x: number, y: number, w: number, h: number];
export type RefBox = { sel: string; tag: string; landmark: boolean; rect: [number, number, number, number]; display: string; position: string; float: string; parent: string };
export type Snapshot = { title: string; height: number; words: Word[]; boxes: RefBox[]; errors: string[] };

const COLLECT = `(() => {
  const REGION = ${REGION}, W = ${WIDTH}, sx = scrollX, sy = scrollY;
  const r1 = (v) => Math.round(v * 10) / 10;
  const path = (el) => {
    const parts = [];
    for (let e = el; e && e !== document.documentElement; e = e.parentElement) {
      let i = 1;
      for (let s = e.previousElementSibling; s; s = s.previousElementSibling) if (s.localName === e.localName) i++;
      parts.unshift(e.localName + ":nth-of-type(" + i + ")");
    }
    return "html > " + parts.join(" > ");
  };
  const words = [];
  const root = document.body || document.documentElement;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const range = document.createRange();
  const skip = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "TITLE", "OPTION"]);
  for (let n = walker.nextNode(); n && words.length < 8000; n = walker.nextNode()) {
    const p = n.parentElement;
    if (!p || skip.has(p.tagName.toUpperCase()) || !/\\S/.test(n.data)) continue;
    range.selectNodeContents(n);
    const br = range.getBoundingClientRect();
    if (br.width < 1 || br.height < 1 || br.top + sy >= REGION || br.bottom + sy <= 0 || br.left + sx >= W || br.right + sx <= 0) continue;
    if (getComputedStyle(p).visibility !== "visible") continue;
    const re = /\\S+/g;
    for (let m = re.exec(n.data); m && words.length < 8000; m = re.exec(n.data)) {
      range.setStart(n, m.index);
      range.setEnd(n, m.index + m[0].length);
      const r = range.getClientRects()[0];
      if (!r || r.width < 1 || r.height < 1) continue;
      const x = r.left + sx, y = r.top + sy;
      if (y >= REGION || x >= W || y + r.height <= 0 || x + r.width <= 0) continue;
      words.push([m[0], r1(x), r1(y), r1(r.width), r1(r.height)]);
    }
  }
  const LAND = new Set(["header", "nav", "main", "footer", "aside", "h1", "h2", "h3", "img", "video", "iframe", "canvas", "form", "input", "button", "table", "svg", "select", "textarea"]);
  const land = [], big = [];
  for (const el of root.querySelectorAll("*")) {
    const tag = el.localName;
    if (el.closest("svg") && tag !== "svg") continue;
    if (skip.has(tag.toUpperCase()) || tag === "br" || tag === "head") continue;
    const r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) continue;
    const x = r.left + sx, y = r.top + sy;
    if (y >= REGION || x >= W || y + r.height <= 0 || x + r.width <= 0) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility !== "visible" || cs.display === "contents") continue;
    const visibleArea = Math.max(0, Math.min(W, x + r.width) - Math.max(0, x)) * Math.max(0, Math.min(REGION, y + r.height) - Math.max(0, y));
    const isLand = LAND.has(tag) || ["banner", "navigation", "main", "contentinfo"].includes(el.getAttribute("role"));
    if (!isLand && (cs.display.startsWith("inline") && cs.display !== "inline-block" || visibleArea < 30000)) continue;
    const parent = el.parentElement ? getComputedStyle(el.parentElement).display : "";
    const box = { sel: path(el), tag, landmark: isLand, rect: [r1(x), r1(y), r1(r.width), r1(r.height)], display: cs.display, position: cs.position, float: cs.float, parent, area: visibleArea };
    (isLand ? land : big).push(box);
  }
  big.sort((a, b) => b.area - a.area);
  const boxes = [...land.slice(0, 80), ...big.slice(0, 60)].map(({ area, ...b }) => b);
  return { title: document.title, height: document.documentElement.scrollHeight, words, boxes, errors: (window.__corpusErrors || []).slice(0, 50) };
})()`;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export async function withTimeout<T>(p: Promise<T>, ms: number): Promise<T | "timeout"> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const t = new Promise<"timeout">((r) => (timer = setTimeout(() => r("timeout"), ms)));
  try {
    return await Promise.race([p, t]);
  } finally {
    clearTimeout(timer);
  }
}

// Waits until `activity()` has not changed for `quiet` ms, at most `max` ms.
export async function settle(activity: () => number, quiet = 1000, max = 8000) {
  const start = Date.now();
  let last = activity();
  let since = Date.now();
  while (Date.now() - start < max) {
    await sleep(200);
    const now = activity();
    if (now !== last) {
      last = now;
      since = Date.now();
    } else if (Date.now() - since >= quiet) return;
  }
}

export type Visit = { loaded: "load" | "timeout" | "error"; error?: string; view: InstanceType<typeof Bun.WebView> };

export async function open(url: string, activity: () => number): Promise<Visit> {
  const view = new Bun.WebView({ width: WIDTH, height: HEIGHT });
  try {
    const r = await withTimeout(view.navigate(url), 45_000);
    await settle(activity, 1500, 10_000);
    return { loaded: r === "timeout" ? "timeout" : "load", view };
  } catch (e) {
    return { loaded: "error", error: e instanceof Error ? e.message : String(e), view };
  }
}

export async function scrollTo(view: Visit["view"], y: number, activity: () => number) {
  await view.evaluate(`(scrollTo(0, ${y}), scrollY)`);
  await settle(activity, 800, 5000);
}

export async function snapshot(view: Visit["view"]): Promise<Snapshot> {
  return (await view.evaluate(COLLECT)) as Snapshot;
}

// WebKit renders at device pixel ratio 2; screenshots are kept at CSS size like ours.
export async function screenshot(view: Visit["view"], out: string) {
  const png = await view.screenshot({ format: "png", encoding: "buffer" });
  await new Bun.Image(png).resize(WIDTH, HEIGHT, { fit: "fill" }).png().write(out);
}
