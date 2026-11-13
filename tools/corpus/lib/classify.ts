// Names why a page lost points. Each page's lost points (100 - score) are split equally
// among the cause categories found on it, and within a category equally among its causes.
import type { LayoutScore, TextScore } from "./score.ts";
import type { Outcome, OursBoxes } from "./ours.ts";

export type Cause = { category: string; cause: string };

// Dropped CSS that cannot change a still screenshot (interaction, motion, other engines' prefixes).
const INERT_DECL = /^(cursor|pointer-events|(-webkit-)?user-select|will-change|touch-action|-webkit-tap-highlight-color|resize|scroll-behavior|scroll-snap-.*|scroll-margin.*|scroll-padding.*|overscroll-behavior.*|-webkit-font-smoothing|text-rendering|transition.*|animation.*|caret-color|speak|zoom|-moz-.*|-ms-.*|-o-.*|print-color-adjust|-webkit-print-color-adjust|backface-visibility|-webkit-backface-visibility|contain-intrinsic-size|interpolate-size|view-transition-name)$/;
const INERT_PSEUDO = /^(hover|focus|focus-visible|focus-within|active|visited|target|selection|-moz-.*|-ms-.*|-webkit-autofill|backdrop|user-invalid|user-valid|-webkit-scrollbar.*|-webkit-search-.*|-webkit-inner-spin-button|-webkit-outer-spin-button|cue|picker.*)$/;
const INERT_AT = /^@(charset|namespace|page|-webkit-keyframes|-moz-.*|-ms-.*|view-transition|font-feature-values|position-try|starting-style)$/;

export function inertDrop(key: string): boolean {
  const [kind, name] = [key.slice(0, key.indexOf(" ")), key.slice(key.indexOf(" ") + 1)];
  if (kind === "decl") return INERT_DECL.test(name);
  if (kind === "pseudo-class" || kind === "pseudo-element") return INERT_PSEUDO.test(name);
  if (kind === "at-rule") return INERT_AT.test(name);
  return false;
}

// The same exception arrives with different prefixes (a script URL, "error:", "uncaught (in promise):")
// and locations; keep the message from its error class on, without the location.
const normError = (e: string) => {
  const body = e.replace(/^\[js\] /, "");
  const at = body.search(/\b[A-Z]\w*(Error|Exception)\b|\bError:/);
  return (at >= 0 ? body.slice(at) : body)
    .replace(/\s*\((native|unknown at [^)]*|https?:[^)]*|<url>[^)]*)\)\s*$/, "")
    .replace(/https?:\/\/\S+/g, "<url>")
    .replace(/\b\d+(:\d+)*\b/g, "N")
    .replace(/'[^']{20,}'/g, "'…'")
    .slice(0, 110);
};

const FORMS = new Set(["input", "button", "select", "textarea", "form"]);

export type PageEvidence = {
  outcome: Outcome;
  boxes: OursBoxes;
  webkitErrors: string[];
  uaDrops: Set<string>;
  text: TextScore;
  layout: LayoutScore;
  misses: string[];
  servedTypes: string[];
};

export function classify(e: PageEvidence): Cause[] {
  if (e.outcome !== "ok") return [{ category: "process crash/panic/timeout", cause: `renderer ${e.outcome}` }];
  const out: Cause[] = [];
  const known = new Set(e.webkitErrors.map(normError));
  for (const err of new Set(e.boxes.jsErrors.map(normError))) if (!known.has(err)) out.push({ category: "uncaught script errors", cause: err });
  for (const key of Object.keys(e.boxes.cssDrops)) if (!e.uaDrops.has(key) && !inertDrop(key)) out.push({ category: "CSS dropped by our parser", cause: key });
  const weak = e.layout.boxes.filter((b) => b.iou < 0.5);
  for (const b of weak) {
    const tag = b.tag;
    if (["video", "canvas", "iframe"].includes(tag)) out.push({ category: "unsupported elements", cause: `<${tag}>` });
    else if (tag === "svg") out.push({ category: "unsupported elements", cause: "inline <svg>" });
    else if (FORMS.has(tag)) out.push({ category: "unsupported elements", cause: "form controls" });
    else if (tag === "img" && b.ours === "nobox") {
      const odd = [...new Set(e.servedTypes.filter((t) => /webp|gif/i.test(t)))].map((t) => t.replace("image/", ""));
      out.push({ category: "missing images/fonts", cause: odd.length ? `<img> not drawn (served ${odd.join("/")}, not decoded)` : "<img> not drawn" });
    } else if (b.ours === "nomatch") out.push({ category: "DOM differs from WebKit", cause: "element missing (scripts built a different DOM)" });
    else {
      const r = b.ref;
      const construct = r.position !== "static" && r.position !== "relative" ? `position:${r.position}` : r.float !== "none" ? "float" : /grid/.test(r.parent) ? "grid item" : /flex/.test(r.parent) ? "flex item" : /table/.test(r.display) || /table/.test(r.parent) ? "table" : /grid|flex/.test(r.display) ? `${r.display} container` : "block flow";
      out.push({ category: "layout diffs", cause: construct });
    }
  }
  const missed = new Map<string, number>();
  for (const m of e.misses) {
    const kind = /\.(png|jpe?g|gif|webp|avif|svg)(\?|$)/i.test(m) ? "image" : /\.(woff2?|ttf|otf)(\?|$)/i.test(m) ? "font" : /\.css(\?|$)/i.test(m) ? "stylesheet" : /\.m?js(\?|$)/i.test(m) ? "script" : null;
    if (kind) missed.set(kind, (missed.get(kind) ?? 0) + 1);
  }
  for (const kind of missed.keys()) out.push({ category: "missing images/fonts", cause: `${kind} not in frozen store (URL varies per load)` });
  if (e.text.totalChars > 0 && e.text.missingChars / e.text.totalChars > 0.2) out.push({ category: "text not laid out", cause: "over 20% of WebKit's text absent" });
  const seen = new Set<string>();
  return out.filter((c) => !seen.has(c.category + c.cause) && seen.add(c.category + c.cause));
}

export type Attributed = Cause & { points: number };

export function attribute(lost: number, causes: Cause[]): Attributed[] {
  const cats = [...new Set(causes.map((c) => c.category))];
  if (cats.length === 0) return [{ category: "unclassified", cause: "no cause detected", points: lost }];
  return causes.map((c) => ({ ...c, points: lost / cats.length / causes.filter((d) => d.category === c.category).length }));
}
