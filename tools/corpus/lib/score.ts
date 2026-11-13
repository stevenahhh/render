// Page score, 0-100, from three parts measured on the first two screens (y < 1600):
//   text   - WebKit's words found in ours (half credit) and in place (half credit, full
//            within 8 px, none past 100 px), as an F-measure against our words in the region;
//   layout - size-weighted IoU of the landmark and large-block boxes WebKit reports;
//   pixels - SSIM of the two 1280x800 screens, each scaled to 320x200 grayscale.
// total = 40 text + 30 layout + 30 pixels.
import type { RefBox, Word } from "./webkit.ts";
import { REGION, WIDTH } from "./webkit.ts";

export const WEIGHTS = { text: 0.4, layout: 0.3, pixels: 0.3 } as const;
const NEAR = 8;
const FAR = 100;

const norm = (w: string) => w.normalize("NFC").toLowerCase();
const inRegion = (w: Word) => w[2] < REGION && w[1] < WIDTH && w[2] + w[4] > 0 && w[1] + w[3] > 0;

export type TextScore = { score: number; recall: number; precision: number; missingChars: number; misplacedChars: number; totalChars: number; missingWords: string[] };

export function textScore(ref: Word[], ours: Word[]): TextScore {
  const want = ref.filter(inRegion);
  const byKey = new Map<string, Word[]>();
  for (const w of ours) {
    const k = norm(w[0]);
    byKey.set(k, [...(byKey.get(k) ?? []), w]);
  }
  const used = new Set<Word>();
  let credit = 0, total = 0, missingChars = 0, misplacedChars = 0;
  const missingWords: string[] = [];
  const pairs: { r: Word; o: Word; d: number }[] = [];
  for (const r of want) {
    for (const o of byKey.get(norm(r[0])) ?? []) pairs.push({ r, o, d: Math.abs(r[1] - o[1]) + Math.abs(r[2] - o[2]) });
  }
  pairs.sort((a, b) => a.d - b.d);
  const matched = new Map<Word, number>();
  for (const { r, o, d } of pairs) {
    if (matched.has(r) || used.has(o)) continue;
    matched.set(r, d);
    used.add(o);
  }
  for (const r of want) {
    const len = r[0].length;
    total += len;
    const d = matched.get(r);
    if (d === undefined) {
      missingChars += len;
      if (missingWords.length < 30) missingWords.push(r[0]);
      continue;
    }
    const place = d <= NEAR ? 1 : d >= FAR ? 0 : 1 - (d - NEAR) / (FAR - NEAR);
    if (place < 1) misplacedChars += len * (1 - place);
    credit += len * (0.5 + 0.5 * place);
  }
  const oursInRegion = ours.filter(inRegion);
  const oursChars = oursInRegion.reduce((s, w) => s + w[0].length, 0);
  const matchedOurs = oursInRegion.filter((w) => used.has(w)).reduce((s, w) => s + w[0].length, 0);
  if (total === 0) return { score: oursChars === 0 ? 1 : 0.5, recall: 1, precision: oursChars === 0 ? 1 : 0, missingChars, misplacedChars, totalChars: 0, missingWords };
  const recall = credit / total;
  const precision = oursChars === 0 ? 0 : matchedOurs / oursChars;
  const score = recall + precision === 0 ? 0 : (2 * recall * precision) / (recall + precision);
  return { score, recall, precision, missingChars, misplacedChars, totalChars: total, missingWords };
}

export type BoxResult = { sel: string; tag: string; iou: number; weight: number; ours: [number, number, number, number] | "nobox" | "nomatch"; ref: RefBox };
export type LayoutScore = { score: number; boxes: BoxResult[] };

function iou(a: [number, number, number, number], b: [number, number, number, number]): number {
  const clip = (r: [number, number, number, number]) => [Math.max(0, r[0]), Math.max(0, r[1]), Math.min(WIDTH, r[0] + r[2]), Math.min(REGION, r[1] + r[3])];
  const [ax0, ay0, ax1, ay1] = clip(a);
  const [bx0, by0, bx1, by1] = clip(b);
  const inter = Math.max(0, Math.min(ax1, bx1) - Math.max(ax0, bx0)) * Math.max(0, Math.min(ay1, by1) - Math.max(ay0, by0));
  const area = (ax1 - ax0) * (ay1 - ay0) + (bx1 - bx0) * (by1 - by0) - inter;
  return area <= 0 ? 0 : inter / area;
}

export function layoutScore(ref: RefBox[], ours: (BoxResult["ours"])[]): LayoutScore {
  const boxes = ref.map((r, i) => {
    const o = ours[i] ?? "nomatch";
    const visible = Math.max(1, Math.min(WIDTH, r.rect[0] + r.rect[2]) - Math.max(0, r.rect[0])) * Math.max(1, Math.min(REGION, r.rect[1] + r.rect[3]) - Math.max(0, r.rect[1]));
    return { sel: r.sel, tag: r.tag, iou: typeof o === "string" ? 0 : iou(r.rect, o), weight: Math.sqrt(visible), ours: o, ref: r };
  });
  const wsum = boxes.reduce((s, b) => s + b.weight, 0);
  return { score: wsum === 0 ? 1 : boxes.reduce((s, b) => s + b.weight * b.iou, 0) / wsum, boxes };
}

export function total(text: number, layout: number, pixels: number): number {
  return 100 * (WEIGHTS.text * text + WEIGHTS.layout * layout + WEIGHTS.pixels * pixels);
}
